//! Setup: the app's Welcome as a card, on first run (dinod's `settings.machine.onboarded`, the
//! flag the app reads and sets too) and with `:setup` any time. Every row can be left as it is;
//! Esc closes it and marks the machine set up, so neither UI asks again.
//!
//! Each row is what dinod already answers: the agents (`AgentSetup`, and `AgentAction` runs an
//! agent's own install or sign-in in a shell dino-tui attaches to), the Claude accounts, the local
//! model servers (`Providers`), the sync (`Sync`'s device sign-in), the agent `o` starts
//! (`Settings`), what's running outside dino (`Found`), and on Linux whether dinod starts at login
//! (the CLI's `dino service`).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dino_core::ipc::{AgentSetupInfo, Request, SyncStatus};
use dino_core::providers::ProviderInfo;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TLine, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Ask, Job, Line, Screen, Tag};
use crate::format;
use crate::input::Input;
use crate::ui::fit;

/// The agents dino works with best, in the app's order (Welcome.swift's `featured`).
const FEATURED: &[&str] = &["claude", "codex", "copilot", "cursor", "amp", "kimi", "qwen", "pi", "hermes", "codewhale", "opencode"];
const LOOK: Duration = Duration::from_secs(1);

/// What the card shows, as dinod last said; kept while it's closed.
pub struct Setup {
    pub agents: Option<Vec<AgentSetupInfo>>,
    pub providers: Option<Vec<ProviderInfo>>,
    pub sync: Option<dino_core::ipc::SyncStatus>,
    /// dinod's settings document, as plain JSON, and the paths an organization sets.
    pub settings: Option<Value>,
    pub locked: Vec<String>,
    /// Running in other terminals, past conversations, and whether there are more of those.
    pub found: Option<(usize, usize, bool)>,
    /// Linux: dinod runs as a systemd user service; or why that can't be told.
    pub service: Option<Result<bool, String>>,
    /// What `dino service install` said.
    pub service_said: Option<String>,
    /// An agent's install or sign-in under way in a shell (agent, action), and one just back.
    pub away: Option<(String, String)>,
    pub back: Option<(String, String)>,
    /// The agents not installed, listed.
    pub show_others: bool,
    /// The sync's device sign-in is looked at again then.
    pub sync_look: Option<Instant>,
    /// Whether the first-run check was made.
    pub checked: bool,
    /// No browser on this machine: adding an account shows a link and asks for the code.
    pub headless: bool,
    /// The systemd row is Linux's.
    pub linux: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            agents: None,
            providers: None,
            sync: None,
            settings: None,
            locked: vec![],
            found: None,
            service: None,
            service_said: None,
            away: None,
            back: None,
            show_others: false,
            sync_look: None,
            checked: false,
            headless: no_browser(),
            linux: cfg!(target_os = "linux"),
        }
    }
}

/// Linux with neither X nor Wayland: nothing here can open a browser.
pub fn no_browser() -> bool {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    cfg!(target_os = "linux") && !set("DISPLAY") && !set("WAYLAND_DISPLAY")
}

#[derive(Default)]
pub struct SetupView {
    pub cursor: usize,
}

/// A row of the card the cursor can be on.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Agent(String),
    /// The agents not installed, folded into one row until opened.
    Others(usize),
    Accounts,
    Servers,
    Sync,
    Default,
    Found,
    Login,
}

impl Setup {
    /// The agents to show: installed ones in the app's order, then (when opened) the rest.
    fn agent_rows(&self) -> (Vec<&AgentSetupInfo>, Vec<&AgentSetupInfo>) {
        let Some(all) = &self.agents else { return (vec![], vec![]) };
        let rank = |a: &AgentSetupInfo| FEATURED.iter().position(|f| *f == a.id).unwrap_or(FEATURED.len());
        let mut sorted: Vec<&AgentSetupInfo> = all.iter().collect();
        sorted.sort_by_key(|a| rank(a));
        sorted.into_iter().partition(|a| a.path.is_some())
    }

