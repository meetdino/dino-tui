//! Snapshot tests: the list, the accounts screen and the new-session line, drawn with ratatui's
//! TestBackend from stand-in data and compared with `tests/snapshots/<name>.txt`. Run with
//! `UPDATE_SNAPSHOTS=1` to write them again after a change you meant; review the diff.

use dino_core::ipc::{ClaudeAccountsInfo, Response, SessionInfo};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

use crate::app::{App, Event, Tag};
use crate::config::Config;
use crate::ui;

fn session(v: serde_json::Value) -> SessionInfo {
    let mut base = serde_json::json!({
        "id": "1", "name": "claude", "agent_id": "claude", "title": null, "exited": false,
        "output_ms_ago": 180_000, "bells": 0, "requests": 0, "in_flight": 0, "input_tokens": 0,
        "output_tokens": 0, "last_model": null, "tier": null, "activity": "done", "cwd": "/srv/dino-app",
    });
    base.as_object_mut().unwrap().extend(v.as_object().unwrap().clone());
    serde_json::from_value(base).unwrap()
}

fn sessions() -> Vec<SessionInfo> {
    vec![
        session(
            serde_json::json!({"id": "1", "label": "fix the tab strip", "activity": "working", "output_ms_ago": 400, "input_tokens": 11_000, "output_tokens": 1_400, "agent_model": "claude-opus-4-5-20251101", "agent_mode": "bypass"}),
        ),
        session(
            serde_json::json!({"id": "2", "label": "release notes", "cwd": "/wt/dino-app-notes", "controls": {"mode": "plan"}, "input_tokens": 2_100_000, "output_tokens": 48_000, "last_model": "claude-sonnet-4-5"}),
        ),
        session(serde_json::json!({"id": "3", "label": "pricing page", "cwd": "/srv/website", "activity": "needs:Run: rm -rf build?", "output_ms_ago": 52_000})),
        session(serde_json::json!({"id": "4", "label": "landing copy", "cwd": "/srv/website", "pinned": true, "output_ms_ago": 7_200_000})),
        session(serde_json::json!({"id": "5", "name": "shell", "agent_id": "shell", "cwd": "/srv/notes", "activity": null})),
        session(serde_json::json!({"id": "6", "label": "old spike", "cwd": "/srv/website", "exited": true, "exit_code": 0, "output_ms_ago": 90_000_000})),
    ]
}

fn tree() -> Response {
    serde_json::from_value(serde_json::json!({"type": "tree", "repos": [
        {"path": "/srv/dino-app", "name": "dino-app", "worktrees": [{"path": "/wt/dino-app-notes", "branch": "dino/notes"}]},
        {"path": "/srv/website", "name": "website", "worktrees": []},
    ]}))
    .unwrap()
}

fn accounts() -> ClaudeAccountsInfo {
    serde_json::from_value(serde_json::json!({"accounts": [
        {"number": 1, "answering": true, "spent": false, "name": "ada@example.com", "email": "ada@example.com", "plan": "max",
         "windows": [{"name": "5h", "utilization": 0.42, "resets_at": null}, {"name": "7d", "utilization": 0.1, "resets_at": null}]},
        {"number": 2, "answering": false, "spent": true, "name": "work@example.com"},
        {"number": 3, "answering": false, "spent": false},
    ]}))
    .unwrap()
}

fn app() -> App {
    let mut a = App::new(Config::default(), "0.1.11".into());
    let state = Response::State { sessions: sessions(), quotas: vec![], power: None, limits: vec![], leftovers: vec![], claude_accounts: None, version: Some(1) };
    a.on_event(Event::State(Box::new(state)));
    a.on_event(Event::Reply(Tag::Tree, Ok(tree())));
    a.on_event(Event::Reply(Tag::Accounts(String::new()), Ok(Response::ClaudeAccounts { accounts: accounts() })));
    let launchers = serde_json::json!({"type": "launchers", "launchers": [
        {"short": "claude", "agent_id": "claude", "label": "Claude Code", "program": "/bin/claude"},
        {"short": "shell", "agent_id": "shell", "label": "Shell", "program": "/bin/zsh"},
    ]});
    a.on_event(Event::Reply(Tag::Launchers, Ok(serde_json::from_value(launchers).unwrap())));
    a.message = None;
    a.outbox.clear();
    a
}

