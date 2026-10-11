//! The sidebar beside an attached session, as the app's: the sessions grouped by project or by
//! state, each a mark and a name, a one-line hint for the selected row at its foot (the TUI's
//! tooltip), and the usage panel. Everything in it comes from dinod's `StateChange` and
//! `OutputRate` long-polls (and the repo tree the list asks for anyway); nothing ticks.

use dino_core::ipc::{ClaudeAccountInfo, SessionInfo, WindowInfo};
use dino_core::status::{self, Status};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TLine, Span};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::format;
use crate::order::{self, Detail, Section};
use crate::ui::fit;

/// The sidebar's columns; a separator follows, then the session.
pub const WIDTH: u16 = 28;
/// Where the session's screen starts.
pub const AGENT_X: u16 = WIDTH + 1;
/// Narrower than this, the sidebar is off and attaching is the raw passthrough.
pub const MIN_COLS: u16 = 100;
/// What's inside the sidebar's edges: column 1 to 26.
const INNER: usize = WIDTH as usize - 2;

const SEL: Color = Color::Indexed(237);
const ORANGE: Color = Color::Indexed(214);

#[derive(Default)]
pub struct Side {
    /// The keyboard is in the sidebar (the prefix, then Tab), not the session.
    pub focus: bool,
    /// Where the keyboard is in it.
    pub cursor: Cursor,
    /// The usage panel opened from its one line.
    pub usage_open: bool,
    /// The session attached now.
    pub attached: Option<String>,
    /// The list's first row on screen.
    pub offset: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Cursor {
    /// On the attached session.
    #[default]
    Attached,
    Session(String),
    Usage,
}

/// A line of the list.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Head(Section),
    Session(usize),
    /// What a session that needs you asks, under it.
    Asks(usize),
    Blank,
}

/// The list as the sidebar draws it.
pub fn rows(app: &App) -> Vec<Row> {
    let sections = if app.config.by_state() { order::states(&app.sessions) } else { order::places(&app.sessions, &app.tree) };
    let mut out = vec![];
    for (k, sec) in sections.into_iter().enumerate() {
        if k > 0 {
            out.push(Row::Blank);
        }
        let ids = sec.sessions.clone();
        out.push(Row::Head(sec));
        for i in ids {
            out.push(Row::Session(i));
            if status::needs(&app.sessions[i]).is_some() {
                out.push(Row::Asks(i));
            }
        }
    }
    out
}

impl Side {
    /// The session the sidebar has selected: the cursor's when it has the keyboard, else the one
    /// attached.
    pub fn selected(&self) -> Option<&str> {
        match (&self.cursor, self.focus) {
            (Cursor::Session(id), true) => Some(id),
            (Cursor::Usage, true) => None,
            _ => self.attached.as_deref(),
        }
    }
}

/// What a key does in the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    None,
    /// The keyboard back to the session.
    Back,
    /// Attach to this one instead.
    Switch(String),
    /// The command line, over the list.
    Command,
}

/// The keys the sidebar takes while it has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    Up,
    Down,
    Top,
    Bottom,
    Enter,
    Esc,
    Char(char),
}

impl App {
    /// The sessions in the sidebar's order, by id.
    fn side_order(&self) -> Vec<String> {
        rows(self).iter().filter_map(|r| if let Row::Session(i) = r { Some(self.sessions[*i].id.clone()) } else { None }).collect()
    }

