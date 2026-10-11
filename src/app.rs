//! dino-tui's state and what keys do. A state machine fed dinod's answers and the terminal's keys;
//! the requests it makes wait in `outbox` for the loop to send, and what needs the terminal itself
//! (attaching, the settings editor) waits in `want`. Nothing here touches the terminal or a socket,
//! so the tests drive it as it is.

use std::collections::HashSet;
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use dino_core::controls::{Controls, MODES};
use dino_core::found::FoundSession;
use dino_core::ipc::{ArchivedInfo, ClaudeAccountInfo, ClaudeAccountsInfo, Deletion, LauncherInfo, QuotaInfo, RepoInfo, Request, Response, SessionInfo};
use dino_core::status::Status;
use dino_core::usage::{Range, Report};
use serde_json::Value;

use crate::config::Config;
use crate::conn;
use crate::format;
use crate::input::Input;
use crate::order::{self, Group};
use crate::setup::{Setup, SetupView};
use crate::sidebar::Side;

/// What wakes the loop besides a key. One at a time through a channel, so the size of the big
/// variant costs nothing worth boxing it for.
#[allow(clippy::large_enum_variant, reason = "an event lives for one call")]
pub enum Event {
    /// A `State` (from `StateChange`).
    State(Box<Response>),
    /// dinod went away; the long-polls wait for it to come back.
    Lost,
    /// Tokens per second across sessions; `None`: this dinod doesn't say.
    Rate(Option<Rate>),
    Reply(Tag, io::Result<Response>),
    /// A request sent as plain JSON, and dinod's answer as it came: for what dino-core's types
    /// would lose (a newer dinod's fields, as `chosen` on an account).
    Raw(Tag, io::Result<Value>),
    /// A job done (a setting changed, `dino service` run): what to say, or why it didn't.
    Job(Tag, Result<String, String>),
}

/// Tokens per second across every session, over dinod's last 10 seconds: what agents write, and
/// what they send that the prompt cache didn't hold (`None` from a dinod older than that).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub out: u32,
    pub input: Option<u32>,
}

/// What the loop runs on a thread of its own, beyond one request.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    /// One setting changed in dinod's settings document, read and written back as plain JSON, so
    /// nothing else in it changes.
    Setting(Vec<&'static str>, Value),
    /// `dino service <what>` (Linux): whether dinod runs as a systemd user service.
    Service(&'static str),
}

/// What an answer is for.
#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Launchers,
    Tree,
    /// The accounts, and what to say once they're back (empty: nothing).
    Accounts(String),
    /// A session was made: say `what`, and attach to it.
    Created {
        attach: bool,
        what: String,
    },
    Forked(String),
    /// `Ok`: say this.
    Done(String),
    Closed {
        id: String,
        name: String,
    },
    DeleteDry {
        id: String,
        name: String,
    },
    Deleted(String),
    Stats,
    Found,
    Archived,
    Unarchived(String),
    /// dinod's settings, as plain JSON: whether this machine is set up (first run, `:setup`).
    Settings,
    AgentSetup,
    /// An agent's install or sign-in, in a shell to attach to.
    AgentAction {
        agent: String,
        action: String,
    },
    Providers,
    /// The sync's state, and what to say once it's back.
    Sync(String),
    /// How many sessions `Found` has, for the setup card.
    FoundCount,
    /// `ClaudeAccounts { use }`: Claude Code's calls go to this account.
    Use(u32, String),
    /// A setting changed (`Job::Setting`): what to say.
    Setting(String),
    Service,
}

/// What only the loop can do.
#[derive(Debug, Clone, PartialEq)]
pub enum Want {
    Attach(String),
    Settings,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Row {
    Group(usize),
    Session(usize),
}

pub enum Screen {
    List,
    Setup(SetupView),
    Accounts(AccountsView),
    Stats(StatsView),
    Found(PickList<FoundSession>),
    Archived(PickList<ArchivedInfo>),
}

#[derive(Default)]
pub struct AccountsView {
    pub cursor: usize,
}

pub struct StatsView {
    pub range: Range,
    pub report: Option<Box<Report>>,
    pub scroll: usize,
    /// The day picked in the tokens-per-day chart (◂ ▸), counted from the chart's last.
    pub day: Option<usize>,
}

pub struct PickList<T> {
    pub items: Vec<T>,
    pub cursor: usize,
    pub loading: bool,
}

impl<T> PickList<T> {
    fn loading() -> Self {
        PickList { items: vec![], cursor: 0, loading: true }
    }

    fn step(&mut self, by: isize) {
        if !self.items.is_empty() {
            self.cursor = (self.cursor as isize + by).clamp(0, self.items.len() as isize - 1) as usize;
        }
    }
}

/// The bottom line, when it's taking input.
pub enum Line {
    Normal,
    Filter(Input),
    Ask(Ask, Input),
    Confirm(Confirm, String),
    New(NewForm),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    Send(String),
    Rename(String),
    Command,
    AccountName(u32),
    LoginCode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Confirm {
    Delete(String, String),
    RemoveAccount(u32),
}

/// `new ▸ agent: claude   folder: ~/x   worktree: no   prompt: _`
#[derive(Debug, Clone, Default)]
pub struct NewForm {
    /// 0 agent, 1 folder, 2 worktree, 3 prompt.
    pub field: usize,
    pub agent: usize,
    pub folder: Input,
    pub worktree: bool,
    pub prompt: Input,
    /// Set with `m`; what's left open comes from Settings → Agents.
    pub controls: Controls,
}

pub const NEW_FIELDS: usize = 4;

pub enum Overlay {
    Help,
    Picker(Picker),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickFor {
    Session(String),
    New,
}

/// The mode / model / effort picker: a column each, `default` first in each.
#[derive(Debug, Clone)]
pub struct Picker {
    pub target: PickFor,
    pub title: String,
    pub cols: Vec<PickCol>,
    pub col: usize,
    /// Typing a model by hand.
    pub typing: Option<Input>,
}

#[derive(Debug, Clone)]
pub struct PickCol {
    pub name: &'static str,
    /// The value (`None`: the default) and how it reads.
    pub options: Vec<(Option<String>, String)>,
    pub cursor: usize,
}

/// A model typed by hand: the last option of the model column.
const OTHER: &str = "\u{0}other";

pub struct Message {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

pub struct Undo {
    pub id: String,
    pub name: String,
    pub until: Instant,
}

const UNDO: Duration = Duration::from_secs(6);
const SPIN: Duration = Duration::from_millis(120);
const LOGIN_LOOK: Duration = Duration::from_secs(1);

pub struct App {
    pub config: Config,
    /// dinod's version, as it said.
    pub dinod: String,
    pub sessions: Vec<SessionInfo>,
    state_version: Option<u64>,
    pub quotas: Vec<QuotaInfo>,
    /// From the state: every Claude account, once there's more than one.
    pub state_accounts: Option<Vec<ClaudeAccountInfo>>,
    /// From `ClaudeAccounts { status }`.
    pub accounts: Option<ClaudeAccountsInfo>,
    pub launchers: Vec<LauncherInfo>,
    pub tree: Vec<RepoInfo>,
    tree_known: Option<String>,
    tree_cwds: Vec<String>,
    tree_asked: bool,
    /// Tokens per second; `None` until dinod says (or when it can't).
    pub rate: Option<Rate>,
    pub connected: bool,
    pub got_state: bool,
    pub groups: Vec<Group>,
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub offset: usize,
    /// The selected session, kept across updates.
    pub selected: Option<String>,
    /// A session to select once it shows up (one just made).
    select_next: Option<String>,
    pub filter: String,
    pub screen: Screen,
    pub line: Line,
    pub overlay: Option<Overlay>,
    pub message: Option<Message>,
    pub undo: Option<Undo>,
    pending: Option<char>,
    pub spinner: usize,
    last_spin: Instant,
    next_login_look: Option<Instant>,
    needs: HashSet<String>,
    pub outbox: Vec<(Tag, Request)>,
    /// Requests sent as plain JSON, and jobs; the loop sends and runs them.
    pub outbox_raw: Vec<(Tag, Value)>,
    pub jobs: Vec<(Tag, Job)>,
    pub want: Option<Want>,
    pub dirty: bool,
    /// Ring the terminal's bell: a session started needing you.
    pub bell: bool,
    /// Rows the list has on screen, and the terminal's size.
    pub height: usize,
    pub size: (u16, u16),
    /// What this dinod refused as a request it doesn't know.
    pub unsupported: HashSet<&'static str>,
    /// The sidebar beside an attached session.
    pub side: Side,
    /// What the setup card shows, as dinod last said.
    pub setup: Setup,
    /// The accounts the user chose (`ClaudeAccounts { use }`), when dinod says (`chosen`).
    pub chosen: Vec<u32>,
    /// Said once after switching accounts, while the accounts screen stays open.
    pub switched: Option<String>,
}

impl App {
    pub fn new(config: Config, dinod: String) -> App {
        App {
            config,
            dinod,
            sessions: vec![],
            state_version: None,
            quotas: vec![],
            state_accounts: None,
            accounts: None,
            launchers: vec![],
            tree: vec![],
            tree_known: None,
            tree_cwds: vec![],
            tree_asked: false,
            rate: None,
            connected: true,
            got_state: false,
            groups: vec![],
            rows: vec![],
            cursor: 0,
            offset: 0,
            selected: None,
            select_next: None,
            filter: String::new(),
            screen: Screen::List,
            line: Line::Normal,
            overlay: None,
            message: None,
            undo: None,
            pending: None,
            spinner: 0,
            last_spin: Instant::now(),
            next_login_look: None,
            needs: HashSet::new(),
            outbox: vec![],
            outbox_raw: vec![],
            jobs: vec![],
            want: None,
            dirty: true,
            bell: false,
            height: 20,
            size: (80, 24),
            unsupported: HashSet::new(),
            side: Side::default(),
            setup: Setup::default(),
            chosen: vec![],
            switched: None,
        }
    }