fn render(app: &mut App, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| ui::draw(f, app)).unwrap();
    t
}

fn text(t: &Terminal<TestBackend>) -> String {
    let buf = t.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..buf.area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            skip = unicode_width::UnicodeWidthStr::width(sym).saturating_sub(1);
            line.push_str(sym);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Compare with the snapshot, or write it.
fn check(name: &str, got: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots").join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap();
    assert!(want == got, "{name} differs from {}:\n--- want\n{want}--- got\n{got}", path.display());
}

#[test]
fn the_list() {
    let mut a = app();
    let t = render(&mut a, 100, 14);
    check("list", &text(&t));
    // The selected row (the first: pinned, in the group with what needs you) is reversed to the edge.
    let buf = t.backend().buffer();
    let row = (0..14).find(|&y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>().contains("landing copy")).unwrap();
    assert!((0..100).all(|x| buf[(x, row)].modifier.contains(Modifier::REVERSED)));
}

#[test]
fn the_list_narrow() {
    let mut a = app();
    check("list-narrow", &text(&render(&mut a, 48, 12)));
}

#[test]
fn the_list_filtered_and_empty() {
    let mut a = app();
    a.filter = "website".into();
    a.rebuild();
    check("list-filtered", &text(&render(&mut a, 100, 8)));
    let mut empty = App::new(Config::default(), "0.1.11".into());
    let state = Response::State { sessions: vec![], quotas: vec![], power: None, limits: vec![], leftovers: vec![], claude_accounts: None, version: Some(1) };
    empty.on_event(Event::State(Box::new(state)));
    check("list-empty", &text(&render(&mut empty, 80, 6)));
}

#[test]
fn the_accounts() {
    let mut a = app();
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('A'), crossterm::event::KeyModifiers::SHIFT));
    a.on_event(Event::Reply(Tag::Accounts(String::new()), Ok(Response::ClaudeAccounts { accounts: accounts() })));
    a.message = None;
    check("accounts", &text(&render(&mut a, 100, 10)));
}

#[test]
fn the_new_session_line() {
    let mut a = app();
    let key = |c: char| crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char(c), crossterm::event::KeyModifiers::NONE);
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('O'), crossterm::event::KeyModifiers::SHIFT));
    for c in "add a dark mode toggle".chars() {
        a.on_key(key(c));
    }
    let t = render(&mut a, 100, 6);
    check("new-session", &text(&t));
    // Enter starts it in a worktree of the selected session's folder, with the prompt.
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE));
    let (_, req) = a.outbox.pop().unwrap();
    let dino_core::ipc::Request::New { launcher, cwd, worktree, prompt, .. } = req else { panic!("{req:?}") };
    assert_eq!((launcher.as_str(), cwd.as_deref(), worktree, prompt.as_deref()), ("claude", Some("/srv/website"), true, Some("add a dark mode toggle")));
}

#[test]
fn the_help() {
    let mut a = app();
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('?'), crossterm::event::KeyModifiers::NONE));
    check("help", &text(&render(&mut a, 90, 26)));
}

// ---- v2: the sidebar, the status line, setup, accounts with switching, usage ----

