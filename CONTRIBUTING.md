# Contributing to dino-tui

dino-tui is a small terminal client for dinod, the background service of
[dino](https://github.com/meetdino/dino). Everything real lives in dinod; dino-tui shows it and
sends requests. [docs/SPEC.md](docs/SPEC.md) is the contract: a change that needs logic dinod
doesn't have belongs in dino first.

## Build and test

Rust through [rustup](https://rustup.rs), which picks the version CI uses from
`rust-toolchain.toml`.

```sh
cargo build --release        # target/release/dino-tui
cargo test                   # unit and snapshot tests
cargo fmt --all              # rustfmt.toml holds the style; CI fails code it would change
cargo clippy --all-targets -- -D warnings
```

The end-to-end test (`tests/e2e.rs`) drives dino-tui in a pseudo-terminal against a real dinod
with stand-in agents, in a temporary `HOME` and `DINO_HOME`. It needs a `dino` binary from the
dino tag `Cargo.toml` pins, named by `DINO_BIN`:

```sh
git clone --branch v0.1.11 https://github.com/meetdino/dino ../dino
(cd ../dino && cargo build -p dino)
DINO_BIN=$PWD/../dino/target/debug/dino cargo test --test e2e
```

Without `DINO_BIN` it says so and passes; CI sets `DINO_TUI_E2E=1`, which makes a missing
`DINO_BIN` a failure. It uses a debug build of dino on purpose: only a debug dinod lets the
stand-in Claude's model calls go to a stand-in API (`DINO_ANTHROPIC_UPSTREAM`) instead of
Anthropic's. No test uses a real account, token or agent.

To try dino-tui by hand without touching the dino you use, give it a dinod of its own:

```sh
export DINO_HOME=/tmp/dino-tui-dev HOME=/tmp/dino-tui-dev/home ZDOTDIR=/tmp/dino-tui-dev/home HISTFILE=/dev/null
mkdir -p "$HOME" && dino daemon &
cargo run
```

## Commits and pull requests

Every change reaches main through a pull request, squash-merged: one commit on main per pull
request, with a title that says what changed for someone using dino-tui, in plain words.

### Sign off your commits (DCO)

dino-tui uses the [Developer Certificate of Origin](https://developercertificate.org): by signing
off, you certify that you wrote the change or otherwise have the right to submit it under the
project's license (MIT). Add the sign-off with `-s`:

```sh
git commit -s -m "The accounts screen shows when a spent account resets"
```

which adds a line like `Signed-off-by: Your Name <you@example.com>`, matching the commit's author.
A check on every pull request looks for it. Forgot? `git commit --amend -s` for the last commit, or
`git rebase --signoff main` for all of them, then force-push your branch.
