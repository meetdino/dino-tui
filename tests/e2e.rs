//! dino-tui end to end: a real dinod (`DINO_BIN`, built from the dino tag Cargo.toml pins) in a
//! temporary HOME and DINO_HOME, with the stand-in Claude in tests/standin, and dino-tui in a
//! pseudo-terminal whose screen a vt100 parser keeps. It creates a session, attaches, detaches
//! with the prefix, renames, closes and undoes, and reorders Claude accounts, checking each on
//! screen and in dinod; and opens the command line's screens (usage, archived, found, settings).
//!
//! Without `DINO_BIN` it says so and passes, unless `DINO_TUI_E2E=1` (CI) makes that a failure. No
//! real account, token, agent or model is involved: the stand-in makes no model calls here
//! (STANDIN_OFFLINE), and the accounts are made-up tokens in the key file.

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
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
            ("STANDIN_OFFLINE".into(), "1".into()),
            ("STANDIN_EMAIL".into(), "ada@example.com".into()),
            // The editor :settings opens: one edit, as a person would make it.
            ("EDITOR".into(), "perl -pi -e 's/close_merged = false/close_merged = true/'".into()),
        ]
    }

    fn start(dino: &Path) -> Dinod {
        // Short: a Unix socket's path has room for about a hundred bytes.
        let root = PathBuf::from(format!("/tmp/dtui-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home/.config", "home/work/app", "home/Library", "dino", "claude-config", "codex", "bin", "tmp"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // A file where launchd's agents would go: nothing the test starts can register with launchd.
        std::fs::write(root.join("home/Library/LaunchAgents"), "").unwrap();
        let standin = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/standin/claude");
        std::os::unix::fs::symlink(&standin, root.join("bin/claude")).unwrap();
        std::fs::write(
            root.join("dino/settings.toml"),
            "[machine]\nonboarded = true\ncomputer_use = false\ncheck_updates = false\nawake_while_working = false\nkeep_awake = false\n\n[machine.build_cache]\nenabled = false\n\n[policies]\nallowed_agents = [\"claude\"]\n",
        )
        .unwrap();
        let token = |w: &str| format!("sk-ant-oat01-STANDIN{w}{}", "a".repeat(60));
        std::fs::write(root.join("dino/keys"), format!("CLAUDE_ACCOUNT_2={}\nCLAUDE_ACCOUNT_3={}\n", token("work"), token("side"))).unwrap();
        let mut d = Dinod { root, child: Command::new("true").spawn().unwrap() };
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
            eprintln!("skipped: set DINO_BIN to a dino built from the pinned tag (see CONTRIBUTING.md)");
            None
        }
    }
}

#[test]
fn create_attach_detach_rename_close_undo_and_account_order() {
    let Some(dino) = dino() else { return };
    let d = Dinod::start(&dino);
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
    tui.wait_for("side second", |t| t.lines().any(|l| l.trim_start().starts_with("2  side")));
    let accounts = d.req(serde_json::json!({"type": "claude_accounts", "action": "status"}));
    let names: Vec<String> = accounts["accounts"]["accounts"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap_or("").to_string()).collect();
    assert_eq!(names[1..], ["side".to_string(), "work".to_string()], "{accounts}");
    tui.keys(b"\x1b");

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