/// Sessions as the sidebar mockup has them (dino-ops/tui/v2/preview/01-sidebar).
fn v2_app() -> App {
    let mut a = App::new(Config::default(), "0.1.12".into());
    let s = |v: serde_json::Value| session(v);
    let sessions = vec![
        s(
            serde_json::json!({"id": "1", "label": "fix the tab strip", "activity": "working", "output_ms_ago": 120_000, "input_tokens": 40_000, "output_tokens": 1_800, "agent_model": "claude-opus-5-5", "agent_mode": "edits", "cwd": "/srv/dino-app"}),
        ),
        s(serde_json::json!({"id": "2", "label": "pricing page", "activity": "needs:Allow npm run e2e?", "cwd": "/srv/dino-app"})),
        s(serde_json::json!({"id": "3", "label": "release notes", "activity": "done", "cwd": "/srv/dino-app"})),
        s(serde_json::json!({"id": "4", "label": "docs", "activity": null, "cwd": "/srv/dino-app"})),
        s(serde_json::json!({"id": "5", "label": "tabs: compact mode", "activity": "waiting:1 agent", "cwd": "/wt/dino-app-tabs"})),
        s(serde_json::json!({"id": "6", "label": "flaky e2e", "activity": "done", "error": "overloaded", "cwd": "/wt/dino-app-tabs",
            "pr": {"number": 7, "url": "u", "title": "t", "state": "open", "draft": false, "checks": {"passed": 1, "failed": 2, "pending": 0, "failing": ["e2e"]}, "review": null}})),
        s(serde_json::json!({"id": "7", "label": "landing copy", "activity": "working", "cwd": "/srv/website", "tier": "free", "input_tokens": 900, "output_tokens": 200})),
        s(serde_json::json!({"id": "8", "label": "pricing table", "activity": null, "cwd": "/srv/website"})),
        s(serde_json::json!({"id": "9", "name": "shell", "agent_id": "shell", "activity": null, "cwd": "/srv/notes"})),
    ];
    let accounts: Vec<dino_core::ipc::ClaudeAccountInfo> = serde_json::from_value(serde_json::json!([
        {"number": 1, "answering": false, "spent": false, "name": "ada@example.com", "plan": "max"},
        {"number": 2, "answering": true, "spent": false, "name": "work",
         "windows": [{"name": "5h", "utilization": 0.18, "resets_at": null}, {"name": "7d", "utilization": 0.31, "resets_at": null}]},
        {"number": 3, "answering": false, "spent": true, "name": "spare"},
    ]))
    .unwrap();
    let state = Response::State { sessions, quotas: vec![], power: None, limits: vec![], leftovers: vec![], claude_accounts: Some(accounts), version: Some(1) };
    a.on_event(Event::State(Box::new(state)));
    let tree = serde_json::json!({"type": "tree", "repos": [
        {"path": "/srv/dino-app", "name": "dino-app", "worktrees": [{"path": "/srv/dino-app", "branch": "main"}, {"path": "/wt/dino-app-tabs", "branch": "tabs-compact"}]},
        {"path": "/srv/website", "name": "website", "worktrees": [{"path": "/srv/website", "branch": "main"}]},
    ]});
    a.on_event(Event::Reply(Tag::Tree, Ok(serde_json::from_value(tree).unwrap())));
    a.on_event(Event::Rate(Some(crate::app::Rate { out: 1200, input: Some(16_400) })));
    a.message = None;
    a.outbox.clear();
    a.outbox_raw.clear();
    a
}

/// A Claude Code screen mid-turn, as bytes it would write.
fn claude_bytes() -> Vec<u8> {
    let mut b = String::new();
    b.push_str("\r\n > the tab strip overflows past 12 tabs; make tabs shrink instead\r\n\r\n");
    b.push_str(" \x1b[37m●\x1b[0m I'll look at how the strip lays out its tabs.\r\n\r\n");
    b.push_str(" \x1b[32m●\x1b[0m \x1b[1mRead(src/tabs/strip.ts)\x1b[0m\r\n   \x1b[2m⎿  Read 214 lines\x1b[0m\r\n\r\n");
    b.push_str(" \x1b[32m●\x1b[0m \x1b[1mUpdate(src/tabs/strip.ts)\x1b[0m\r\n");
    b.push_str(" \x1b[48;5;52m       84 -    const min = 120;\x1b[0m\r\n");
    b.push_str(" \x1b[48;5;22m       84 +    const min = compact ? 44 : 120;\x1b[0m\r\n\r\n");
    b.push_str(" \x1b[38;5;214m✻ Testing… (14s · ↓ 1.4k tokens · esc to interrupt)\x1b[0m\r\n\r\n");
    b.push_str(" \x1b[90m╭──────────────────────────────────╮\x1b[0m\r\n \x1b[90m│\x1b[0m > ");
    b.into_bytes()
}

fn render_attached(app: &mut App, screen: &crate::screen::Screen, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::emulate::frame(f, app, screen)).unwrap();
    t
}