    /// What to ask dinod for once at the start.
    pub fn start(&mut self) {
        self.send(Tag::Launchers, Request::Launchers);
        self.send(Tag::Accounts(String::new()), accounts("status"));
        // Whether this machine was set up: the setup card comes up if it wasn't.
        self.send_raw(Tag::Settings, serde_json::json!({"type": "settings"}));
    }

    pub fn send(&mut self, tag: Tag, req: Request) {
        // The accounts as plain JSON: `chosen` is newer than the pinned dino-core.
        if matches!(tag, Tag::Accounts(_) | Tag::Use(..))
            && let Ok(v) = serde_json::to_value(&req)
        {
            return self.send_raw(tag, v);
        }
        self.outbox.push((tag, req));
    }

    pub fn send_raw(&mut self, tag: Tag, req: Value) {
        self.outbox_raw.push((tag, req));
    }

    pub fn job(&mut self, tag: Tag, job: Job) {
        self.jobs.push((tag, job));
    }

    pub fn flash(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), error: false, until: Instant::now() + Duration::from_secs(4) });
        self.dirty = true;
    }

    pub fn flash_err(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), error: true, until: Instant::now() + Duration::from_secs(8) });
        self.dirty = true;
    }

    // ---- What dinod says ----

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::State(resp) => self.on_state(*resp),
            Event::Lost => {
                self.connected = false;
                self.dirty = true;
            }
            Event::Rate(r) => {
                if r != self.rate {
                    self.rate = r;
                    self.dirty = true;
                }
            }
            Event::Reply(tag, r) => self.on_reply(tag, r),
            Event::Raw(tag, r) => self.on_raw(tag, r),
            Event::Job(tag, r) => self.on_job(tag, r),
        }
    }

    fn on_raw(&mut self, tag: Tag, r: io::Result<Value>) {
        self.dirty = true;
        let v = match r {
            Ok(v) => v,
            Err(e) => return self.flash_err(format!("dinod: {e}")),
        };
        match tag {
            Tag::Settings => self.on_settings(v),
            Tag::Use(n, name) => match conn::raw_error(&v) {
                // A dinod from before `use`: it says so, never an error.
                Some(m) if m.starts_with("unknown action") || conn::unknown_request(&m) => {
                    self.unsupported.insert("use");
                    self.flash(format!("this dinod ({}) can't switch accounts yet: update dino", self.dinod));
                }
                _ => {
                    // Expected, not verified: a switch moves the conversation to another
                    // organization's prompt cache. Said once.
                    self.switched = Some(if n == 1 {
                        "Back to Claude Code's own sign-in. Sessions move at their next turn; that first answer has no prompt cache, so it's slower.".to_string()
                    } else {
                        format!("Switched to {name}. Sessions move at their next turn; that first answer has no prompt cache, so it's slower and uses more of {name}'s limit.")
                    });
                    self.on_accounts_raw(v, String::new());
                }
            },
            Tag::Accounts(say) => self.on_accounts_raw(v, say),
            other => match serde_json::from_value::<Response>(v) {
                Ok(resp) => self.on_reply(other, Ok(resp)),
                Err(e) => self.flash_err(format!("dinod answered in a way dino-tui can't read: {e}")),
            },
        }
    }

    /// The accounts as dinod sent them: the typed list, and `chosen` where a newer dinod says it.
    fn on_accounts_raw(&mut self, v: Value, say: String) {
        if let Some(list) = v["accounts"]["accounts"].as_array() {
            self.chosen = list.iter().filter(|a| a["chosen"] == true).filter_map(|a| a["number"].as_u64().map(|n| n as u32)).collect();
        }
        match serde_json::from_value::<Response>(v) {
            Ok(resp) => self.on_reply(Tag::Accounts(say), Ok(resp)),
            Err(e) => self.flash_err(format!("dinod's accounts don't read: {e}")),
        }
    }

    fn on_job(&mut self, tag: Tag, r: Result<String, String>) {
        self.dirty = true;
        match (tag, r) {
            (Tag::Service, r) => self.on_service(r),
            (Tag::Setting(say), Ok(_)) => {
                if !say.is_empty() {
                    self.flash(say);
                }
                // What the card shows comes from the settings as they are now.
                self.send_raw(Tag::Settings, serde_json::json!({"type": "settings"}));
            }
            (_, Err(e)) => self.flash_err(format::printable(&e)),
            _ => {}
        }
    }

    fn on_state(&mut self, resp: Response) {
        let Response::State { sessions, quotas, claude_accounts, version, .. } = resp else { return };
        let ages = |s: &[SessionInfo]| s.iter().map(|s| (s.id.clone(), s.output_ms_ago.map(|ms| format::ago(ms / 1000)))).collect::<Vec<_>>();
        // Redrawn when what it shows changed: dinod's tag for it, or an age on screen ticking over.
        let changed = !self.got_state || !self.connected || version.is_none() || version != self.state_version || ages(&sessions) != ages(&self.sessions);
        let needs: HashSet<String> = sessions.iter().filter(|s| Status::of(s) == Status::NeedsYou).map(|s| s.id.clone()).collect();
        if self.got_state && needs.iter().any(|id| !self.needs.contains(id)) {
            self.bell = true;
        }
        self.needs = needs;
        self.sessions = sessions;
        self.quotas = quotas;
        self.state_accounts = claude_accounts;
        self.state_version = version;
        self.connected = true;
        self.got_state = true;
        // The tree again when sessions run somewhere new.
        let mut cwds: Vec<String> = self.sessions.iter().filter(|s| s.host.is_none()).map(|s| s.cwd.clone()).collect();
        cwds.sort();
        cwds.dedup();
        if cwds != self.tree_cwds || !self.tree_asked {
            self.tree_cwds = cwds;
            self.tree_asked = true;
            self.send(Tag::Tree, Request::Tree { folders: vec![], known: self.tree_known.clone() });
        }
        self.rebuild();
        if changed {
            self.dirty = true;
        }
    }

    fn on_reply(&mut self, tag: Tag, r: io::Result<Response>) {
        self.dirty = true;
        let resp = match r {
            Ok(r) => r,
            Err(e) => return self.flash_err(format!("dinod: {e}")),
        };
        if let Response::Error { message } = &resp {
            if conn::unknown_request(message) {
                let what = tag_feature(&tag);
                self.unsupported.insert(what);
                // The setup card asks for several at once: it says which it can't show, on its rows.
                if matches!(tag, Tag::AgentSetup | Tag::Providers | Tag::Sync(_) | Tag::FoundCount) {
                    return;
                }
                return self.flash(format!("dinod {} can't {what} yet: update dino", self.dinod));
            }
            if matches!(tag, Tag::Sync(_)) {
                self.setup.sync_look = None;
            }
            if matches!(tag, Tag::Accounts(_)) {
                self.next_login_look = None;
            }
            return self.flash_err(format::printable(message));
        }
        match (tag, resp) {
            (Tag::Launchers, Response::Launchers { launchers }) => self.launchers = launchers,
            (Tag::Tree, Response::Tree { repos, version, same }) => {
                if !same {
                    self.tree = repos;
                }
                self.tree_known = version;
                self.rebuild();
            }
            (Tag::Accounts(say), Response::ClaudeAccounts { accounts }) => self.on_accounts(accounts, say),
            (Tag::Created { attach, what }, Response::Created { id }) => {
                self.select_next = Some(id.clone());
                if !what.is_empty() {
                    self.flash(what);
                }
                if attach {
                    self.want = Some(Want::Attach(id));
                }
            }
            (Tag::Forked(name), Response::Created { id }) => {
                self.select_next = Some(id.clone());
                self.flash(format!("forked {name}: session {id}"));
            }
            (Tag::Done(say), Response::Ok) => self.flash(say),
            (Tag::Closed { id, name }, Response::Ok) => {
                self.flash(format!("closed {name} · u undoes it"));
                self.undo = Some(Undo { id, name, until: Instant::now() + UNDO });
            }
            (Tag::DeleteDry { id, name }, Response::Deletion { deletion }) => {
                let text = deletion_text(&name, &deletion);
                self.line = Line::Confirm(Confirm::Delete(id, name), text);
            }
            (Tag::Deleted(name), Response::Deletion { .. }) => self.flash(format!("deleted {name}")),
            (Tag::Stats, Response::Stats { report }) => {
                if let Screen::Stats(v) = &mut self.screen {
                    v.report = Some(report);
                }
            }
            (Tag::Found, Response::Found { sessions, .. }) => {
                if let Screen::Found(l) = &mut self.screen {
                    l.items = sessions;
                    l.loading = false;
                    l.step(0);
                }
            }
            (Tag::Archived, Response::Archived { sessions }) => {
                if let Screen::Archived(l) = &mut self.screen {
                    l.items = sessions;
                    l.loading = false;
                    l.step(0);
                }
            }
            (Tag::Unarchived(name), Response::Created { id }) => {
                self.select_next = Some(id);
                self.screen = Screen::List;
                self.flash(format!("{name} is back"));
            }
            (Tag::AgentSetup, Response::AgentSetup { agents }) => {
                if let Some(say) = self.setup.on_agents(agents) {
                    self.flash(say);
                }
            }
            (Tag::AgentAction { agent, action }, Response::Created { id }) => {
                // Its own command runs in a new shell: the user answers it there, then comes back.
                self.setup.away = Some((agent, action));
                self.want = Some(Want::Attach(id));
            }
            (Tag::Providers, Response::Providers { providers }) => self.setup.providers = Some(providers),
            (Tag::Sync(say), Response::Sync { status }) => {
                self.setup.on_sync(status);
                if !say.is_empty() {
                    self.flash(say);
                }
            }
            (Tag::FoundCount, Response::Found { sessions, more, .. }) => {
                let running = sessions.iter().filter(|s| s.pid.is_some()).count();
                self.setup.found = Some((running, sessions.len() - running, more));
            }
            (tag, resp) => self.flash_err(format!("dinod answered {tag:?} with {}", kind(&resp))),
        }
    }

    fn on_accounts(&mut self, a: ClaudeAccountsInfo, say: String) {
        let login = a.login.clone();
        let was = self.accounts.as_ref().and_then(|a| a.login.as_ref()).map(|l| (l.id, l.stage.clone()));
        if let Screen::Accounts(v) = &mut self.screen {
            v.cursor = v.cursor.min(a.accounts.len().saturating_sub(1));
        }
        self.accounts = Some(a);
        if !say.is_empty() {
            self.flash(say);
        }
        // A sign-in under way: looked at again every second, while this screen shows it.
        self.next_login_look = None;
        if let Some(l) = login {
            let now = (l.id, l.stage.clone());
            match l.stage.as_str() {
                "starting" | "browser" | "checking" => {
                    self.next_login_look = Some(Instant::now() + LOGIN_LOOK);
                    // No browser here: once claude.ai's link shows, the code it ends on is asked for.
                    let fresh = was.as_ref().is_none_or(|(id, stage)| *id != l.id || stage != "browser");
                    if self.setup.headless && l.stage == "browser" && l.url.is_some() && fresh && matches!(self.line, Line::Normal) {
                        self.line = Line::Ask(Ask::LoginCode, Input::default());
                    }
                }
                // How it ended, once.
                "added" if was.as_ref() != Some(&now) && was.is_some() => {
                    let n = l.account.unwrap_or(0);
                    self.flash(match &l.error {
                        Some(note) => format!("added Account {n}. {}", format::printable(note)),
                        None => format!("added Account {n}: r names it"),
                    });
                    if let (Screen::Accounts(v), Some(list)) = (&mut self.screen, self.accounts.as_ref()) {
                        v.cursor = list.accounts.iter().position(|a| a.number == n).unwrap_or(v.cursor);
                    }
                }
                "failed" if was.as_ref() != Some(&now) && was.is_some() => self.flash_err(format!("sign-in failed: {}", format::printable(l.error.as_deref().unwrap_or("no reason given")))),
                _ => {}
            }
        }
    }

    // ---- The list ----

    pub fn rebuild(&mut self) {
        self.groups = order::groups(&self.sessions, &self.tree, &self.filter);
        self.rows = self.groups.iter().enumerate().flat_map(|(gi, g)| std::iter::once(Row::Group(gi)).chain(g.sessions.iter().map(|&i| Row::Session(i)))).collect();
        if let Some(id) = self.select_next.clone()
            && self.sessions.iter().any(|s| s.id == id)
        {
            self.selected = Some(id);
            self.select_next = None;
        }
        let at = self.selected.as_ref().and_then(|id| self.rows.iter().position(|r| matches!(r, Row::Session(i) if self.sessions[*i].id == *id)));
        self.cursor = match at {
            Some(r) => r,
            // Gone (or nothing chosen yet): the nearest session to where the cursor was.
            None => {
                let rows = self.session_rows();
                rows.iter().copied().find(|&r| r >= self.cursor).or_else(|| rows.last().copied()).unwrap_or(0)
            }
        };
        self.selected = self.selected_session().map(|s| s.id.clone());
    }

    fn session_rows(&self) -> Vec<usize> {
        self.rows.iter().enumerate().filter(|(_, r)| matches!(r, Row::Session(_))).map(|(i, _)| i).collect()
    }

    pub fn selected_session(&self) -> Option<&SessionInfo> {
        match self.rows.get(self.cursor)? {
            Row::Session(i) => self.sessions.get(*i),
            Row::Group(_) => None,
        }
    }

    fn select_row(&mut self, row: usize) {
        self.cursor = row;
        self.selected = self.selected_session().map(|s| s.id.clone());
    }

    fn step(&mut self, by: isize) {
        let rows = self.session_rows();
        if rows.is_empty() {
            return;
        }
        let at = rows.iter().position(|&r| r == self.cursor).unwrap_or(0) as isize;
        let to = (at + by).clamp(0, rows.len() as isize - 1) as usize;
        self.select_row(rows[to]);
    }

    fn first(&mut self) {
        if let Some(&r) = self.session_rows().first() {
            self.select_row(r);
        }
    }

    fn last(&mut self) {
        if let Some(&r) = self.session_rows().last() {
            self.select_row(r);
        }
    }

    /// Keep the cursor on screen (and its group's name above it, when it fits).
    pub fn scroll(&mut self, height: usize) {
        self.height = height.max(1);
        if self.cursor < self.offset + 1 {
            self.offset = self.cursor.saturating_sub(1);
        }
        if self.cursor >= self.offset + self.height {
            self.offset = self.cursor + 1 - self.height;
        }
        self.offset = self.offset.min(self.rows.len().saturating_sub(self.height));
    }

    /// The session after (or before) `id` in list order, or the next one that needs you; never `id`.
    pub fn neighbour(&self, id: &str, which: Neighbour) -> Option<String> {
        let order: Vec<&SessionInfo> = self.rows.iter().filter_map(|r| if let Row::Session(i) = r { Some(&self.sessions[*i]) } else { None }).collect();
        let n = order.len();
        let at = order.iter().position(|s| s.id == id);
        let pick = |k: usize| order[k].id.clone();
        match (which, at) {
            (_, _) if n == 0 => None,
            (Neighbour::Next, Some(a)) => Some(pick((a + 1) % n)),
            (Neighbour::Prev, Some(a)) => Some(pick((a + n - 1) % n)),
            (Neighbour::Next | Neighbour::Prev, None) => Some(pick(0)),
            (Neighbour::NeedsYou, at) => {
                let start = at.map_or(0, |a| a + 1);
                (0..n).map(|k| (start + k) % n).find(|&k| Some(k) != at && Status::of(order[k]) == Status::NeedsYou).map(pick)
            }
        }
        .filter(|next| next != id)
    }

    pub fn needs_count(&self) -> usize {
        self.sessions.iter().filter(|s| Status::of(s) == Status::NeedsYou).count()
    }

    /// The Claude account Claude Code's calls go to now, and its quota, when dinod knows it.
    pub fn account_line(&self) -> Option<String> {
        let several = self.state_accounts.as_ref().filter(|l| l.len() > 1);
        let list = several.or(self.accounts.as_ref().map(|a| &a.accounts))?;
        let a = list.iter().find(|a| a.answering).or(list.first())?;
        if list.len() < 2 && a.name.is_none() && a.windows.is_empty() {
            return None;
        }
        let mut text = format::printable(&a.label());
        if a.spent {
            text += &match a.resets_at {
                Some(t) => format!(" spent, back {}", format::until(t.saturating_sub(format::now()))),
                None => " spent".into(),
            };
            return Some(text);
        }
        let quota = self.quotas.iter().find(|q| q.provider == "anthropic");
        let windows = if a.windows.is_empty() { quota.map(|q| q.windows.as_slice()).unwrap_or_default() } else { a.windows.as_slice() };
        if let Some(w) = windows.iter().max_by(|x, y| x.utilization.total_cmp(&y.utilization)) {
            text += &format!(" {} {:.0}%", format::printable(&w.name), w.utilization * 100.0);
        }
        Some(text)
    }

    /// Something visible is working: the spinner turns.
    pub fn spinning(&self) -> bool {
        matches!(self.screen, Screen::List) && self.rows.iter().skip(self.offset).take(self.height).any(|r| matches!(r, Row::Session(i) if Status::of(&self.sessions[*i]) == Status::Working))
    }

    // ---- Time ----

    /// When something on screen changes by itself: a message or an undo running out, the
    /// spinner's next frame, a sign-in to look at again. Nothing: sleep until dinod or a key.
    pub fn deadline(&self) -> Option<Instant> {
        let spin = self.spinning().then(|| self.last_spin + SPIN);
        let login = self.next_login_look.filter(|_| matches!(self.screen, Screen::Accounts(_) | Screen::Setup(_)));
        [self.message.as_ref().map(|m| m.until), self.undo.as_ref().map(|u| u.until), spin, login, self.setup_deadline()].into_iter().flatten().min()
    }

    pub fn tick(&mut self, now: Instant) {
        if self.message.as_ref().is_some_and(|m| m.until <= now) {
            self.message = None;
            self.dirty = true;
        }
        if self.undo.as_ref().is_some_and(|u| u.until <= now) {
            self.undo = None;
            self.dirty = true;
        }
        if self.spinning() && now >= self.last_spin + SPIN {
            self.spinner = self.spinner.wrapping_add(1);
            self.last_spin = now;
            self.dirty = true;
        }
        if matches!(self.screen, Screen::Accounts(_) | Screen::Setup(_)) && self.next_login_look.is_some_and(|t| t <= now) {
            self.next_login_look = None;
            self.send(Tag::Accounts(String::new()), accounts("status"));
        }
        self.setup_tick(now);
    }

    // ---- Keys ----

    pub fn on_term(&mut self, ev: TermEvent) {
        match ev {
            TermEvent::Key(k) if k.kind != KeyEventKind::Release => {
                self.dirty = true;
                self.on_key(k);
            }
            TermEvent::Paste(text) => {
                self.dirty = true;
                self.on_paste(&text);
            }
            TermEvent::Resize(..) => self.dirty = true,
            _ => {}
        }
    }

    fn on_paste(&mut self, text: &str) {
        match &mut self.line {
            Line::Filter(i) | Line::Ask(_, i) => i.insert_str(text),
            Line::New(f) if f.field == 1 => f.folder.insert_str(text),
            Line::New(f) if f.field == 3 => f.prompt.insert_str(text),
            _ => {}
        }
        if matches!(self.line, Line::Filter(_)) {
            self.apply_filter();
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if self.overlay.is_some() {
            return self.overlay_key(k);
        }
        if !matches!(self.line, Line::Normal) {
            return self.line_key(k);
        }
        match self.screen {
            Screen::List => self.list_key(k),
            Screen::Setup(_) => self.setup_key(k),
            Screen::Accounts(_) => self.accounts_key(k),
            Screen::Stats(_) => self.stats_key(k),
            Screen::Found(_) => self.found_key(k),
            Screen::Archived(_) => self.archived_key(k),
        }
    }

    fn list_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let pending = self.pending.take();
        let half = (self.height / 2).max(1) as isize;
        match k.code {
            KeyCode::Char('g') if pending == Some('g') => self.first(),
            KeyCode::Char('d') if pending == Some('d') && !ctrl => self.delete(),
            KeyCode::Char('d') if ctrl => self.step(half),
            KeyCode::Char('u') if ctrl => self.step(-half),
            KeyCode::Char('c') if ctrl => self.want = Some(Want::Quit),
            KeyCode::Char('l') if ctrl => self.dirty = true,
            _ if ctrl => {}
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::PageDown => self.step(half * 2),
            KeyCode::PageUp => self.step(-half * 2),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('d') => self.pending = Some('d'),
            KeyCode::Char('G') | KeyCode::End => self.last(),
            KeyCode::Home => self.first(),
            KeyCode::Char('/') => self.line = Line::Filter(Input::with(&self.filter)),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                if let Some(s) = self.selected_session() {
                    self.want = Some(Want::Attach(s.id.clone()));
                }
            }
            KeyCode::Char('o') => self.new_form(false),
            KeyCode::Char('O') => self.new_form(true),
            KeyCode::Char('i') => {
                if let Some(s) = self.selected_session() {
                    self.line = Line::Ask(Ask::Send(s.id.clone()), Input::default());
                }
            }
            KeyCode::Char('I') => self.with_selected(|s| (Tag::Done(format!("interrupted {}", format::name(s))), Request::Interrupt { id: s.id.clone() })),
            KeyCode::Char('m') => {
                if let Some(s) = self.selected_session() {
                    let id = s.id.clone();
                    self.open_picker(PickFor::Session(id));
                }
            }
            KeyCode::Char(c @ ('f' | 'F')) => self.with_selected(|s| (Tag::Forked(format::name(s)), Request::Fork { id: s.id.clone(), name: None, worktree: c == 'F', prompt: None })),
            KeyCode::Char('R') => match self.selected_session() {
                Some(s) if !s.exited => {
                    let name = format::name(s);
                    self.flash(format!("{name} is running"));
                }
                _ => self.with_selected(|s| (Tag::Done(format!("resumed {}", format::name(s))), Request::Resume { id: s.id.clone() })),
            },
            KeyCode::Char('r') => {
                if let Some(s) = self.selected_session() {
                    let current = s.label.clone().unwrap_or_else(|| format::name(s));
                    self.line = Line::Ask(Ask::Rename(s.id.clone()), Input::with(&current));
                }
            }
            KeyCode::Char('p') => self.with_selected(|s| {
                let say = if s.pinned { "unpinned" } else { "pinned" };
                (Tag::Done(format!("{say} {}", format::name(s))), Request::Pin { id: s.id.clone(), pinned: !s.pinned })
            }),
            KeyCode::Char('a') => self.with_selected(|s| (Tag::Done(format!("archived {} · :archived brings it back", format::name(s))), Request::Archive { id: s.id.clone() })),
            KeyCode::Char('x') => self.with_selected(|s| (Tag::Closed { id: s.id.clone(), name: format::name(s) }, Request::Close { id: s.id.clone(), undo_ms: UNDO.as_millis() as u64 })),
            KeyCode::Char('u') => match self.undo.take() {
                Some(u) if u.until > Instant::now() => {
                    self.message = None;
                    self.select_next = Some(u.id.clone());
                    self.send(Tag::Done(format!("reopened {}", u.name)), Request::Reopen { id: u.id });
                }
                _ => self.flash("nothing to undo"),
            },
            KeyCode::Char('A') => self.open_accounts(),
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Char(':') => self.line = Line::Ask(Ask::Command, Input::default()),
            KeyCode::Char('q') => self.want = Some(Want::Quit),
            KeyCode::Esc => {
                self.message = None;
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.rebuild();
                }
            }
            _ => {}
        }
    }

    fn with_selected(&mut self, f: impl FnOnce(&SessionInfo) -> (Tag, Request)) {
        if let Some(s) = self.selected_session() {
            let (tag, req) = f(s);
            self.send(tag, req);
        }
    }

    fn delete(&mut self) {
        let Some(s) = self.selected_session() else { return };
        let (id, name) = (s.id.clone(), format::name(s));
        if self.config.confirm_delete {
            self.send(Tag::DeleteDry { id: id.clone(), name }, Request::Delete { id, dry_run: true });
        } else {
            self.send(Tag::Deleted(name), Request::Delete { id, dry_run: false });
        }
    }

    fn apply_filter(&mut self) {
        if let Line::Filter(i) = &self.line {
            self.filter = i.text.clone();
            self.rebuild();
            if self.selected_session().is_none() {
                self.first();
            }
        }
    }

    // ---- The bottom line's input ----

    fn line_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let cancel = k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c'));
        match &mut self.line {
            Line::Filter(i) => {
                if cancel {
                    self.line = Line::Normal;
                    self.filter.clear();
                    self.rebuild();
                } else if k.code == KeyCode::Enter {
                    self.line = Line::Normal;
                } else if i.key(&k) {
                    self.apply_filter();
                }
            }
            Line::Confirm(c, _) => {
                let c = c.clone();
                self.line = Line::Normal;
                if matches!(k.code, KeyCode::Char('y' | 'Y')) {
                    match c {
                        Confirm::Delete(id, name) => self.send(Tag::Deleted(name), Request::Delete { id, dry_run: false }),
                        Confirm::RemoveAccount(n) => {
                            self.send(Tag::Accounts(format!("removed Account {n}")), Request::ClaudeAccounts { action: "remove".into(), value: None, account: Some(n), order: vec![] })
                        }
                    }
                } else {
                    self.flash("left as it is");
                }
            }
            Line::Ask(ask, i) => {
                if cancel {
                    self.line = Line::Normal;
                } else if k.code == KeyCode::Tab && *ask == Ask::Command {
                    let text = i.text.clone();
                    let done = complete_command(&text, self);
                    if let Line::Ask(_, i) = &mut self.line {
                        match done {
                            Completion::Line(l) => i.set(&l),
                            Completion::Choices(c) => {
                                let c = c.join("  ");
                                self.flash(c);
                            }
                            Completion::None => {}
                        }
                    }
                } else if k.code == KeyCode::Enter {
                    let (ask, text) = (ask.clone(), i.text.clone());
                    self.line = Line::Normal;
                    self.answer(ask, text);
                } else {
                    i.key(&k);
                }
            }
            Line::New(_) => self.new_key(k),
            Line::Normal => {}
        }
    }

    fn answer(&mut self, ask: Ask, text: String) {
        let name = |app: &App, id: &str| app.sessions.iter().find(|s| s.id == id).map_or_else(|| id.to_string(), format::name);
        match ask {
            Ask::Send(id) if !text.trim().is_empty() => {
                let n = name(self, &id);
                self.send(Tag::Done(format!("sent to {n}")), Request::SendInput { id, text, submit: true });
            }
            Ask::Rename(id) => {
                let say = if text.trim().is_empty() { "named by its agent again".to_string() } else { format!("renamed to {}", format::printable(text.trim())) };
                self.send(Tag::Done(say), Request::Rename { id, name: text.trim().to_string() });
            }
            Ask::Command => self.command(&text),
            Ask::AccountName(n) => {
                self.send(Tag::Accounts(format!("Account {n} named")), Request::ClaudeAccounts { action: "rename".into(), value: Some(text.trim().to_string()), account: Some(n), order: vec![] })
            }
            Ask::LoginCode if !text.trim().is_empty() => self.send(Tag::Accounts("checking the code…".into()), accounts_value("login_code", text.trim())),
            _ => {}
        }
    }

    // ---- New session ----

    fn new_form(&mut self, worktree: bool) {
        let folder = self.selected_session().filter(|s| s.host.is_none()).map(|s| s.shell_cwd.clone().unwrap_or_else(|| s.cwd.clone()));
        let folder = folder.or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string())).unwrap_or_default();
        self.send(Tag::Launchers, Request::Launchers);
        self.line = Line::New(NewForm { field: 3, folder: Input::with(&format::short_path(&folder)), worktree, ..NewForm::default() });
    }

    fn new_key(&mut self, k: KeyEvent) {
        let Line::New(f) = &mut self.line else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.launchers.len().max(1);
        match k.code {
            KeyCode::Esc => self.line = Line::Normal,
            KeyCode::Char('c') if ctrl => self.line = Line::Normal,
            KeyCode::Enter => self.start_new(),
            KeyCode::BackTab => f.field = (f.field + NEW_FIELDS - 1) % NEW_FIELDS,
            KeyCode::Tab if f.field == 1 => {
                let done = complete_folder(&f.folder.text, &self.tree, &self.sessions);
                let Line::New(f) = &mut self.line else { return };
                match done {
                    Completion::Line(l) if l != f.folder.text => f.folder.set(&l),
                    Completion::Choices(c) => {
                        let c = c.join("  ");
                        self.flash(c);
                    }
                    _ => f.field = 2,
                }
            }
            KeyCode::Tab => f.field = (f.field + 1) % NEW_FIELDS,
            KeyCode::Char('m') if matches!(f.field, 0 | 2) => self.open_picker(PickFor::New),
            // The agent and worktree fields change only with these: a letter typed there (a prompt
            // begun in the wrong field) changes nothing.
            KeyCode::Left if f.field == 0 => f.agent = (f.agent + n - 1) % n,
            KeyCode::Right | KeyCode::Char(' ') if f.field == 0 => f.agent = (f.agent + 1) % n,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if f.field == 2 => f.worktree = !f.worktree,
            KeyCode::Char('y') if f.field == 2 => f.worktree = true,
            KeyCode::Char('n') if f.field == 2 => f.worktree = false,
            _ if f.field == 1 => {
                f.folder.key(&k);
            }
            _ if f.field == 3 => {
                f.prompt.key(&k);
            }
            _ => {}
        }
    }

    fn start_new(&mut self) {
        let Line::New(f) = std::mem::replace(&mut self.line, Line::Normal) else { return };
        let Some(l) = self.launchers.get(f.agent) else {
            return self.flash_err("dinod offers no agents to start (Settings → Agents)");
        };
        let (short, folder) = (l.short.clone(), format::expand_home(f.folder.text.trim()));
        let prompt = Some(f.prompt.text.trim().to_string()).filter(|p| !p.is_empty());
        let req = self.new_request(&short, &folder, f.worktree, prompt, f.controls);
        let what = format!("started {short}{}", if f.worktree { " in a new worktree" } else { "" });
        self.send(Tag::Created { attach: true, what }, req);
    }

    pub fn new_request(&self, launcher: &str, folder: &str, worktree: bool, prompt: Option<String>, controls: Controls) -> Request {
        let (cols, rows) = self.attach_size();
        let cwd = Some(folder.to_string()).filter(|f| !f.is_empty());
        Request::New { launcher: launcher.into(), args: vec![], cwd, cols, rows, worktree, controls, host: None, prompt, by: None, route: None, reveal: false, tmux: None, stay: false }
    }

    /// The size a session gets: the terminal's, a row less with the status line.
    pub fn attach_size(&self) -> (u16, u16) {
        let (c, r) = self.size;
        (c.max(20), if self.config.status_line { r.saturating_sub(1).max(5) } else { r.max(5) })
    }

    // ---- The picker ----

    fn open_picker(&mut self, target: PickFor) {
        let (agent, current, title) = match &target {
            PickFor::Session(id) => match self.sessions.iter().find(|s| s.id == *id) {
                Some(s) => {
                    let mode = s.agent_mode.clone().or(s.controls.mode.clone());
                    (s.agent_id.clone(), Controls { mode, ..s.controls.clone() }, format::name(s))
                }
                None => return,
            },
            PickFor::New => match (&self.line, self.launchers.as_slice()) {
                (Line::New(f), l) if !l.is_empty() => (l[f.agent % l.len()].agent_id.clone(), f.controls.clone(), "new session".into()),
                _ => return self.flash("no agents to pick for yet"),
            },
        };
        match picker(&self.launchers, &agent, &current, target, title) {
            Some(p) => self.overlay = Some(Overlay::Picker(p)),
            None => self.flash(format!("{agent} has no mode, model or effort to pick")),
        }
    }

    fn overlay_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::Picker(p)) = &mut self.overlay else {
            // Help: any key closes it.
            self.overlay = None;
            return;
        };
        if let Some(i) = &mut p.typing {
            match k.code {
                KeyCode::Esc => p.typing = None,
                KeyCode::Enter => {
                    let typed = i.text.trim().to_string();
                    p.typing = None;
                    if !typed.is_empty() {
                        let col = &mut p.cols[p.col];
                        col.options.insert(col.options.len() - 1, (Some(typed.clone()), typed));
                        col.cursor = col.options.len() - 2;
                    }
                }
                _ => {
                    i.key(&k);
                }
            }
            return;
        }
        let col = &mut p.cols[p.col];
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Char('c') if ctrl => self.overlay = None,
            KeyCode::Char('j') | KeyCode::Down => col.cursor = (col.cursor + 1).min(col.options.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => col.cursor = col.cursor.saturating_sub(1),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab => p.col = (p.col + 1) % p.cols.len(),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::BackTab => p.col = (p.col + p.cols.len() - 1) % p.cols.len(),
            KeyCode::Enter if col.options[col.cursor].0.as_deref() == Some(OTHER) => p.typing = Some(Input::default()),
            KeyCode::Enter => {
                let Some(Overlay::Picker(p)) = self.overlay.take() else { return };
                let pick = |name: &str| p.cols.iter().find(|c| c.name == name).and_then(|c| c.options[c.cursor].0.clone());
                let controls = Controls { mode: pick("mode"), model: pick("model"), effort: pick("effort") };
                let says = [&controls.mode, &controls.model, &controls.effort].iter().filter_map(|v| v.as_deref()).collect::<Vec<_>>().join(" · ");
                match p.target {
                    PickFor::Session(id) => {
                        self.send(Tag::Done(format!("{}: {}", p.title, if says.is_empty() { "defaults" } else { &says })), Request::SetControls { id, controls });
                    }
                    PickFor::New => {
                        if let Line::New(f) = &mut self.line {
                            f.controls = controls;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ---- Accounts ----

    fn open_accounts(&mut self) {
        self.screen = Screen::Accounts(AccountsView::default());
        self.send(Tag::Accounts(String::new()), accounts("status"));
    }

    fn accounts_key(&mut self, k: KeyEvent) {
        let pending = self.pending.take();
        let list: Vec<ClaudeAccountInfo> = self.accounts.as_ref().map(|a| a.accounts.clone()).unwrap_or_default();
        let Screen::Accounts(v) = &mut self.screen else { return };
        let at = v.cursor.min(list.len().saturating_sub(1));
        let chosen = list.get(at).cloned();
        // The order dinod takes: every added account's number (Claude Code's own, 1, is always
        // tried first and isn't in it).
        let added: Vec<u32> = list.iter().map(|a| a.number).filter(|&n| n != 1).collect();
        let reorder = |app: &mut App, order: Vec<u32>, say: String| app.send(Tag::Accounts(say), Request::ClaudeAccounts { action: "order".into(), value: None, account: None, order });
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.switched = None;
                self.screen = Screen::List;
            }
            KeyCode::Char('j') | KeyCode::Down => v.cursor = (at + 1).min(list.len().saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => v.cursor = at.saturating_sub(1),
            KeyCode::Char(c @ ('J' | 'K')) => {
                let Some(a) = chosen else { return };
                let Some(i) = added.iter().position(|&n| n == a.number) else {
                    return self.flash("Account 1 is Claude Code's own sign-in: it's always tried first");
                };
                let to = if c == 'J' { i + 1 } else { i.wrapping_sub(1) };
                if to >= added.len() {
                    return;
                }
                let mut order = added.clone();
                order.swap(i, to);
                v.cursor = if c == 'J' { at + 1 } else { at - 1 };
                reorder(self, order, String::new());
            }
            KeyCode::Enter => {
                let Some(a) = chosen else { return };
                // A dinod from before `use` said so once: it isn't asked again.
                if self.unsupported.contains("use") {
                    return self.flash(format!("this dinod ({}) can't switch accounts yet: update dino", self.dinod));
                }
                if self.chosen.first().copied().unwrap_or(1) == a.number {
                    return self.flash(format!("{} is the one Claude Code goes to first already", format::printable(&a.label())));
                }
                let name = format::printable(&a.label());
                self.switched = None;
                self.send(Tag::Use(a.number, name), Request::ClaudeAccounts { action: "use".into(), value: None, account: Some(a.number), order: vec![] });
            }
            KeyCode::Char('a') => {
                let say = if self.setup.headless { "signing in: open the link below somewhere with a browser" } else { "signing in: a browser opens where dinod runs" };
                self.send(Tag::Accounts(say.into()), accounts("login"));
            }
            KeyCode::Char('c') if self.login_under_way() => self.line = Line::Ask(Ask::LoginCode, Input::default()),
            KeyCode::Char('x') if self.login_under_way() => self.send(Tag::Accounts("sign-in cancelled".into()), accounts("login_cancel")),
            KeyCode::Char('r') => {
                if let Some(a) = chosen {
                    self.line = Line::Ask(Ask::AccountName(a.number), Input::with(a.name.as_deref().unwrap_or("")));
                }
            }
            KeyCode::Char('d') if pending == Some('d') => match chosen {
                Some(a) if a.number == 1 => self.flash("Account 1 is Claude Code's own sign-in: sign out in Claude Code to remove it"),
                Some(a) => self.line = Line::Confirm(Confirm::RemoveAccount(a.number), format!("remove {}? its token leaves the Keychain y/N", format::printable(&a.label()))),
                None => {}
            },
            KeyCode::Char('d') => self.pending = Some('d'),
            KeyCode::Char(':') => self.line = Line::Ask(Ask::Command, Input::default()),
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            _ => {}
        }
    }

    pub fn login_under_way(&self) -> bool {
        self.accounts.as_ref().and_then(|a| a.login.as_ref()).is_some_and(|l| matches!(l.stage.as_str(), "starting" | "browser"))
    }

    // ---- Usage, found, archived ----

    fn stats_key(&mut self, k: KeyEvent) {
        let Screen::Stats(v) = &mut self.screen else { return };
        let days = v.report.as_ref().map_or(0, |r| r.days.len());
        let range = |app: &mut App, r: Range| {
            app.screen = Screen::Stats(StatsView { range: r, report: None, scroll: 0, day: None });
            app.send(Tag::Stats, Request::Stats { range: r });
        };
        match k.code {
            KeyCode::Esc if v.day.is_some() => v.day = None,
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Char('j') | KeyCode::Down => v.scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => v.scroll = v.scroll.saturating_sub(1),
            // ◂ ▸: a day of the chart, counted back from the last.
            KeyCode::Char('h') | KeyCode::Left if days > 0 => v.day = Some(v.day.map_or(0, |d| (d + 1).min(days - 1))),
            KeyCode::Char('l') | KeyCode::Right => v.day = v.day.and_then(|d| d.checked_sub(1)),
            KeyCode::Char('7') => range(self, Range::Week),
            KeyCode::Char('3') => range(self, Range::Month),
            KeyCode::Char('a') => range(self, Range::All),
            KeyCode::Char('r') => {
                let r = v.range;
                range(self, r);
            }
            KeyCode::Char(':') => self.line = Line::Ask(Ask::Command, Input::default()),
            _ => {}
        }
    }

    fn found_key(&mut self, k: KeyEvent) {
        let Screen::Found(l) = &mut self.screen else { return };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Char('j') | KeyCode::Down => l.step(1),
            KeyCode::Char('k') | KeyCode::Up => l.step(-1),
            KeyCode::Enter => {
                let Some(f) = l.items.get(l.cursor).cloned() else { return };
                if f.session_id.is_empty() && f.pid.is_none() {
                    return self.flash("nothing to continue there");
                }
                self.screen = Screen::List;
                let what = format!("continuing {} in dino{}", format::printable(&f.agent), if f.pid.is_some() { " (it waits for its turn to end)" } else { "" });
                self.flash(what.clone());
                self.send(Tag::Created { attach: true, what }, Request::Adopt { session: f, cwd: None });
            }
            KeyCode::Char(':') => self.line = Line::Ask(Ask::Command, Input::default()),
            _ => {}
        }
    }

    fn archived_key(&mut self, k: KeyEvent) {
        let Screen::Archived(l) = &mut self.screen else { return };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Char('j') | KeyCode::Down => l.step(1),
            KeyCode::Char('k') | KeyCode::Up => l.step(-1),
            KeyCode::Enter => {
                let Some(a) = l.items.get(l.cursor) else { return };
                let name = format::printable(a.label.as_deref().unwrap_or(&a.name));
                let id = a.id.clone();
                self.send(Tag::Unarchived(name), Request::Unarchive { id });
            }
            KeyCode::Char(':') => self.line = Line::Ask(Ask::Command, Input::default()),
            _ => {}
        }
    }

    // ---- The command line ----

    pub fn command(&mut self, line: &str) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(&cmd) = words.first() else { return };
        match cmd {
            "q" | "q!" | "quit" | "qa" | "wq" => self.want = Some(Want::Quit),
            "new" => self.command_new(line),
            "found" => {
                self.screen = Screen::Found(PickList::loading());
                self.send(Tag::Found, Request::Found { cloud: false, running_only: false, limit: Some(200), query: None, hidden: false });
            }
            "archived" => {
                self.screen = Screen::Archived(PickList::loading());
                self.send(Tag::Archived, Request::Archived);
            }
            "stats" => {
                let range = match words.get(1) {
                    None => Range::Month,
                    Some(r) => match Range::parse(r) {
                        Some(r) => r,
                        None => return self.flash_err(format!("{}: :stats takes 7d, 30d or all", format::printable(r))),
                    },
                };
                self.screen = Screen::Stats(StatsView { range, report: None, scroll: 0, day: None });
                self.send(Tag::Stats, Request::Stats { range });
            }
            "accounts" => self.open_accounts(),
            "setup" => self.open_setup(),
            "settings" => self.want = Some(Want::Settings),
            "set" => match words.as_slice() {
                [_] => self.flash(self.config.show()),
                [_, opt, value] => match self.config.set(opt, value) {
                    Ok(say) => match self.config.save() {
                        Ok(()) => self.flash(say),
                        Err(e) => self.flash_err(format!("{say}, but it isn't saved: {e}")),
                    },
                    Err(e) => self.flash_err(e),
                },
                _ => self.flash_err(":set <option> <value>: prefix, status_line, confirm_delete, sidebar or group_by"),
            },
            "help" | "h" => self.overlay = Some(Overlay::Help),
            _ => self.flash_err(format!("no command :{}  (:new :found :archived :stats :accounts :setup :settings :set :q)", format::printable(cmd))),
        }
    }

    /// `:new <agent> [folder] [--worktree] [-- prompt]`
    fn command_new(&mut self, line: &str) {
        let (head, prompt) = match line.split_once(" -- ") {
            Some((h, p)) => (h, Some(p.trim().to_string()).filter(|p| !p.is_empty())),
            None => (line.strip_suffix(" --").unwrap_or(line), None),
        };
        let mut words = head.split_whitespace().skip(1);
        let mut agent = None;
        let mut folder = None;
        let mut worktree = false;
        for w in words.by_ref() {
            match w {
                "--worktree" | "-w" => worktree = true,
                w if agent.is_none() => agent = Some(w.to_string()),
                w if folder.is_none() => folder = Some(w.to_string()),
                w => return self.flash_err(format!(":new doesn't take {}", format::printable(w))),
            }
        }
        let Some(agent) = agent else { return self.flash_err(":new <agent> [folder] [--worktree] [-- prompt]") };
        let launcher = self
            .launchers
            .iter()
            .find(|l| l.short == agent)
            .or_else(|| self.launchers.iter().find(|l| l.short.starts_with(&agent) || l.label.to_lowercase().starts_with(&agent.to_lowercase())));
        let short = match launcher {
            Some(l) => l.short.clone(),
            None if self.launchers.is_empty() => agent,
            None => return self.flash_err(format!("dinod has no agent {}: {}", format::printable(&agent), self.launchers.iter().map(|l| l.short.as_str()).collect::<Vec<_>>().join(", "))),
        };
        let folder = folder.map(|f| format::expand_home(&f)).or_else(|| self.selected_session().filter(|s| s.host.is_none()).map(|s| s.cwd.clone()));
        let folder = folder.or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string())).unwrap_or_default();
        let req = self.new_request(&short, &folder, worktree, prompt, Controls::default());
        self.send(Tag::Created { attach: true, what: format!("started {short}") }, req);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Neighbour {
    Next,
    Prev,
    NeedsYou,
}

