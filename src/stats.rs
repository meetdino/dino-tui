//! `:stats [7d|30d|all]`: the app's Usage window (app/Sources/Dino/Stats.swift) in text, from
//! dinod's `Stats { range }` report and nothing else. The token total with in, out and cache; the
//! split between the conversation, its subagents and side requests as one bar; tokens per day as
//! stacked bars by model (◂ ▸ picks a day); the activity heatmap; each route's requests, errors,
//! limit hits and cost; the sessions that used the most.

use std::collections::HashMap;

use dino_core::usage::Report;
use dino_core::usage::report::{Day, Part, Parts};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, StatsView};
use crate::format;
use crate::ui::fit;

/// The parts' colours: the conversation, subagents, side requests.
const PARTS: [Color; 3] = [Color::Indexed(75), Color::Indexed(179), Color::Indexed(139)];
/// The top models' colours, then everything else.
const MODELS: [Color; 4] = [Color::Indexed(75), Color::Indexed(114), Color::Indexed(179), Color::Indexed(139)];
const OTHER: Color = Color::Indexed(245);
/// The heatmap's five shades: nothing, then four even groups of the active days.
const HEAT: [Color; 5] = [Color::Indexed(236), Color::Indexed(22), Color::Indexed(28), Color::Indexed(34), Color::Indexed(40)];
/// Rows of the tokens-per-day chart.
const CHART_H: usize = 8;
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn grey() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A line built cell by cell, so blocks can be put at columns.
struct Row {
    cells: Vec<(String, Style)>,
}

impl Row {
    fn new(w: usize) -> Row {
        Row { cells: vec![(" ".to_string(), Style::new()); w] }
    }

    fn put(&mut self, x: usize, text: &str, style: Style) {
        let mut x = x;
        for c in text.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if cw == 0 {
                continue;
            }
            if x + cw > self.cells.len() {
                break;
            }
            self.cells[x] = (c.to_string(), style);
            for k in 1..cw {
                self.cells[x + k] = (String::new(), style);
            }
            x += cw;
        }
    }

    fn line(self) -> Line<'static> {
        let mut spans: Vec<Span> = vec![];
        let mut cur = String::new();
        let mut style = Style::new();
        for (t, s) in self.cells {
            if s != style && !cur.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut cur), style));
            }
            style = s;
            cur.push_str(&t);
        }
        let trimmed = cur.trim_end().to_string();
        if !trimmed.is_empty() || style != Style::new() {
            spans.push(Span::styled(if style == Style::new() { trimmed } else { cur }, style));
        }
        Line::from(spans)
    }
}

/// The days the report covers, for the title: `last 30 days · Sep 12 – Oct 11`.
fn covers(r: &Report) -> String {
    let span = match (r.days.first(), r.days.last()) {
        (Some(a), Some(b)) => format!(" · {} – {}", format::day(&a.date), format::day(&b.date)),
        _ => String::new(),
    };
    match r.range.as_str() {
        "7d" => format!("last 7 days{span}"),
        "30d" => format!("last 30 days{span}"),
        _ => format!("all time{span}"),
    }
}

fn part_total(p: &Parts) -> u64 {
    p.main.tokens.total + p.subagents.tokens.total + p.side.tokens.total
}

