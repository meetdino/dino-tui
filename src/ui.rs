//! Drawing: one screen at a time, the bottom line always. The terminal's own colours only.

use dino_core::ipc::SessionInfo;
use dino_core::status::Status;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TLine, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Ask, HELP, Line, NewForm, Overlay, Picker, Row, Screen};
use crate::format;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn rev() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

/// `s` cut to `w` columns, with `…` when it was wider.
pub fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    if w > 0 {
        out.push('…');
    }
    out
}

/// `s` padded (or cut) to exactly `w` columns.
pub fn pad(s: &str, w: usize) -> String {
    let s = fit(s, w);
    let n = w.saturating_sub(s.width());
    format!("{s}{}", " ".repeat(n))
}

pub fn rpad(s: &str, w: usize) -> String {
    let s = fit(s, w);
    format!("{}{s}", " ".repeat(w.saturating_sub(s.width())))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.size = (area.width, area.height);
    if area.height == 0 {
        return;
    }
    let body = Rect { height: area.height - 1, ..area };
    let bottom = Rect { y: area.y + area.height - 1, height: 1, ..area };
    match &app.screen {
        Screen::List => list(f, app, body),
        Screen::Setup(_) => crate::setup::draw(f, app, body),
        Screen::Accounts(_) => accounts(f, app, body),
        Screen::Stats(_) => stats(f, app, body),
        Screen::Found(_) => found(f, app, body),
        Screen::Archived(_) => archived(f, app, body),
    }
    bottom_line(f, app, bottom);
    match &app.overlay {
        Some(Overlay::Help) => help(f, body, &app.config.prefix().label),
        Some(Overlay::Picker(p)) => picker(f, p, body),
        None => {}
    }
}

fn centered(f: &mut Frame, area: Rect, text: &str) {
    let y = area.y + area.height / 2;
    let line = TLine::styled(fit(text, area.width as usize), dim());
    let w = line.width() as u16;
    f.render_widget(line, Rect { x: area.x + area.width.saturating_sub(w) / 2, y, width: w.min(area.width), height: 1 });
}

// ---- The list ----

fn list(f: &mut Frame, app: &mut App, area: Rect) {
    app.scroll(area.height as usize);
    if !app.got_state {
        return centered(f, area, "asking dinod…");
    }
    if app.rows.is_empty() {
        let text = if app.filter.is_empty() { "No sessions. o starts one here; :new <agent> [folder] anywhere." } else { "Nothing matches. Esc clears the filter." };
        return centered(f, area, text);
    }
    let width = area.width as usize;
    // The name column: as wide as the widest name on screen, within bounds.
    let shown: Vec<Row> = app.rows.iter().skip(app.offset).take(area.height as usize).copied().collect();
    let name_w = app.rows.iter().filter_map(|r| if let Row::Session(i) = r { Some(format::name(&app.sessions[*i]).width()) } else { None }).max().unwrap_or(10).clamp(10, 32);
    for (k, row) in shown.iter().enumerate() {
        let y = area.y + k as u16;
        let rect = Rect { y, height: 1, ..area };
        let line = match row {
            Row::Group(g) => {
                let g = &app.groups[*g];
                let path = format::short_path(&g.path);
                let mut spans = vec![Span::styled(fit(&g.name, width), bold())];
                if path != g.name && width > g.name.width() + 4 {
                    spans.push(Span::styled(format!("  {}", fit(&path, width - g.name.width() - 2)), dim()));
                }
                TLine::from(spans)
            }
            Row::Session(i) => session_line(&app.sessions[*i], name_w, width, app.spinner, app.offset + k == app.cursor),
        };
        f.render_widget(line, rect);
    }
}

