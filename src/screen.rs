//! The agent's screen beside the sidebar: the bytes `Attach` streams, read by alacritty_terminal
//! (the parser dinod keeps its replays with, so a session looks the same in both) and drawn into
//! ratatui's buffer, cell by cell, in the columns right of the sidebar.
//!
//! What the agent asks of the terminal it runs in goes where it belongs. Modes that change what
//! the keyboard and mouse send (cursor keys, keypad, bracketed paste, mouse reporting, the kitty
//! keyboard protocol, modifyOtherKeys) and the cursor's shape are passed on to the real terminal,
//! which encodes the keys. Queries are answered here, as dinod's own emulator answers them while
//! no client is attached: the cursor's position is this screen's, not the real terminal's. Colour
//! queries go on to the real terminal, whose answer comes back as input.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AColor, CursorShape, CursorStyle, NamedColor, Processor};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

/// How much of the session's history the screen keeps (the replay sends dinod's).
const HISTORY: usize = 10_000;

/// What the parser asked for while it read, collected for the loop to act on.
#[derive(Clone, Default)]
struct Events(Rc<RefCell<Vec<TermEvent>>>);

impl EventListener for Events {
    fn send_event(&self, event: TermEvent) {
        match event {
            // Nothing to act on here: drawing follows every read anyway.
            TermEvent::Wakeup | TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange => {}
            e => self.0.borrow_mut().push(e),
        }
    }
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// What the loop does about something the program asked of its terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// An answer for the program (a query's).
    Reply(Vec<u8>),
    /// For the real terminal: a colour query (its answer comes back as input), the bell, a copy.
    Outer(Vec<u8>),
}

/// The modes the real terminal is told about, as last told.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Told {
    mode: TermMode,
    cursor: CursorStyle,
    /// xterm's modifyOtherKeys level.
    other_keys: u8,
}

/// The modes that change what the real terminal sends.
const SHARED: TermMode = TermMode::APP_CURSOR
    .union(TermMode::APP_KEYPAD)
    .union(TermMode::BRACKETED_PASTE)
    .union(TermMode::MOUSE_REPORT_CLICK)
    .union(TermMode::MOUSE_DRAG)
    .union(TermMode::MOUSE_MOTION)
    .union(TermMode::SGR_MOUSE)
    .union(TermMode::UTF8_MOUSE)
    .union(TermMode::KITTY_KEYBOARD_PROTOCOL);

