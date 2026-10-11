# dino-tui: spec v1

A tiny, keyboard-only terminal client for dinod. "vim for agents": a list you move through with
j/k, Enter to be inside an agent, one key to get back. Everything real (sessions, accounts,
worktrees, usage, settings) already lives in dinod. dino-tui only shows it and sends requests.
If a feature needs logic dinod doesn't have, it doesn't belong in v1.

It also runs on Linux, where dinod and the CLI already work. So it is dino's Linux frontend too.

## Principles

- **Minimal.** Every screen fits in one sentence. No mouse, panes, splits or tabs (tmux exists
  and stays in charge), and no themes: the terminal's own 16 colours only.
- **No polling.** Live data comes from dinod's long-polls: `State { seen }` and
  `OutputRate { seen }`. At idle, dino-tui uses ~0% CPU and wakes only when dinod answers.
- **Wraps, doesn't emulate.** Opening a session is a raw passthrough attach, as `dino attach`
  does, so the user's terminal renders the agent. No terminal emulator inside dino-tui.
- **Degrades.** It checks `Version` on connect. When an older dinod refuses a request, the
  feature hides; it never errors.
- **Stores nothing.** dino-tui has no token, account or session state of its own. The
  only thing it keeps is ~/.config/dino-tui/config.toml (keys, three options).

## Screens

### 1. Sessions (home)

A list grouped by folder or repo (`Tree`), in the same order as `dino ls`: needs-you first, then
working, idle, ended. Pinned sessions stay at the top of their group.

Each row is one line:
`●  name        claude · opus · bypass     12.4k tok   3m`
- The status glyph: needs you (bold or bell), working (spinner, redrawn only while working),
  idle, or ended (dim).
- Name, then agent · model · mode.
- Tokens, then the time since its last output.

The bottom line has, right-aligned:
- **The live tok/s** (`OutputRate`), hidden when 0, like the sidebar.
- **The active Claude account's name** and its quota, if dinod knows it.
- **A count of what needs you.**

| key | action | dinod request |
|---|---|---|
| j / k, gg / G, Ctrl-d / Ctrl-u | move | |
| / | filter by name, folder or agent (Esc clears) | |
| Enter or l | attach | `Attach` |
| o | new session in the selected row's folder (see 3) | `New` |
| O | new session in a new worktree | `New { worktree }` |
| i | send a prompt without attaching (one-line input, Enter submits) | `SendInput { submit }` |
| I | interrupt the agent's turn | `Interrupt` |
| m | mode / model / effort picker | `SetControls` |
| f | fork (F: fork into a worktree) | `Fork` |
| R | resume an ended session | `Resume` |
| r | rename | `Rename` |
| p | pin / unpin | `Pin` |
| a | archive (a list of archived sessions is under `:archived`) | `Archive` / `Unarchive` |
| x | close, with `u` to undo for a few seconds | `Close` / `Reopen` |
| dd | delete (asks y/N and shows the dry run: worktree, branch) | `Delete { dry_run }` |
| A | accounts (see 4) | |
| ? | the key list | |
| : | command line (see 6) | |
| q | quit (agents keep running; dinod owns them) | |

### 2. Attached

Full screen and raw: the agent's terminal passes through as `dino attach` does. That includes
replay, resize (SIGWINCH) and focus reporting. Port `crates/dino/src/client.rs` `attach_raw`; don't
reinvent it.

**The prefix key** is configurable, with Ctrl-\ as the default (as in dtach). Pressing it twice
sends a literal Ctrl-\. After the prefix:
- `d` or `q`: back to the list
- `n` / `p`: the next or previous session in list order, without going through the list
- `N`: jump to the next session that needs you
- `m`: the mode/model picker, as an overlay drawn on return to the list
- `:`: the command line

No status bar while attached, by default. The agent gets every row. Option: `status_line = true`
gives a one-line bar at the bottom, and the session is attached one row shorter.

### 3. New session