/// The page, whole; the screen shows it from `v.scroll`.
pub fn lines(app: &App, v: &StatsView, width: usize) -> Vec<Line<'static>> {
    let w = width.max(60);
    let mut out: Vec<Line> = vec![];
    // The title, and the ranges.
    let mut head = Row::new(w);
    head.put(0, "Usage", bold());
    let sub = v.report.as_ref().map_or_else(|| v.range.as_str().to_string(), |r| covers(r));
    head.put(7, &sub, dim());
    let mut x = w.saturating_sub(30);
    for (k, r) in ["7d", "30d", "all"].iter().enumerate() {
        if k > 0 {
            head.put(x, " · ", grey());
            x += 3;
        }
        head.put(x, r, if *r == v.range.as_str() { Style::new().add_modifier(Modifier::REVERSED) } else { grey() });
        x += r.len();
    }
    head.put(x + 3, "r refresh", grey());
    out.push(head.line());
    out.push(Line::raw(""));
    let Some(r) = &v.report else {
        out.push(Line::styled("asking dinod…", dim()));
        return out;
    };
    let t = &r.totals.tokens;
    let mut row = Row::new(w);
    row.put(0, "Tokens", dim());
    row.put(8, &format::tokens(t.total), bold());
    row.put(16, &format!("{} in · {} out · {} cache", format::tokens(t.input), format::tokens(t.output), format::tokens(t.cache_read + t.cache_write)), dim());
    out.push(row.line());
    if r.totals.requests == 0 {
        out.push(Line::raw(""));
        out.push(Line::styled("Nothing used in this range yet.", dim()));
        return out;
    }
    // Where the tokens went, as one bar.
    let parts = &r.totals.parts;
    let total = part_total(parts);
    if total > 0 {
        let bw = w.saturating_sub(50).clamp(20, 60);
        let mut row = Row::new(w);
        let shares: Vec<(&Part, &str)> = vec![(&parts.main, "conversation"), (&parts.subagents, "subagents"), (&parts.side, "side")];
        let mut x = 0;
        for (k, (p, _)) in shares.iter().enumerate() {
            let n = ((p.tokens.total as f64 / total as f64) * bw as f64).round() as usize;
            if n > 1 {
                row.put(x, &"█".repeat(n - 1), Style::new().fg(PARTS[k]));
                x += n;
            } else if p.tokens.total > 0 {
                row.put(x, "▌", Style::new().fg(PARTS[k]));
                x += 2;
            }
        }
        let mut lx = bw + 2;
        for (k, (p, name)) in shares.iter().enumerate() {
            let share = (p.tokens.total as f64 / total as f64 * 100.0).round();
            let text = format!("{name} {share}%");
            row.put(lx, "■", Style::new().fg(PARTS[k]));
            row.put(lx + 2, &text, Style::new());
            lx += text.width() + 4;
        }
        out.push(row.line());
    }
    out.push(Line::raw(""));
    out.extend(chart(r, v, w));
    out.push(Line::raw(""));
    out.extend(activity_and_routes(r, w));
    out.push(Line::raw(""));
    out.extend(sessions(app, r, w));
    out
}

/// Tokens per day, stacked by model: the top four by name, the rest as `other`.
fn chart(r: &Report, v: &StatsView, w: usize) -> Vec<Line<'static>> {
    let mut out = vec![Line::styled("Tokens per day", dim())];
    let top: Vec<String> = r.models.iter().take(4).map(|m| m.model.clone()).collect();
    let slot_of = |model: &str| top.iter().position(|m| m == model).unwrap_or(4);
    // Each day's tokens by slot (0–3 the top models, 4 the rest).
    let mut by_day: HashMap<&str, [u64; 5]> = HashMap::new();
    for d in &r.daily_models {
        by_day.entry(d.date.as_str()).or_default()[slot_of(&d.model)] += d.tokens;
    }
    let legend_w = 34;
    let chart_w = w.saturating_sub(8 + legend_w + 2).clamp(14, 61);
    let n = r.days.len();
    let slot = if n * 2 <= chart_w { 2 } else { 1 };
    let shown: Vec<&Day> = r.days.iter().skip(n.saturating_sub(chart_w / slot)).collect();
    let stacks: Vec<[u64; 5]> = shown
        .iter()
        .map(|d| {
            let mut s = by_day.get(d.date.as_str()).copied().unwrap_or_default();
            // A day the per-model split doesn't cover (dinod leaves undated answers out of both).
            let sum: u64 = s.iter().sum();
            if sum < d.tokens {
                s[4] += d.tokens - sum;
            }
            s
        })
        .collect();
    let max = stacks.iter().map(|s| s.iter().sum::<u64>()).max().unwrap_or(0).max(1);
    let picked = v.day.map(|k| shown.len().saturating_sub(1 + k.min(shown.len().saturating_sub(1))));
    let lx = 8 + chart_w + 5;
    let mut rows: Vec<Row> = (0..CHART_H + 2).map(|_| Row::new(w)).collect();
    rows[0].put(0, &format!("{:>6} ┤", format::tokens(max)), grey());
    for row in rows.iter_mut().take(CHART_H).skip(1) {
        row.put(7, "│", grey());
    }
    rows[CHART_H].put(0, &format!("{:>6} ┼{}", "0", "─".repeat(chart_w)), grey());
    for (j, s) in stacks.iter().enumerate() {
        let x = 8 + j * slot;
        let day_total: u64 = s.iter().sum();
        for level in 0..CHART_H {
            let lo = max as f64 * level as f64 / CHART_H as f64;
            let hi = max as f64 * (level + 1) as f64 / CHART_H as f64;
            if day_total as f64 <= lo {
                break;
            }
            // The model with most of this cell.
            let mut acc = 0f64;
            let mut best = (0f64, 4usize);
            for (k, &tok) in s.iter().enumerate() {
                let (a, b) = (acc, acc + tok as f64);
                acc = b;
                let cover = (b.min(hi) - a.max(lo)).max(0.0);
                if cover > best.0 {
                    best = (cover, k);
                }
            }
            let fill = ((day_total as f64).min(hi) - lo) / (hi - lo);
            let ch = BLOCKS[((fill * 8.0).ceil() as usize).clamp(1, 8) - 1];
            let color = if best.1 < 4 { MODELS[best.1] } else { OTHER };
            rows[CHART_H - 1 - level].put(x, &ch.to_string(), Style::new().fg(color));
        }
        if picked == Some(j) {
            rows[CHART_H].put(x, "▲", Style::new());
        }
    }
    // The dates under it: first, middle, last.
    if let (Some(first), Some(last)) = (shown.first(), shown.last()) {
        rows[CHART_H + 1].put(8, &format::day(&first.date), grey());
        let mid = shown.len() / 2;
        let mx = 8 + mid * slot;
        if shown.len() > 4 {
            rows[CHART_H + 1].put(mx.saturating_sub(2), &format::day(&shown[mid].date), grey());
        }
        let l = format::day(&last.date);
        rows[CHART_H + 1].put((8 + (shown.len() - 1) * slot + 1).saturating_sub(l.width()).max(mx + 8), &l, grey());
    }
    // The legend: the range's tokens per model, or the picked day's.
    let day_stack = picked.map(|j| (shown[j], stacks[j]));
    let mut names: Vec<String> = top.iter().map(|m| format::short_model(m)).collect();
    let mut values: Vec<u64> = match &day_stack {
        Some((_, s)) => s[..top.len()].to_vec(),
        None => r.models.iter().take(4).map(|m| m.tokens.total).collect(),
    };
    let rest = match &day_stack {
        Some((_, s)) => s[4],
        None => r.models.iter().skip(4).map(|m| m.tokens.total).sum(),
    };
    if rest > 0 || names.is_empty() {
        names.push("other".into());
        values.push(rest);
    }
    for (k, (name, value)) in names.iter().zip(&values).enumerate() {
        let color = if k < top.len() { MODELS[k] } else { OTHER };
        let row = &mut rows[1 + k];
        row.put(lx, "■", Style::new().fg(color));
        row.put(lx + 2, &fit(name, 11), Style::new());
        row.put(lx + 14, &format::tokens(*value), dim());
    }
    if let Some((d, s)) = day_stack {
        rows[6].put(lx, &format!("{}: {} tokens", format::day(&d.date), format::tokens(s.iter().sum())), Style::new());
    }
    rows[7].put(lx, if picked.is_some() { "◂ ▸ another day · Esc none" } else { "◂ ▸ pick a day" }, grey());
    out.extend(rows.into_iter().map(Row::line));
    out
}