    pub fn items(&self) -> Vec<Item> {
        let (here, missing) = self.agent_rows();
        let mut out: Vec<Item> = here.iter().map(|a| Item::Agent(a.id.clone())).collect();
        if !missing.is_empty() {
            if self.show_others || here.is_empty() {
                out.extend(missing.iter().map(|a| Item::Agent(a.id.clone())));
            } else {
                out.push(Item::Others(missing.len()));
            }
        }
        out.extend([Item::Accounts, Item::Servers, Item::Sync, Item::Default, Item::Found]);
        if self.linux {
            out.push(Item::Login);
        }
        out
    }

    fn agent(&self, id: &str) -> Option<&AgentSetupInfo> {
        self.agents.as_ref()?.iter().find(|a| a.id == id)
    }

    /// New answers from `AgentSetup`: how an install or sign-in the user just came back from went.
    pub fn on_agents(&mut self, agents: Vec<AgentSetupInfo>) -> Option<String> {
        self.agents = Some(agents);
        let (id, action) = self.back.take()?;
        let a = self.agent(&id)?;
        Some(match action.as_str() {
            "install" if a.path.is_some() => format!("{} is installed", a.name),
            "install" => format!("{} isn't installed yet: its shell is still in the list", a.name),
            _ if a.signed_in == Some(true) => format!("signed in to {}", a.name),
            _ => format!("{} isn't signed in yet: its shell is still in the list", a.name),
        })
    }

    pub fn on_sync(&mut self, status: SyncStatus) {
        self.sync_look = (status.phase == "signing_in").then(|| Instant::now() + LOOK);
        self.sync = Some(status);
    }

    fn setting(&self, path: &[&str]) -> Option<&Value> {
        let mut v = self.settings.as_ref()?;
        for p in path {
            v = v.get(p)?;
        }
        Some(v)
    }

    pub fn onboarded(&self) -> Option<bool> {
        self.setting(&["machine", "onboarded"]).and_then(Value::as_bool)
    }
}