/// `  ●  name        claude · opus · bypass     12.4k tok   3m`
pub fn session_line(s: &SessionInfo, name_w: usize, width: usize, spin: usize, selected: bool) -> TLine<'static> {
    let st = Status::of(s);
    let (glyph, gstyle) = match st {
        Status::NeedsYou => ("●".to_string(), Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Status::Working => (SPINNER[spin % SPINNER.len()].to_string(), Style::new().fg(Color::Green)),
        Status::Done => ("✓".to_string(), Style::new().fg(Color::Green)),
        Status::Idle => ("○".to_string(), Style::new()),
        Status::Ended => ("–".to_string(), dim()),
        Status::Exited => ("✗".to_string(), Style::new().fg(Color::Red)),
    };
    let ended = matches!(st, Status::Ended | Status::Exited);
    let base = if ended { dim() } else { Style::new() };
    let name_style = if st == Status::NeedsYou { bold() } else { base };
    let info = [Some(format::printable(format::agent(s))), format::model(s), format::mode(s)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let total = s.input_tokens + s.output_tokens;
    let tok = if total > 0 { format!("{} tok", format::tokens(total)) } else { String::new() };
    let ago = s.output_ms_ago.map(|ms| format::ago(ms / 1000)).unwrap_or_default();
    // Narrow: the agent goes first, then the tokens.
    let fixed = 2 + 1 + 2 + name_w + 2;
    let right = if width >= fixed + 8 + 2 + 10 + 2 + 4 {
        10 + 2 + 4
    } else if width >= fixed + 6 {
        4
    } else {
        0
    };
    let info_w = width.saturating_sub(fixed + right + 2);
    let lead = if s.pinned { "▴ " } else { "  " };
    let mut spans = vec![Span::styled(lead, dim()), Span::styled(glyph, gstyle), Span::raw("  "), Span::styled(pad(&format::name(s), name_w), name_style), Span::raw("  ")];
    if info_w >= 6 {
        spans.push(Span::styled(pad(&info, info_w), dim().patch(base)));
        spans.push(Span::raw("  "));
    } else {
        spans.push(Span::raw(" ".repeat(info_w + if right > 0 { 2 } else { 0 })));
    }
    if right == 16 {
        spans.push(Span::styled(rpad(&tok, 10), base));
        spans.push(Span::raw("  "));
    }
    if right > 0 {
        spans.push(Span::styled(rpad(&ago, 4), dim()));
    }
    let mut line = TLine::from(spans);
    if selected {
        // The whole row reversed, to the edge, in one colour: dim and coloured parts read badly
        // reversed.
        let w = line.width();
        if w < width {
            line.spans.push(Span::raw(" ".repeat(width - w)));
        }
        for s in &mut line.spans {
            s.style = if s.style.add_modifier.contains(Modifier::BOLD) { rev().add_modifier(Modifier::BOLD) } else { rev() };
        }
    }
    line
}

// ---- The bottom line ----

fn bottom_line(f: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    match &app.line {
        Line::Filter(i) => return input_line(f, area, "/", &i.text, i.cursor),
        Line::Ask(ask, i) => {
            let name = |id: &str| app.sessions.iter().find(|s| s.id == id).map_or_else(|| id.to_string(), format::name);
            let label = match ask {
                Ask::Send(id) => format!("send to {} ▸ ", name(id)),
                Ask::Rename(id) => format!("rename {} ▸ ", name(id)),
                Ask::Command => ":".into(),
                Ask::AccountName(n) => format!("name Account {n} ▸ "),
                Ask::LoginCode => "code from claude.ai ▸ ".into(),
            };
            return input_line(f, area, &label, &i.text, i.cursor);
        }
        Line::Confirm(_, text) => return f.render_widget(TLine::styled(fit(text, width), bold()), area),
        Line::New(form) => return new_line(f, app, form, area),
        Line::Normal => {}
    }
    // Left: what just happened, else where things stand.
    let left = if !app.connected {
        Span::styled("dinod stopped: waiting for it to come back", Style::new().fg(Color::Red))
    } else if let Some(m) = &app.message {
        Span::styled(m.text.clone(), if m.error { Style::new().fg(Color::Red) } else { Style::new() })
    } else if !app.filter.is_empty() && matches!(app.screen, Screen::List) {
        Span::styled(format!("/{}  (Esc clears)", app.filter), dim())
    } else {
        Span::styled(hint(app), dim())
    };
    // Right: the live tok/s (hidden at 0), the Claude account and its quota, what needs you. When
    // it's tight, the rate and then the account give way, what needs you last.
    // The setup card says where things stand instead; accounts and usage show it all above.
    let mut parts: Vec<Span> = vec![];
    match &app.screen {
        Screen::Setup(_) => parts.push(Span::styled(crate::setup::summary(app), dim())),
        Screen::Accounts(_) | Screen::Stats(_) => {}
        _ => {
            if let Some(r) = format::rate(app.rate) {
                parts.push(Span::raw(r));
            }
            if let Some(a) = app.account_line() {
                parts.push(Span::raw(a));
            }
            let needs = app.needs_count();
            if needs > 0 {
                parts.push(Span::styled(format::need_you(needs), Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
            }
        }
    }
    let room = width.saturating_sub(left.content.width().min(width / 3) + 2);
    let joined = |parts: &[Span<'static>]| {
        let mut spans = vec![];
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", dim()));
            }
            spans.push(p.clone());
        }
        TLine::from(spans)
    };
    while parts.len() > 1 && joined(&parts).width() > room {
        parts.remove(0);
    }
    let right = joined(&parts);
    let rw = (right.width() + 1).min(width);
    let lw = width.saturating_sub(rw + 1);
    let left = Span::styled(fit(&left.content, lw), left.style);
    f.render_widget(TLine::from(left), Rect { width: lw as u16, ..area });
    f.render_widget(right, Rect { x: area.x + area.width - (rw as u16).saturating_sub(1).min(area.width), width: rw as u16, ..area });
}

fn hint(app: &App) -> String {
    match &app.screen {
        Screen::List if app.rows.is_empty() => "o new · : command · ? keys · q quit".into(),
        Screen::List => "Enter attach · o new · : command · ? keys".into(),
        Screen::Accounts(_) if app.login_under_way() => "c paste the code · x cancel the sign-in · Esc back".into(),
        Screen::Accounts(_) => "Esc back".into(),
        Screen::Stats(_) => crate::stats::HINT.into(),
        Screen::Setup(_) if app.login_under_way() => "c paste the code · x cancel the sign-in · Esc done".into(),
        Screen::Setup(_) => crate::setup::HINT.into(),
        Screen::Found(_) => "Enter continue it in dino · Esc back".into(),
        Screen::Archived(_) => "Enter bring it back · Esc back".into(),
    }
}

fn input_line(f: &mut Frame, area: Rect, label: &str, text: &str, cursor: usize) {
    let width = area.width as usize;
    let lw = label.width();
    // Long input scrolls so the cursor stays in view.
    let before: String = text.chars().take(cursor).collect();
    let room = width.saturating_sub(lw + 1);
    let skip = before.width().saturating_sub(room);
    let mut shown = String::new();
    let mut col = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if col >= skip {
            shown.push(c);
        }
        col += w;
    }
    f.render_widget(TLine::from(vec![Span::styled(label.to_string(), bold()), Span::raw(fit(&shown, room))]), area);
    let x = area.x + (lw + before.width() - skip).min(width.saturating_sub(1)) as u16;
    f.set_cursor_position((x, area.y));
}