fn accounts(action: &str) -> Request {
    Request::ClaudeAccounts { action: action.into(), value: None, account: None, order: vec![] }
}

fn accounts_value(action: &str, value: &str) -> Request {
    Request::ClaudeAccounts { action: action.into(), value: Some(value.into()), account: None, order: vec![] }
}

/// What a request does, for "dinod 0.1.9 can't … yet".
fn tag_feature(tag: &Tag) -> &'static str {
    match tag {
        Tag::Accounts(_) => "manage Claude accounts",
        Tag::Created { .. } | Tag::Unarchived(_) => "start that",
        Tag::Forked(_) => "fork",
        Tag::Closed { .. } => "close with undo",
        Tag::DeleteDry { .. } | Tag::Deleted(_) => "delete sessions",
        Tag::Stats => "show usage",
        Tag::Found => "find sessions outside dino",
        Tag::Archived => "list archived sessions",
        Tag::Tree => "group sessions by repo",
        Tag::Launchers => "list agents",
        Tag::Done(_) | Tag::Setting(_) | Tag::Service => "do that",
        Tag::Settings => "show its settings",
        Tag::AgentSetup | Tag::AgentAction { .. } => "set up agents",
        Tag::Providers => "list model servers",
        Tag::Sync(_) => "sync settings",
        Tag::FoundCount => "find sessions outside dino",
        Tag::Use(..) => "switch accounts",
    }
}

