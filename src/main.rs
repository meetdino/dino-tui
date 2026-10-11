//! dino-tui: a minimal, keyboard-only terminal client for dinod. See docs/SPEC.md.

mod app;
mod attach;
mod config;
mod conn;
mod format;
mod input;
mod live;
mod order;
mod settings;
#[cfg(test)]
mod snapshots;
mod ui;
mod wake;

use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use dino_core::ipc::{Request, Response};
use dino_core::status::Status;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::{App, Event, Line, Overlay, PickFor, Want};

const HELP: &str = "dino-tui: a minimal, keyboard-only terminal client for dinod.

usage: dino-tui [--version]

Talks to the dinod `dino` uses ($DINO_HOME, else ~/.config/dino); it never starts one.
In it, ? lists the keys. Its own config: ~/.config/dino-tui/config.toml (`:set` changes it).
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            return;
        }
        Some("-V" | "--version" | "version") => {
            println!("dino-tui {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some(a) => {
            eprintln!("dino-tui doesn't take `{}`\n\n{HELP}", format::printable(a));
            std::process::exit(2);
        }
        None => {}
    }
    if let Err(e) = run() {
        eprintln!("dino-tui: {e:#}");
        std::process::exit(1);
    }
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// The terminal as the list wants it: raw, the alternate screen, pastes as one.
fn enter() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, cursor::Hide)
}

fn leave() {
    let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();
}

fn run() -> anyhow::Result<()> {
    let config = config::Config::load()?;
    // dinod first: if it isn't there, say how to start it, before touching the terminal.
    let mut c = conn::Conn::open().map_err(|_| anyhow::anyhow!(conn::not_running()))?;
    let dinod = match c.request(&Request::Version)? {
        Response::Version { dino, .. } => dino,
        _ => "?".into(),
    };
    drop(c);
    let (waker, mut wake) = wake::new::<Event>()?;
    let mut app = App::new(config, dinod);
    if version_below(&app.dinod, (0, 1, 11)) {
        app.flash(format!("dinod {} is older than dino-tui expects (0.1.11): some keys won't work. Update dino.", app.dinod));
    }
    app.start();
    live::start(waker.clone());

    enter()?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        hook(info);
    }));
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    fresh(&mut term)?;
    let result = event_loop(&mut term, &mut app, &waker, &mut wake);
    leave();
    result
}

/// A blank screen, drawn whole at the next draw. Not ratatui's `clear`, which asks the terminal
/// where its cursor is and waits for the answer.
fn fresh(term: &mut Term) -> io::Result<()> {
    execute!(io::stdout(), terminal::Clear(terminal::ClearType::All), cursor::MoveTo(0, 0))?;
    *term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok(())
}

/// `v` (`0.1.12`, `0.1.12-dev`) is older than `min`.
fn version_below(v: &str, min: (u32, u32, u32)) -> bool {
    let mut parts = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u32>().unwrap_or(0));
    let got = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    got < min
}

fn event_loop(term: &mut Term, app: &mut App, waker: &wake::Waker<Event>, wake: &mut wake::Wake<Event>) -> anyhow::Result<()> {
    loop {
        // Requests go out on threads of their own; each answer wakes the loop.
        for (tag, req) in app.outbox.drain(..) {
            let w = waker.clone();
            std::thread::spawn(move || w.send(Event::Reply(tag, conn::request(&req))));
        }
        match app.want.take() {
            Some(Want::Quit) => return Ok(()),
            Some(Want::Attach(id)) => {
                attach(term, app, waker, wake, id)?;
                continue;
            }
            Some(Want::Settings) => {
                leave();
                let said = settings::edit(&mut |v| conn::raw(v));
                enter()?;
                fresh(term)?;
                match said {
                    Ok(s) => app.flash(s),
                    Err(e) => app.flash_err(format!("{e:#}")),
                }
                continue;
            }
            None => {}
        }
        if app.bell && matches!(app.screen, app::Screen::List) {
            let _ = io::stdout().write_all(b"\x07");
        }
        app.bell = false;
        if app.dirty {
            app.dirty = false;
            term.draw(|f| ui::draw(f, app))?;
        }
        let timeout = app.deadline().map(|t| t.saturating_duration_since(Instant::now()));
        let ready = wake::poll(&[0, wake.fd()], timeout)?;
        if ready[0] {
            // Everything the terminal sent, as crossterm reads it.
            while event::poll(Duration::ZERO)? {
                app.on_term(event::read()?);
            }
        }
        if ready[1] {
            wake.drain();
            while let Ok(ev) = wake.rx.try_recv() {
                app.on_event(ev);
            }
            // A size change: ratatui redraws at the new size.
            if terminal::size().ok() != Some(app.size) {
                app.dirty = true;
            }
        }
        app.tick(Instant::now());
    }
}