#[test]
fn the_sidebar_beside_a_session() {
    let mut a = v2_app();
    a.side.attached = Some("1".into());
    a.side.usage_open = true;
    let (cols, rows) = crate::emulate::session_size((140, 36));
    let mut screen = crate::screen::Screen::new(cols, rows);
    screen.feed(&claude_bytes());
    let t = render_attached(&mut a, &screen, 140, 36);
    let got = text(&t);
    check("sidebar", &got);
    // The session's screen starts right of the separator, the cursor where it left it.
    assert!(got.lines().nth(1).unwrap().starts_with(&format!("{:28}│ > the tab strip", " dino-app  /srv/dino-app")), "{got}");
    let buf = t.backend().buffer();
    let row = (0..36).find(|&y| (0..28).map(|x| buf[(x, y)].symbol()).collect::<String>().contains("fix the tab strip")).unwrap();
    assert_eq!(buf[(0, row)].bg, ratatui::style::Color::Indexed(237), "the attached row is selected");
    assert_eq!(buf[(2, row)].fg, ratatui::style::Color::Green);
    let flaky = (0..36).find(|&y| (0..28).map(|x| buf[(x, y)].symbol()).collect::<String>().contains("flaky e2e")).unwrap();
    assert_eq!((buf[(2, flaky)].fg, buf[(25, flaky)].symbol()), (ratatui::style::Color::Red, "✕"), "an error's red dot and failing checks' ✕");
}

#[test]
fn the_sidebar_with_the_keyboard_grouped_by_state() {
    let mut a = v2_app();
    a.config.group_by = "state".into();
    a.side.attached = Some("1".into());
    a.side.focus = true;
    // Down twice from the attached session: the second session in the sidebar's order.
    a.side_key(crate::sidebar::Key::Down);
    a.side_key(crate::sidebar::Key::Down);
    let screen = crate::screen::Screen::new(111, 24);
    let t = render_attached(&mut a, &screen, 140, 24);
    check("sidebar-focus-by-state", &text(&t));
    // Enter on another session switches to it; on the usage line it opens the panel.
    assert!(matches!(a.side_key(crate::sidebar::Key::Enter), crate::sidebar::Act::Switch(_)));
    a.side_key(crate::sidebar::Key::Bottom);
    assert_eq!(a.side_key(crate::sidebar::Key::Enter), crate::sidebar::Act::None);
    assert!(a.side.usage_open);
}

#[test]
fn the_status_line_without_the_sidebar() {
    let mut a = v2_app();
    a.config.status_line = true;
    // A dinod that says the output rate only, as the mockup's.
    a.on_event(Event::Rate(Some(crate::app::Rate { out: 1200, input: None })));
    let bar = crate::status_bar(&a, "1");
    let line = bar.draw(90);
    let plain = String::from_utf8(strip(line.as_bytes())).unwrap();
    // The account's fullest window, as the list's bottom line has it.
    assert!(plain.starts_with(" ● fix the tab strip  ! 1 needs you   1.2k tok/s  work 7d 31%  "), "{plain:?}");
    assert!(plain.ends_with("^\\ d list · ^\\ N next ! "), "{plain:?}");
    assert_eq!(plain.chars().count(), 90);
}

