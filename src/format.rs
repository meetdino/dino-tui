//! How things read on screen: a session's name and folder, token counts, how long ago. The same
//! words as `dino ls` and the app.

use dino_core::ipc::SessionInfo;

/// `s` safe to draw: control characters (C0, DEL and C1), with which a title or a folder name
/// could move the cursor or retitle the window, become `?`.
pub fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// What the app and `dino ls` call a session: the name you gave it, else what its agent calls the
/// conversation, else its own name (`claude-2`).
pub fn name(s: &SessionInfo) -> String {
    let title = |t: &str| {
        let t = t.trim_start_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '~' | '/' | '.'))).trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    // Until it has a topic, an agent's title is only its own name ("Claude Code").
    let topic = |t: String| {
        let words: Vec<_> = t.split_whitespace().collect();
        let only_agent = words.len() <= 2 && words.first().is_some_and(|w| s.agent_id.to_lowercase().starts_with(&w.to_lowercase()));
        (!only_agent).then_some(t)
    };
    let shown = s.label.clone().or_else(|| match &s.inside {
        Some(f) => title(&f.title),
        None if s.agent_id != "shell" => s.title.as_deref().and_then(title).and_then(topic),
        None => None,
    });
    printable(&shown.unwrap_or_else(|| s.name.clone()))
}

/// The agent a session runs: an agent typed into a dino shell counts as that agent.
pub fn agent(s: &SessionInfo) -> &str {
    s.inside.as_ref().map_or(&s.agent_id, |f| &f.agent)
}

/// The model it's on, as short as it reads: what the agent says it switched to, else what it was
/// started with, else its last call's.
pub fn model(s: &SessionInfo) -> Option<String> {
    s.agent_model.as_deref().or(s.controls.model.as_deref()).or(s.last_model.as_deref()).filter(|m| !m.is_empty()).map(short_model)
}

/// Its permission mode in dino's words (`ask`, `edits`, `plan`, `auto`, `bypass`): what the agent
/// says it's in, else what it was started with.
pub fn mode(s: &SessionInfo) -> Option<String> {
    s.agent_mode.as_deref().or(s.controls.mode.as_deref()).filter(|m| !m.is_empty()).map(printable)
}

/// `claude-opus-4-5-20251101` → `opus-4-5`; `anthropic/claude-sonnet-4` → `sonnet-4`.
pub fn short_model(m: &str) -> String {
    let m = m.rsplit('/').next().unwrap_or(m);
    let m = m.strip_prefix("claude-").unwrap_or(m);
    let parts: Vec<&str> = m.split('-').filter(|p| !(p.len() == 8 && p.chars().all(|c| c.is_ascii_digit()))).collect();
    truncate(&printable(&parts.join("-")), 16)
}

/// Where a session runs: its folder (a shell's, where it is now), `host:` before it over SSH.
pub fn folder(s: &SessionInfo) -> String {
    let path = s.shell_cwd.as_deref().unwrap_or(&s.cwd);
    printable(&match &s.host {
        Some(h) => format!("{h}:{path}"),
        None => short_path(path),
    })
}

/// `path` with the home folder as `~`, and macOS's `/private/tmp` as `/tmp`.
pub fn short_path(p: &str) -> String {
    let unprivate = |p: &str| ["/private/tmp", "/private/var", "/private/etc"].iter().find_map(|d| under(p, d).map(|_| p["/private".len()..].to_string())).unwrap_or_else(|| p.to_string());
    if let Ok(home) = std::env::var("HOME")
        && home.len() > 1
    {
        let home = home.trim_end_matches('/');
        for (path, home) in [(p.to_string(), home.to_string()), (unprivate(p), unprivate(home))] {
            if let Some(rest) = under(&path, &home) {
                return format!("~{rest}");
            }
        }
    }
    unprivate(p)
}

/// `~/x` → `$HOME/x`, for a path someone typed.
pub fn expand_home(p: &str) -> String {
    match (p.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(home)) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home.trim_end_matches('/')),
        _ => p.to_string(),
    }
}