fn kind(r: &Response) -> String {
    serde_json::to_value(r).ok().and_then(|v| v["type"].as_str().map(String::from)).unwrap_or_else(|| "something else".into())
}

/// The y/N line for deleting `name`: what goes with it, and what would be lost.
pub fn deletion_text(name: &str, d: &Deletion) -> String {
    let mut parts = vec![];
    if let Some(w) = &d.worktree {
        parts.push(format!("removes worktree {}", format::short_path(w)));
        match (&d.branch, d.keeps_branch) {
            (Some(b), false) => parts.push(format!("branch {}", format::printable(b))),
            (Some(b), true) => parts.push(format!("keeps branch {} (unmerged commits)", format::printable(b))),
            _ => {}
        }
        if d.uncommitted > 0 {
            parts.push(format!("{} uncommitted {} lost", d.uncommitted, if d.uncommitted == 1 { "file" } else { "files" }));
        }
        if d.unpushed > 0 {
            parts.push(format!("{} unpushed {}", d.unpushed, if d.unpushed == 1 { "commit" } else { "commits" }));
        }
    }
    if let Some(other) = &d.kept_for {
        parts.push(format!("its worktree stays for {}", format::printable(other)));
    }
    // y/N first: a long path may not fit.
    let what = if parts.is_empty() { String::new() } else { format!("  {}", parts.join(", ")) };
    format!("delete {name}? y/N{what}")
}

