# dino-tui: spec v1

A tiny, keyboard-only terminal client for dinod. "vim for agents": a list you move through with
j/k, Enter to be inside an agent, one key to get back. Everything real (sessions, accounts,
worktrees, usage, settings) already lives in dinod. dino-tui only shows it and sends requests.
If a feature needs logic dinod doesn't have, it doesn't belong in v1.

It also runs on Linux, where dinod and the CLI already work. So it is dino's Linux frontend too.

## Principles

- **Minimal.** Every screen fits in one sentence. No mouse, panes, splits or tabs (tmux exists
  and stays in charge), and no themes: the terminal's own 16 colours only.
- **No polling.** Live data comes from dinod's long-polls: `StateChange { seen }` and
  `OutputRate { seen }`. At idle, dino-tui uses ~0% CPU and wakes only when dinod answers
  (`StateChange` answers at least every 5 s; `OutputRate` is dinod 0.1.12's, see Deviations).
- **Wraps, doesn't emulate.** Opening a session is a raw passthrough attach, as `dino attach`
  does, so the user's terminal renders the agent. No terminal emulator inside dino-tui.
- **Degrades.** It checks `Version` on connect. When an older dinod refuses a request, the
  feature hides; it never errors.
- **Stores nothing.** dino-tui has no token, account or session state of its own. The
  only thing it keeps is ~/.config/dino-tui/config.toml (three options: the prefix key,
  `status_line`, `confirm_delete`).

## Screens

### 1. Sessions (home)

A list grouped by folder or repo (`Tree`), in the same order as `dino ls`: needs-you first, then
working, idle, ended. Pinned sessions stay at the top of their group.

Each row is one line:
`●  name        claude · opus · bypass     12.4k tok   3m`
- The status glyph: needs you (`●`, bold, and one bell when a session starts needing you while
  the list shows), working (spinner, redrawn only while working), done (`✓`), idle (`○`), ended
  (`–`, dim) or exited with an error (`✗`), as `dino ls` tells them apart.
- Name, then agent · model · mode.
- Tokens (input + output, as `dino ls --usage` counts them), then the time since its last output.

The bottom line has, right-aligned:
- **The live tok/s** (`OutputRate`), hidden when 0, like the sidebar.
- **The active Claude account's name** and its quota (its fullest window), if dinod knows it.
  When the line is tight the rate gives way first, then the account.
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
| x | close, with `u` to undo for 6 seconds | `Close` / `Reopen` |
| dd | delete (asks y/N and shows the dry run: worktree, branch) | `Delete { dry_run }` |
| A | accounts (see 4) | |
| ? | the key list | |
| : | command line (see 6) | |
| q | quit (agents keep running; dinod owns them) | |

### 2. Attached

Full screen and raw: the agent's terminal passes through as `dino attach` does. That includes
replay, resize (SIGWINCH) and focus reporting. Port `crates/dino/src/client.rs` `attach_raw`; don't
reinvent it.

Attaching starts from a clean screen and scrollback (as `dino attach --fresh` does), so what
scrolls back is the session's own replay; the list lives in the alternate screen, as vim does
around `:!`.

**The prefix key** is configurable, with Ctrl-\ as the default (as in dtach). Pressing it twice
sends a literal Ctrl-\. It's also recognised as an agent may have the terminal send it (the
kitty keyboard protocol's `CSI 92;5u`, xterm's modifyOtherKeys), never inside a paste. Any other
key after it is dropped, as tmux does. After the prefix:
- `d` or `q`: back to the list
- `n` / `p`: the next or previous session in list order, without going through the list
- `N`: jump to the next session that needs you
- `m`: the mode/model picker, as an overlay drawn on return to the list
- `:`: the command line

No status bar while attached, by default. The agent gets every row. Option: `status_line = true`
gives a one-line bar at the bottom, and the session is attached one row shorter. It's drawn
outside the session's scroll region and redrawn after output that clears the screen or resets the
region; best effort, since a program that saves the cursor across its output (DECSC) can find it
moved.

### 3. New session

A one-line form at the bottom, vim-style, not a dialog:
`new ▸ agent: claude   folder: ~/priv/dino-app-poc   worktree: no   prompt: _`
- Tab moves between fields; in the folder field it completes first when there's a completion
  (Shift-Tab goes back).
