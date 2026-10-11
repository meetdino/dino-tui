//! dino-tui: a minimal, keyboard-only terminal client for dinod. See docs/SPEC.md.

// The pieces the full-screen client is built from; it comes next.
#![allow(dead_code)]

mod config;
mod conn;
mod format;
mod input;
mod order;
mod wake;

use dino_core::ipc::{Request, Response};

const HELP: &str = "dino-tui: a minimal, keyboard-only terminal client for dinod.

usage: dino-tui [--version]

Talks to the dinod `dino` uses ($DINO_HOME, else ~/.config/dino); it never starts one.
Its own config: ~/.config/dino-tui/config.toml.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => return print!("{HELP}"),
        Some("-V" | "--version" | "version") => return println!("dino-tui {}", env!("CARGO_PKG_VERSION")),
        Some(a) => {
            eprintln!("dino-tui doesn't take `{}`\n\n{HELP}", format::printable(a));
            std::process::exit(2);
        }
        None => {}
    }
    if let Err(e) = run() {
        eprintln!("dino-tui: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    config::Config::load()?;
    let mut c = conn::Conn::open().map_err(|_| anyhow::anyhow!(conn::not_running()))?;
    if let Response::Version { dino, .. } = c.request(&Request::Version)? {
        println!("dinod {dino} at {}", conn::socket().display());
    }
    Ok(())
}
