//! Attached with the sidebar: the session's bytes go through the screen (screen.rs) and are drawn
//! beside the sidebar, the session sized to the columns right of it. Keys go to the session as
//! they came, but for the prefix (attach.rs's `Keys`); mouse reports move past the sidebar.
//!
//! One thread, as the raw passthrough: it waits on the terminal, the wake socket (dinod's
//! long-polls, size changes) and the session's stream, and draws when something changed. A flood
//! of output is drawn at most 60 times a second; a little output after a quiet spell (an echo, a
//! keystroke's answer) is drawn at once.

use std::io::BufReader;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use dino_core::ipc::{self, Request, Response};
use ratatui::layout::Rect;

use crate::app::{App, Event, Neighbour};
use crate::attach::{self, Act, Keys, Outcome, Reattach};
use crate::screen::{Ask, Screen, shift_mouse};
use crate::sidebar::{self, AGENT_X, MIN_COLS};
use crate::wake::{Wake, Waker};
use crate::{Term, conn, wake};

/// The fastest a flood is drawn.
const FRAME: Duration = Duration::from_micros(16_667);
/// Output up to this much since the last frame is drawn at once, frame or not: an echo, a prompt.
const SMALL: usize = 4096;

/// The session's size beside the sidebar, for a terminal of `size`.
pub fn session_size(size: (u16, u16)) -> (u16, u16) {
    (size.0.saturating_sub(AGENT_X).max(1), size.1.max(1))
}

/// The real terminal's pixels per cell, for a program that asks.
fn cell_px() -> (u16, u16) {
    let mut ws = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCGWINSZ writes the winsize it's given.
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 && ws.ws_row > 0 { (ws.ws_xpixel / ws.ws_col, ws.ws_ypixel / ws.ws_row) } else { (0, 0) }
}

