//! dino-tui: a minimal, keyboard-only terminal client for dinod. See docs/SPEC.md.

mod app;
mod attach;
mod config;
mod conn;
mod emulate;
mod format;
mod input;
mod live;
mod order;
mod screen;
mod settings;
mod setup;
mod sidebar;
#[cfg(test)]
mod snapshots;
mod stats;
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

use app::{App, Event, Job, Line, Overlay, PickFor, Want};
use attach::Bar;

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

pub type Term = Terminal<CrosstermBackend<io::Stdout>>;

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
pub fn fresh(term: &mut Term) -> io::Result<()> {
    execute!(io::stdout(), terminal::Clear(terminal::ClearType::All), cursor::MoveTo(0, 0))?;
    *term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok(())
}

/// Requests (and jobs) go out on threads of their own; each answer wakes the loop.
pub fn dispatch(app: &mut App, waker: &wake::Waker<Event>) {
    for (tag, req) in app.outbox.drain(..) {
        let w = waker.clone();
        std::thread::spawn(move || w.send(Event::Reply(tag, conn::request(&req))));
    }
    for (tag, req) in app.outbox_raw.drain(..) {
        let w = waker.clone();
        std::thread::spawn(move || w.send(Event::Raw(tag, conn::raw(&req))));
    }
    for (tag, job) in app.jobs.drain(..) {
        let w = waker.clone();
        std::thread::spawn(move || w.send(Event::Job(tag, run_job(job))));
    }
}

/// A job: one setting changed in dinod's settings (read, changed, written back as plain JSON, so
/// nothing else in them changes), or `dino service`.
fn run_job(job: Job) -> Result<String, String> {
    match job {
        Job::Setting(path, value) => {
            let got = conn::raw(&serde_json::json!({"type": "settings"})).map_err(|e| format!("dinod: {e}"))?;
            if let Some(e) = conn::raw_error(&got) {
                return Err(format!("dinod: {e}"));
            }
            let mut settings = got["settings"].clone();
            let (last, parents) = path.split_last().ok_or("no setting")?;
            let mut at = &mut settings;
            for p in parents {
                at = at.as_object_mut().ok_or("dinod's settings aren't a table")?.entry(p.to_string()).or_insert(serde_json::json!({}));
            }
            at.as_object_mut().ok_or("dinod's settings aren't a table")?.insert(last.to_string(), value);
            let said = conn::raw(&serde_json::json!({"type": "set_settings", "settings": settings})).map_err(|e| format!("dinod: {e}"))?;
            match conn::raw_error(&said) {
                Some(e) => Err(format!("dinod: {e}")),
                None => Ok(String::new()),
            }
        }
        Job::Service(what) => {
            let dino = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("dino")).find(|f| f.is_file())).ok_or("dino isn't on your PATH")?;
            let out = std::process::Command::new(dino).args(["service", what]).stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
            let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
            if out.status.success() {
                Ok(text(&out.stdout))
            } else {
                let e = text(&out.stderr);
                Err(if e.is_empty() { text(&out.stdout) } else { e })
            }
        }
    }
}

/// `v` (`0.1.12`, `0.1.12-dev`) is older than `min`.
fn version_below(v: &str, min: (u32, u32, u32)) -> bool {
    let mut parts = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u32>().unwrap_or(0));
    let got = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    got < min
}

fn event_loop(term: &mut Term, app: &mut App, waker: &wake::Waker<Event>, wake: &mut wake::Wake<Event>) -> anyhow::Result<()> {
    loop {
        dispatch(app, waker);
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
            term_events(app)?;
        }
        if ready[1] {
            wake.drain();
            while let Ok(ev) = wake.rx.try_recv() {
                app.on_event(ev);
            }
            // A size change: ratatui redraws at the new size. crossterm takes its own word of it
            // now, while no key waits (see `term_events`).
            if terminal::size().ok() != Some(app.size) {
                app.dirty = true;
                term_events(app)?;
            }
        }
        app.tick(Instant::now());
    }
}

/// What the terminal sent, as crossterm reads it. crossterm waits on the terminal and on its own
/// word of size changes (SIGWINCH) together; given both, it answers with the size change and the
/// terminal's readiness is gone until more comes, so a key typed then waited for the next one. So
/// it's also asked as soon as the size changed, and after an attach (which reads the terminal
/// itself, leaving crossterm only the size changes meanwhile).
fn term_events(app: &mut App) -> io::Result<()> {
    while event::poll(Duration::ZERO)? {
        app.on_term(event::read()?);
    }
    Ok(())
}

