//! Being inside a session: the raw passthrough `dino attach` does (crates/dino/src/client.rs,
//! `attach_raw`), ported: the replay, size changes (SIGWINCH, a run of them sent as its first and
//! settled size), focus reports, an ended session's last screen with Enter resuming it, and a
//! reattach when the socket drops. The user's terminal renders the agent; nothing here emulates
//! one.
//!
//! What's added is the way back: the prefix key (Ctrl-\ unless the config says otherwise), then
//! `d`/`q` for the list, `n`/`p`/`N` for another session, `m` for the picker, `:` for the command
//! line, or the prefix again to send it. One thread polls the terminal, the session's stream and
//! the wake socket, so leaving is a return, not a thread to stop.

use std::io::{self, BufReader, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dino_core::ipc::{self, Request, Response};

use crate::app::Neighbour;
use crate::config::Prefix;
use crate::conn;

/// How an attach ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Back to the list.
    Detach,
    /// Into another session, without the list in between.
    Switch(String),
    /// Back to the list with the picker, or the command line, open.
    Picker,
    Command,
    /// The session is gone (killed, archived, closed).
    Gone,
    /// Couldn't attach; nothing on screen changed.
    Failed(String),
    /// The terminal went away.
    Hangup,
}

/// What the attach asks of the rest of dino-tui while it runs.
pub trait Host {
    /// Something woke the loop (dinod's state, a reply): take it in. With the status line on, its
    /// text when it changed.
    fn woke(&mut self) -> Option<String>;
    fn neighbour(&self, id: &str, which: Neighbour) -> Option<String>;
}

pub struct Opts<'a> {
    pub id: &'a str,
    pub prefix: &'a Prefix,
    pub status_line: bool,
}