fn under<'a>(p: &'a str, dir: &str) -> Option<&'a str> {
    p.strip_prefix(dir).filter(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `950`, `12.4k`, `3.1M`.
pub fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..999_950 => format!("{:.1}k", n as f64 / 1e3),
        999_950..999_950_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

/// How long ago, in one short word: `now`, `42s`, `3m`, `5h`, `2d`.
pub fn ago(secs: u64) -> String {
    match secs {
        0..5 => "now".into(),
        5..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// How long until, in a word or two: `in 5m`, `in 2h 10m`.
pub fn until(secs: u64) -> String {
    match secs {
        0..60 => "in a moment".into(),
        60..3600 => format!("in {}m", secs / 60),
        3600..86_400 => format!("in {}h {}m", secs / 3600, secs % 3600 / 60),
        _ => format!("in {}d {}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// Unix seconds `secs` in local time: (year, month, day, hour, minute, weekday 0 = Sunday).
pub fn local(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r only writes the tm it's given.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    (tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32, tm.tm_wday as u32)
}

/// When, as a clock says it: `16:30` today, `Tue 09:00` another day this week, `Oct 13 09:00`
/// further off.
pub fn clock(secs: u64) -> String {
    let (y, m, d, hh, mm, wd) = local(secs);
    let (ny, nm, nd, ..) = local(now());
    let time = format!("{hh:02}:{mm:02}");
    if (y, m, d) == (ny, nm, nd) {
        time
    } else if secs.abs_diff(now()) < 6 * 86_400 {
        format!("{} {time}", ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][wd as usize % 7])
    } else {
        format!("{} {d} {time}", MONTHS[(m as usize + 11) % 12])
    }
}

pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `2026-09-12` as `Sep 12`.
pub fn day(date: &str) -> String {
    let mut p = date.split('-');
    let (_, m, d) = (p.next(), p.next().and_then(|m| m.parse::<usize>().ok()), p.next().and_then(|d| d.parse::<u32>().ok()));
    match (m, d) {
        (Some(m @ 1..=12), Some(d)) => format!("{} {d}", MONTHS[m - 1]),
        _ => printable(date),
    }
}

/// `3412` as `3,412`.
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `s` cut to `n` characters, with `…` when it was longer.
pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.into()
    } else if n == 0 {
        String::new()
    } else {
        s.chars().take(n - 1).chain(['…']).collect()
    }
}

/// The live rate as the app's usage panel says it: `2.0k out · 16.4k in tok/s`, whichever isn't
/// 0; `2.0k tok/s` from a dinod older than the input rate; nothing while neither moves.
pub fn rate(r: Option<crate::app::Rate>) -> Option<String> {
    let r = r?;
    let Some(input) = r.input else { return (r.out > 0).then(|| format!("{} tok/s", tokens(r.out as u64))) };
    let parts: Vec<String> = [(r.out, "out"), (input, "in")].iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{} {w}", tokens(*n as u64))).collect();
    (!parts.is_empty()).then(|| format!("{} tok/s", parts.join(" · ")))
}

/// `1 needs you`, `2 need you`.
pub fn need_you(n: usize) -> String {
    format!("{n} {}", if n == 1 { "needs you" } else { "need you" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(v: serde_json::Value) -> SessionInfo {
        let mut base = serde_json::json!({
            "id": "1", "name": "claude", "agent_id": "claude", "title": null, "exited": false,
            "output_ms_ago": 60_000, "bells": 0, "requests": 0, "in_flight": 0, "input_tokens": 0,
            "output_tokens": 0, "last_model": null, "tier": null, "activity": null,
        });
        base.as_object_mut().unwrap().extend(v.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn names_are_the_apps() {
        assert_eq!(name(&session(serde_json::json!({"title": "✳ Claude Code"}))), "claude");
        assert_eq!(name(&session(serde_json::json!({"title": "✳ Fix the tab strip"}))), "Fix the tab strip");
        assert_eq!(name(&session(serde_json::json!({"title": "✳ Fix it", "label": "mine"}))), "mine");
        assert_eq!(name(&session(serde_json::json!({"name": "a\u{7}b"}))), "a?b");
        assert_eq!(name(&session(serde_json::json!({"title": "✳ ab\u{1b}[2Jc"}))), "ab?[2Jc");
    }

    #[test]
    fn models_and_numbers_read_short() {
        assert_eq!(short_model("claude-opus-4-5-20251101"), "opus-4-5");
        assert_eq!(short_model("anthropic/claude-sonnet-4"), "sonnet-4");
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_400), "12.4k");
        assert_eq!(tokens(999_999), "1.0M");
        assert_eq!(tokens(3_100_000), "3.1M");
        assert_eq!(ago(3), "now");
        assert_eq!(ago(42), "42s");
        assert_eq!(ago(190), "3m");
        assert_eq!(ago(5 * 3600 + 1), "5h");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(need_you(1), "1 needs you");
        assert_eq!(count(3412), "3,412");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(count(12), "12");
        assert_eq!(day("2026-09-12"), "Sep 12");
        assert_eq!(clock(now()).len(), 5);
        let r = |out, input| rate(Some(crate::app::Rate { out, input }));
        assert_eq!(r(2000, Some(16_400)).as_deref(), Some("2.0k out · 16.4k in tok/s"));
        assert_eq!(r(0, Some(900)).as_deref(), Some("900 in tok/s"));
        assert_eq!(r(1200, None).as_deref(), Some("1.2k tok/s"));
        assert_eq!(r(0, Some(0)), None);
        assert_eq!(rate(None), None);
    }

    #[test]
    fn home_reads_as_tilde() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(short_path(&format!("{home}/priv/x")), "~/priv/x");
        assert_eq!(expand_home("~/priv/x"), format!("{}/priv/x", home.trim_end_matches('/')));
        assert_eq!(short_path("/private/tmp/x"), "/tmp/x");
    }
}