/// What the attach asks of the app while it runs.
struct Host<'a> {
    app: &'a mut App,
    wake: &'a mut wake::Wake<Event>,
    waker: &'a wake::Waker<Event>,
    id: String,
}

impl attach::Host for Host<'_> {
    fn woke(&mut self) -> Option<Bar> {
        while let Ok(ev) = self.wake.rx.try_recv() {
            self.app.on_event(ev);
        }
        // Requests made meanwhile (none from keys: those go to the session) still go out.
        dispatch(self.app, self.waker);
        self.app.config.status_line.then(|| status_bar(self.app, &self.id))
    }

    fn neighbour(&self, id: &str, which: app::Neighbour) -> Option<String> {
        self.app.neighbour(id, which)
    }
}

/// The status line while attached without the sidebar: the session and its mark, what needs you
/// elsewhere, the live rate, the Claude account in use; the way back on the right.
fn status_bar(app: &App, id: &str) -> Bar {
    let caret = app.config.prefix().caret();
    let mut b = Bar::default();
    let Some(s) = app.sessions.iter().find(|s| s.id == id) else {
        b.left.push((format!(" {id}"), ""));
        return b;
    };
    let (mark, style) = sidebar::mark(s);
    let sgr = match (mark, style.fg) {
        ("!", _) => "38;5;214;1",
        (_, Some(ratatui::style::Color::Green)) => "32",
        (_, Some(ratatui::style::Color::Blue)) => "34",
        (_, Some(ratatui::style::Color::Red)) => "31",
        _ => "",
    };
    b.left.push((" ".into(), ""));
    b.left.push((mark.into(), sgr));
    b.left.push((" ".into(), ""));
    b.left.push((format::name(s), "1"));
    let others = app.needs_count().saturating_sub((Status::of(s) == Status::NeedsYou) as usize);
    if others > 0 {
        b.left.push(("  ".into(), ""));
        b.left.push(("!".into(), "38;5;214;1"));
        b.left.push((format!(" {}", format::need_you(others)), ""));
    }
    if let Some(r) = format::rate(app.rate) {
        b.left.push((format!("   {r}"), ""));
    }
    if let Some(a) = app.account_line() {
        b.left.push((format!("  {a}"), "2"));
    }
    let next = if others > 0 { format!("{caret} N next !") } else { format!("{caret} n next") };
    b.right.push((format!("{caret} d list · {next}"), "90"));
    b
}

fn attach(term: &mut Term, app: &mut App, waker: &wake::Waker<Event>, wake: &mut wake::Wake<Event>, mut id: String) -> anyhow::Result<()> {
    let prefix = app.config.prefix();
    let bell = waker_bell(waker);
    let mut focus_sidebar = false;
    loop {
        term.backend_mut().flush()?;
        let wide = terminal::size().is_ok_and(|s| s.0 >= sidebar::MIN_COLS);
        app.side.attached = Some(id.clone());
        app.side.cursor = sidebar::Cursor::Attached;
        let outcome = if app.config.sidebar && wide {
            app.side.focus = std::mem::take(&mut focus_sidebar);
            emulate::run(term, app, waker, wake, &id)
        } else {
            let status_line = app.config.status_line;
            let mut host = Host { app, wake, waker, id: id.clone() };
            attach::run(&attach::Opts { id: &id, prefix: &prefix, status_line, sidebar: host.app.config.sidebar }, host.wake.fd(), bell.clone(), &mut host)
        };
        match outcome {
            // The sidebar off or on: remembered for the next attach and the next start.
            attach::Outcome::Toggle | attach::Outcome::SidebarFocus => {
                focus_sidebar = outcome == attach::Outcome::SidebarFocus;
                app.config.sidebar = focus_sidebar || !app.config.sidebar;
                if let Err(e) = app.config.save() {
                    app.flash_err(format!("the sidebar's choice isn't saved: {e}"));
                }
                continue;
            }
            attach::Outcome::Remode => continue,
            attach::Outcome::Switch(next) => {
                id = next;
                continue;
            }
            _ => {}
        }
        // Back on the list's screen, drawn whole again; the size changes crossterm saw meanwhile taken.
        let _ = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, cursor::Hide);
        fresh(term)?;
        term_events(app)?;
        app.dirty = true;
        app.selected = Some(id.clone());
        app.rebuild();
        app.back_from_setup_shell();
        match outcome {
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
            _ => {}
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