const FOCUS_ON: &[u8] = b"\x1b[?1004h";
const FOCUS_OFF: &[u8] = b"\x1b[?1004l";
/// What a program may leave on, undone when leaving it: mouse reporting, bracketed paste, focus
/// reports, the alternate screen, a scroll region, a hidden or reshaped cursor, the kitty keyboard
/// protocol and xterm's modifyOtherKeys, colours.
pub const RESET: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[r\x1b[?25h\x1b[0 q\x1b[<u\x1b[<u\x1b[<u\x1b[<u\x1b[>4m\x1b[0m";
/// What `dino attach --fresh` starts from: a clean screen and scrollback, then the session's replay.
const CLEAR: &[u8] = b"\x1b[H\x1b[2J\x1b[3J";
/// A run of size changes reaches the program as its first and its settled size (see attach_raw).
const SETTLE: Duration = Duration::from_millis(50);

/// How much scrollback the terminal keeps, when it says (as `dino attach` reads it).
fn scrollback() -> Option<u64> {
    std::env::var("DINO_SCROLLBACK_LIMIT").ok()?.parse().ok()
}

/// `wait`: if the session has ended, answer once it's resumed.
fn attach_on(mut s: UnixStream, id: &str, cols: u16, rows: u16, wait: bool) -> io::Result<UnixStream> {
    ipc::write_json(&mut s, &Request::Attach { id: id.into(), cols, rows, wait, scrollback: scrollback() })?;
    let (_, payload) = ipc::read_frame(&mut s)?;
    match serde_json::from_slice(&payload).map_err(io::Error::other)? {
        Response::Ok => Ok(s),
        Response::Error { message } => Err(io::Error::other(message)),
        other => Err(io::Error::other(format!("unexpected {other:?}"))),
    }
}

/// Whether terminal output draws any text: more than escape sequences, titles and blank lines.
fn visible(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b => {
                i += 1;
                match bytes.get(i) {
                    Some(b'[') => {
                        i += 1;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                    }
                    Some(b']' | b'P' | b'_' | b'^') => {
                        while i < bytes.len() && bytes[i] != 0x07 && !(bytes[i] == b'\\' && bytes[i - 1] == 0x1b) {
                            i += 1;
                        }
                    }
                    _ => {}
                }
            }
            b if b.is_ascii_whitespace() || b.is_ascii_control() => {}
            _ => return true,
        }
        i += 1;
    }
    false
}

/// Input without its focus reports (`ESC [ I`, `ESC [ O`), and the focus the last one reported.
pub fn take_focus(input: &[u8]) -> (Vec<u8>, Option<bool>) {
    let mut keys = Vec::with_capacity(input.len());
    let mut focused = None;
    let mut i = 0;
    while i < input.len() {
        match input[i..] {
            [0x1b, b'[', f @ (b'I' | b'O'), ..] => {
                focused = Some(f == b'I');
                i += 3;
            }
            _ => {
                keys.push(input[i]);
                i += 1;
            }
        }
    }
    (keys, focused)
}

/// What the prefix and the key after it ask for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    Detach,
    Next,
    Prev,
    NeedsYou,
    Picker,
    Command,
}

/// Finds the prefix in what the terminal sends: its byte, or how the kitty keyboard protocol
/// (`CSI 92;5u`) or xterm's modifyOtherKeys (`CSI 27;5;92~`) send it once an agent turned those on.
/// Never inside a bracketed paste.
#[derive(Default)]
pub struct Keys {
    /// The prefix came; the next key says what for.
    armed: bool,
    /// How it came, to send as it came when it's pressed twice.
    raw: Vec<u8>,
    in_paste: bool,
}

/// A CSI sequence at the start of `b`: its parameters and final byte, and its length.
fn csi(b: &[u8]) -> Option<(&[u8], u8, usize)> {
    if b.len() < 3 || b[0] != 0x1b || b[1] != b'[' {
        return None;
    }
    let end = b[2..].iter().position(|c| (0x40..=0x7e).contains(c))? + 2;
    Some((&b[2..end], b[end], end + 1))
}

/// A key as the kitty protocol or modifyOtherKeys sends it: its code point, modifiers (1 = none),
/// and whether it's a release.
fn csi_key(params: &[u8], fin: u8) -> Option<(u32, u32, bool)> {
    let p = std::str::from_utf8(params).ok()?;
    match fin {
        b'u' => {
            let mut parts = p.split(';');
            let code: u32 = parts.next()?.split(':').next()?.parse().ok()?;
            let (mods, event) = match parts.next() {
                Some(m) => {
                    let mut m = m.split(':');
                    (m.next()?.parse().ok()?, m.next().map_or(Ok(1), str::parse).ok()?)
                }
                None => (1, 1),
            };
            Some((code, mods, event == 3))
        }
        b'~' => {
            let v: Vec<u32> = p.split(';').map(str::parse).collect::<Result<_, _>>().ok()?;
            match v.as_slice() {
                [27, mods, code] => Some((*code, *mods, false)),
                _ => None,
            }
        }
        _ => None,
    }
}

impl Keys {
    /// `input` split into what goes to the session and what the prefix asked for, if it did.
    pub fn feed(&mut self, input: &[u8], prefix: &Prefix) -> (Vec<u8>, Option<Act>) {
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let rest = &input[i..];
            if self.in_paste {
                if rest.starts_with(b"\x1b[201~") {
                    self.in_paste = false;
                }
                out.push(input[i]);
                i += 1;
                continue;
            }
            // What key starts here: its char (with Shift for an upper case letter), its length,
            // whether it's the prefix, a release.
            let (key, len, is_prefix, release) = match csi(rest) {
                Some((p, f, len)) => match csi_key(p, f) {
                    Some((code, mods, release)) => {
                        let ctrl = mods.saturating_sub(1) & 4 != 0;
                        let shift = mods.saturating_sub(1) & 1 != 0;
                        let c = char::from_u32(code).map(|c| if shift { c.to_ascii_uppercase() } else { c });
                        (c, len, ctrl && code == prefix.code, release)
                    }
                    None => {
                        if rest.starts_with(b"\x1b[200~") {
                            self.in_paste = true;
                        }
                        (None, len, false, false)
                    }
                },
                None => {
                    let c = std::str::from_utf8(&rest[..utf8_len(rest[0]).min(rest.len())]).ok().and_then(|s| s.chars().next());
                    (c, c.map_or(1, char::len_utf8), rest[0] == prefix.byte, false)
                }
            };
            let bytes = &rest[..len];
            i += len;
            if !self.armed {
                if is_prefix && !release {
                    self.armed = true;
                    self.raw = bytes.to_vec();
                } else if !(is_prefix && release) {
                    // A release of the prefix never reaches the session: it never saw the press.
                    out.extend_from_slice(bytes);
                }
                continue;
            }
            if release {
                continue;
            }
            self.armed = false;
            if is_prefix {
                out.extend_from_slice(&self.raw);
                continue;
            }
            let act = match key {
                Some('d' | 'q') => Act::Detach,
                Some('n') => Act::Next,
                Some('p') => Act::Prev,
                Some('N') => Act::NeedsYou,
                Some('m') => Act::Picker,
                Some(':') => Act::Command,
                // Anything else after the prefix (Escape among them) is dropped, as tmux does.
                _ => continue,
            };
            return (out, Some(act));
        }
        (out, None)
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0xf0.. => 4,
        0xe0.. => 3,
        0xc0.. => 2,
        _ => 1,
    }
}

/// Writes to the terminal unbuffered: each frame as it comes, as `dino attach` does.
struct Out;

impl Out {
    fn write(&self, b: &[u8]) -> io::Result<()> {
        let mut f = std::mem::ManuallyDrop::new(unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(1) });
        f.write_all(b)
    }
}

/// A reattach under way on a thread of its own, so keys still work (prefix d leaves even while
/// an ended session waits to be resumed).
struct Reattach {
    rx: Receiver<Option<UnixStream>>,
    /// The connection it waits on, to cut when leaving.
    waiting: Arc<Mutex<Option<UnixStream>>>,
    cancelled: Arc<AtomicBool>,
}

impl Reattach {
    fn start(id: &str, status_line: bool, wait: bool, bell: Arc<UnixStream>) -> Reattach {
        let (tx, rx): (Sender<Option<UnixStream>>, _) = channel();
        let waiting = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (id, w, c) = (id.to_string(), waiting.clone(), cancelled.clone());
        std::thread::spawn(move || {
            let got = reattach(&id, status_line, wait, &w, &c);
            let _ = tx.send(got);
            let _ = (&*bell).write(&[1]);
        });
        Reattach { rx, waiting, cancelled }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(s) = self.waiting.lock().unwrap().take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// attach_raw's reattach loop: every 100 ms; dinod saying no means the session is gone; not
/// reachable for 5 s, gone too. `wait`: the session ended, so this blocks until it's resumed.
fn reattach(id: &str, status_line: bool, wait: bool, waiting: &Mutex<Option<UnixStream>>, cancelled: &AtomicBool) -> Option<UnixStream> {
    let mut started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
        let (cols, rows) = session_size(crossterm::terminal::size().unwrap_or((80, 24)), status_line);
        let tried = Instant::now();
        let attempt = UnixStream::connect(conn::socket()).and_then(|s| {
            *waiting.lock().unwrap() = s.try_clone().ok();
            attach_on(s, id, cols, rows, wait)
        });
        waiting.lock().unwrap().take();
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
        match attempt {
            Ok(s) => return Some(s),
            Err(e) if e.kind() == io::ErrorKind::Other => return None,
            Err(_) if tried.elapsed() > Duration::from_secs(1) => started = Instant::now(),
            Err(_) if started.elapsed() > Duration::from_secs(5) => return None,
            Err(_) => {}
        }
    }
}

/// The session's size for a terminal of `size`: a row less for the status line.
fn session_size(size: (u16, u16), status_line: bool) -> (u16, u16) {
    (size.0, if status_line { size.1.saturating_sub(1).max(1) } else { size.1 })
}

/// The status line at the bottom row, outside the session's scroll region; the cursor and the
/// session's own saved cursor stay where they were (DECSC/DECRC around it).
fn bar(rows: u16, cols: u16, text: &str) -> Vec<u8> {
    let text = crate::ui::fit(text, cols as usize);
    format!("\x1b7\x1b[1;{}r\x1b[{rows};1H\x1b[0m\x1b[7m\x1b[2K{text}\x1b[K\x1b[0m\x1b8", rows.saturating_sub(1).max(1)).into_bytes()
}

/// Output that clears the screen or resets the scroll region, after which the bar is drawn again.
fn wipes_bar(b: &[u8]) -> bool {
    [&b"\x1b[2J"[..], b"\x1b[J", b"\x1b[0J", b"\x1b[r", b"\x1b[?1049", b"\x1bc", b"\x1b[?47", b"\x1b[?1047"].iter().any(|p| memchr::memmem::find(b, p).is_some())
}

/// Attach to session `opts.id` until the prefix (or the session) says otherwise. The terminal is in
/// raw mode and shows dino-tui's list (the alternate screen) when this is called; it leaves the
/// alternate screen only once dinod has said yes.
pub fn run(opts: &Opts, wake: RawFd, bell: Arc<UnixStream>, host: &mut dyn Host) -> Outcome {
    let size = crossterm::terminal::size().unwrap_or((80, 24));
    let (cols, rows) = session_size(size, opts.status_line);
    let stream = match UnixStream::connect(conn::socket()).and_then(|s| attach_on(s, opts.id, cols, rows, false)) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let out = Out;
    // Off the list's screen, to a clean one: the session's replay draws its scrollback and screen.
    let _ = out.write(&[b"\x1b[?2004l\x1b[?1049l\x1b[?25h", CLEAR, FOCUS_ON].concat());
    let mut status = host.woke();
    if opts.status_line {
        let _ = out.write(&bar(size.1, size.0, status.as_deref().unwrap_or("")));
    }
    let outcome = relay(opts, stream, size, wake, bell, host, &mut status);
    // Leave the terminal as the list wants it: whatever the program turned on, off.
    let _ = out.write(RESET);
    outcome
}

#[allow(clippy::too_many_arguments)]
fn relay(opts: &Opts, stream: UnixStream, mut size: (u16, u16), wake: RawFd, bell: Arc<UnixStream>, host: &mut dyn Host, status: &mut Option<String>) -> Outcome {
    let out = Out;
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let mut reader = Some(BufReader::with_capacity(1 << 16, stream));
    let mut keys = Keys::default();
    // As this terminal last reported it: told again after a reattach.
    let mut focus: Option<bool> = None;
    // The session's program has ended: keys don't go to it, Enter resumes it.
    let mut ended = false;
    let mut held: Option<(Vec<u8>, Instant)> = None;
    let mut reattaching: Option<Reattach> = None;
    // Size changes: when the last one went, and one waiting for the size to settle.
    let mut sent_size = Instant::now().checked_sub(SETTLE).unwrap_or_else(Instant::now);
    let mut settle_by: Option<Instant> = None;
    let mut buf = [0u8; 8192];
    let leave = |r: &Option<Reattach>, o: Outcome| {
        if let Some(r) = r {
            r.cancel();
        }
        o
    };
    loop {
        let sock = reader.as_ref().map(|r| r.get_ref().as_raw_fd());
        // Whatever the reader holds already is read before waiting again.
        let buffered = reader.as_ref().is_some_and(|r| !r.buffer().is_empty());
        let mut fds = vec![0, wake];
        fds.extend(sock);
        let timeout = if buffered { Some(Duration::ZERO) } else { settle_by.map(|t| t.saturating_duration_since(Instant::now())) };
        let ready = match crate::wake::poll(&fds, timeout) {
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
                let (input, focused) = take_focus(&buf[..n as usize]);
                if let Some(on) = focused {
                    focus = Some(on);
                    let _ = ipc::write_frame(&mut writer, ipc::FOCUS, &[on as u8]);
                }
                let (fwd, act) = keys.feed(&input, opts.prefix);
                if ended {
                    if fwd.contains(&b'\r') {
                        // The reattach below is already waiting for it to come back.
                        if let Ok(Response::Error { message }) = conn::request(&Request::Resume { id: opts.id.into() }) {
                            let _ = out.write(format!("\r\n\x1b[2m{}\x1b[0m\r\n", crate::format::printable(&message)).as_bytes());
                        }
                    }
                } else if !fwd.is_empty() {
                    // A failed write means the socket dropped; the read side notices and reattaches.
                    let _ = ipc::write_frame(&mut writer, ipc::DATA, &fwd);
                }
                if let Some(act) = act {
                    let to = |w| host.neighbour(opts.id, w);
                    match act {
                        Act::Detach => return leave(&reattaching, Outcome::Detach),
                        Act::Picker => return leave(&reattaching, Outcome::Picker),
                        Act::Command => return leave(&reattaching, Outcome::Command),
                        Act::Next | Act::Prev | Act::NeedsYou => {
                            let which = match act {
                                Act::Next => Neighbour::Next,
                                Act::Prev => Neighbour::Prev,
                                _ => Neighbour::NeedsYou,
                            };
                            match to(which) {
                                Some(id) => return leave(&reattaching, Outcome::Switch(id)),
                                // Nowhere to go: the bell says so.
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
            drain(wake);
            if let Some(text) = host.woke()
                && opts.status_line
                && status.as_deref() != Some(&text)
            {
                let _ = out.write(&bar(size.1, size.0, &text));
                *status = Some(text);
            }
            if let Ok(now) = crossterm::terminal::size()
                && now != size
            {
                size = now;
                // Within a run of changes: wait for it to settle. Else at once.
                if settle_by.is_some() || sent_size.elapsed() < SETTLE {
                    settle_by = Some(Instant::now() + SETTLE);
                } else {
                    resize = true;
                }
            }
        }
        if resize {
            size = crossterm::terminal::size().unwrap_or(size);
            sent_size = Instant::now();
            let (c, r) = session_size(size, opts.status_line);
            let _ = ipc::write_frame(&mut writer, ipc::RESIZE, &ipc::resize_payload(c, r));
            if opts.status_line {
                let _ = out.write(&bar(size.1, size.0, status.as_deref().unwrap_or("")));
            }
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
                        let mut shown = Vec::new();
                        if let Some((b, since)) = &mut held {
                            b.extend_from_slice(&payload);
                            if visible(b) || since.elapsed() >= Duration::from_secs(3) {
                                // The reattach replays the session's scrollback, so from a clean screen.
                                shown.extend_from_slice(CLEAR);
                                shown.append(b);
                                held = None;
                            }
                        } else {
                            shown = payload;
                        }
                        if !shown.is_empty() {
                            if memchr::memmem::find(&shown, FOCUS_OFF).is_some() {
                                shown.extend_from_slice(FOCUS_ON);
                            }
                            if opts.status_line && wipes_bar(&shown) {
                                shown.extend_from_slice(&bar(size.1, size.0, status.as_deref().unwrap_or("")));
                            }
                            if out.write(&shown).is_err() {
                                return leave(&reattaching, Outcome::Hangup);
                            }
                        }
                    }
                    // Removed (killed, archived, closed): nothing to come back to.
                    Ok((ipc::EXIT, payload)) if payload.is_empty() => return Outcome::Gone,
                    Ok((ipc::EXIT, payload)) => {
                        // Kept: its last screen stays up, saying so, until it's resumed.
                        ended = true;
                        let note = crate::format::printable(&String::from_utf8_lossy(&payload));
                        let _ = out.write(
                            format!(
                                "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[<u\x1b[0m\r\n\x1b[2m── {note} ──  Enter resumes it · {} d back to the list\x1b[0m\r\n",
                                opts.prefix.label
                            )
                            .as_bytes(),
                        );
                        reader = None;
                        reattaching = Some(Reattach::start(opts.id, opts.status_line, true, bell.clone()));
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        // The socket dropped but the session may live on (seen across sleep/wake).
                        reader = None;
                        reattaching = Some(Reattach::start(opts.id, opts.status_line, ended, bell.clone()));
                        break;
                    }
                }
                if reader.as_ref().is_none_or(|r| r.buffer().is_empty()) {
                    break;
                }
            }
        }
    }
}

fn drain(fd: RawFd) {
    let mut buf = [0u8; 256];
    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(k: &mut Keys, b: &[u8]) -> (Vec<u8>, Option<Act>) {
        k.feed(b, &Prefix::default())
    }

    #[test]
    fn focus_reports_are_taken_out_of_the_keys() {
        assert_eq!(take_focus(b"ab"), (b"ab".to_vec(), None));
        assert_eq!(take_focus(b"a\x1b[Ob\x1b[Ic"), (b"abc".to_vec(), Some(true)));
        assert_eq!(take_focus(b"\x1b[A\x1b\x1bOP\x1b["), (b"\x1b[A\x1b\x1bOP\x1b[".to_vec(), None));
    }

    #[test]
    fn only_drawn_text_is_visible() {
        assert!(!visible(b"\r\n\x1b[?2026h\x1b[H\x1b[2J\x1b]0;\xe2\x9c\xb3 Pelican\x07\x1b7\x1b8"));
        assert!(visible(b"\x1b[1m\xe2\x96\x90 Claude Code"));
    }

    #[test]
    fn the_prefix_then_a_key() {
        let mut k = Keys::default();
        assert_eq!(feed(&mut k, b"ls\r\x1cd"), (b"ls\r".to_vec(), Some(Act::Detach)));
        // Split across reads.
        assert_eq!(feed(&mut k, b"x\x1c"), (b"x".to_vec(), None));
        assert_eq!(feed(&mut k, b"N"), (vec![], Some(Act::NeedsYou)));
        // Twice: the key itself goes.
        assert_eq!(feed(&mut k, b"\x1c\x1cy"), (b"\x1cy".to_vec(), None));
        // Anything else after it is dropped.
        assert_eq!(feed(&mut k, b"\x1czb"), (b"b".to_vec(), None));
        assert_eq!(feed(&mut k, b"\x1c\x1b"), (vec![], None));
        assert_eq!(feed(&mut k, b"\x1c:"), (vec![], Some(Act::Command)));
    }

    #[test]
    fn the_prefix_as_the_kitty_protocol_and_modify_other_keys_send_it() {
        let mut k = Keys::default();
        // Press, then `d`, then the prefix's release: none of it reaches the session.
        assert_eq!(feed(&mut k, b"a\x1b[92;5u"), (b"a".to_vec(), None));
        assert_eq!(feed(&mut k, b"\x1b[92;5:3ud"), (vec![], Some(Act::Detach)));
        assert_eq!(feed(&mut k, b"\x1b[92;5:3u"), (vec![], None));
        // Every key as an escape code (flag 8): `n` too.
        assert_eq!(feed(&mut k, b"\x1b[92;5u\x1b[110u"), (vec![], Some(Act::Next)));
        // Shift+n.
        assert_eq!(feed(&mut k, b"\x1b[92;5u\x1b[110;2u"), (vec![], Some(Act::NeedsYou)));
        assert_eq!(feed(&mut k, b"\x1b[27;5;92~q"), (vec![], Some(Act::Detach)));
        // Twice sends it as it came.
        assert_eq!(feed(&mut k, b"\x1b[92;5u\x1b[92;5u"), (b"\x1b[92;5u".to_vec(), None));
        // Other keys pass through untouched.
        assert_eq!(feed(&mut k, b"\x1b[13;2u\x1b[A"), (b"\x1b[13;2u\x1b[A".to_vec(), None));
    }

    #[test]
    fn never_inside_a_paste() {
        let mut k = Keys::default();
        let paste = b"\x1b[200~a\x1cdb\x1b[201~";
        assert_eq!(feed(&mut k, paste), (paste.to_vec(), None));
        assert_eq!(feed(&mut k, b"\x1cp"), (vec![], Some(Act::Prev)));
    }

    #[test]
    fn another_prefix() {
        let mut k = Keys::default();
        let p = Prefix::parse("C-a").unwrap();
        assert_eq!(k.feed(b"\x1c\x01m", &p), (b"\x1c".to_vec(), Some(Act::Picker)));
        assert_eq!(k.feed(b"\x1b[97;5ud", &p), (vec![], Some(Act::Detach)));
    }

    #[test]
    fn the_bar_keeps_the_cursor() {
        let b = String::from_utf8(bar(24, 10, "claude-1 working and more")).unwrap();
        assert!(b.starts_with("\x1b7\x1b[1;23r\x1b[24;1H") && b.ends_with("\x1b8"), "{b:?}");
        assert!(b.contains("claude-1 …"));
        assert!(wipes_bar(b"x\x1b[2Jy") && !wipes_bar(b"\x1b[2K"));
    }
}
