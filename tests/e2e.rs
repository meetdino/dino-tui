//! dino-tui end to end: a real dinod (`DINO_BIN`, built from the dino commit Cargo.toml pins) in a
//! temporary HOME and DINO_HOME, with the stand-in Claude in tests/standin, and dino-tui in a
//! pseudo-terminal whose screen a vt100 parser keeps. It creates a session, attaches, detaches
//! with the prefix, renames, closes and undoes, archives and brings back, and reorders Claude
//! accounts, checking each on screen and in dinod; and opens the command line's screens (usage,
//! archived, found, settings).
//! v2's: the first-run setup card, the sidebar beside a session and its keys, collapsing it to the
//! raw passthrough and back, a narrow terminal's status line, using an account, and usage.
//!
//! Without `DINO_BIN` it says so and passes, unless `DINO_TUI_E2E=1` (CI) makes that a failure. No
//! real account, token, agent or model is involved: the stand-in makes no model calls here
//! (STANDIN_OFFLINE), and the accounts are made-up tokens in the key file.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COLS: u16 = 100;
const ROWS: u16 = 30;

/// A dinod of the test's own, stopped (with every session) when dropped.
struct Dinod {
    root: PathBuf,
    child: Child,
    /// The stand-in Messages API, when the stand-in Claude makes calls (through dinod's proxy).
    api: Option<(Child, u16)>,
}