/// What the attach asks of the app while it runs.
struct Host<'a> {
    app: &'a mut App,
    wake: &'a mut wake::Wake<Event>,
    id: String,
}

impl attach::Host for Host<'_> {
    fn woke(&mut self) -> Option<String> {
        while let Ok(ev) = self.wake.rx.try_recv() {
            self.app.on_event(ev);
        }
        // Requests made meanwhile (none from keys: those go to the session) still go out.
        self.app.config.status_line.then(|| status_text(self.app, &self.id))
    }

    fn neighbour(&self, id: &str, which: app::Neighbour) -> Option<String> {
        self.app.neighbour(id, which)
    }
}

/// The status line while attached: the session, what it's doing, the way back.
fn status_text(app: &App, id: &str) -> String {
    let Some(s) = app.sessions.iter().find(|s| s.id == id) else { return format!(" {id}") };
    let st = match Status::of(s) {
        Status::NeedsYou => "needs you",
        Status::Working => "working",
        Status::Done => "done",
        Status::Idle => "idle",
        Status::Ended => "ended",
        Status::Exited => "exited",
    };
    let others = app.needs_count().saturating_sub((Status::of(s) == Status::NeedsYou) as usize);
    let needs = if others > 0 { format!(" · {} elsewhere", format::need_you(others)) } else { String::new() };
    let info = [Some(format::printable(format::agent(s))), format::model(s), format::mode(s)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let (name, prefix) = (format::name(s), app.config.prefix().label);
    format!(" {name} · {info} · {st}{needs}   {prefix} d list · {prefix} n/p next/prev")
}

fn attach(term: &mut Term, app: &mut App, waker: &wake::Waker<Event>, wake: &mut wake::Wake<Event>, mut id: String) -> anyhow::Result<()> {
    let prefix = app.config.prefix();
    let status_line = app.config.status_line;
    let bell = waker_bell(waker);
    loop {
        term.backend_mut().flush()?;
        let outcome = {
            let mut host = Host { app, wake, id: id.clone() };
            attach::run(&attach::Opts { id: &id, prefix: &prefix, status_line }, host.wake.fd(), bell.clone(), &mut host)
        };
        // Back on the list's screen, drawn whole again.
        let _ = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, cursor::Hide);
        fresh(term)?;
        app.dirty = true;
        app.selected = Some(id.clone());
        app.rebuild();
        match outcome {
            attach::Outcome::Switch(next) => {
                id = next;
                continue;
            }
            attach::Outcome::Detach => {}
            attach::Outcome::Picker => {
                app.selected = Some(id.clone());
                app.rebuild();
                app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('m'), crossterm::event::KeyModifiers::NONE));
                if !matches!(app.overlay, Some(Overlay::Picker(ref p)) if p.target == PickFor::Session(id.clone())) {
                    app.overlay = None;
                }
            }
            attach::Outcome::Command => app.line = Line::Ask(app::Ask::Command, input::Input::default()),
            attach::Outcome::Gone => app.flash(format!("session {id} is gone")),
            attach::Outcome::Failed(e) => app.flash_err(format!("can't attach to {id}: {}", format::printable(&e))),
            attach::Outcome::Hangup => app.want = Some(Want::Quit),
        }
        return Ok(());
    }
}

/// The wake socket's writing end, for the attach's reattach thread.
fn waker_bell(waker: &wake::Waker<Event>) -> Arc<std::os::unix::net::UnixStream> {
    waker.bell()
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_compare() {
        assert!(super::version_below("0.1.9", (0, 1, 11)));
        assert!(!super::version_below("0.1.11", (0, 1, 11)));
        assert!(!super::version_below("0.1.12-dev", (0, 1, 11)));
        assert!(!super::version_below("0.2.0", (0, 1, 11)));
    }
}