    pub fn side_key(&mut self, k: Key) -> Act {
        let order = self.side_order();
        let at = match &self.side.cursor {
            Cursor::Usage => order.len(),
            Cursor::Session(id) => order.iter().position(|x| x == id).unwrap_or(0),
            Cursor::Attached => self.side.attached.as_ref().and_then(|a| order.iter().position(|x| x == a)).unwrap_or(0),
        };
        // The usage panel is the last stop.
        let to = |i: usize| if i >= order.len() { Cursor::Usage } else { Cursor::Session(order[i].clone()) };
        match k {
            Key::Down | Key::Char('j') => self.side.cursor = to((at + 1).min(order.len())),
            Key::Up | Key::Char('k') => self.side.cursor = to(at.saturating_sub(1)),
            Key::Top | Key::Char('g') => self.side.cursor = to(0),
            Key::Bottom | Key::Char('G') => self.side.cursor = Cursor::Usage,
            Key::Esc | Key::Char('q') => return Act::Back,
            Key::Char(':') => return Act::Command,
            Key::Char('u') => self.side.usage_open = !self.side.usage_open,
            Key::Enter | Key::Char('l') => match to(at) {
                Cursor::Usage => self.side.usage_open = !self.side.usage_open,
                Cursor::Session(id) if Some(&id) == self.side.attached.as_ref() => return Act::Back,
                Cursor::Session(id) => return Act::Switch(id),
                Cursor::Attached => return Act::Back,
            },
            _ => {}
        }
        Act::None
    }
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn grey() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A session's mark, as the app's minimal row: Needs you an orange `!`, Working a green dot (a ring
/// while it waits on subagents or commands), Done a blue dot, an error a red dot, Idle nothing.
pub fn mark(s: &SessionInfo) -> (&'static str, Style) {
    let st = Status::of(s);
    match st {
        Status::NeedsYou => ("!", Style::new().fg(ORANGE).add_modifier(Modifier::BOLD)),
        _ if s.error.is_some() || st == Status::Exited => ("●", Style::new().fg(Color::Red)),
        Status::Working if s.activity.as_deref().is_some_and(|a| a.starts_with("waiting:")) => ("○", Style::new().fg(Color::Green)),
        Status::Working => ("●", Style::new().fg(Color::Green)),
        Status::Done => ("●", Style::new().fg(Color::Blue)),
        _ => (" ", Style::new()),
    }
}

/// Its PR's checks are failing: the row's one PR mark.
fn failing(s: &SessionInfo) -> bool {
    s.pr.as_ref().is_some_and(|p| p.state == "open" && p.checks.failed > 0)
}

/// Text at column `x` of row `y`, cut to `w` columns.
fn put(buf: &mut Buffer, area: Rect, x: usize, y: u16, w: usize, spans: Vec<Span>) {
    if x >= area.width as usize || y >= area.y + area.height {
        return;
    }
    let w = w.min(area.width as usize - x);
    buf.set_line(area.x + x as u16, y, &TLine::from(spans), w as u16);
}

/// `left` and `right` on one line `INNER` wide, from column 1; the left gives way.
fn spread(buf: &mut Buffer, area: Rect, y: u16, left: Vec<Span>, right: Vec<Span>) {
    let rw: usize = right.iter().map(|s| s.content.width()).sum();
    let lw = INNER.saturating_sub(rw + if rw > 0 { 1 } else { 0 });
    let mut used = 0;
    let mut l = vec![];
    for s in left {
        let room = lw.saturating_sub(used);
        if room == 0 {
            break;
        }
        let t = fit(&s.content, room);
        used += t.width();
        l.push(Span::styled(t, s.style));
    }
    put(buf, area, 1, y, lw, l);
    put(buf, area, 1 + INNER - rw, y, rw, right);
}

/// `frac` as a bar `w` cells wide, coloured as the app's quota bars: red at the limit, yellow past
/// 80%, green below.
fn bar(frac: f32, w: usize) -> Vec<Span<'static>> {
    let n = ((frac.clamp(0.0, 1.0) * w as f32).round() as usize).min(w);
    let c = if frac >= 1.0 {
        Color::Red
    } else if frac > 0.8 {
        Color::Yellow
    } else {
        Color::Green
    };
    vec![Span::styled("█".repeat(n), Style::new().fg(c)), Span::styled("░".repeat(w - n), grey())]
}

fn pct(frac: f32) -> String {
    format!("{:>3}%", (frac * 100.0).round() as i64)
}

/// A window's bar line: `5h ███░░░░░░░░░░░  18%`.
fn window_line(label: &str, w: &WindowInfo) -> Vec<Span<'static>> {
    let lw = label.width();
    let bw = INNER.saturating_sub(lw + 1 + 5).min(14);
    let mut spans = vec![Span::styled(format!("{label} "), dim())];
    spans.extend(bar(w.utilization, bw));
    spans.push(Span::raw(format!(" {}", pct(w.utilization))));
    spans
}

fn is_window(w: &WindowInfo) -> bool {
    w.name.ends_with('h') || w.name.ends_with('d')
}

