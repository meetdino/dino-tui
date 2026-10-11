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