/// The picker for agent `agent`'s knobs, at `current`.
pub fn picker(launchers: &[LauncherInfo], agent: &str, current: &Controls, target: PickFor, title: String) -> Option<Picker> {
    let k = &launchers.iter().find(|l| l.agent_id == agent || l.short == agent)?.knobs;
    let mut cols = vec![];
    let col = |name: &'static str, mut options: Vec<(Option<String>, String)>, now: &Option<String>| {
        if let Some(v) = now
            && !options.iter().any(|(o, _)| o.as_ref() == Some(v))
        {
            options.insert(1, (Some(v.clone()), format::printable(v)));
        }
        let cursor = options.iter().position(|(o, _)| o == now).unwrap_or(0);
        PickCol { name, options, cursor }
    };
    let default = || (None, "default".to_string());
    if !k.modes.is_empty() {
        let label = |m: &str| k.mode_labels.get(m).cloned().or_else(|| MODES.iter().find(|x| x.0 == m).map(|x| x.1.to_string())).unwrap_or_else(|| m.to_string());
        let options = std::iter::once(default()).chain(k.modes.iter().map(|m| (Some(m.clone()), label(m)))).collect();
        cols.push(col("mode", options, &current.mode));
    }
    if k.model {
        let mut options: Vec<_> = std::iter::once(default()).chain(k.models.iter().map(|m| (Some(m.id.clone()), format::printable(&m.label)))).collect();
        options.push((Some(OTHER.into()), "other…".into()));
        cols.push(col("model", options, &current.model));
    }
    if !k.efforts.is_empty() {
        let options = std::iter::once(default()).chain(k.efforts.iter().map(|e| (Some(e.clone()), e.clone()))).collect();
        cols.push(col("effort", options, &current.effort));
    }
    (!cols.is_empty()).then_some(Picker { target, title, cols, col: 0, typing: None })
}