/// Text without its escape sequences.
fn strip(b: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b {
            i += 2;
            while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                i += 1;
            }
            i += 1;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn setup_app() -> App {
    let mut a = v2_app();
    a.setup.headless = true;
    a.setup.linux = true;
    let agents = serde_json::json!({"type": "agent_setup", "agents": [
        {"id": "opencode", "name": "OpenCode", "path": null, "version": null, "signed_in": null, "account": null, "install": "npm i -g opencode-ai", "sign_in": null, "sign_in_hint": null, "homepage": "h"},
        {"id": "codex", "name": "Codex", "path": "/bin/codex", "version": "0.50", "signed_in": false, "account": null, "install": "npm i -g @openai/codex", "sign_in": "codex login", "sign_in_hint": null, "homepage": "h"},
        {"id": "claude", "name": "Claude Code", "path": "/bin/claude", "version": "2.1", "signed_in": true, "account": "ada@example.com", "install": "x", "sign_in": "claude /login", "sign_in_hint": null, "homepage": "h"},
        {"id": "pi", "name": "pi", "path": null, "version": null, "signed_in": null, "account": null, "install": "npm i -g pi", "sign_in": null, "sign_in_hint": null, "homepage": "h"},
    ]});
    a.on_event(Event::Reply(Tag::AgentSetup, Ok(serde_json::from_value(agents).unwrap())));
    let accounts: ClaudeAccountsInfo = serde_json::from_value(serde_json::json!({"accounts": [{"number": 1, "answering": true, "spent": false, "name": "ada@example.com", "plan": "max"}]})).unwrap();
    a.on_event(Event::Reply(Tag::Accounts(String::new()), Ok(Response::ClaudeAccounts { accounts })));
    let providers = serde_json::json!({"type": "providers", "providers": [
        {"id": "ollama", "name": "Ollama", "base": "http://127.0.0.1:11434", "formats": [], "local": true, "connected": true, "key": null, "version": null, "account": null, "error": null, "plan": null},
        {"id": "lmstudio", "name": "LM Studio", "base": "http://127.0.0.1:1234", "formats": [], "local": true, "connected": false, "key": null, "version": null, "account": null, "error": null, "plan": null},
    ]});
    a.on_event(Event::Reply(Tag::Providers, Ok(serde_json::from_value(providers).unwrap())));
    let sync = serde_json::json!({"type": "sync", "status": {"phase": "signed_out", "server": "https://dino.dev", "email": null, "account_url": null, "last_sync": null,
        "pending": 0, "synced": 0, "device_code": null, "device_url": null, "conflict": null, "snapshots": 0, "message": null}});
    a.on_event(Event::Reply(Tag::Sync(String::new()), Ok(serde_json::from_value(sync).unwrap())));
    let launchers = serde_json::json!({"type": "launchers", "launchers": [
        {"short": "claude", "agent_id": "claude", "label": "Claude Code", "program": "/bin/claude"},
        {"short": "codex", "agent_id": "codex", "label": "Codex", "program": "/bin/codex"},
        {"short": "shell", "agent_id": "shell", "label": "Shell", "program": "/bin/zsh"},
    ]});
    a.on_event(Event::Reply(Tag::Launchers, Ok(serde_json::from_value(launchers).unwrap())));
    a.setup.found = Some((2, 14, false));
    a.setup.service = Some(Ok(false));
    a.on_settings(serde_json::json!({"type": "settings", "settings": {"machine": {"onboarded": true}, "policies": {"default_agent": null}}, "locked": []}));
    a.screen = crate::app::Screen::Setup(crate::setup::SetupView { cursor: 1 });
    a.message = None;
    a
}

#[test]
fn the_setup_card() {
    let mut a = setup_app();
    check("setup", &text(&render(&mut a, 110, 30)));
    // The selected row's key: Codex isn't signed in, so `s` (or Enter) runs its sign-in in a shell.
    a.outbox.clear();
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('s'), crossterm::event::KeyModifiers::NONE));
    let (tag, req) = a.outbox.pop().unwrap();
    assert!(matches!(req, dino_core::ipc::Request::AgentAction { ref id, ref action } if id == "codex" && action == "sign_in"), "{req:?}");
    a.on_event(Event::Reply(tag, Ok(Response::Created { id: "42".into() })));
    assert_eq!(a.want, Some(crate::app::Want::Attach("42".into())));
    // Esc on a machine not set up marks it set up.
    a.setup.settings = Some(serde_json::json!({"machine": {"onboarded": false}}));
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Esc, crossterm::event::KeyModifiers::NONE));
    assert!(matches!(a.screen, crate::app::Screen::List));
    assert!(a.jobs.iter().any(|(_, j)| *j == crate::app::Job::Setting(vec!["machine", "onboarded"], serde_json::Value::Bool(true))));
}

#[test]
fn the_first_run_opens_setup() {
    let mut a = App::new(Config::default(), "0.1.12".into());
    a.on_event(Event::Raw(Tag::Settings, Ok(serde_json::json!({"type": "settings", "settings": {"machine": {"onboarded": false}}}))));
    assert!(matches!(a.screen, crate::app::Screen::Setup(_)));
    let mut b = App::new(Config::default(), "0.1.12".into());
    b.on_event(Event::Raw(Tag::Settings, Ok(serde_json::json!({"type": "settings", "settings": {"machine": {"onboarded": true}}}))));
    assert!(matches!(b.screen, crate::app::Screen::List));
}