fn new_line(f: &mut Frame, app: &App, form: &NewForm, area: Rect) {
    let agent = app.launchers.get(form.agent).map_or("…".to_string(), |l| format::printable(&l.short));
    let field = |k: usize, label: &str, value: String| {
        let style = if form.field == k { rev() } else { Style::new() };
        vec![Span::styled(format!("{label}: "), dim()), Span::styled(value, style), Span::raw("   ")]
    };
    let mut spans = vec![Span::styled("new ▸ ", bold())];
    spans.extend(field(0, "agent", agent.clone()));
    spans.extend(field(1, "folder", form.folder.text.clone()));
    spans.extend(field(2, "worktree", if form.worktree { "yes" } else { "no" }.into()));
    spans.push(Span::styled("prompt: ", dim()));
    let before_prompt = TLine::from(spans.clone()).width();
    spans.push(Span::raw(form.prompt.text.clone()));
    let picked: Vec<&str> = [&form.controls.mode, &form.controls.model, &form.controls.effort].into_iter().filter_map(|v| v.as_deref()).collect();
    if !picked.is_empty() {
        spans.push(Span::styled(format!("   ({})", picked.join(" · ")), dim()));
    }
    f.render_widget(TLine::from(spans), area);
    let cursor = match form.field {
        1 => Some(TLine::from(vec![Span::raw("new ▸ "), Span::raw(format!("agent: {agent}   folder: "))]).width() + form.folder.text.chars().take(form.folder.cursor).collect::<String>().width()),
        3 => Some(before_prompt + form.prompt.text.chars().take(form.prompt.cursor).collect::<String>().width()),
        _ => None,
    };
    if let Some(x) = cursor.filter(|&x| x < area.width as usize) {
        f.set_cursor_position((area.x + x as u16, area.y));
    }
}