- The agent field cycles through `Launchers` with the default first (← → or Space; letters do
  nothing there, so a prompt begun in the wrong field can't change the agent). The worktree field
  toggles with Space, ← →, y or n.
- The folder field completes from `Tree` folders, session folders and paths on disk.
- Enter starts it and attaches.

Mode, model and effort come from Settings → Agents defaults unless they're set with `m` first
(`m` on the agent or worktree field; in a text field it's a letter).

### 4. Accounts

The Claude accounts from `ClaudeAccounts { status }`, in their try order. Each row shows the
account's name (email-based, as the app shows since #206), its plan, whether it's active, whether
it's at its limit and until when, and its usage windows.

dinod always tries Claude Code's own sign-in (account 1) first; `order` orders the accounts added
after it. So J/K and Enter move added accounts, and on account 1 they say so.

| key | action |
|---|---|
| J / K | move the selected added account down or up in the order (`order`) |
| Enter | make it the first added account: the one Claude Code goes on with when account 1 is spent (`order`) |
| a | add: `login` opens the browser sign-in where dinod runs and shows claude.ai's URL; for SSH/Linux boxes without a browser, open it elsewhere and press `c` to paste the code (`login_code`). `x` cancels a sign-in under way (`login_cancel`). |
| r | name it (`rename`), as the app asks right after adding one |
| dd | remove (y/N); not account 1, Claude Code's own |
| Esc | back |

While a sign-in is under way, the screen asks for its status once a second (dinod has no
long-poll for it); at no other time.

Tokens never pass through dino-tui; dinod keeps them in the Keychain, as now.

### 5. Usage

`:stats [7d|30d|all]` shows `Stats` as a small table: per agent, then per model (with the agents
that used it), the input, output, cache and total tokens. dinod's report has no cost per agent or
model (it keeps no price list), so cost shows on the total, when a route reported one. j/k to
scroll; nothing else. The live tok/s stays on the bottom line.

### 6. Command line (`:`)

These are vim-style, with Tab completion:
- `:new <agent> [folder] [--worktree] [-- prompt]`
- `:found`: agent sessions on this machine started outside dino (running ones, and the newest
  200 finished conversations; not the cloud's). Enter continues one in dino (`Adopt`).
- `:archived`: the archived sessions. Enter brings one back (`Unarchive`).
- `:stats [range]`
- `:accounts`
- `:settings`: opens dinod's settings document in `$VISUAL`, else `$EDITOR`, else vi, as TOML
  (`Settings`, then `SetSettings` on save). dinod validates it; an error reopens the editor with
  the message at the top (a TOML syntax error is caught before it reaches dinod). The document
  goes both ways as plain JSON, not through dino-core's `Settings` type, so a newer dinod's
  settings come back as they went. Quitting without saving leaves them as they are. That is the
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
  a tag: v0.1.11 now, v0.1.12 once out. Never copy the types. The two requests sent as plain JSON
  are `OutputRate` (not in v0.1.11's types; typed once pinned to v0.1.12) and the settings round
  trip (above). dino-core also brings rusqlite (the system SQLite; Linux builds need
  `libsqlite3-dev`) and, on macOS, security-framework, though dino-tui uses neither.
- **Connection:** the socket path is the one `dino` uses (DINO_HOME aware). If dinod isn't
  running, say how to start it (`dino daemon`, or that the app starts it); don't start it silently.
- **Platforms:** macOS (arm64) and Linux (x64, arm64).
- **Performance budget,** checked before every release:
  - Idle CPU is ~0 (it's asleep in two long-polls).
  - Keystroke-to-echo while attached is within 1 ms of `dino attach`.
  - `cat` of 100 MB through it is within 5% of `dino attach`.
  - The list redraws only on a State change (dinod's tag for what clients show, or an age on
    screen ticking over), a key, or a spinner tick while something is working; ratatui writes
    only the cells that changed.

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
  prefix, rename, close and undo, account order. It runs dino-tui in a pseudo-terminal read by a
  vt100 parser (no tmux needed in CI); the stand-in accounts are made-up tokens in dinod's key
  file (`DINO_CLAUDE_ACCOUNT_STORE=file`), never a Keychain.

## Deviations in v1

Where v1 differs from the spec as first written, and why. Each is applied above.

- **The live tok/s** needs `OutputRate`, which dinod has from 0.1.12. Pinned to v0.1.11, dino-tui
  asks for it as plain JSON and hides the rate when dinod refuses it, as it does from 0.1.11.
- **Account order:** dinod tries Claude Code's own sign-in first, always; `order` orders only the
  added accounts. "Switch to it" makes an account the first added one.
- **Accounts keys added:** `c` pastes the sign-in code (`login_code`), `x` cancels a sign-in, `r`
  names an account (`rename`), all requests dinod has.
- **Usage:** no cost per agent or model (dinod's report has none); agents and models are separate
  sections of the table.
- **Settings** travel as plain JSON, not dino-core's type, so nothing a newer dinod adds is lost.
- **New-session form keys:** Tab completes in the folder field before moving on; letters don't
  change the agent; `m` opens the picker only from the agent or worktree field.
- **Glyphs:** done and exited are shown apart from idle and ended, as `dino ls` shows them.
- **Attaching** clears the terminal's screen and scrollback first, as `dino attach --fresh` does.
- **The prefix** is also recognised in the kitty keyboard protocol's and modifyOtherKeys' forms,
  since agents turn those on; other keys after it are dropped.
- **Tests** drive dino-tui in a pty with a vt100 parser instead of tmux, and use the key file for
  stand-in accounts instead of a test Keychain.