fn v2_accounts() -> serde_json::Value {
    serde_json::json!({"type": "claude_accounts", "accounts": {"accounts": [
        {"number": 1, "answering": false, "spent": false, "name": "ada@example.com", "plan": "max",
         "windows": [{"name": "5h", "utilization": 0.42, "resets_at": null}, {"name": "7d", "utilization": 0.1, "resets_at": null}]},
        {"number": 2, "answering": true, "spent": false, "chosen": true, "name": "work", "plan": "max",
         "windows": [{"name": "5h", "utilization": 0.18, "resets_at": null}, {"name": "7d", "utilization": 0.31, "resets_at": null}]},
        {"number": 3, "answering": false, "spent": true, "name": "spare", "plan": "pro",
         "windows": [{"name": "5h", "utilization": 1.0, "resets_at": null}, {"name": "7d", "utilization": 0.44, "resets_at": null}]},
        {"number": 4, "answering": false, "spent": false, "name": "side project", "plan": "pro"},
    ]}})
}

#[test]
fn the_accounts_with_switching() {
    let mut a = v2_app();
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('A'), crossterm::event::KeyModifiers::SHIFT));
    a.on_event(Event::Raw(Tag::Accounts(String::new()), Ok(v2_accounts())));
    assert_eq!(a.chosen, [2]);
    // Enter on 3: `use`, and once dinod says yes, what that means, said once.
    let key = |c| crossterm::event::KeyEvent::new(c, crossterm::event::KeyModifiers::NONE);
    a.on_key(key(crossterm::event::KeyCode::Char('j')));
    a.on_key(key(crossterm::event::KeyCode::Char('j')));
    a.outbox_raw.clear();
    a.on_key(key(crossterm::event::KeyCode::Enter));
    let (tag, req) = a.outbox_raw.pop().unwrap();
    assert_eq!(req, serde_json::json!({"type": "claude_accounts", "action": "use", "value": null, "account": 3, "order": []}));
    a.on_event(Event::Raw(tag, Ok(v2_accounts())));
    a.on_key(key(crossterm::event::KeyCode::Char('k')));
    a.message = None;
    check("accounts-v2", &text(&render(&mut a, 110, 26)));
}

#[test]
fn an_older_dinod_cant_switch_and_says_so() {
    let mut a = v2_app();
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('A'), crossterm::event::KeyModifiers::SHIFT));
    a.on_event(Event::Raw(Tag::Accounts(String::new()), Ok(v2_accounts())));
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('j'), crossterm::event::KeyModifiers::NONE));
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('j'), crossterm::event::KeyModifiers::NONE));
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE));
    let (tag, _) = a.outbox_raw.pop().unwrap();
    // dinod 0.1.12's answer to an action it doesn't know.
    a.on_event(Event::Raw(tag, Ok(serde_json::json!({"type": "error", "message": "unknown action use"}))));
    let m = a.message.as_ref().unwrap();
    assert!(!m.error && m.text.contains("can't switch accounts yet"), "{}", m.text);
    let got = text(&render(&mut a, 110, 26));
    assert!(!got.contains("Enter use"), "the key hides:\n{got}");
    a.outbox_raw.clear();
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE));
    assert!(a.outbox_raw.is_empty(), "not asked again");
}