impl App {
    /// dinod's settings came back: kept for the card, and on the first look, the card comes up on
    /// a machine that hasn't been set up.
    pub fn on_settings(&mut self, v: Value) {
        if let Some(e) = crate::conn::raw_error(&v) {
            if crate::conn::unknown_request(&e) {
                self.unsupported.insert("show its settings");
            }
            self.setup.checked = true;
            return;
        }
        self.setup.locked = v["locked"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        self.setup.settings = Some(v["settings"].clone());
        if !self.setup.checked {
            self.setup.checked = true;
            if self.setup.onboarded() == Some(false) && matches!(self.screen, Screen::List) {
                self.open_setup();
            }
        }
    }

    pub fn open_setup(&mut self) {
        self.screen = Screen::Setup(SetupView::default());
        self.send(Tag::AgentSetup, Request::AgentSetup);
        self.send(Tag::Accounts(String::new()), Request::ClaudeAccounts { action: "status".into(), value: None, account: None, order: vec![] });
        self.send(Tag::Providers, Request::Providers);
        self.send(Tag::Sync(String::new()), Request::Sync { action: "status".into(), value: None });
        self.send(Tag::Launchers, Request::Launchers);
        self.send(Tag::FoundCount, Request::Found { cloud: false, running_only: false, limit: Some(200), query: None, hidden: false });
        self.send_raw(Tag::Settings, serde_json::json!({"type": "settings"}));
        if self.setup.linux {
            self.job(Tag::Service, Job::Service("status"));
        }
    }

    /// Esc: the card goes, and this machine counts as set up (as the app's Done).
    fn close_setup(&mut self) {
        self.screen = Screen::List;
        if self.setup.onboarded() == Some(false) {
            if let Some(Value::Object(m)) = self.setup.settings.as_mut().and_then(|s| s.get_mut("machine")) {
                m.insert("onboarded".into(), Value::Bool(true));
            }
            self.job(Tag::Setting(String::new()), Job::Setting(vec!["machine", "onboarded"], Value::Bool(true)));
        }
    }

    /// Back from an agent's install or sign-in shell: ask how it went.
    pub fn back_from_setup_shell(&mut self) {
        if let Some(a) = self.setup.away.take() {
            self.setup.back = Some(a);
            self.send(Tag::AgentSetup, Request::AgentSetup);
        }
    }

    /// What `dino service` said: its status, or what installing did (then its status again).
    pub fn on_service(&mut self, r: Result<String, String>) {
        match r {
            Ok(text) if text.lines().any(|l| l.trim_start().starts_with("Running")) => {
                let running = text.lines().any(|l| l.trim_start().starts_with("Running") && l.contains("under systemd"));
                self.setup.service = Some(Ok(running));
            }
            Ok(text) => {
                self.setup.service_said = Some(text.lines().next().unwrap_or("").to_string());
                self.job(Tag::Service, Job::Service("status"));
            }
            Err(e) => self.setup.service = Some(Err(e)),
        }
    }

    pub fn setup_key(&mut self, k: KeyEvent) {
        let items = self.setup.items();
        let Screen::Setup(v) = &mut self.screen else { return };
        v.cursor = v.cursor.min(items.len().saturating_sub(1));
        let item = items.get(v.cursor).cloned();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return self.close_setup(),
            KeyCode::Char('c') if ctrl => return self.close_setup(),
            KeyCode::Char('j') | KeyCode::Down => v.cursor = (v.cursor + 1).min(items.len().saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => v.cursor = v.cursor.saturating_sub(1),
            KeyCode::Char(':') => self.line = Line::Ask(Ask::Command, Input::default()),
            _ => {}
        }
        let Some(item) = item else { return };
        // Enter does what the row's key does.
        let key = match k.code {
            KeyCode::Enter => '\n',
            KeyCode::Char(c) => c,
            _ => '\0',
        };
        match (item, k.code) {
            (Item::Agent(id), KeyCode::Char('i' | 's') | KeyCode::Enter) => {
                let Some(a) = self.setup.agent(&id) else { return };
                let action = match (key, a.path.is_some(), a.signed_in, a.sign_in.is_some()) {
                    ('i' | '\n', false, _, _) => "install",
                    ('s' | '\n', true, Some(false), true) => "sign_in",
                    _ => return,
                };
                let what = format!("{} {}", if action == "install" { "installing" } else { "signing in to" }, a.name);
                let req = Request::AgentAction { id: a.id.clone(), action: action.into() };
                self.flash(what);
                self.send(Tag::AgentAction { agent: id, action: action.into() }, req);
            }
            (Item::Others(_), KeyCode::Enter) => self.setup.show_others = true,
            (Item::Accounts, KeyCode::Char('a') | KeyCode::Enter) if !self.login_under_way() => {
                let say = if self.setup.headless { "signing in: open the link below somewhere with a browser" } else { "signing in: a browser opens where dinod runs" };
                self.send(Tag::Accounts(say.into()), Request::ClaudeAccounts { action: "login".into(), value: None, account: None, order: vec![] });
            }
            (Item::Accounts, KeyCode::Char('c')) if self.login_under_way() => self.line = Line::Ask(Ask::LoginCode, Input::default()),
            (Item::Accounts, KeyCode::Char('x')) if self.login_under_way() => {
                self.send(Tag::Accounts("sign-in cancelled".into()), Request::ClaudeAccounts { action: "login_cancel".into(), value: None, account: None, order: vec![] })
            }
            (Item::Sync, KeyCode::Char('g') | KeyCode::Enter) if self.setup.sync.as_ref().is_none_or(|s| s.phase == "signed_out") => {
                self.send(Tag::Sync(String::new()), Request::Sync { action: "login_device".into(), value: None })
            }
            (Item::Sync, KeyCode::Char('x')) if self.setup.sync.as_ref().is_some_and(|s| s.phase == "signing_in") => {
                self.send(Tag::Sync("sign-in cancelled".into()), Request::Sync { action: "cancel_login".into(), value: None })
            }
            (Item::Default, KeyCode::Char('h') | KeyCode::Left) => self.step_default(-1),
            (Item::Default, KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ')) => self.step_default(1),
            (Item::Found, KeyCode::Enter) => self.command("found"),
            (Item::Login, KeyCode::Enter) if self.setup.service == Some(Ok(false)) => {
                self.flash("running `dino service install`…");
                self.job(Tag::Service, Job::Service("install"));
            }
            _ => {}
        }
    }

    /// The agents `o` can start (no plain shell), and the one it starts now.
    fn defaults(&self) -> (Vec<(String, String)>, usize) {
        let list: Vec<(String, String)> = self.launchers.iter().filter(|l| l.agent_id != "shell").map(|l| (l.short.clone(), format::printable(&l.label))).collect();
        let want = self.setup.setting(&["policies", "default_agent"]).and_then(Value::as_str).unwrap_or("claude");
        let at = list.iter().position(|(s, _)| s == want).unwrap_or(0);
        (list, at)
    }

    fn step_default(&mut self, by: isize) {
        if self.setup.locked.iter().any(|l| l == "policies.default_agent") {
            return self.flash("your organization sets the default agent");
        }
        let (list, at) = self.defaults();
        if list.len() < 2 {
            return;
        }
        let to = (at as isize + by).rem_euclid(list.len() as isize) as usize;
        let (short, label) = list[to].clone();
        // Claude Code is the default when none is set, as the app keeps it.
        let value = if short == "claude" { Value::Null } else { Value::String(short.clone()) };
        if let Some(Value::Object(p)) = self.setup.settings.as_mut().and_then(|s| s.get_mut("policies")) {
            p.insert("default_agent".into(), value.clone());
        }
        self.job(Tag::Setting(format!("o and :new start {label}")), Job::Setting(vec!["policies", "default_agent"], value));
    }

    /// Something on the card changes by itself: a sign-in under way, looked at again.
    pub fn setup_deadline(&self) -> Option<Instant> {
        matches!(self.screen, Screen::Setup(_)).then_some(self.setup.sync_look).flatten()
    }

    pub fn setup_tick(&mut self, now: Instant) {
        if matches!(self.screen, Screen::Setup(_)) && self.setup.sync_look.is_some_and(|t| t <= now) {
            self.setup.sync_look = None;
            self.send(Tag::Sync(String::new()), Request::Sync { action: "status".into(), value: None });
        }
    }
}

// ---- Drawing ----

const SEL: Color = Color::Indexed(237);

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn grey() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A row: what it says (spans from column `x`), the key it takes, and lines under it.
struct Row {
    item: Option<Item>,
    label: Option<String>,
    spans: Vec<Span<'static>>,
    key: Option<String>,
    under: Vec<Span<'static>>,
    gap: bool,
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let Screen::Setup(v) = &app.screen else { return };
    let s = &app.setup;
    let w = area.width as usize;
    let buf = f.buffer_mut();
    let put = |buf: &mut ratatui::buffer::Buffer, x: usize, y: u16, spans: Vec<Span<'static>>| {
        if x < w && y < area.y + area.height {
            buf.set_line(area.x + x as u16, y, &TLine::from(spans), (w - x) as u16);
        }
    };
    let right = w.saturating_sub(4);
    put(buf, 2, area.y + 1, vec![Span::styled("dino", bold().fg(Color::Green)), Span::styled(" setup", bold())]);
    let note = "every row can be left as it is";
    put(buf, right.saturating_sub(note.width()), area.y + 1, vec![Span::styled(note, dim())]);
    put(buf, 2, area.y + 2, vec![Span::styled("What dino found on this machine. Esc when you're done; :setup brings this back.", dim())]);
    put(buf, 2, area.y + 4, vec![Span::styled("Agents", bold())]);

    let items = s.items();
    let cursor = v.cursor.min(items.len().saturating_sub(1));
    let sel = items.get(cursor);
    let key_x = w.saturating_sub(26).max(60);
    let mut y = area.y + 5;
    if s.agents.is_none() {
        let text = if app.unsupported.contains("set up agents") { "this dinod can't say: update dino" } else { "looking on this machine…" };
        put(buf, 4, y, vec![Span::styled(text, dim())]);
        y += 1;
    }
    // The agents: one line each.
    for item in &items {
        let (mark, name, state, key) = match item {
            Item::Agent(id) => {
                let Some(a) = s.agent(id) else { continue };
                let away = s.away.as_ref().is_some_and(|(x, _)| x == id);
                let (mark, ms) = match (a.path.is_some(), a.signed_in) {
                    (true, Some(false)) => ("✓", Style::new().fg(Color::Green)),
                    (true, _) => ("✓", Style::new().fg(Color::Green)),
                    (false, _) => ("○", grey()),
                };
                let state = match (a.path.is_some(), a.signed_in) {
                    _ if away => "in its shell…".to_string(),
                    (false, _) => "not installed".into(),
                    (true, Some(true)) => a.account.as_deref().map_or("signed in".to_string(), |acc| format!("signed in · {}", format::printable(acc))),
                    (true, Some(false)) => "installed · not signed in".into(),
                    (true, None) => match &a.sign_in_hint {
                        Some(h) => format!("installed · signs in with {} inside it", format::printable(h)),
                        None => "installed".into(),
                    },
                };
                let key = match (a.path.is_some(), a.signed_in, a.sign_in.is_some()) {
                    (false, _, _) => Some("i  install"),
                    (true, Some(false), true) => Some("s  sign in"),
                    _ => None,
                };
                (Span::styled(mark, ms), format::printable(&a.name), state, key)
            }
            Item::Others(n) => (Span::styled("○", grey()), format!("{n} more"), "not installed".into(), Some("Enter  show")),
            _ => continue,
        };
        let on = Some(item) == sel;
        if on {
            buf.set_style(Rect { x: area.x + 2, y, width: (w.saturating_sub(4)) as u16, height: 1 }, Style::new().bg(SEL));
            put(buf, 2, y, vec![Span::styled("▸", Style::new().fg(Color::Cyan))]);
        }
        put(buf, 4, y, vec![mark]);
        put(buf, 6, y, vec![Span::raw(fit(&name, 15))]);
        put(buf, 22, y, vec![Span::styled(fit(&state, key_x.saturating_sub(24)), if on { Style::new() } else { dim() })]);
        if let Some(k) = key {
            put(buf, key_x, y, vec![Span::styled(k.to_string(), if on { Style::new().fg(Color::Cyan) } else { grey() })]);
        }
        y += 1;
    }
    y += 1;
    // The rest: a label, what it is, the key, and a line under it.
    for row in rows(app) {
        if y >= area.y + area.height {
            break;
        }
        let on = row.item.is_some() && row.item.as_ref() == sel;
        if on {
            buf.set_style(Rect { x: area.x + 2, y, width: (w.saturating_sub(4)) as u16, height: 1 }, Style::new().bg(SEL));
            put(buf, 0, y, vec![Span::styled("▸", Style::new().fg(Color::Cyan))]);
        }
        if let Some(l) = &row.label {
            put(buf, 2, y, vec![Span::styled(l.clone(), bold())]);
        }
        let room = key_x.saturating_sub(28);
        let spans = fit_spans(row.spans, room);
        put(buf, 26, y, spans);
        if let Some(k) = &row.key {
            put(buf, key_x, y, vec![Span::styled(k.clone(), if on { Style::new().fg(Color::Cyan) } else { grey() })]);
        }
        y += 1;
        if !row.under.is_empty() {
            let text: String = row.under.iter().map(|s| s.content.as_ref()).collect();
            // A link goes whole, from the card's edge, so it can be copied from the terminal.
            if text.starts_with("http") {
                for chunk in chunks(&text, w.saturating_sub(4)) {
                    put(buf, 2, y, vec![Span::raw(chunk)]);
                    y += 1;
                }
            } else {
                put(buf, 26, y, fit_spans(row.under, w.saturating_sub(28)));
                y += 1;
            }
        }
        if row.gap {
            y += 1;
        }
    }
}

/// `text` in pieces `w` columns wide.
fn chunks(text: &str, w: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(w.max(1)).map(|c| c.iter().collect()).collect()
}

fn fit_spans(spans: Vec<Span<'static>>, w: usize) -> Vec<Span<'static>> {
    let mut out = vec![];
    let mut used = 0;
    for s in spans {
        let sw = s.content.width();
        if used + sw <= w {
            used += sw;
            out.push(s);
        } else {
            out.push(Span::styled(fit(&s.content, w - used), s.style));
            break;
        }
    }
    out
}

/// The card's rows after the agents.
fn rows(app: &App) -> Vec<Row> {
    let s = &app.setup;
    let mut out = vec![];
    // Claude accounts.
    let (value, under): (Vec<Span>, Vec<Span>) = match &app.accounts {
        None if app.unsupported.contains("manage Claude accounts") => (vec![Span::styled("this dinod can't say: update dino", dim())], vec![]),
        None => (vec![Span::styled("…", dim())], vec![]),
        Some(info) => {
            let n = info.accounts.len();
            let names: Vec<String> = info
                .accounts
                .iter()
                .map(|a| {
                    let label = format::printable(&a.label());
                    match &a.plan {
                        Some(p) => format!("{label} ({})", format::printable(p)),
                        None => label,
                    }
                })
                .collect();
            let head = match n {
                0 => "none yet".to_string(),
                1 => format!("1 account: {}", names[0]),
                _ => format!("{n} accounts: {}", names.join(", ")),
            };
            let under = match info.login.as_ref() {
                Some(l) if matches!(l.stage.as_str(), "starting" | "checking") => {
                    vec![Span::styled(if l.stage == "starting" { "Signing in: starting Claude Code's sign-in…" } else { "Signing in: checking the account with Anthropic…" }, dim())]
                }
                Some(l) if l.stage == "browser" => match &l.url {
                    Some(u) => vec![Span::raw(format::printable(u))],
                    None => vec![Span::styled("Signing in: waiting for claude.ai…", dim())],
                },
                _ if s.headless => vec![Span::styled("No browser here: adding shows a link and asks for the code.", dim())],
                _ => vec![Span::styled("Adding opens a browser where dinod runs; c pastes a code from claude.ai instead.", dim())],
            };
            (vec![Span::raw(head)], under)
        }
    };
    let login = app.login_under_way();
    out.push(Row {
        item: Some(Item::Accounts),
        label: Some("Claude accounts".into()),
        spans: value,
        key: Some(if login { "c  paste the code".into() } else { "a  add another".into() }),
        under,
        gap: true,
    });
    if login && let Some(l) = app.accounts.as_ref().and_then(|a| a.login.as_ref()).filter(|l| l.stage == "browser" && l.url.is_some()) {
        let _ = l;
        out.push(Row { item: None, label: None, spans: vec![Span::styled("Open the link above, sign in, then c pastes the code it shows; x cancels.", dim())], key: None, under: vec![], gap: true });
    }
    // Local model servers.
    let servers = match &s.providers {
        None if app.unsupported.contains("list model servers") => vec![Span::styled("this dinod can't say: update dino", dim())],
        None => vec![Span::styled("…", dim())],
        Some(list) => {
            let here: Vec<String> = list
                .iter()
                .filter(|p| p.local && p.connected)
                .map(|p| {
                    let port = p.base.rsplit(':').next().and_then(|t| t.split('/').next()).filter(|t| t.chars().all(|c| c.is_ascii_digit()) && !t.is_empty());
                    let name = format::printable(&p.name);
                    let version = p.version.as_deref().map(|v| format!(" {}", format::printable(v))).unwrap_or_default();
                    match port {
                        Some(port) => format!("{name}{version} on :{port}"),
                        None => format!("{name}{version}"),
                    }
                })
                .collect();
            if here.is_empty() { vec![Span::styled("none running", dim())] } else { vec![Span::raw(here.join(" · "))] }
        }
    };
    out.push(Row { item: Some(Item::Servers), label: Some("Local model servers".into()), spans: servers, key: None, under: vec![], gap: true });
    // Sync.
    let (spans, key, under) = match &s.sync {
        None if app.unsupported.contains("sync settings") => (vec![Span::styled("this dinod can't say: update dino", dim())], None, vec![]),
        None => (vec![Span::styled("…", dim())], None, vec![]),
        Some(st) => match st.phase.as_str() {
            "signed_out" => (vec![Span::raw("off")], Some("g  sign in with GitHub".to_string()), vec![Span::styled("Shows a code to enter at github.com/login/device.", dim())]),
            "signing_in" => match (&st.device_code, &st.device_url) {
                (Some(code), Some(url)) => (
                    vec![Span::raw("enter "), Span::styled(format::printable(code), bold()), Span::raw(format!(" at {}", format::printable(url)))],
                    Some("x  cancel".to_string()),
                    vec![Span::styled("Waiting for GitHub…", dim())],
                ),
                _ => (vec![Span::raw("signing in…")], Some("x  cancel".to_string()), vec![]),
            },
            phase => {
                let who = st.email.as_deref().map(|e| format!(" · {}", format::printable(e))).unwrap_or_default();
                let what = if phase == "conflict" {
                    " · this machine and the account differ: choose in dino's settings".to_string()
                } else {
                    format!(" · {} {} synced", st.synced, if st.synced == 1 { "setting" } else { "settings" })
                };
                (vec![Span::raw(format!("on{who}{what}"))], None, vec![])
            }
        },
    };
    let under = match s.sync.as_ref().and_then(|st| st.message.as_deref()) {
        Some(m) if under.is_empty() => vec![Span::styled(format::printable(m), dim())],
        _ => under,
    };
    out.push(Row { item: Some(Item::Sync), label: Some("Sync".into()), spans, key, under, gap: true });
    // The default agent.
    let (list, at) = app.defaults();
    let spans = match list.get(at) {
        None => vec![Span::styled("…", dim())],
        Some((_, label)) if s.locked.iter().any(|l| l == "policies.default_agent") => vec![Span::raw(label.clone()), Span::styled("  set by your organization", dim())],
        Some((_, label)) => vec![Span::raw(label.clone()), Span::styled("  ◂ ▸", grey())],
    };
    out.push(Row { item: Some(Item::Default), label: Some("Default agent".into()), spans, key: None, under: vec![], gap: true });
    // Found.
    let spans = match s.found {
        None if app.unsupported.contains("find sessions outside dino") => vec![Span::styled("this dinod can't say: update dino", dim())],
        None => vec![Span::styled("…", dim())],
        Some((running, past, more)) => vec![Span::raw(format!("{running} running in other terminals · {past}{} past ones", if more { "+" } else { "" }))],
    };
    out.push(Row { item: Some(Item::Found), label: Some("Found on this machine".into()), spans, key: Some("Enter  look".into()), under: vec![], gap: true });
    // Start at login (Linux).
    if s.linux {
        let (spans, key, under) = match &s.service {
            None => (vec![Span::styled("…", dim())], None, vec![]),
            Some(Ok(true)) => (vec![Span::raw("on: dinod runs as a systemd user service")], None, vec![]),
            Some(Ok(false)) => (
                vec![Span::raw("off: the first `dino` you run starts dinod")],
                Some("Enter  turn on".to_string()),
                vec![Span::styled("Runs `dino service install`: a systemd user service, up at boot.", dim())],
            ),
            Some(Err(e)) => (vec![Span::styled(format!("can't tell: {}", format::printable(e)), dim())], None, vec![]),
        };
        let under = match &s.service_said {
            Some(said) => vec![Span::styled(format::printable(said), dim())],
            None => under,
        };
        out.push(Row { item: Some(Item::Login), label: Some("Start at login".into()), spans, key, under, gap: true });
    }
    out
}

/// The bottom line's right side on the card: the default agent and whether it's signed in, the
/// accounts, the sync.
pub fn summary(app: &App) -> String {
    let s = &app.setup;
    let mut parts = vec![];
    let (list, at) = app.defaults();
    if let Some((short, label)) = list.get(at) {
        let launcher = app.launchers.iter().find(|l| &l.short == short);
        let signed = launcher.and_then(|l| s.agent(&l.agent_id)).and_then(|a| a.signed_in);
        parts.push(format!("{label}{}", if signed == Some(true) { " ✓" } else { "" }));
    }
    if let Some(a) = &app.accounts {
        let n = a.accounts.len();
        parts.push(format!("{n} {}", if n == 1 { "account" } else { "accounts" }));
    }
    match s.sync.as_ref().map(|s| s.phase.as_str()) {
        Some("signed_out") => parts.push("no sync".into()),
        Some("signing_in") => parts.push("signing in to sync".into()),
        Some(_) => parts.push("sync on".into()),
        None => {}
    }
    parts.join(" · ")
}

/// The line under the card.
pub const HINT: &str = "j/k move · the key shown on a row · Esc done";