/// Days since 1970-01-01 of a `2026-10-04`.
fn days_of(date: &str) -> Option<i64> {
    let mut p = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, d) = (p.next()??, p.next()??, p.next()??);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// The heatmap (weeks as columns, Monday on top) beside the routes; one above the other when narrow.
fn activity_and_routes(r: &Report, w: usize) -> Vec<Line<'static>> {
    let side_by_side = w >= 100;
    let weeks = 13usize;
    let mut heat: Vec<Row> = (0..9).map(|_| Row::new(w)).collect();
    heat[0].put(0, &format!("Activity · {weeks} weeks"), dim());
    // Quartiles of the days with any tokens split them into four shades, as the app does.
    let mut active: Vec<u64> = r.heatmap.iter().map(|d| d.tokens).filter(|&t| t > 0).collect();
    active.sort_unstable();
    let cuts: Vec<u64> = if active.is_empty() { vec![1, 1, 1] } else { [0.25, 0.5, 0.75].iter().map(|q| active[((active.len() - 1) as f64 * q) as usize]).collect() };
    let level = |n: u64| if n == 0 { 0 } else { 1 + cuts.iter().filter(|&&c| n > c).count() };
    let by_day: HashMap<i64, &Day> = r.heatmap.iter().filter_map(|d| Some((days_of(&d.date)?, d))).collect();
    let mut shown_active = 0;
    if let Some(last) = r.heatmap.last().and_then(|d| days_of(&d.date)) {
        // 1970-01-01 was a Thursday: Monday is 0.
        let weekday = (last + 3).rem_euclid(7);
        let start = last - weekday - 7 * (weeks as i64 - 1);
        for (row, label) in ["Mon", "", "Wed", "", "Fri", "", "Sun"].iter().enumerate() {
            heat[1 + row].put(0, label, grey());
            for wk in 0..weeks {
                let day = start + wk as i64 * 7 + row as i64;
                if day > last {
                    continue;
                }
                let d = by_day.get(&day);
                if d.is_some_and(|d| d.requests > 0) {
                    shown_active += 1;
                }
                heat[1 + row].put(5 + wk * 2, "■", Style::new().fg(HEAT[level(d.map_or(0, |d| d.tokens))]));
            }
        }
    }
    heat[8].put(5, "Less", grey());
    for (k, c) in HEAT.iter().enumerate() {
        heat[8].put(10 + k * 2, "■", Style::new().fg(*c));
    }
    heat[8].put(21, &format!("More · {shown_active} active {}", if shown_active == 1 { "day" } else { "days" }), grey());
    // The routes: what each carried, and what it said it cost.
    let mut routes: Vec<Row> = vec![];
    let rx = if side_by_side { 44 } else { 0 };
    // The numbers end 6 columns from the edge; the names get what's left.
    let nx = w.saturating_sub(40).max(rx + 26);
    let mut h = Row::new(w);
    h.put(rx, "Routes", dim());
    h.put(nx, "requests  errors  limits     cost", grey());
    routes.push(h);
    if r.routes.is_empty() {
        let mut row = Row::new(w);
        row.put(rx, "none routed through dino", dim());
        routes.push(row);
    }
    let room = if side_by_side { 8 } else { r.routes.len() + 1 };
    for (k, route) in r.routes.iter().enumerate() {
        let mut row = Row::new(w);
        if k + 2 > room && r.routes.len() > room - 1 {
            row.put(rx, &format!("and {} more", r.routes.len() - k), dim());
            routes.push(row);
            break;
        }
        let label = if route.label.is_empty() { &route.route } else { &route.label };
        row.put(rx, &fit(&format::printable(label), nx - rx - 1), Style::new());
        let cost = route.cost.map_or_else(|| "–".to_string(), |c| format!("${c:.2}"));
        row.put(nx, &format!("{:>8}  {:>6}  {:>6}  {:>7}", format::count(route.requests), format::count(route.errors), format::count(route.limit_hits), cost), Style::new());
        routes.push(row);
    }
    if side_by_side {
        // The routes drawn into the heatmap's rows, right of it.
        for (k, rrow) in routes.into_iter().enumerate() {
            if k < heat.len() {
                for (x, cell) in rrow.cells.into_iter().enumerate().skip(rx) {
                    if cell.0 != " " || cell.1 != Style::new() {
                        heat[k].cells[x] = cell;
                    }
                }
            }
        }
        heat.into_iter().map(Row::line).collect()
    } else {
        let mut out: Vec<Line> = heat.into_iter().map(Row::line).collect();
        out.push(Line::raw(""));
        out.extend(routes.into_iter().map(Row::line));
        out
    }
}