/// A month of made-up use, as dinod's `Stats` reports it.
fn report() -> Box<dino_core::usage::Report> {
    use dino_core::usage::report::{DailyModel, Day, Model, Part, RouteUse, SessionUse, Tokens};
    let mut seed = 7u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as f64 / (1u64 << 31) as f64
    };
    let tok = |t: u64| Tokens { input: t / 100, cache_read: t * 90 / 100, cache_write: t * 5 / 100, output: t * 4 / 100, total: t };
    let mut r = dino_core::usage::Report { range: dino_core::usage::Range::Month, ..Default::default() };
    // 2026-09-12 to 2026-10-11.
    let date = |n: i64| {
        let (mut y, mut m, mut d) = (2026, 9, 12 + n);
        if d > 30 {
            d -= 30;
            m = 10;
        }
        if m > 12 {
            y += 1;
        }
        format!("{y:04}-{m:02}-{d:02}")
    };
    let models = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "gpt-5.5-codex", "glm-5"];
    let split = [0.6, 0.25, 0.09, 0.04, 0.02];
    let mut totals = [0u64; 5];
    for n in 0..30 {
        let weekend = n % 7 == 1 || n % 7 == 2;
        let t = if weekend && rnd() < 0.6 { 0 } else { (rnd() * 2_200_000.0) as u64 + 100_000 };
        r.days.push(Day { date: date(n), tokens: t, requests: t / 4000 });
        for (k, m) in models.iter().enumerate() {
            let part = (t as f64 * split[k]) as u64;
            totals[k] += part;
            if part > 0 {
                r.daily_models.push(DailyModel { date: date(n), model: if k < 5 { m.to_string() } else { "Other".into() }, tokens: part });
            }
        }
    }
    for (k, m) in models.iter().enumerate() {
        r.models.push(Model { model: m.to_string(), tokens: tok(totals[k]), requests: totals[k] / 4000, share: split[k], agents: vec!["claude".into()] });
    }
    let all: u64 = totals.iter().sum();
    r.totals.tokens = tok(all);
    r.totals.requests = all / 4000;
    r.totals.parts.main = Part { tokens: tok(all * 71 / 100), requests: 1 };
    r.totals.parts.subagents = Part { tokens: tok(all * 22 / 100), requests: 1 };
    r.totals.parts.side = Part { tokens: tok(all * 7 / 100), requests: 1 };
    // The heatmap: a year to 2026-10-11 (a Sunday).
    for n in 0..371i64 {
        let day = 20_737 - 370 + n;
        let (y, m, d) = {
            let z = day + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
        };
        let t = if rnd() < 0.3 { 0 } else { (rnd() * 3_000_000.0) as u64 };
        r.heatmap.push(Day { date: format!("{y:04}-{m:02}-{d:02}"), tokens: t, requests: t / 4000 });
    }
    for (label, req, err, lim, cost) in
        [("Claude · ada@example.com", 3412, 12, 2, Some(41.2)), ("Claude · work", 881, 3, 0, Some(10.85)), ("OpenRouter", 120, 0, 0, Some(0.94)), ("Ollama (local)", 64, 0, 0, None)]
    {
        r.routes.push(RouteUse { route: label.to_lowercase(), label: label.into(), kind: "own".into(), requests: req, errors: err, limit_hits: lim, cost, ..Default::default() });
    }
    for (session, cwd, t, subs, sub_t) in [(Some("1"), "/srv/dino-app", 9_800_000u64, 6u64, 2_100_000u64), (None, "/srv/website", 6_100_000, 2, 400_000), (Some("3"), "/srv/dino-app", 3_300_000, 0, 0)]
    {
        let mut s = SessionUse { agent: "claude".into(), session: session.map(String::from), cwd: Some(cwd.into()), tokens: tok(t), subagents: subs, ..Default::default() };
        s.parts.subagents = Part { tokens: tok(sub_t), requests: 1 };
        r.sessions.push(s);
    }
    Box::new(r)
}

#[test]
fn the_usage() {
    let mut a = v2_app();
    a.command("stats 30d");
    a.on_event(Event::Reply(Tag::Stats, Ok(Response::Stats { report: report() })));
    a.message = None;
    check("usage", &text(&render(&mut a, 110, 34)));
    // ◂ picks the last day; its numbers replace the range's beside the chart.
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Left, crossterm::event::KeyModifiers::NONE));
    let got = text(&render(&mut a, 110, 34));
    assert!(got.contains("Oct 11: ") && got.contains("▲"), "{got}");
    // 7 asks for the week.
    a.outbox.clear();
    a.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('7'), crossterm::event::KeyModifiers::NONE));
    assert!(matches!(a.outbox.pop().unwrap().1, dino_core::ipc::Request::Stats { range: dino_core::usage::Range::Week }));
}