/// Keys for the sidebar while it has the keyboard: letters, Enter, Esc, the arrows, as a terminal
/// sends them (the kitty keyboard protocol's forms too, which an agent may have turned on).
pub fn side_keys(input: &[u8]) -> Vec<sidebar::Key> {
    use sidebar::Key;
    let mut out = vec![];
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        if let Some((p, f, len)) = attach::csi(rest) {
            i += len;
            let k = match (p, f) {
                (b"" | b"1", b'A') => Some(Key::Up),
                (b"" | b"1", b'B') => Some(Key::Down),
                (b"" | b"1", b'H') => Some(Key::Top),
                (b"" | b"1", b'F') => Some(Key::Bottom),
                _ => match attach::csi_key(p, f) {
                    Some((_, _, true)) => None,
                    Some((27, ..)) => Some(Key::Esc),
                    Some((13, ..)) => Some(Key::Enter),
                    Some((c, mods, _)) => char::from_u32(c).map(|c| Key::Char(if mods.saturating_sub(1) & 1 != 0 { c.to_ascii_uppercase() } else { c })),
                    None => None,
                },
            };
            out.extend(k);
            continue;
        }
        match rest {
            [0x1b, b'O', c, ..] => {
                out.extend(match c {
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    _ => None,
                });
                i += 3;
            }
            [0x1b, ..] => {
                out.push(Key::Esc);
                i += 1;
            }
            [b'\r' | b'\n', ..] => {
                out.push(Key::Enter);
                i += 1;
            }
            [c, ..] if c.is_ascii_graphic() => {
                out.push(Key::Char(*c as char));
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Attach to `id` beside the sidebar until the prefix (or the session) says otherwise. The terminal
/// is in raw mode and on the alternate screen (the list's) when this is called, and is left there.
pub fn run(term: &mut Term, app: &mut App, waker: &Waker<Event>, wake: &mut Wake<Event>, id: &str) -> Outcome {
    let size = crossterm::terminal::size().unwrap_or((80, 24));
    if size.0 < MIN_COLS {
        return Outcome::Remode;
    }
    let (cols, rows) = session_size(size);
    let scrollback = std::env::var("DINO_SCROLLBACK_LIMIT").ok().and_then(|v| v.parse().ok());
    let stream = match UnixStream::connect(conn::socket()).and_then(|mut s| {
        ipc::write_json(&mut s, &Request::Attach { id: id.into(), cols, rows, wait: false, scrollback })?;
        let (_, payload) = ipc::read_frame(&mut s)?;
        match serde_json::from_slice(&payload).map_err(std::io::Error::other)? {
            Response::Ok => Ok(s),
            Response::Error { message } => Err(std::io::Error::other(message)),
            other => Err(std::io::Error::other(format!("unexpected {other:?}"))),
        }
    }) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let out = attach::Out;
    // The real terminal as the session needs it: no bracketed paste until the session asks, focus
    // reports (dinod passes them on), and the wheel sending arrows only while the session's own
    // alternate screen is up (saved, to be put back as it was).
    let _ = out.write(&[b"\x1b[?1049h\x1b[?2004l\x1b[?1007s\x1b[?1007l", attach::FOCUS_ON].concat());
    let _ = crate::fresh(term);
    app.side.attached = Some(id.to_string());
    let mut screen = Screen::new(cols, rows);
    screen.cell_px = cell_px();
    let outcome = relay(term, app, waker, wake, id, stream, size, &mut screen);
    let _ = out.write(&[screen.untell().as_slice(), b"\x1b[?1007r\x1b[0m", attach::FOCUS_OFF].concat());
    app.side.focus = false;
    outcome
}

#[allow(clippy::too_many_arguments)]
fn relay(term: &mut Term, app: &mut App, waker: &Waker<Event>, wake: &mut Wake<Event>, id: &str, stream: UnixStream, mut size: (u16, u16), screen: &mut Screen) -> Outcome {
    let out = attach::Out;
    let prefix = app.config.prefix();
    let bell = waker.bell();
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let mut reader = Some(BufReader::with_capacity(1 << 16, stream));
    let mut keys = Keys::default();
    let mut focus: Option<bool> = None;
    let mut ended = false;
    let mut reattaching: Option<Reattach> = None;
    // After a reattach: the old screen stays until the new one has something to show.
    let mut held: Option<(Vec<u8>, Instant)> = None;
    let mut sent_size = Instant::now().checked_sub(attach::SETTLE).unwrap_or_else(Instant::now);
    let mut settle_by: Option<Instant> = None;
    let mut buf = [0u8; 8192];
    // What changed since the last frame, when it went, and a frame waiting for its time.
    let (mut agent_dirty, mut side_dirty) = (true, true);
    let mut last_frame = Instant::now().checked_sub(FRAME).unwrap_or_else(Instant::now);
    let mut since_frame = 0usize;
    let mut frame_at: Option<Instant> = None;
    let leave = |r: &Option<Reattach>, o: Outcome| {
        if let Some(r) = r {
            r.cancel();
        }
        o
    };
    loop {
        // Requests the state asked for meanwhile (the repo tree, for the sidebar's groups).
        crate::dispatch(app, waker);
        // ---- Draw ----
        let now = Instant::now();
        if let Some(t) = screen.sync_deadline()
            && t <= now
        {
            screen.end_sync();
            agent_dirty = true;
        }
        let in_sync = screen.sync_deadline().is_some();
        if side_dirty || (agent_dirty && !in_sync) {
            if side_dirty || since_frame <= SMALL || now >= last_frame + FRAME {
                if draw(term, app, screen).is_err() {
                    return leave(&reattaching, Outcome::Hangup);
                }
                agent_dirty = false;
                side_dirty = false;
                since_frame = 0;
                last_frame = Instant::now();
                frame_at = None;
            } else {
                frame_at = Some(last_frame + FRAME);
            }
        }
        // ---- Wait ----
        let sock = reader.as_ref().map(|r| r.get_ref().as_raw_fd());
        let buffered = reader.as_ref().is_some_and(|r| !r.buffer().is_empty());
        let mut fds = vec![0, wake.fd()];
        fds.extend(sock);
        let deadline = [settle_by, frame_at.filter(|_| agent_dirty), screen.sync_deadline()].into_iter().flatten().min();
        let timeout = if buffered { Some(Duration::ZERO) } else { deadline.map(|t| t.saturating_duration_since(Instant::now())) };
        let ready = match wake::poll(&fds, timeout) {
            Ok(r) => r,
            Err(e) => return leave(&reattaching, Outcome::Failed(e.to_string())),
        };
        // ---- Keys ----
        if ready[0] {
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            if n == 0 {
                return leave(&reattaching, Outcome::Hangup);
            }
            if n > 0 {
                let (input, focused) = attach::take_focus(&buf[..n as usize]);
                if let Some(on) = focused {
                    focus = Some(on);
                    let _ = ipc::write_frame(&mut writer, ipc::FOCUS, &[on as u8]);
                }
                let (fwd, act) = keys.feed(&input, &prefix);
                if app.side.focus {
                    for k in side_keys(&fwd) {
                        side_dirty = true;
                        match app.side_key(k) {
                            sidebar::Act::None => {}
                            sidebar::Act::Back => app.side.focus = false,
                            sidebar::Act::Switch(to) => return leave(&reattaching, Outcome::Switch(to)),
                            sidebar::Act::Command => return leave(&reattaching, Outcome::Command),
                        }
                    }
                } else if ended {
                    if fwd.contains(&b'\r')
                        && let Ok(Response::Error { message }) = conn::request(&Request::Resume { id: id.into() })
                    {
                        screen.feed(format!("\r\n\x1b[2m{}\x1b[0m\r\n", crate::format::printable(&message)).as_bytes());
                        agent_dirty = true;
                    }
                } else if !fwd.is_empty() {
                    let fwd = if screen.mouse().is_some() { shift_mouse(&fwd, AGENT_X) } else { fwd };
                    if !fwd.is_empty() {
                        // Typing goes back to the live screen.
                        if screen.scrolled() > 0 {
                            screen.scroll_to_bottom();
                            agent_dirty = true;
                        }
                        let _ = ipc::write_frame(&mut writer, ipc::DATA, &fwd);
                    }
                }
                if let Some(act) = act {
                    match act {
                        Act::Detach => return leave(&reattaching, Outcome::Detach),
                        Act::Picker => return leave(&reattaching, Outcome::Picker),
                        Act::Command => return leave(&reattaching, Outcome::Command),
                        Act::Toggle => return leave(&reattaching, Outcome::Toggle),
                        Act::Sidebar => {
                            app.side.focus = !app.side.focus;
                            app.side.cursor = sidebar::Cursor::Attached;
                            side_dirty = true;
                            agent_dirty = true;
                        }
                        Act::PageUp | Act::PageDown => {
                            let half = (screen.size().1 / 2).max(1) as i32;
                            screen.scroll(if act == Act::PageUp { half } else { -half });
                            agent_dirty = true;
                        }
                        Act::Next | Act::Prev | Act::NeedsYou => {
                            let which = match act {
                                Act::Next => Neighbour::Next,
                                Act::Prev => Neighbour::Prev,
                                _ => Neighbour::NeedsYou,
                            };
                            match app.neighbour(id, which) {
                                Some(to) => return leave(&reattaching, Outcome::Switch(to)),
                                None => {
                                    let _ = out.write(b"\x07");
                                }
                            }
                        }
                    }
                }
            }
        }
        // ---- dinod's state, replies, size changes, a reattach ----
        let mut resize = false;
        if settle_by.is_some_and(|t| t <= Instant::now()) {
            settle_by = None;
            resize = true;
        }
        if ready[1] {
            attach::drain(wake.fd());
            while let Ok(ev) = wake.rx.try_recv() {
                app.on_event(ev);
            }
            // The state changed what the sidebar shows.
            if app.dirty {
                app.dirty = false;
                side_dirty = true;
            }
            if let Ok(now) = crossterm::terminal::size()
                && now != size
            {
                size = now;
                if settle_by.is_some() || sent_size.elapsed() < attach::SETTLE {
                    settle_by = Some(Instant::now() + attach::SETTLE);
                } else {
                    resize = true;
                }
            }
        }
        if resize {
            size = crossterm::terminal::size().unwrap_or(size);
            // Too narrow for the sidebar now: the raw passthrough instead.
            if size.0 < MIN_COLS {
                return leave(&reattaching, Outcome::Remode);
            }
            sent_size = Instant::now();
            let (c, r) = session_size(size);
            screen.resize(c, r);
            screen.cell_px = cell_px();
            let _ = ipc::write_frame(&mut writer, ipc::RESIZE, &ipc::resize_payload(c, r));
            side_dirty = true;
            agent_dirty = true;
        }
        if ready[1]
            && let Some(r) = &reattaching
            && let Ok(got) = r.rx.try_recv()
        {
            reattaching = None;
            let Some(s) = got else { return Outcome::Gone };
            match s.try_clone() {
                Ok(w) => writer = w,
                Err(_) => return Outcome::Gone,
            }
            reader = Some(BufReader::with_capacity(1 << 16, s));
            if let Some(on) = focus {
                let _ = ipc::write_frame(&mut writer, ipc::FOCUS, &[on as u8]);
            }
            ended = false;
            held = Some((Vec::new(), Instant::now()));
        }
        // ---- The session's output ----
        if sock.is_some() && (buffered || ready.get(2).copied().unwrap_or(false)) {
            while let Some(r) = reader.as_mut() {
                match ipc::read_frame(r) {
                    Ok((ipc::DATA, payload)) => {
                        if let Some((b, since)) = &mut held {
                            b.extend_from_slice(&payload);
                            if attach::visible(b) || since.elapsed() >= Duration::from_secs(3) {
                                // The reattach replays the session: onto a fresh screen.
                                let (c, rr) = screen.size();
                                let px = screen.cell_px;
                                let old = std::mem::replace(screen, Screen::new(c, rr));
                                drop(old);
                                screen.cell_px = px;
                                screen.feed(b);
                                held = None;
                                agent_dirty = true;
                                since_frame = 0;
                            }
                        } else {
                            since_frame = since_frame.saturating_add(payload.len());
                            screen.feed(&payload);
                            agent_dirty = true;
                        }
                    }
                    // Removed (killed, archived, closed): nothing to come back to.
                    Ok((ipc::EXIT, payload)) if payload.is_empty() => return Outcome::Gone,
                    Ok((ipc::EXIT, payload)) => {
                        // Kept: its last screen stays up, saying so, until it's resumed.
                        ended = true;
                        let note = crate::format::printable(&String::from_utf8_lossy(&payload));
                        screen.feed(
                            format!(
                                "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[<u\x1b[0m\r\n\x1b[2m── {note} ──  Enter resumes it · {} d back to the list\x1b[0m\r\n",
                                prefix.label
                            )
                            .as_bytes(),
                        );
                        agent_dirty = true;
                        reader = None;
                        reattaching = Some(Reattach::start(id, session_size, true, bell.clone()));
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        reader = None;
                        reattaching = Some(Reattach::start(id, session_size, ended, bell.clone()));
                        break;
                    }
                }
                if reader.as_ref().is_none_or(|r| r.buffer().is_empty()) {
                    break;
                }
            }
        }
        // What the program asked of its terminal: answers to it, the rest to the real one.
        let mut to_outer = vec![];
        for ask in screen.asks() {
            match ask {
                Ask::Reply(b) if !ended => {
                    let _ = ipc::write_frame(&mut writer, ipc::DATA, &b);
                }
                Ask::Reply(_) => {}
                Ask::Outer(b) => to_outer.extend(b),
            }
        }
        to_outer.extend(screen.outer());
        if !to_outer.is_empty() && out.write(&to_outer).is_err() {
            return leave(&reattaching, Outcome::Hangup);
        }
    }
}

/// One frame on the terminal.
fn draw(term: &mut Term, app: &mut App, screen: &Screen) -> std::io::Result<()> {
    term.draw(|f| frame(f, app, screen))?;
    Ok(())
}

/// The sidebar, and the session's screen beside it with its cursor.
pub fn frame(f: &mut ratatui::Frame, app: &mut App, screen: &Screen) {
    let area = f.area();
    sidebar::draw(f, app, Rect { x: 0, y: 0, width: AGENT_X.min(area.width), height: area.height });
    let agent = Rect { x: AGENT_X.min(area.width), y: 0, width: area.width.saturating_sub(AGENT_X), height: area.height };
    let cursor = screen.draw(f.buffer_mut(), agent);
    if !app.side.focus
        && let Some(c) = cursor
    {
        f.set_cursor_position(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::Key;

    #[test]
    fn keys_for_the_sidebar() {
        assert_eq!(side_keys(b"jk\r\x1b"), [Key::Char('j'), Key::Char('k'), Key::Enter, Key::Esc]);
        assert_eq!(side_keys(b"\x1b[A\x1bOB"), [Key::Up, Key::Down]);
        // The kitty keyboard protocol's Escape and Enter, and a release that's dropped.
        assert_eq!(side_keys(b"\x1b[27u\x1b[13u\x1b[106;1:3u"), [Key::Esc, Key::Enter]);
    }
}