// ---- Accounts ----

const SEL: Color = Color::Indexed(237);

/// A quota bar `w` cells wide, as the app colours them: red at the limit, yellow past 80%.
fn quota_bar(frac: f32, w: usize) -> Vec<Span<'static>> {
    let n = ((frac.clamp(0.0, 1.0) * w as f32).round() as usize).min(w);
    let c = if frac >= 1.0 {
        Color::Red
    } else if frac > 0.8 {
        Color::Yellow
    } else {
        Color::Green
    };
    vec![Span::styled("█".repeat(n), Style::new().fg(c)), Span::styled("░".repeat(w - n), Style::new().fg(Color::DarkGray))]
}

/// Who answers, and in what order the others take over: from the one in use on, then (when the
/// user chose it) round to the ones before it.
pub fn in_use_text(list: &[dino_core::ipc::ClaudeAccountInfo], chosen: &[u32]) -> Vec<(String, bool)> {
    let mut out = vec![];
    let Some(at) = list.iter().position(|a| a.answering) else {
        if !list.is_empty() {
            out.push(("Every account is at its limit: Claude Code waits for the first one back.".to_string(), false));
        }
        return out;
    };
    let a = &list[at];
    // dinod says which account it goes to first (`chosen`): the user's choice, else account 1. A
    // dinod from before `use` says none, and tries account 1 first.
    let first = chosen.first().copied().unwrap_or(1);
    let picked = first != 1;
    let first_name = list.iter().find(|x| x.number == first).map(|x| format::printable(&x.label()));
    // From the one in use on, then round to the ones before the first (dinod tries them last).
    let from = list.iter().position(|x| x.number == first).unwrap_or(0);
    let order: Vec<&dino_core::ipc::ClaudeAccountInfo> = list[from..].iter().chain(list[..from].iter()).collect();
    let me = order.iter().position(|x| x.number == a.number).unwrap_or(0);
    let after: Vec<String> = order[me + 1..].iter().map(|x| x.number.to_string()).collect();
    let by = match (a.number == first, a.number == 1) {
        (true, false) => ", chosen by you".to_string(),
        (true, true) => ", Claude Code's own sign-in".to_string(),
        (false, _) => match &first_name {
            Some(n) => format!(", while {n} is at its limit"),
            None => String::new(),
        },
    };
    let then = if after.is_empty() { String::new() } else { format!(" When it's spent, Claude Code falls back to {}.", after.join(", then ")) };
    out.push((format!("In use: {}{by}.{then}", format::printable(&a.label())), false));
    if picked {
        out.push(("Enter on 1 goes back to Claude Code's own sign-in.".to_string(), true));
    }
    out
}