impl Dinod {
    fn env(&self) -> Vec<(String, String)> {
        let r = &self.root;
        let p = |s: &str| r.join(s).display().to_string();
        vec![
            ("HOME".into(), p("home")),
            ("ZDOTDIR".into(), p("home")),
            ("HISTFILE".into(), "/dev/null".into()),
            ("PATH".into(), format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", p("bin"))),
            ("SHELL".into(), "/bin/sh".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
            ("DINO_HOME".into(), p("dino")),
            ("CLAUDE_CONFIG_DIR".into(), p("claude-config")),
            ("CODEX_HOME".into(), p("codex")),
            ("XDG_CONFIG_HOME".into(), p("home/.config")),
            ("TMPDIR".into(), format!("{}/", p("tmp"))),
            // Account tokens in the key file, never a Keychain.
            ("DINO_CLAUDE_ACCOUNT_STORE".into(), "file".into()),
            ("STANDIN_EMAIL".into(), "ada@example.com".into()),
            // The editor :settings opens: one edit, as a person would make it.
            ("EDITOR".into(), "perl -pi -e 's/close_merged = false/close_merged = true/'".into()),
        ]
        .into_iter()
        .chain(match &self.api {
            // A debug dinod sends the stand-in's calls to the stand-in API, never Anthropic's.
            Some((_, port)) => vec![("DINO_ANTHROPIC_UPSTREAM".to_string(), format!("http://127.0.0.1:{port}"))],
            None => vec![("STANDIN_OFFLINE".to_string(), "1".to_string())],
        })
        .collect()
    }

    /// A dinod of its own for test `tag`; `onboarded`: this machine has been set up; `calls`: the
    /// stand-in Claude makes model calls, to a stand-in API.
    fn start(dino: &Path, tag: &str, onboarded: bool, calls: bool) -> Dinod {
        // Short: a Unix socket's path has room for about a hundred bytes.
        let root = PathBuf::from(format!("/tmp/dtui-e2e-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home/.config", "home/work/app", "home/Library", "dino", "claude-config", "codex", "bin", "tmp"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // A file where launchd's agents would go: nothing the test starts can register with launchd.
        std::fs::write(root.join("home/Library/LaunchAgents"), "").unwrap();
        let standins = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/standin");
        std::os::unix::fs::symlink(standins.join("claude"), root.join("bin/claude")).unwrap();
        // dinod probes every agent it finds (`--version`, its sign-in status) on the login shell's
        // PATH and in installers' folders (/opt/homebrew/bin…): these are found first, never a
        // real one. The login shell's PATH as the test's, after macOS's path_helper reordered it.
        for agent in ["codex", "pi"] {
            std::os::unix::fs::symlink(standins.join("plain"), root.join("bin").join(agent)).unwrap();
        }
        for profile in [".profile", ".zprofile"] {
            std::fs::write(root.join("home").join(profile), format!("export PATH='{}/bin:/usr/bin:/bin:/usr/sbin:/sbin'\n", root.display())).unwrap();
        }
        std::fs::write(
            root.join("dino/settings.toml"),
            format!("[machine]\nonboarded = {onboarded}\ncomputer_use = false\ncheck_updates = false\nawake_while_working = false\nkeep_awake = false\n\n[machine.build_cache]\nenabled = false\n\n[policies]\nallowed_agents = [\"claude\"]\n"),
        )
        .unwrap();
        let api = calls.then(|| {
            let mut child = Command::new("python3").arg("-I").arg(standins.join("api.py")).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("python3 for the stand-in API");
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.take().unwrap()), &mut line).unwrap();
            (child, line.trim().parse::<u16>().expect("the stand-in API's port"))
        });
        let token = |w: &str| format!("sk-ant-oat01-STANDIN{w}{}", "a".repeat(60));
        std::fs::write(root.join("dino/keys"), format!("CLAUDE_ACCOUNT_2={}\nCLAUDE_ACCOUNT_3={}\n", token("work"), token("side"))).unwrap();
        let mut d = Dinod { root, child: Command::new("true").spawn().unwrap(), api };
        let _ = d.child.wait();
        let mut cmd = Command::new(dino);
        cmd.arg("daemon").env_clear().envs(d.env()).current_dir(d.root.join("home")).stdin(Stdio::null());
        let log = std::fs::File::create(d.root.join("dinod.log")).unwrap();
        cmd.stdout(log.try_clone().unwrap()).stderr(log);
        // Its own process group, so nothing it starts outlives the test.
        cmd.process_group(0);
        d.child = cmd.spawn().unwrap();
        let sock = d.root.join("dino/dinod.sock");
        let t = Instant::now();
        while UnixStream::connect(&sock).is_err() {
            assert!(t.elapsed() < Duration::from_secs(20), "dinod didn't start: {}", std::fs::read_to_string(d.root.join("dinod.log")).unwrap_or_default());
            std::thread::sleep(Duration::from_millis(50));
        }
        d
    }

    fn req(&self, msg: serde_json::Value) -> serde_json::Value {
        let mut s = UnixStream::connect(self.root.join("dino/dinod.sock")).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let body = serde_json::to_vec(&msg).unwrap();
        let mut frame = vec![0u8];
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);
        s.write_all(&frame).unwrap();
        let mut head = [0u8; 5];
        s.read_exact(&mut head).unwrap();
        let mut payload = vec![0u8; u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize];
        s.read_exact(&mut payload).unwrap();
        serde_json::from_slice(&payload).unwrap()
    }

    fn sessions(&self) -> Vec<serde_json::Value> {
        self.req(serde_json::json!({"type": "state"}))["sessions"].as_array().cloned().unwrap_or_default()
    }
}

impl Drop for Dinod {
    fn drop(&mut self) {
        if let Ok(st) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.sessions())) {
            for s in st {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.req(serde_json::json!({"type": "kill", "id": s["id"]}))));
            }
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.req(serde_json::json!({"type": "shutdown"}))));
        std::thread::sleep(Duration::from_millis(300));
        unsafe { libc::killpg(self.child.id() as i32, libc::SIGKILL) };
        let _ = self.child.wait();
        if let Some((api, _)) = &mut self.api {
            let _ = api.kill();
            let _ = api.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// dino-tui in a pseudo-terminal, its screen as a vt100 parser sees it.
struct Tui {
    child: Child,
    master: std::fs::File,
    screen: Arc<Mutex<vt100::Parser>>,
}

impl Tui {
    fn start(env: Vec<(String, String)>, cwd: &Path) -> Tui {
        let (mut m, mut s) = (0, 0);
        let mut ws = libc::winsize { ws_row: ROWS, ws_col: COLS, ws_xpixel: 0, ws_ypixel: 0 };
        // macOS takes the size as `*mut`, Linux as `*const`.
        assert_eq!(unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), (&raw mut ws).cast()) }, 0);
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dino-tui"));
        cmd.env_clear().envs(env).env("TERM", "xterm-256color").current_dir(cwd);
        cmd.stdin(Stdio::from(slave.try_clone().unwrap())).stdout(Stdio::from(slave.try_clone().unwrap())).stderr(Stdio::from(slave));
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        let master = std::fs::File::from(master);
        let screen = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let (mut r, sc) = (master.try_clone().unwrap(), screen.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sc.lock().unwrap().process(&buf[..n]);
            }
        });
        Tui { child, master, screen }
    }

    fn text(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    /// The terminal resized, as a window is: the program gets SIGWINCH.
    fn resize(&mut self, cols: u16, rows: u16) {
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        self.screen.lock().unwrap().screen_mut().set_size(rows, cols);
        assert_eq!(unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) }, 0);
    }

    /// Row `y`'s background at column `x`.
    fn bg(&self, y: u16, x: u16) -> vt100::Color {
        self.screen.lock().unwrap().screen().cell(y, x).map_or(vt100::Color::Default, |c| c.bgcolor())
    }

    /// The row whose sidebar part (its first 28 columns) says `what`.
    fn side_row(&self, what: &str) -> Option<u16> {
        self.text().lines().position(|l| l.chars().take(28).collect::<String>().contains(what)).map(|y| y as u16)
    }

    fn keys(&mut self, b: &[u8]) {
        self.master.write_all(b).unwrap();
        // One read per key on the other side, as a person types.
        std::thread::sleep(Duration::from_millis(60));
    }

    fn typed(&mut self, s: &str) {
        self.master.write_all(s.as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(60));
    }

    /// Wait for the screen to say `what`.
    fn wait(&self, what: &str) -> String {
        self.wait_for(what, |t| t.contains(what))
    }

    fn wait_for(&self, what: &str, f: impl Fn(&str) -> bool) -> String {
        let t = Instant::now();
        loop {
            let text = self.text();
            if f(&text) {
                return text;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "the screen never showed {what:?}:\n{text}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(20), "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn dino() -> Option<PathBuf> {
    match std::env::var_os("DINO_BIN") {
        Some(p) => Some(PathBuf::from(p)),
        None if std::env::var("DINO_TUI_E2E").is_ok_and(|v| v == "1") => panic!("DINO_TUI_E2E=1 but no DINO_BIN"),
        None => {
            eprintln!("skipped: set DINO_BIN to a dino built from the pinned commit (see CONTRIBUTING.md)");
            None
        }
    }
}

#[test]
fn create_attach_detach_rename_close_undo_and_account_order() {
    let Some(dino) = dino() else { return };
    let d = Dinod::start(&dino, "v1", true, false);
    // Name the made-up accounts as the app would, so they read as people's.
    d.req(serde_json::json!({"type": "claude_accounts", "action": "rename", "account": 2, "value": "work"}));
    d.req(serde_json::json!({"type": "claude_accounts", "action": "rename", "account": 3, "value": "side"}));
    // dino-tui starts where a new session goes when nothing is selected.
    let mut tui = Tui::start(d.env(), &d.root.join("home/work/app"));
    tui.wait("No sessions");

    // Create: o, a prompt, Enter: started and attached.
    tui.keys(b"o");
    tui.wait("new \u{25b8} agent: claude");
    tui.typed("hello from e2e");
    tui.keys(b"\r");
    tui.wait("Stand-in Claude");
    tui.wait("hello from e2e");
    let id = d.sessions().first().map(|s| s["id"].as_str().unwrap().to_string()).expect("a session");

    // Keys reach the agent; the prefix twice sends one Ctrl-\ through; prefix d detaches.
    tui.typed("typed while attached");
    tui.wait("typed while attached");
    tui.keys(b"\x1c");
    tui.keys(b"d");
    let list = tui.wait("Enter attach");
    assert!(list.contains("claude") && !list.contains("Stand-in Claude"), "{list}");

    // Rename.
    tui.keys(b"r");
    tui.keys(b"\x15");
    tui.typed("e2e name");
    tui.keys(b"\r");
    tui.wait("e2e name");
    wait_until("dinod has the name", || d.sessions().iter().any(|s| s["id"] == id.as_str() && s["label"] == "e2e name"));

    // Close, then undo.
    tui.keys(b"x");
    tui.wait("u undoes it");
    tui.wait_for("the row gone", |t| !t.lines().any(|l| l.contains("e2e name") && !l.contains("closed")));
    assert!(d.sessions().is_empty(), "closed: gone from dinod's list");
    tui.keys(b"u");
    tui.wait("reopened e2e name");
    wait_until("dinod has it back", || d.sessions().iter().any(|s| s["id"] == id.as_str()));
    // Attached again, it's the same session, its screen kept.
    tui.keys(b"\r");
    tui.wait("hello from e2e");
    tui.keys(b"\x1c");
    tui.keys(b"q");
    tui.wait("Enter attach");

    // The command line's screens: each asks dinod and shows its answer.
    for (cmd, shows) in [("stats 7d", "Usage"), ("archived", "nothing archived"), ("found", "Found")] {
        tui.keys(b":");
        tui.typed(cmd);
        tui.keys(b"\r");
        tui.wait(shows);
        tui.wait_for("its answer", |t| !t.contains("asking dinod"));
        tui.keys(b"\x1b");
        tui.wait("Enter attach");
    }
    // :settings: the editor (here a one-line edit) changes it in dinod.
    tui.keys(b":");
    tui.typed("settings");
    tui.keys(b"\r");
    tui.wait("settings saved");
    let settings = std::fs::read_to_string(d.root.join("dino/settings.toml")).unwrap();
    assert!(settings.contains("close_merged = true"), "{settings}");

    // Account order: account 3 up above 2.
    tui.keys(b"A");
    tui.wait("side");
    tui.keys(b"j");
    tui.keys(b"j");
    tui.keys(b"K");
    tui.wait_for("side second", |t| t.lines().any(|l| l.trim_start_matches([' ', '▸']).starts_with("2  side")));
    let accounts = d.req(serde_json::json!({"type": "claude_accounts", "action": "status"}));
    let names: Vec<String> = accounts["accounts"]["accounts"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap_or("").to_string()).collect();
    assert_eq!(names[1..], ["side".to_string(), "work".to_string()], "{accounts}");
    tui.keys(b"\x1b");

    // Archived, a session is listed by the name it went by, here its agent's for the
    // conversation (Claude's /rename), not dino's "claude"; Enter brings it back.
    d.req(serde_json::json!({"type": "rename", "id": id, "name": ""}));
    tui.keys(b"\r");
    tui.wait("hello from e2e");
    // What was typed while attached is still on its line.
    tui.keys(b"\x15");
    tui.typed("/rename Bio rewrite");
    tui.keys(b"\r");
    wait_until("the agent's name for it", || d.sessions().iter().any(|s| s["id"] == id.as_str() && s["title"] == "Bio rewrite"));
    tui.keys(b"\x1c");
    tui.keys(b"q");
    tui.wait("Enter attach");
    tui.keys(b"a");
    tui.wait(":archived brings it back");
    wait_until("dinod archived it", || d.sessions().is_empty());
    tui.keys(b":");
    tui.typed("archived");
    tui.keys(b"\r");
    let archived = tui.wait_for("the archive", |t| t.contains("Enter bring it back") && !t.contains("asking dinod"));
    assert!(archived.lines().any(|l| l.trim_start_matches([' ', '\u{25b8}']).starts_with("Bio rewrite")), "{archived}");
    tui.keys(b"\r");
    tui.wait("Bio rewrite is back");
    wait_until("dinod started it again", || d.sessions().len() == 1);
    let id = d.sessions()[0]["id"].as_str().unwrap().to_string();

    // q quits; the agent keeps running in dinod.
    std::thread::sleep(Duration::from_millis(200));
    tui.keys(b"q");
    let t = Instant::now();
    let status = loop {
        if let Some(s) = tui.child.try_wait().unwrap() {
            break s;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "dino-tui didn't quit:\n{}", tui.text());
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success());
    assert!(d.sessions().iter().any(|s| s["id"] == id.as_str() && s["exited"] == false), "the agent runs on");
}

#[test]
fn first_run_setup_the_sidebar_collapsing_accounts_and_usage() {
    let Some(dino) = dino() else { return };
    // A machine not set up yet, and a stand-in Claude that answers through dinod's proxy.
    let d = Dinod::start(&dino, "v2", false, true);
    d.req(serde_json::json!({"type": "claude_accounts", "action": "rename", "account": 2, "value": "work"}));
    d.req(serde_json::json!({"type": "claude_accounts", "action": "rename", "account": 3, "value": "side"}));
    let cwd = d.root.join("home/work/app").display().to_string();
    let new = |name: &str| {
        let id = d.req(serde_json::json!({"type": "new", "launcher": "claude", "args": [], "cwd": cwd, "cols": COLS - 29, "rows": ROWS}))["id"].as_str().unwrap().to_string();
        d.req(serde_json::json!({"type": "rename", "id": id, "name": name}));
        id
    };
    let (alpha, _beta) = (new("alpha"), new("beta"));
    let config = d.root.join("home/.config/dino-tui/config.toml");
    let mut tui = Tui::start(d.env(), &d.root.join("home/work/app"));

    // First run: the setup card, from dinod's answers. Esc marks the machine set up in dinod.
    tui.wait("dino setup");
    tui.wait("Claude Code");
    tui.wait("Default agent");
    tui.keys(b"\x1b");
    tui.wait("Enter attach");
    wait_until("dinod has the machine set up (settings.toml says onboarded = true)", || std::fs::read_to_string(d.root.join("dino/settings.toml")).unwrap_or_default().contains("onboarded = true"));

    // A turn in alpha, for usage to report: asked again if the stand-in wasn't at its prompt yet.
    let t = Instant::now();
    let answered = || d.sessions().iter().any(|s| s["id"] == alpha.as_str() && s["output_tokens"].as_u64().unwrap_or(0) > 0);
    while !answered() {
        assert!(t.elapsed() < Duration::from_secs(60), "alpha's turn never reached the stand-in API: {:?}", d.sessions());
        d.req(serde_json::json!({"type": "send_input", "id": alpha, "text": "summarize the repo", "submit": true}));
        let asked = Instant::now();
        while !answered() && asked.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // Attached beside the sidebar: the session from column 29, its row selected.
    tui.keys(b"/");
    tui.typed("alpha");
    tui.keys(b"\r");
    tui.keys(b"\r");
    tui.wait("^\\ Tab list");
    tui.wait_for("the session beside the sidebar", |t| t.lines().any(|l| l.chars().nth(28) == Some('\u{2502}') && l.contains("Stand-in Claude")));
    assert_eq!(tui.side_row("alpha").map(|y| tui.bg(y, 0)), Some(vt100::Color::Idx(237)));
    tui.typed("typed beside the sidebar");
    tui.wait("typed beside the sidebar");

    // The prefix and Tab: the keyboard in the sidebar; j and Enter attach to beta.
    tui.keys(b"\x1c");
    tui.keys(b"\t");
    tui.wait("Esc back");
    tui.keys(b"j");
    tui.keys(b"\r");
    tui.wait_for("beta attached", |t| !t.contains("typed beside the sidebar") && tui.side_row("beta").is_some_and(|y| tui.bg(y, 0) == vt100::Color::Idx(237)));

    // The prefix and s: the sidebar off, the raw passthrough, remembered; again: back, redrawn.
    tui.keys(b"\x1c");
    tui.keys(b"s");
    tui.wait_for("the sidebar gone", |t| !t.contains("^\\ Tab list") && t.contains("Stand-in Claude"));
    assert!(std::fs::read_to_string(&config).unwrap().contains("sidebar = false"));
    tui.keys(b"\x1c");
    tui.keys(b"s");
    tui.wait("^\\ Tab list");
    tui.wait("Stand-in Claude");
    assert!(std::fs::read_to_string(&config).unwrap().contains("sidebar = true"));

    // Under 100 columns: the raw passthrough, and the status line when it's on.
    tui.keys(b"\x1c");
    tui.keys(b"d");
    // The filter alpha was found with goes.
    tui.keys(b"\x1b");
    tui.wait("Enter attach");
    tui.keys(b":");
    tui.typed("set status_line true");
    tui.keys(b"\r");
    tui.wait("status_line is true");
    tui.keys(b"\r");
    tui.wait("^\\ Tab list");
    tui.resize(90, ROWS);
    tui.wait_for("the status line", |t| t.contains("^\\ d list") && !t.contains("^\\ Tab list"));
    tui.resize(COLS, ROWS);
    tui.wait("^\\ Tab list");
    tui.keys(b"\x1c");
    tui.keys(b"d");
    tui.wait("Enter attach");

    // Accounts: Enter uses one. A dinod without `use` says it can't yet, once, never as an error.
    tui.keys(b"A");
    tui.wait("side");
    tui.keys(b"j");
    tui.keys(b"\r");
    let said = tui.wait_for("dinod's answer", |t| t.contains("can't switch accounts yet") || t.contains("Switched to"));
    // A dinod with `use` (the pinned one has it): account 2 is the one it goes to first now.
    if said.contains("Switched to work") {
        let accounts = d.req(serde_json::json!({"type": "claude_accounts", "action": "status"}));
        let chosen: Vec<u64> = accounts["accounts"]["accounts"].as_array().unwrap().iter().filter(|a| a["chosen"] == true).filter_map(|a| a["number"].as_u64()).collect();
        assert_eq!(chosen, [2], "{accounts}");
        tui.wait("In use: work, chosen by you");
    }
    tui.keys(b"\x1b");
    tui.wait("Enter attach");

    // Usage: alpha's turn as dinod's report has it; ◂ picks a day.
    tui.keys(b":");
    tui.typed("stats 7d");
    tui.keys(b"\r");
    tui.wait("Tokens per day");
    let t = Instant::now();
    while !tui.text().lines().any(|l| l.trim_start().starts_with("alpha")) {
        assert!(t.elapsed() < Duration::from_secs(20), "alpha never showed in usage:\n{}", tui.text());
        tui.keys(b"r");
        std::thread::sleep(Duration::from_millis(1000));
    }
    tui.keys(b"h");
    tui.wait("\u{25b2}");
    tui.keys(b"\x1b");
    tui.keys(b"\x1b");
    tui.wait("Enter attach");

    // :setup brings the card back any time.
    tui.keys(b":");
    tui.typed("setup");
    tui.keys(b"\r");
    tui.wait("dino setup");
    tui.keys(b"\x1b");
    tui.wait("Enter attach");
    tui.keys(b"q");
    let t = Instant::now();
    while tui.child.try_wait().unwrap().is_none() {
        assert!(t.elapsed() < Duration::from_secs(5), "dino-tui didn't quit:\n{}", tui.text());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn without_dinod_it_says_how_to_start_it() {
    let dir = PathBuf::from(format!("/tmp/dtui-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dino-tui")).env_clear().env("HOME", &dir).env("DINO_HOME", &dir).env("PATH", "/usr/bin:/bin").output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("dinod isn't running") && err.contains("dino daemon"), "{err}");
    assert!(!dir.join("dinod.sock").exists(), "it never starts dinod");
}