fn is_past(w: &WindowInfo) -> bool {
    w.resets_at.is_some_and(|t| t <= format::now())
}

/// Several Claude accounts: the state lists them (it does only once there's more than one).
fn accounts(app: &App) -> Option<&Vec<ClaudeAccountInfo>> {
    app.state_accounts.as_ref().filter(|a| a.len() > 1)
}

/// The quota windows drawn as bars: every provider's, but Claude's by account when there are several.
fn windows(app: &App) -> Vec<(String, &WindowInfo)> {
    let several = accounts(app).is_some();
    let name = |p: &str| match p {
        "anthropic" => "Claude".to_string(),
        "chatgpt" => "Codex".to_string(),
        other => format::printable(other),
    };
    app.quotas
        .iter()
        .filter(|q| !(several && q.provider == "anthropic"))
        .flat_map(|q| q.windows.iter().filter(|w| is_window(w)).map(move |w| (format!("{} {}", name(&q.provider), w.name), w)))
        .collect()
}

/// The collapsed line: the window closest to its limit (it's the one that stops work), of the
/// account in use when there are several.
fn summary(app: &App) -> Vec<Span<'static>> {
    let accts = accounts(app);
    let answering = accts.and_then(|a| a.iter().find(|a| a.answering));
    if let Some(list) = accts
        && answering.is_none()
    {
        let back = list.iter().filter_map(|a| a.resets_at.or(a.retry_at)).min();
        return vec![Span::styled(back.map_or("Claude at its limit".to_string(), |t| format!("Claude back {}", format::clock(t))), Style::new().fg(Color::Red))];
    }
    let mut current: Vec<(String, &WindowInfo)> = windows(app);
    if let Some(a) = answering {
        current.extend(a.windows.iter().filter(|w| is_window(w)).map(|w| (format!("Claude {}", w.name), w)));
    }
    let fullest = current.into_iter().filter(|(_, w)| !is_past(w)).max_by(|a, b| a.1.utilization.total_cmp(&b.1.utilization));
    let who = answering.filter(|a| a.number != 1).map(|a| format::printable(&a.label()));
    match (fullest, who) {
        (Some((label, w)), Some(who)) => {
            let window = label.rsplit(' ').next().unwrap_or("").to_string();
            vec![Span::styled(format!("{who} {window} "), dim()), Span::raw(pct(w.utilization).trim_start().to_string())]
        }
        (Some((label, w)), None) => vec![Span::styled(format!("{label} "), dim()), Span::raw(pct(w.utilization).trim_start().to_string())],
        (None, Some(who)) => vec![Span::styled(format!("{who} in use"), dim())],
        (None, None) => vec![Span::styled("no data yet", dim())],
    }
}

/// The open panel's lines, under its title.
fn usage_lines(app: &App) -> Vec<(Vec<Span<'static>>, Vec<Span<'static>>)> {
    let mut out: Vec<(Vec<Span>, Vec<Span>)> = vec![];
    let accts = accounts(app);
    if let Some(list) = accts {
        if let Some(a) = list.iter().find(|a| a.answering) {
            out.push((vec![Span::styled(format::printable(&a.label()), bold())], vec![Span::styled("in use", Style::new().fg(Color::Green))]));
            let ws: Vec<&WindowInfo> = a.windows.iter().filter(|w| is_window(w)).collect();
            if ws.is_empty() {
                // Never a made-up 0%: Anthropic says an account's use on the calls it signs.
                out.push((vec![Span::styled("its use isn't reported yet", dim())], vec![]));
            }
            for w in ws {
                out.push((window_line(&w.name, w), vec![]));
            }
        }
        for a in list.iter().filter(|a| !a.answering) {
            let (dot, right) = if a.spent {
                (Style::new().fg(ORANGE), a.resets_at.or(a.retry_at).map_or("at its limit".to_string(), |t| format!("back at {}", format::clock(t))))
            } else {
                (grey(), "ready".to_string())
            };
            out.push((vec![Span::styled("● ", dot), Span::raw(format::printable(&a.label()))], vec![Span::styled(right, dim())]));
        }
    }
    let ws = windows(app);
    for (label, w) in &ws {
        out.push((window_line(label, w), vec![]));
    }
    if ws.is_empty() && accts.is_none() {
        out.push((vec![Span::styled("No quota data yet", dim())], vec![]));
    }
    // Tokens worked through, not the context read again from the prompt cache on every call.
    let used: u64 = app.sessions.iter().map(|s| (s.input_tokens + s.output_tokens).saturating_sub(s.cache_read_tokens)).sum();
    let free: u64 = app.sessions.iter().filter(|s| s.tier.is_some()).map(|s| (s.input_tokens + s.output_tokens).saturating_sub(s.cache_read_tokens)).sum();
    if used > 0 {
        out.push((vec![Span::styled("Open sessions", dim())], vec![Span::raw(format!("{} tok", format::tokens(used)))]));
    }
    if free > 0 {
        out.push((vec![Span::styled("Free models", dim())], vec![Span::raw(format!("{} tok ", format::tokens(free))), Span::styled("$0.00", Style::new().fg(Color::Green))]));
    }
    // The rate first: the hint for the whole picture goes when they don't both fit.
    let rate = format::rate(app.rate);
    let hint = if rate.as_ref().is_none_or(|r| r.width() + 7 <= INNER) { vec![Span::styled(":stats", dim())] } else { vec![] };
    out.push((rate.map(Span::raw).into_iter().collect(), hint));
    out
}