fn accounts(f: &mut Frame, app: &App, area: Rect) {
    let Screen::Accounts(v) = &app.screen else { return };
    let width = area.width as usize;
    let grey = Style::new().fg(Color::DarkGray);
    let keys = if app.unsupported.contains("use") { "J/K fallback order · a add · r rename · dd remove" } else { "Enter use · J/K fallback order · a add · r rename · dd remove" };
    let mut head = vec![Span::styled("Claude accounts", bold())];
    let kw = keys.width();
    if width >= kw + 20 {
        head.push(Span::raw(" ".repeat(width - kw - 15 - 2)));
        head.push(Span::styled(keys, grey));
    }
    f.render_widget(TLine::from(head), Rect { height: 1, ..area });
    let Some(info) = &app.accounts else {
        f.render_widget(TLine::styled("asking dinod…", dim()), Rect { y: area.y + 2, height: 1, ..area });
        return;
    };
    let mut y = area.y + 2;
    let bottom = area.y + area.height;
    for (i, a) in info.accounts.iter().enumerate() {
        if y >= bottom {
            break;
        }
        let on = i == v.cursor;
        let row = Rect { y, height: 1, ..area };
        if on {
            f.buffer_mut().set_style(row, Style::new().bg(SEL));
        }
        let (state, ss) = if a.answering {
            ("in use".to_string(), Style::new().fg(Color::Green))
        } else if a.spent {
            (a.resets_at.or(a.retry_at).map_or("at its limit".to_string(), |t| format!("back at {}", format::clock(t))), Style::new().fg(Color::Indexed(214)))
        } else {
            ("ready".to_string(), dim())
        };
        let mut spans = vec![
            Span::styled(if on { "▸ " } else { "  " }, Style::new().fg(Color::Cyan)),
            Span::styled(format!("{:<3}", a.number), dim()),
            Span::styled(pad(&format::printable(&a.label()), 19), if on { bold() } else { Style::new() }),
            Span::styled(pad(&a.plan.as_deref().map(format::printable).unwrap_or_default(), 7), dim()),
            Span::styled(pad(&state, 18), ss),
        ];
        // Its 5h and 7d bars, when Anthropic has reported them: never a made-up 0%.
        let windows: Vec<_> = a.windows.iter().filter(|w| w.name.ends_with('h') || w.name.ends_with('d')).collect();
        if windows.is_empty() {
            spans.push(Span::styled("use not reported yet", dim()));
        }
        for (k, w) in windows.iter().take(2).enumerate() {
            if k > 0 {
                spans.push(Span::raw("      "));
            }
            spans.push(Span::styled(format!("{} ", format::printable(&w.name)), dim()));
            spans.extend(quota_bar(w.utilization, 12));
            spans.push(Span::raw(format!(" {:>3}%", (w.utilization * 100.0).round() as i64)));
        }
        f.render_widget(TLine::from(spans), row);
        y += 2;
    }
    let mut lines: Vec<TLine> = in_use_text(&info.accounts, &app.chosen).into_iter().map(|(t, d)| if d { TLine::styled(t, dim()) } else { TLine::raw(t) }).collect();
    if info.accounts.len() < 2 {
        lines.push(TLine::styled("a adds another: Claude Code goes on with the next one when one is at its limit.", dim()));
    }
    if let Some(say) = &app.switched {
        lines.push(TLine::raw(""));
        lines.push(TLine::styled(say.clone(), Style::new().fg(Color::Yellow)));
    }
    if let Some(e) = &info.error {
        lines.push(TLine::raw(""));
        lines.push(TLine::styled(format::printable(e), Style::new().fg(Color::Red)));
    }
    if let Some(l) = &info.login {
        lines.push(TLine::raw(""));
        let say = match l.stage.as_str() {
            "starting" => "Signing in: starting Claude Code's sign-in…".to_string(),
            "browser" if app.setup.headless => "Signing in: open this link somewhere with a browser, sign in, then press c and paste the code it shows:".to_string(),
            "browser" => {
                "Signing in: a browser opened on claude.ai where dinod runs. Signing in somewhere else (SSH)? Open this link there, sign in, then press c and paste the code it shows:".to_string()
            }
            "checking" => "Signing in: checking the account with Anthropic…".to_string(),
            "added" => format!("Added Account {}.", l.account.unwrap_or(0)),
            "failed" => format!("The sign-in didn't work: {}", format::printable(l.error.as_deref().unwrap_or("no reason given"))),
            "cancelled" => "The sign-in was cancelled.".to_string(),
            other => format!("Signing in: {}", format::printable(other)),
        };
        lines.push(TLine::raw(say));
        if let (Some(url), "browser") = (&l.url, l.stage.as_str()) {
            lines.push(TLine::raw(""));
            // Whole, unstyled, so it can be copied from the terminal.
            lines.push(TLine::raw(format::printable(url)));
        }
    }
    if y < bottom {
        let rest = Rect { x: area.x + 2, y, height: bottom - y, width: area.width.saturating_sub(2) };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rest);
    }
}