pub enum Completion {
    /// The line, completed.
    Line(String),
    /// More than one way on: these.
    Choices(Vec<String>),
    None,
}

const COMMANDS: &[&str] = &["new", "found", "archived", "stats", "accounts", "setup", "settings", "set", "q"];

/// Tab on the command line: the command, then its arguments.
pub fn complete_command(line: &str, app: &App) -> Completion {
    let words: Vec<&str> = line.split(' ').collect();
    let (last, before) = words.split_last().unwrap_or((&"", &[]));
    let candidates: Vec<String> = match before {
        [] => COMMANDS.iter().map(|c| c.to_string()).collect(),
        ["new"] => app.launchers.iter().map(|l| l.short.clone()).collect(),
        ["new", _] => return complete_folder(last, &app.tree, &app.sessions).map_line(|l| format!("{} {l}", before.join(" "))),
        ["new", _, _] => vec!["--worktree".into(), "--".into()],
        ["stats"] => ["7d", "30d", "all"].iter().map(|s| s.to_string()).collect(),
        ["set"] => crate::config::OPTIONS.iter().map(|s| s.to_string()).collect(),
        ["set", "status_line" | "confirm_delete" | "sidebar"] => vec!["true".into(), "false".into()],
        ["set", "group_by"] => vec!["project".into(), "state".into()],
        _ => vec![],
    };
    let prefix = |l: String| if before.is_empty() { l } else { format!("{} {l}", before.join(" ")) };
    pick(last, candidates).map_line(prefix)
}