/// The sessions that used the most: named as the list names them, else by their folder.
fn sessions(app: &App, r: &Report, w: usize) -> Vec<Line<'static>> {
    let mut out = vec![];
    let mut h = Row::new(w);
    h.put(0, "Top sessions", dim());
    h.put(44, "tokens", grey());
    h.put(56, "subagents", grey());
    out.push(h.line());
    if r.sessions.is_empty() {
        out.push(Line::styled("  none in this range", dim()));
    }
    for s in &r.sessions {
        let name = s
            .session
            .as_ref()
            .and_then(|id| app.sessions.iter().find(|x| &x.id == id))
            .map(format::name)
            .or_else(|| s.cwd.as_deref().map(|c| format::printable(c.trim_end_matches('/').rsplit('/').next().unwrap_or(c))))
            .unwrap_or_else(|| "Untitled".into());
        let sub_t = s.parts.subagents.tokens.total;
        let sub = match (s.subagents, sub_t) {
            (0, 0) => "–".to_string(),
            (0, t) => format::tokens(t),
            (n, 0) => n.to_string(),
            (n, t) => format!("{n} · {}", format::tokens(t)),
        };
        let mut row = Row::new(w);
        row.put(2, &fit(&name, 40), Style::new());
        row.put(44, &format::tokens(s.tokens.total), Style::new());
        row.put(56, &sub, dim());
        out.push(row.line());
    }
    out
}

/// The line under the screen.
pub const HINT: &str = "j/k scroll · ◂ ▸ day · 7 / 3 / a range · r refresh · Esc back";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_count_from_1970() {
        assert_eq!(days_of("1970-01-01"), Some(0));
        assert_eq!(days_of("2026-10-11"), Some(20_737));
        // 2026-10-12 is a Monday.
        assert_eq!((days_of("2026-10-12").unwrap() + 3).rem_euclid(7), 0);
    }
}