// ---- Usage ----

fn stats(f: &mut Frame, app: &App, area: Rect) {
    let Screen::Stats(v) = &app.screen else { return };
    let lines = crate::stats::lines(app, v, area.width as usize);
    let skip = v.scroll.min(lines.len().saturating_sub(1));
    f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), area);
}

// ---- Found and archived ----

#[allow(clippy::too_many_arguments, reason = "one screen's parts, each said once")]
fn pick_rows(f: &mut Frame, area: Rect, title: &str, sub: &str, rows: Vec<String>, cursor: usize, loading: bool, empty: &str) {
    let width = area.width as usize;
    let mut lines = vec![TLine::from(vec![Span::styled(title.to_string(), bold()), Span::styled(format!("  {sub}"), dim())]), TLine::raw("")];
    if loading {
        lines.push(TLine::styled("asking dinod…", dim()));
    } else if rows.is_empty() {
        lines.push(TLine::styled(empty.to_string(), dim()));
    }
    let room = (area.height as usize).saturating_sub(2).max(1);
    let skip = cursor.saturating_sub(room - 1);
    for (i, r) in rows.iter().enumerate().skip(skip).take(room) {
        let text = pad(r, width);
        lines.push(if i == cursor { TLine::styled(text, rev()) } else { TLine::raw(text) });
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn found(f: &mut Frame, app: &App, area: Rect) {
    let Screen::Found(l) = &app.screen else { return };
    let now = format::now();
    let rows = l
        .items
        .iter()
        .map(|s| {
            let state = match (s.pid, s.status.as_deref()) {
                (Some(_), Some("busy")) => "working",
                (Some(_), _) => "running",
                (None, _) => "",
            };
            let folder = s.cwd.as_deref().map(format::short_path).unwrap_or_default();
            let title = if s.title.trim().is_empty() { "(untitled)".to_string() } else { format::printable(s.title.trim()) };
            format!(
                "  {}  {}  {}  {}  {}",
                pad(&format::printable(&s.agent), 8),
                pad(&title, 40),
                pad(&folder, 28),
                rpad(&format::ago(now.saturating_sub(s.updated_at / if s.updated_at > 10_000_000_000 { 1000 } else { 1 })), 4),
                state
            )
        })
        .collect();
    pick_rows(f, area, "Found", "agent sessions on this machine started outside dino", rows, l.cursor, l.loading, "none found");
}

fn archived(f: &mut Frame, app: &App, area: Rect) {
    let Screen::Archived(l) = &app.screen else { return };
    let now = format::now();
    let rows = l
        .items
        .iter()
        .map(|a| {
            let name = format::printable(a.label.as_deref().unwrap_or(&a.name));
            let branch = a.branch.as_deref().map(format::printable).unwrap_or_default();
            format!(
                "  {}  {}  {}  {}  {}",
                pad(&name, 28),
                pad(&format::printable(&a.launcher), 8),
                pad(&format::short_path(&a.cwd), 28),
                pad(&branch, 20),
                rpad(&format::ago(now.saturating_sub(a.archived_at)), 4)
            )
        })
        .collect();
    pick_rows(f, area, "Archived", "stopped sessions kept to pick up later", rows, l.cursor, l.loading, "nothing archived")
}

// ---- Overlays ----

fn boxed(f: &mut Frame, area: Rect, w: u16, h: u16, title: &str) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, r);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(format!(" {title} "), bold()));
    let inner = block.inner(r);
    f.render_widget(block, r);
    inner
}