impl Completion {
    fn map_line(self, f: impl FnOnce(String) -> String) -> Completion {
        match self {
            Completion::Line(l) => Completion::Line(f(l)),
            other => other,
        }
    }
}

/// `typed` completed among `candidates`: one match whole (with a space after it), several to
/// what they share.
fn pick(typed: &str, candidates: Vec<String>) -> Completion {
    let hits: Vec<String> = candidates.into_iter().filter(|c| c.starts_with(typed)).collect();
    match hits.as_slice() {
        [] => Completion::None,
        [one] => Completion::Line(format!("{one} ")),
        _ => {
            let common = common_prefix(&hits);
            if common.len() > typed.len() { Completion::Line(common) } else { Completion::Choices(hits) }
        }
    }
}

fn common_prefix(items: &[String]) -> String {
    let mut p = items[0].clone();
    for s in &items[1..] {
        while !s.starts_with(&p) {
            p.pop();
        }
    }
    p
}

/// The folder field: folders where sessions run and repos dinod knows, then the disk.
pub fn complete_folder(typed: &str, tree: &[RepoInfo], sessions: &[SessionInfo]) -> Completion {
    let full = format::expand_home(typed);
    let mut known: Vec<String> = tree
        .iter()
        .flat_map(|r| std::iter::once(r.path.clone()).chain(r.worktrees.iter().map(|w| w.path.clone())))
        .chain(sessions.iter().filter(|s| s.host.is_none()).map(|s| s.cwd.clone()))
        .collect();
    // The disk: folders in the one typed so far.
    let (dir, base) = match full.rfind('/') {
        Some(i) => (&full[..=i], &full[i + 1..]),
        None => ("./", full.as_str()),
    };
    if let Ok(entries) = std::fs::read_dir(if dir.is_empty() { "/" } else { dir }) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(base) && (base.starts_with('.') || !name.starts_with('.')) && e.path().is_dir() {
                known.push(format!("{dir}{name}/"));
            }
        }
    }
    known.sort();
    known.dedup();
    let hits: Vec<String> = known.into_iter().filter(|k| k.starts_with(&full)).collect();
    let short = |p: &str| {
        let s = format::short_path(p.trim_end_matches('/'));
        if p.ends_with('/') && s != "/" { format!("{s}/") } else { s }
    };
    match hits.as_slice() {
        [] => Completion::None,
        [one] => Completion::Line(short(one)),
        _ => {
            let common = common_prefix(&hits);
            if common.len() > full.len() { Completion::Line(short(&common)) } else { Completion::Choices(hits.iter().take(8).map(|h| short(h)).collect()) }
        }
    }
}