A one-line form at the bottom, vim-style, not a dialog:
`new ▸ agent: claude   folder: ~/priv/dino-app-poc   worktree: no   prompt: _`
- Tab moves between fields.
- The agent field cycles through `Launchers` with the default first.
- The folder field completes from `Tree` folders and paths.
- Enter starts it and attaches.

Mode, model and effort come from Settings → Agents defaults unless they're set with `m` first.

### 4. Accounts

The Claude accounts from `ClaudeAccounts { status }`, in their try order. Each row shows the
account's name (email-based, as the app shows since #206), whether it's active, and whether it's
at its limit and until when.

| key | action |
|---|---|
| J / K | move the selected account down or up in the order (`order`) |
| Enter | make it first: "switch to it" (`order`) |
| a | add: `login` opens the browser sign-in. The code variant (`login_code`) serves SSH/Linux boxes without a browser: show the URL, read the code. |
| dd | remove (y/N) |
| Esc | back |

Tokens never pass through dino-tui; dinod keeps them in the Keychain, as now.

### 5. Usage

`:stats [7d|30d|all]` shows `Stats` as a small table: per agent and model, the input, output and
cache tokens and cost. j/k to scroll; nothing else. The live tok/s stays on the bottom line.

### 6. Command line (`:`)

These are vim-style, with Tab completion:
- `:new <agent> [folder] [--worktree] [-- prompt]`
- `:found`: agent sessions on this machine started outside dino. Enter continues one in dino
  (`Adopt`).
- `:archived`: the archived sessions. Enter brings one back (`Unarchive`).
- `:stats [range]`
- `:accounts`
- `:settings`: opens dinod's settings document in `$EDITOR` (`Settings`, then `SetSettings` on
  save). dinod validates it; an error reopens the editor with the message at the top. That is the
  whole settings UI: minimal, and every setting is covered.
- `:set <tui option> <value>`: changes dino-tui's own config (prefix, status_line, confirm_delete).
- `:q`

## Not in v1

- The PR flow (Create PR, fix checks, merge, review), Changes/diff, previews and dev servers,
  automations, providers and plans, sync, power, computer use, the build cache. dinod has all of
  them; v2 can add a key or command each, once v1 feels right.
- Any embedded terminal, split view, or "watch several agents at once". That is tmux's job.

## Tech

- **Language and libraries:** Rust (edition 2024, toolchain 1.99 like dino), ratatui + crossterm.
  The full-screen TUI removed in meetdino/dino d934b07 (its parent's crates/dino/src/main.rs) is a
  source to borrow from, not to restore.
- **Protocol:** use `dino-core`'s `ipc` types through a git dependency on meetdino/dino, pinned to
  a tag (v0.1.12 once out). Never copy the types.
- **Connection:** the socket path is the one `dino` uses (DINO_HOME aware). If dinod isn't
  running, say how to start it (`dino daemon`, or that the app starts it); don't start it silently.
- **Platforms:** macOS (arm64) and Linux (x64, arm64).
- **Performance budget,** checked before every release:
  - Idle CPU is ~0 (it's asleep in two long-polls).
  - Keystroke-to-echo while attached is within 1 ms of `dino attach`.
  - `cat` of 100 MB through it is within 5% of `dino attach`.
  - The list redraws only on a State change, a key, or a spinner tick while something is working.

## Repo and shipping

- **Repo:** meetdino/dino-tui, **private** until the founder has used it, then public (MIT, like
  dino). The same contribution rules as dino: DCO, only the Signed-off-by trailer, and the author
  is asdf9384.
- **CI:** fmt, clippy -D warnings, and tests on macOS and Linux. Tests run against a real dinod
  from the pinned dino tag, in a temp HOME and DINO_HOME, with stand-in agents (as in dinod's own
  tests). No real accounts.
- **Release:** v0.1.0 as tarballs for darwin-arm64, linux-x64 and linux-arm64 on its GitHub
  release. Homebrew and `dino tui` only after the founder says so.
- **Tests:** snapshot tests of the list, accounts and new-session line, rendered with ratatui's
  TestBackend. An end-to-end test drives a dinod with stand-ins: create, attach, detach with the
  prefix, rename, close and undo, account order.