fn help(f: &mut Frame, area: Rect, prefix: &str) {
    // The prefix as it's set: `C-\ d`.
    let keys: Vec<(String, &str)> = HELP.iter().map(|(k, d)| (k.replace("prefix", prefix), *d)).collect();
    let kw = keys.iter().map(|(k, _)| k.width()).max().unwrap_or(10);
    let w = keys.iter().map(|(_, d)| d.width()).max().unwrap_or(30) + kw + 6;
    let inner = boxed(f, area, w as u16, keys.len() as u16 + 2, "keys");
    let lines: Vec<TLine> = keys.iter().map(|(k, d)| TLine::from(vec![Span::styled(format!(" {}", pad(k, kw + 2)), bold()), Span::raw(d.to_string())])).collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn picker(f: &mut Frame, p: &Picker, area: Rect) {
    let col_w = p.cols.iter().map(|c| c.options.iter().map(|(_, l)| l.width()).max().unwrap_or(8).max(c.name.len())).map(|w| w + 4).collect::<Vec<_>>();
    let h = p.cols.iter().map(|c| c.options.len()).max().unwrap_or(1) + 4;
    const FOOT: &str = " j/k choose · h/l column · Enter apply · Esc";
    let w = col_w.iter().sum::<usize>().max(p.title.width() + 6).max(FOOT.width() + 1) + 2;
    let inner = boxed(f, area, w as u16, h as u16, &fit(&p.title, w.saturating_sub(4)));
    let mut x = inner.x + 1;
    for (ci, c) in p.cols.iter().enumerate() {
        let active = ci == p.col;
        let mut lines = vec![TLine::styled(c.name, if active { bold().add_modifier(Modifier::UNDERLINED) } else { dim() })];
        for (oi, (_, label)) in c.options.iter().enumerate() {
            let style = match (oi == c.cursor, active) {
                (true, true) => rev(),
                (true, false) => bold(),
                _ => Style::new(),
            };
            lines.push(TLine::styled(label.clone(), style));
        }
        let cw = col_w[ci] as u16;
        f.render_widget(Paragraph::new(lines), Rect { x, y: inner.y, width: cw.min(inner.x + inner.width - x), height: inner.height.saturating_sub(1) });
        x += cw;
    }
    let foot = Rect { y: inner.y + inner.height.saturating_sub(1), height: 1, ..inner };
    match &p.typing {
        Some(i) => input_line(f, foot, " model ▸ ", &i.text, i.cursor),
        None => f.render_widget(TLine::styled(FOOT, dim()), foot),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_answers_and_who_follows() {
        let list = |answering: u32, spent: &[u32]| -> Vec<dino_core::ipc::ClaudeAccountInfo> {
            let names = ["ada", "work", "spare", "side"];
            let rows: Vec<serde_json::Value> = (1..=4u32).map(|n| serde_json::json!({"number": n, "answering": n == answering, "spent": spent.contains(&n), "name": names[n as usize - 1]})).collect();
            serde_json::from_value(serde_json::Value::Array(rows)).unwrap()
        };
        let text = |l: &[dino_core::ipc::ClaudeAccountInfo], chosen: &[u32]| in_use_text(l, chosen).into_iter().map(|(t, _)| t).collect::<Vec<_>>().join(" | ");
        // A dinod from before `use` (no `chosen`), and a newer one's default (account 1): the same.
        let own = "In use: ada, Claude Code's own sign-in. When it's spent, Claude Code falls back to 2, then 3, then 4.";
        assert_eq!(text(&list(1, &[]), &[]), own);
        assert_eq!(text(&list(1, &[]), &[1]), own);
        // Chosen: from it on, then round to the ones before it.
        assert_eq!(text(&list(2, &[]), &[2]), "In use: work, chosen by you. When it's spent, Claude Code falls back to 3, then 4, then 1. | Enter on 1 goes back to Claude Code's own sign-in.");
        // The chosen one spent: the next answers.
        assert_eq!(text(&list(3, &[2]), &[2]), "In use: spare, while work is at its limit. When it's spent, Claude Code falls back to 4, then 1. | Enter on 1 goes back to Claude Code's own sign-in.");
        assert_eq!(text(&list(2, &[1]), &[]), "In use: work, while ada is at its limit. When it's spent, Claude Code falls back to 3, then 4.");
    }

    #[test]
    fn fit_and_pad_count_columns() {
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(rpad("ab", 4), "  ab");
        assert_eq!(fit("日本語", 4), "日…");
    }
}