/// The keys, for `?`.
pub const HELP: &[(&str, &str)] = &[
    ("j k  gg G  C-d C-u", "move"),
    ("/", "filter by name, folder or agent (Esc clears)"),
    ("Enter l", "attach"),
    ("o / O", "new session here / in a new worktree"),
    ("i", "send a prompt without attaching"),
    ("I", "interrupt the agent's turn"),
    ("m", "mode, model, effort"),
    ("f / F", "fork / fork into a worktree"),
    ("R", "resume an ended session"),
    ("r", "rename"),
    ("p", "pin or unpin"),
    ("a", "archive (:archived lists them)"),
    ("x  u", "close, u undoes it"),
    ("dd", "delete"),
    ("A", "Claude accounts"),
    (":", "command line (:new :stats :setup :set …, Tab completes)"),
    ("q", "quit; agents keep running"),
    ("prefix d / q", "attached: back to the list"),
    ("prefix n / p / N", "attached: next, previous, next that needs you"),
    ("prefix m / :", "attached: picker, command line"),
    ("prefix Tab", "attached: the keyboard to the sidebar (j/k, Enter, Esc)"),
    ("prefix s", "attached: the sidebar off or on"),
    ("prefix PgUp PgDn", "attached with the sidebar: scroll the session back"),
    ("prefix prefix", "attached: send the prefix key itself"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), if c.is_ascii_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE })
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn session(id: &str, cwd: &str, activity: Option<&str>) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": format!("claude-{id}"), "agent_id": "claude", "title": null, "exited": false,
            "output_ms_ago": 60_000, "bells": 0, "requests": 0, "in_flight": 0, "input_tokens": 0,
            "output_tokens": 0, "last_model": null, "tier": null, "activity": activity, "cwd": cwd,
        }))
        .unwrap()
    }

    fn state(sessions: Vec<SessionInfo>, version: u64) -> Event {
        Event::State(Box::new(Response::State { sessions, quotas: vec![], power: None, limits: vec![], leftovers: vec![], claude_accounts: None, version: Some(version) }))
    }

    fn app() -> App {
        let mut a = App::new(Config::default(), "0.1.11".into());
        a.on_event(state(vec![session("1", "/r/a", None), session("2", "/r/a", Some("needs:Run: ls?")), session("3", "/r/b", Some("working"))], 1));
        a.outbox.clear();
        a
    }

    #[test]
    fn moves_skip_group_names_and_keep_the_selection() {
        let mut a = app();
        // a (needs you) first: 2, then 1; then b: 3.
        assert_eq!(a.selected.as_deref(), Some("2"));
        a.on_key(key('j'));
        assert_eq!(a.selected.as_deref(), Some("1"));
        a.on_key(key('j'));
        assert_eq!(a.selected.as_deref(), Some("3"));
        a.on_key(key('g'));
        a.on_key(key('g'));
        assert_eq!(a.selected.as_deref(), Some("2"));
        a.on_key(key('G'));
        assert_eq!(a.selected.as_deref(), Some("3"));
        // An update that moves it keeps it selected.
        a.on_event(state(vec![session("3", "/r/b", Some("needs:x")), session("1", "/r/a", None)], 2));
        assert_eq!(a.selected.as_deref(), Some("3"));
        assert!(a.bell, "a session started needing you");
    }

    #[test]
    fn close_then_undo() {
        let mut a = app();
        a.on_key(key('x'));
        let (tag, req) = a.outbox.pop().unwrap();
        assert!(matches!(req, Request::Close { ref id, undo_ms: 6000 } if id == "2"));
        a.on_event(Event::Reply(tag, Ok(Response::Ok)));
        assert!(a.message.as_ref().unwrap().text.contains("u undoes"));
        a.on_key(key('u'));
        assert!(matches!(a.outbox.pop().unwrap().1, Request::Reopen { ref id } if id == "2"));
        a.on_key(key('u'));
        assert!(a.outbox.is_empty());
    }

    #[test]
    fn delete_asks_with_what_goes() {
        let mut a = app();
        a.on_key(key('d'));
        a.on_key(key('d'));
        let (tag, req) = a.outbox.pop().unwrap();
        assert!(matches!(req, Request::Delete { dry_run: true, .. }));
        let d = Deletion { worktree: Some("/w/x".into()), branch: Some("dino/x".into()), uncommitted: 2, ..Deletion::default() };
        a.on_event(Event::Reply(tag, Ok(Response::Deletion { deletion: d })));
        let Line::Confirm(_, text) = &a.line else { panic!("no y/N") };
        assert_eq!(text, "delete claude-2? y/N  removes worktree /w/x, branch dino/x, 2 uncommitted files lost");
        a.on_key(key('y'));
        assert!(matches!(a.outbox.pop().unwrap().1, Request::Delete { dry_run: false, .. }));
    }

    #[test]
    fn rename_and_send_go_to_the_selected_session() {
        let mut a = app();
        a.on_key(key('r'));
        a.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in "fixer".chars() {
            a.on_key(key(c));
        }
        a.on_key(code(KeyCode::Enter));
        assert!(matches!(a.outbox.pop().unwrap().1, Request::Rename { ref id, ref name } if id == "2" && name == "fixer"));
        a.on_key(key('i'));
        for c in "go on".chars() {
            a.on_key(key(c));
        }
        a.on_key(code(KeyCode::Enter));
        assert!(matches!(a.outbox.pop().unwrap().1, Request::SendInput { ref text, submit: true, .. } if text == "go on"));
    }

    #[test]
    fn filter_narrows_and_esc_clears() {
        let mut a = app();
        a.on_key(key('/'));
        a.on_key(key('b'));
        assert_eq!(a.selected.as_deref(), Some("3"));
        assert_eq!(a.rows.len(), 2);
        a.on_key(code(KeyCode::Enter));
        a.on_key(code(KeyCode::Esc));
        assert_eq!(a.rows.len(), 5);
    }

    #[test]
    fn neighbours_go_round_in_list_order() {
        let a = app();
        assert_eq!(a.neighbour("2", Neighbour::Next).as_deref(), Some("1"));
        assert_eq!(a.neighbour("3", Neighbour::Next).as_deref(), Some("2"));
        assert_eq!(a.neighbour("2", Neighbour::Prev).as_deref(), Some("3"));
        assert_eq!(a.neighbour("1", Neighbour::NeedsYou).as_deref(), Some("2"));
        assert_eq!(a.neighbour("2", Neighbour::NeedsYou), None, "the only one that needs you is this one");
    }

    #[test]
    fn new_command_parses() {
        let mut a = app();
        a.command("new claude /tmp/x --worktree -- fix the tests");
        let Request::New { launcher, cwd, worktree, prompt, .. } = a.outbox.pop().unwrap().1 else { panic!() };
        assert_eq!((launcher.as_str(), cwd.as_deref(), worktree, prompt.as_deref()), ("claude", Some("/tmp/x"), true, Some("fix the tests")));
    }

    #[test]
    fn commands_complete() {
        let a = app();
        assert!(matches!(complete_command("ac", &a), Completion::Line(l) if l == "accounts "));
        assert!(matches!(complete_command("s", &a), Completion::Choices(c) if c == ["stats", "setup", "settings", "set"]));
        assert!(matches!(complete_command("stats 3", &a), Completion::Line(l) if l == "stats 30d "));
        assert!(matches!(complete_command("set st", &a), Completion::Line(l) if l == "set status_line "));
    }

    #[test]
    fn folders_complete_from_dinod_and_the_disk() {
        let dir = std::env::temp_dir().join(format!("dino-tui-complete-{}", std::process::id()));
        for d in ["alpha", "alps", "beta", ".hidden"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        let base = dir.display().to_string();
        // Several: as far as they agree, then the choices.
        assert!(matches!(complete_folder(&format!("{base}/al"), &[], &[]), Completion::Line(l) if l.ends_with("/alp")));
        assert!(matches!(complete_folder(&format!("{base}/alp"), &[], &[]), Completion::Choices(c) if c.len() == 2));
        // One: whole, as a folder to go on in.
        assert!(matches!(complete_folder(&format!("{base}/b"), &[], &[]), Completion::Line(l) if l.ends_with("/beta/")));
        // Hidden ones only when asked for.
        assert!(matches!(complete_folder(&format!("{base}/"), &[], &[]), Completion::Choices(c) if c.len() == 3));
        // Where sessions run counts too, though it isn't on this disk.
        let s = session("1", "/srv/somewhere/app", None);
        assert!(matches!(complete_folder("/srv/some", &[], &[s]), Completion::Line(l) if l == "/srv/somewhere/app"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_older_dinod_hides_rather_than_errs() {
        let mut a = app();
        a.on_event(Event::Reply(Tag::Stats, Ok(Response::Error { message: "bad request: unknown variant `stats`".into() })));
        let m = a.message.as_ref().unwrap();
        assert!(!m.error && m.text.contains("update dino"), "{}", m.text);
        a.on_event(Event::Rate(None));
        assert_eq!(a.rate, None);
    }

    #[test]
    fn nothing_ticks_at_idle() {
        let mut a = app();
        a.message = None;
        // Session 3 works: the spinner turns while it's on screen.
        a.height = 10;
        assert!(a.deadline().is_some());
        a.on_event(state(vec![session("1", "/r/a", None)], 3));
        assert_eq!(a.deadline(), None);
    }
}