pub struct Screen {
    term: Term<Events>,
    parser: Processor,
    events: Events,
    told: Told,
    /// xterm's modifyOtherKeys, as the program last set it (alacritty doesn't keep it).
    other_keys: u8,
    /// The end of a `CSI > 4 ; n m` split across reads.
    carry: Vec<u8>,
    /// The real terminal's size in pixels per cell, for a program that asks (`CSI 14 t`).
    pub cell_px: (u16, u16),
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Screen {
        let events = Events::default();
        let config = Config { scrolling_history: HISTORY, kitty_keyboard: true, ..Config::default() };
        let term = Term::new(config, &Size { cols: cols.max(1) as usize, rows: rows.max(1) as usize }, events.clone());
        let told = Told { mode: TermMode::empty(), cursor: CursorStyle::default(), other_keys: 0 };
        Screen { term, parser: Processor::new(), events, told, other_keys: 0, carry: Vec::new(), cell_px: (0, 0) }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.term.columns() as u16, self.term.screen_lines() as u16)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(Size { cols: cols.max(1) as usize, rows: rows.max(1) as usize });
    }

    /// Read what the program wrote.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.other_keys(bytes);
        self.parser.advance(&mut self.term, bytes);
    }

    /// Inside a synchronized update (`CSI ? 2026 h`), until when: drawing waits for its end.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// The program never ended its synchronized update: what it drew so far, as a terminal shows it.
    pub fn end_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
    }

    /// `CSI > 4 ; n m` (and `CSI > 4 m`, off): xterm's modifyOtherKeys, which alacritty ignores.
    fn other_keys(&mut self, bytes: &[u8]) {
        const HEAD: &[u8] = b"\x1b[>4";
        let joined: Vec<u8>;
        let b = if self.carry.is_empty() {
            bytes
        } else {
            joined = [self.carry.as_slice(), bytes].concat();
            &joined
        };
        for at in memchr::memmem::find_iter(b, HEAD) {
            let rest = &b[at + HEAD.len()..];
            let Some(end) = rest.iter().position(|c| !(c.is_ascii_digit() || *c == b';')) else { continue };
            if rest[end] == b'm' {
                self.other_keys = std::str::from_utf8(&rest[..end]).ok().and_then(|p| p.strip_prefix(';')).and_then(|n| n.parse().ok()).unwrap_or(0);
            }
        }
        // One cut short at the end of the read waits for the next.
        self.carry.clear();
        if let Some(esc) = memchr::memrchr(0x1b, &b[b.len().saturating_sub(12)..]).map(|p| b.len().saturating_sub(12) + p) {
            let t = &b[esc..];
            let unfinished = if t.len() <= HEAD.len() { HEAD.starts_with(t) } else { t.starts_with(HEAD) && t[HEAD.len()..].iter().all(|c| c.is_ascii_digit() || *c == b';') };
            if unfinished {
                self.carry = t.to_vec();
            }
        }
    }

    /// What the program asked of its terminal since the last call.
    pub fn asks(&mut self) -> Vec<Ask> {
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        events
            .into_iter()
            .filter_map(|e| match e {
                TermEvent::PtyWrite(text) => Some(Ask::Reply(text.into_bytes())),
                TermEvent::ColorRequest(index, _) => {
                    let q = match index {
                        i if i < 256 => format!("\x1b]4;{i};?\x1b\\"),
                        i if i == NamedColor::Foreground as usize => "\x1b]10;?\x1b\\".to_string(),
                        i if i == NamedColor::Background as usize => "\x1b]11;?\x1b\\".to_string(),
                        i if i == NamedColor::Cursor as usize => "\x1b]12;?\x1b\\".to_string(),
                        _ => return None,
                    };
                    Some(Ask::Outer(q.into_bytes()))
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let (cols, rows) = self.size();
                    Some(Ask::Reply(format(WindowSize { num_lines: rows, num_cols: cols, cell_width: self.cell_px.0, cell_height: self.cell_px.1 }).into_bytes()))
                }
                TermEvent::Bell => Some(Ask::Outer(b"\x07".to_vec())),
                TermEvent::ClipboardStore(_, text) => {
                    use base64::Engine;
                    Some(Ask::Outer(format!("\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text)).into_bytes()))
                }
                _ => None,
            })
            .collect()
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// What to tell the real terminal so its keys and mouse reports are what the program expects:
    /// the shared modes and the cursor's shape that changed since the last call.
    pub fn outer(&mut self) -> Vec<u8> {
        let now = Told { mode: self.mode() & SHARED, cursor: self.term.cursor_style(), other_keys: self.other_keys };
        let out = tell(&self.told, &now);
        self.told = now;
        out
    }

    /// The real terminal as it was before any of this: for leaving.
    pub fn untell(&mut self) -> Vec<u8> {
        let none = Told { mode: TermMode::empty(), cursor: CursorStyle::default(), other_keys: 0 };
        let mut out = tell(&self.told, &none);
        // The shape the user's terminal has by default.
        out.extend_from_slice(b"\x1b[0 q");
        self.told = none;
        out
    }

    /// Scroll the view back (positive) or forward into the history; 0 is the live screen.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    pub fn scrolled(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// The program's mouse reports are wanted, and how.
    pub fn mouse(&self) -> Option<bool> {
        let m = self.mode();
        m.intersects(TermMode::MOUSE_MODE).then(|| m.contains(TermMode::SGR_MOUSE))
    }

    /// The screen into `buf` at `area` (as it was scrolled), and where the cursor is, when it shows.
    pub fn draw(&self, buf: &mut Buffer, area: Rect) -> Option<(u16, u16)> {
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        let cols = grid.columns().min(area.width as usize);
        let rows = grid.screen_lines().min(area.height as usize);
        let palette = self.term.colors();
        let color = |c: AColor, default: Color| -> Color {
            match c {
                AColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
                AColor::Indexed(i) => match palette[i as usize] {
                    Some(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
                    None if i < 16 => named(i),
                    None => Color::Indexed(i),
                },
                AColor::Named(n) => {
                    let i = n as usize;
                    match (palette[i], i) {
                        (Some(rgb), _) => Color::Rgb(rgb.r, rgb.g, rgb.b),
                        (None, 0..16) => named(i as u8),
                        // The dim colours: the base colour, dimmed by the cell's flag.
                        (None, _) if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => named((i - NamedColor::DimBlack as usize) as u8),
                        _ => default,
                    }
                }
            }
        };
        for y in 0..rows {
            let row = &grid[Line(y as i32 - offset)];
            let mut x = 0;
            while x < cols {
                let cell = &row[Column(x)];
                let out = &mut buf[(area.x + x as u16, area.y + y as u16)];
                let flags = cell.flags;
                if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    // Covered by the wide character before it (or a wide one that didn't fit).
                    out.reset();
                    if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                        out.set_char(' ');
                    }
                    x += 1;
                    continue;
                }
                let wide = flags.contains(Flags::WIDE_CHAR);
                if wide && x + 1 >= cols {
                    // Half off the edge of a narrower area: a blank, as a terminal would leave it.
                    out.reset();
                    x += 1;
                    continue;
                }
                match cell.zerowidth() {
                    Some(marks) => {
                        let mut s = String::with_capacity(8);
                        s.push(cell.c);
                        s.extend(marks);
                        out.set_symbol(&s);
                    }
                    None => {
                        out.set_char(if cell.c == '\t' || cell.c < ' ' { ' ' } else { cell.c });
                    }
                }
                let mut m = Modifier::empty();
                if flags.contains(Flags::BOLD) {
                    m |= Modifier::BOLD;
                }
                if flags.contains(Flags::DIM) {
                    m |= Modifier::DIM;
                }
                if flags.contains(Flags::ITALIC) {
                    m |= Modifier::ITALIC;
                }
                if flags.intersects(Flags::ALL_UNDERLINES) {
                    m |= Modifier::UNDERLINED;
                }
                if flags.contains(Flags::INVERSE) {
                    m |= Modifier::REVERSED;
                }
                if flags.contains(Flags::HIDDEN) {
                    m |= Modifier::HIDDEN;
                }
                if flags.contains(Flags::STRIKEOUT) {
                    m |= Modifier::CROSSED_OUT;
                }
                out.fg = color(cell.fg, Color::Reset);
                out.bg = color(cell.bg, Color::Reset);
                out.modifier = m;
                if wide {
                    // The next column is the wide character's: ratatui skips it when it writes.
                    buf[(area.x + x as u16 + 1, area.y + y as u16)].reset();
                    x += 2;
                } else {
                    x += 1;
                }
            }
            // Past the program's width (never, once sized to the area): blank.
            for x in cols..area.width as usize {
                buf[(area.x + x as u16, area.y + y as u16)].reset();
            }
        }
        for y in rows..area.height as usize {
            for x in 0..area.width as usize {
                buf[(area.x + x as u16, area.y + y as u16)].reset();
            }
        }
        let cursor = self.term.renderable_content().cursor;
        let line = cursor.point.line.0 + offset;
        (cursor.shape != CursorShape::Hidden && line >= 0 && (line as usize) < rows && cursor.point.column.0 < cols).then(|| (area.x + cursor.point.column.0 as u16, area.y + line as u16))
    }

    /// The screen's text, row by row (for tests).
    #[cfg(test)]
    pub fn text(&self) -> String {
        let grid = self.term.grid();
        (0..grid.screen_lines())
            .map(|y| {
                let row = &grid[Line(y as i32)];
                (0..grid.columns()).filter(|&x| !row[Column(x)].flags.contains(Flags::WIDE_CHAR_SPACER)).map(|x| row[Column(x)].c).collect::<String>().trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The 16 colours as the terminal's own: SGR 30–37 and 90–97, never an RGB guess.
fn named(i: u8) -> Color {
    [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ][i as usize & 15]
}

/// The sequences that take the real terminal from `was` to `now`.
fn tell(was: &Told, now: &Told) -> Vec<u8> {
    let mut out = String::new();
    let changed = was.mode ^ now.mode;
    let set = |out: &mut String, on: bool, code: &str| out.push_str(&format!("\x1b[?{code}{}", if on { 'h' } else { 'l' }));
    if changed.contains(TermMode::APP_CURSOR) {
        set(&mut out, now.mode.contains(TermMode::APP_CURSOR), "1");
    }
    if changed.contains(TermMode::APP_KEYPAD) {
        out.push_str(if now.mode.contains(TermMode::APP_KEYPAD) { "\x1b=" } else { "\x1b>" });
    }
    if changed.contains(TermMode::BRACKETED_PASTE) {
        set(&mut out, now.mode.contains(TermMode::BRACKETED_PASTE), "2004");
    }
    // Mouse reporting: the encodings first, so the first report comes encoded as asked.
    for (m, code) in [(TermMode::SGR_MOUSE, "1006"), (TermMode::UTF8_MOUSE, "1005")] {
        if changed.contains(m) {
            set(&mut out, now.mode.contains(m), code);
        }
    }
    for (m, code) in [(TermMode::MOUSE_REPORT_CLICK, "1000"), (TermMode::MOUSE_DRAG, "1002"), (TermMode::MOUSE_MOTION, "1003")] {
        if changed.contains(m) {
            set(&mut out, now.mode.contains(m), code);
        }
    }
    let kitty = |m: TermMode| {
        [(TermMode::DISAMBIGUATE_ESC_CODES, 1), (TermMode::REPORT_EVENT_TYPES, 2), (TermMode::REPORT_ALTERNATE_KEYS, 4), (TermMode::REPORT_ALL_KEYS_AS_ESC, 8), (TermMode::REPORT_ASSOCIATED_TEXT, 16)]
            .iter()
            .filter(|(f, _)| m.contains(*f))
            .map(|(_, b)| b)
            .sum::<u8>()
    };
    let (k0, k1) = (kitty(was.mode), kitty(now.mode));
    if k0 != k1 {
        // Set, not pushed: the real terminal's stack stays as dino-tui found it.
        out.push_str(&format!("\x1b[={k1};1u"));
    }
    if was.other_keys != now.other_keys {
        out.push_str(&if now.other_keys == 0 { "\x1b[>4m".to_string() } else { format!("\x1b[>4;{}m", now.other_keys) });
    }
    if was.cursor != now.cursor {
        let n = match (now.cursor.shape, now.cursor.blinking) {
            (CursorShape::Underline, true) => 3,
            (CursorShape::Underline, false) => 4,
            (CursorShape::Beam, true) => 5,
            (CursorShape::Beam, false) => 6,
            (_, true) => 1,
            (_, false) => 2,
        };
        out.push_str(&format!("\x1b[{n} q"));
    }
    out.into_bytes()
}

/// Mouse reports from the real terminal, moved `dx` columns left into the program's screen; ones
/// left of it (on the sidebar) are dropped. SGR (`CSI < b ; x ; y M`) and the X10 form
/// (`CSI M b x y`); the rest of the input as it came.
pub fn shift_mouse(input: &[u8], dx: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        if rest.starts_with(b"\x1b[<") {
            let end = rest.iter().skip(3).position(|c| *c == b'M' || *c == b'm').map(|p| p + 3);
            if let Some(end) = end {
                let params = std::str::from_utf8(&rest[3..end]).unwrap_or("");
                let v: Vec<i64> = params.split(';').filter_map(|p| p.parse().ok()).collect();
                if let [b, x, y] = v[..] {
                    let x = x - dx as i64;
                    if x >= 1 {
                        out.extend_from_slice(format!("\x1b[<{b};{x};{y}{}", rest[end] as char).as_bytes());
                    }
                    i += end + 1;
                    continue;
                }
            }
        } else if rest.len() >= 6 && rest.starts_with(b"\x1b[M") {
            let x = rest[4] as i64 - 32 - dx as i64;
            if x >= 1 {
                out.extend_from_slice(&[0x1b, b'[', b'M', rest[3], (x + 32) as u8, rest[5]]);
            }
            i += 6;
            continue;
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_colours_and_wide_characters_land_in_the_buffer() {
        let mut s = Screen::new(10, 3);
        s.feed("ab\x1b[1;31mc\x1b[0m\r\n日x\x1b[38;5;200my\x1b[38;2;1;2;3mz".as_bytes());
        assert_eq!(s.text(), "abc\n日xyz\n");
        let mut buf = Buffer::empty(Rect::new(0, 0, 14, 3));
        let cursor = s.draw(&mut buf, Rect::new(4, 0, 10, 3));
        assert_eq!(buf[(4, 0)].symbol(), "a");
        assert_eq!((buf[(6, 0)].fg, buf[(6, 0)].modifier), (Color::Red, Modifier::BOLD));
        assert_eq!(buf[(4, 1)].symbol(), "日");
        assert_eq!(buf[(6, 1)].symbol(), "x");
        assert_eq!(buf[(7, 1)].fg, Color::Indexed(200));
        assert_eq!(buf[(8, 1)].fg, Color::Rgb(1, 2, 3));
        assert_eq!(cursor, Some((9, 1)));
        s.feed(b"\x1b[?25l");
        assert_eq!(s.draw(&mut buf, Rect::new(4, 0, 10, 3)), None);
    }

    #[test]
    fn queries_are_answered_here_and_colours_asked_of_the_real_terminal() {
        let mut s = Screen::new(20, 5);
        s.feed(b"hi\x1b[6n\x1b[c\x1b]11;?\x07\x07");
        let asks = s.asks();
        assert_eq!(asks[0], Ask::Reply(b"\x1b[1;3R".to_vec()));
        assert_eq!(asks[1], Ask::Reply(b"\x1b[?6c".to_vec()));
        assert_eq!(asks[2], Ask::Outer(b"\x1b]11;?\x1b\\".to_vec()));
        assert_eq!(asks[3], Ask::Outer(b"\x07".to_vec()));
    }

    #[test]
    fn input_modes_go_on_to_the_real_terminal_once() {
        let mut s = Screen::new(20, 5);
        assert!(s.outer().is_empty());
        s.feed(b"\x1b[?1h\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[>1u\x1b[>4;2m\x1b[6 q");
        let told = String::from_utf8(s.outer()).unwrap();
        assert_eq!(told, "\x1b[?1h\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[=1;1u\x1b[>4;2m\x1b[6 q");
        assert!(s.outer().is_empty(), "told once");
        s.feed(b"\x1b[<u\x1b[?1l");
        assert_eq!(String::from_utf8(s.outer()).unwrap(), "\x1b[?1l\x1b[=0;1u");
        let left = String::from_utf8(s.untell()).unwrap();
        assert!(left.contains("\x1b[?2004l") && left.contains("\x1b[?1002l") && left.contains("\x1b[>4m") && left.ends_with("\x1b[0 q"), "{left:?}");
    }

    #[test]
    fn modify_other_keys_split_across_reads() {
        let mut s = Screen::new(20, 5);
        s.feed(b"x\x1b[>");
        s.feed(b"4;1mz");
        assert_eq!(s.other_keys, 1);
        s.feed(b"\x1b[>4m");
        assert_eq!(s.other_keys, 0);
    }

    #[test]
    fn a_synchronized_update_shows_whole() {
        let mut s = Screen::new(20, 3);
        s.feed(b"\x1b[?2026hone");
        assert!(s.sync_deadline().is_some());
        assert_eq!(s.text(), "\n\n", "not drawn mid-update");
        s.feed(b" two\x1b[?2026l");
        assert_eq!(s.text(), "one two\n\n");
        s.feed(b"\x1b[?2026hthree");
        s.end_sync();
        assert!(s.text().contains("three"));
    }

    #[test]
    fn mouse_reports_move_past_the_sidebar() {
        assert_eq!(shift_mouse(b"a\x1b[<0;40;5Mb\x1b[<0;10;5m", 29), b"a\x1b[<0;11;5Mb".to_vec());
        assert_eq!(shift_mouse(&[0x1b, b'[', b'M', 32, 32 + 40, 37], 29), vec![0x1b, b'[', b'M', 32, 32 + 11, 37]);
        assert_eq!(shift_mouse(b"\x1b[A", 29), b"\x1b[A".to_vec());
    }

    #[test]
    fn scrolled_back_and_forth() {
        let mut s = Screen::new(10, 2);
        s.feed(b"1\r\n2\r\n3\r\n4");
        s.scroll(2);
        assert_eq!(s.scrolled(), 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));
        assert_eq!(s.draw(&mut buf, Rect::new(0, 0, 10, 2)), None, "the cursor is below the view");
        assert_eq!(buf[(0, 0)].symbol(), "1");
        s.scroll_to_bottom();
        assert_eq!(s.scrolled(), 0);
    }
}