/// The selected row's hint: what the app's tooltip says first.
fn hint(app: &App, s: &SessionInfo) -> [Vec<Span<'static>>; 2] {
    let info = [Some(format::printable(format::agent(s))), format::model(s), format::mode(s)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let mut second = vec![];
    if let Some(b) = order::branch_of(s, &app.tree) {
        second.push(format!("⎇ {}", format::printable(&b)));
    }
    let total = s.input_tokens + s.output_tokens;
    if total > 0 {
        second.push(format!("{} tok", format::tokens(total)));
    }
    if let Some(ms) = s.output_ms_ago {
        second.push(format::ago(ms / 1000));
    }
    if failing(s) {
        second.push("checks failing".into());
    }
    [vec![Span::styled(info, dim())], vec![Span::styled(second.join(" · "), dim())]]
}

/// The sidebar, in `area` (its 28 columns and the separator after them).
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let h = area.height;
    let buf = f.buffer_mut();
    for y in 0..h {
        buf[(area.x + WIDTH, area.y + y)].set_char('│').set_style(grey());
    }
    let sel = app.side.selected().map(String::from);
    let side_area = Rect { width: WIDTH, ..area };
    // From the bottom: the keys, the hint's two lines, a rule, the usage panel, a rule.
    let foot = |n: u16| area.y + h.saturating_sub(n);
    let keys_y = foot(2);
    let caret = app.config.prefix().caret();
    let keys = if app.side.focus { "j/k · Enter go · Esc back".to_string() } else { format!("{caret} Tab list  {caret} s hide") };
    put(buf, side_area, 1, keys_y, INNER, vec![Span::styled(keys, grey())]);
    let selected = sel.as_deref().and_then(|id| app.sessions.iter().find(|s| s.id == id));
    let [h1, h2] = match (selected, &app.side.cursor, app.side.focus) {
        (_, Cursor::Usage, true) => [vec![Span::styled(if app.side.usage_open { "Enter folds it" } else { "Enter opens it" }, dim())], vec![]],
        (Some(s), _, _) => hint(app, s),
        _ => [vec![], vec![]],
    };
    put(buf, side_area, 1, foot(4), INNER, h1);
    put(buf, side_area, 1, foot(3), INNER, h2);
    let rule = |buf: &mut Buffer, y: u16| put(buf, side_area, 1, y, INNER, vec![Span::styled("─".repeat(INNER), grey())]);
    rule(buf, foot(5));
    let lines = if app.side.usage_open { usage_lines(app) } else { vec![] };
    let usage_y = foot(6 + lines.len() as u16);
    let on_usage = app.side.focus && app.side.cursor == Cursor::Usage;
    if on_usage {
        buf.set_style(Rect { x: area.x, y: usage_y, width: WIDTH, height: 1 }, Style::new().bg(SEL));
    }
    let head_right = if app.side.usage_open { vec![Span::styled("▾", grey())] } else { [summary(app), vec![Span::styled(" ▸", grey())]].concat() };
    spread(buf, side_area, usage_y, vec![Span::styled("Usage", bold())], head_right);
    for (k, (l, r)) in lines.into_iter().enumerate() {
        spread(buf, side_area, usage_y + 1 + k as u16, l, r);
    }
    let top_rule = usage_y.saturating_sub(1);
    rule(buf, top_rule);

    // The list, from the second row to above the rule (a blank row before it).
    let list_top = area.y + 1;
    let list_h = top_rule.saturating_sub(list_top + 1) as usize;
    let all = rows(app);
    // Keep the selected row (and what it asks) in view.
    if let Some(at) = sel.as_deref().and_then(|id| all.iter().position(|r| matches!(r, Row::Session(i) if app.sessions[*i].id == id))) {
        let end = at + if matches!(all.get(at + 1), Some(Row::Asks(_))) { 2 } else { 1 };
        if at < app.side.offset {
            app.side.offset = at.saturating_sub(1);
        }
        if end > app.side.offset + list_h {
            app.side.offset = end.saturating_sub(list_h);
        }
    }
    app.side.offset = app.side.offset.min(all.len().saturating_sub(list_h));
    for (k, row) in all.iter().skip(app.side.offset).take(list_h).enumerate() {
        let y = list_top + k as u16;
        match row {
            Row::Blank => {}
            Row::Head(sec) => {
                let mut spans = vec![Span::styled(sec.name.clone(), bold())];
                match &sec.detail {
                    Detail::Path(p) => spans.push(Span::styled(format!("  {p}"), dim())),
                    Detail::Branch(b) => spans.push(Span::styled(format!(" ⎇ {b}"), bold())),
                    Detail::Count(n) => spans.push(Span::styled(format!("  {n}"), dim())),
                }
                let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
                let cut = fit(&text, INNER);
                // Cut as a whole, styled as the parts were.
                let mut out = vec![];
                let mut left = cut.width();
                for s in spans {
                    if left == 0 {
                        break;
                    }
                    let t = fit(&s.content, left);
                    left -= t.width().min(left);
                    out.push(Span::styled(t, s.style));
                }
                put(buf, side_area, 1, y, INNER, out);
            }
            Row::Session(i) => {
                let s = &app.sessions[*i];
                let on = sel.as_deref() == Some(s.id.as_str());
                if on {
                    buf.set_style(Rect { x: area.x, y, width: WIDTH, height: 1 }, Style::new().bg(SEL));
                }
                let (m, ms) = mark(s);
                put(buf, side_area, 2, y, 1, vec![Span::styled(m, ms)]);
                let st = Status::of(s);
                let mut ns = match st {
                    Status::Done => bold(),
                    Status::Idle | Status::Ended => dim(),
                    _ => Style::new(),
                };
                if on {
                    ns = bold();
                }
                let fail = failing(s);
                let room = INNER - 3 - if fail { 3 } else { 0 };
                let mut name = vec![Span::styled(fit(&format::name(s), room), ns)];
                if app.config.by_state() {
                    // Grouped by state, the row says where it is, as the app's tag.
                    let used = name[0].content.width();
                    let tag = order::project_of(s, &app.tree);
                    if room >= used + 2 + tag.width() {
                        name.push(Span::styled(format!("  {tag}"), dim()));
                    }
                }
                put(buf, side_area, 4, y, room, name);
                if fail {
                    put(buf, side_area, WIDTH as usize - 3, y, 1, vec![Span::styled("✕", Style::new().fg(Color::Red))]);
                }
            }
            Row::Asks(i) => {
                let q = status::needs(&app.sessions[*i]).map(format::printable).unwrap_or_default();
                put(buf, side_area, 5, y, INNER - 4, vec![Span::styled(fit(&q, INNER - 4), dim())]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_and_marks() {
        let b = bar(0.18, 14);
        assert_eq!((b[0].content.as_ref(), b[1].content.chars().count()), ("███", 11));
        assert_eq!(bar(1.0, 12)[0].style.fg, Some(Color::Red));
        assert_eq!(pct(0.18), " 18%");
    }
}
