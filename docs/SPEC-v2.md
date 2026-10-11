# dino-tui v2: dino's UI on Linux

v1 (SPEC.md) was a small extra for Mac users: a list, raw attach, and a few screens. On 2026-10-11 the
founder decided that dino-tui ships as **the only UI for dino on Linux**. A Linux user gets everything
the Mac app's window gives them that makes sense without a Mac: the sidebar beside the agent, first-run
setup, Claude accounts with real switching, and usage. This spec covers what changes from v1.

Status: built (sections 1 to 4) on the `v2` branch. The spec as the founder approved it on
2026-10-11 follows (only this paragraph and the previews' first line are new); where the build
differs, and what it measured, is under
[Deviations in v2](#deviations-in-v2) at the end. Section 5 is dino's release tooling, not
dino-tui's.

## What bends from v1

| v1 principle | v2 |
|---|---|
| **Wraps, doesn't emulate.** Attach is a raw passthrough; the user's terminal draws the agent. | **Bends.** A sidebar that stays beside a running agent means dino-tui draws the agent's screen itself, in the columns right of the sidebar. Raw passthrough stays, one key away (`Ctrl-\ s`), and is what a narrow terminal gets. |
| **Minimal.** Every screen fits in one sentence. | **Bends where the app has the feature.** The measure is parity with the Mac app's window, not v1's size. Still no mouse-first UI, no themes, no splits of several agents (tmux's job). |
| No polling; degrades; stores nothing. | **Unchanged.** Everything new comes from requests dinod already answers, or from the dinod changes listed below. |

## 1. The sidebar

What the app's sidebar shows (app/Sources/Dino/SidebarRows.swift, DinoApp.swift `UsagePanel`), and
what dino-tui shows:

- **Sessions,** grouped by project (repo, worktree, folder, SSH host) or by state (Needs you, Working,
  Done, Idle), as the app's View → Group Sidebar By. Pinned first in each group.
- **A row** is a status mark and the name: `!` Needs you (with what it asks on the next line), a
  green dot for Working (a ring while it waits on subagents), a blue dot and a bold name for Done, a
  red dot for an agent error, nothing and a muted name for Idle. A red `✕` when its PR's checks fail.
  That's the app's minimal row; the rest (agent, model, branch, tokens) goes in a one-line hint at
  the sidebar's foot for the selected row, the TUI's version of the tooltip.
- **The usage panel** at the sidebar's foot, as in the app: one collapsed line with the fullest
  window ("Claude 5h 42% used"), which opens to the account in use with its 5h and 7d bars, each
  other account as `ready` or `back at 14:05`, the open sessions' tokens, free models' tokens, and
  the live tok/s (hidden at 0).
- **Data:** all of it comes from the `State { seen }` long-poll (sessions, `quotas`,
  `claude_accounts`) and `OutputRate { seen }`. No new request.

**Keys while attached** (prefix `Ctrl-\`, as v1):
- `Ctrl-\ Tab` puts the keyboard in the sidebar; j/k, Enter switches to the session there, Esc goes back to the agent.
- `Ctrl-\ n` / `p` / `N`: next, previous, next that needs you (v1's).
- `Ctrl-\ s`: sidebar off, back to v1's raw passthrough for this attach; again to bring it back.
- `Ctrl-\ d`: back to the full-screen list (v1's home stays).

**Collapsing.** `Ctrl-\ s` collapses the sidebar. The agent gets the whole terminal, through v1's raw
passthrough at full speed; `Ctrl-\ s` again brings the sidebar back, redrawn from the attach's replay.
dino-tui remembers the choice (`sidebar = true|false` in its config) for the next attach and the next
start. Collapsed, nothing else is drawn, unless `status_line = true` (v1's option) adds the one-line
status of `05-narrow.png`. The usage panel at the sidebar's foot collapses on its own to one line
(the fullest window), as in the app; Enter on it opens it.

**Width.** Sidebar 28 columns. Under 100 columns the sidebar is off and the attach is v1's raw
passthrough, with an optional 1-line status at the bottom (v1's `status_line`).

**How the agent is drawn.** `Attach` streams the session's bytes; dino-tui feeds them into a terminal
parser and draws its grid in the columns beside the sidebar, resizing the session to that width.
Three ways, and a recommendation:
1. **alacritty_terminal** (recommended): the parser dinod already uses for replays (crates/dino-term),
   so a session looks the same in dinod's replay and in dino-tui. Use `alacritty_terminal` directly,
   not through dino-term's `Pane`, which brings portable-pty and spawning along and has no public
   walk over the grid's cells.
2. `vt100` + `tui-term` (a ratatui widget): less code; v1's e2e test already uses vt100. A second
   parser with its own quirks.
3. Inside tmux only: the sidebar is a narrow tmux pane running `dino-tui --sidebar`, the agent a raw
   `dino attach` in the pane beside it. No emulation at all, but only for tmux users. Could come
   later as an option; not the default.

**Not verified:** how well either parser handles Claude Code's and Codex's screens (synchronized
output, the kitty keyboard protocol, OSC 8 links, images). The prototype has to check that with the
real agents before the sidebar ships.

## 2. Setup (first run)

The app's Welcome (app/Sources/Dino/Welcome.swift) as a full-screen card on first run, and `:setup`
any time later. Each row can be left alone; Esc or `q` closes it and marks the machine onboarded, as
the app does.

| Row | What it does | dinod request |
|---|---|---|
| **Agents** | Each agent dino knows: installed or not, signed in or not. `i` installs, `s` signs in. Both run the agent's own command in a new shell session that dino-tui attaches to, so the user answers it there; on exit, back to the card with the outcome. | `AgentSetup`, `AgentAction { id, install / sign_in }` |
| **Claude accounts** | The accounts, and `a` to add one: the code sign-in (`login_code`) by default on a machine without a browser, the browser one when `$DISPLAY`/`$WAYLAND_DISPLAY` is set. | `ClaudeAccounts` |
| **Local model servers** | Ollama, LM Studio and the like found running on this machine. | `Providers` |
| **Sync** | Sign in with GitHub to sync settings across machines: the device-code sign-in (`login_device`) shows a code and the URL. | `Sync` |
| **Default agent** | What `o` / `:new` starts. | `Launchers`, `Settings` / `SetSettings` |
| **Found sessions** | Agents already running in other terminals and past conversations; Enter continues one in dino. | `Found`, `Adopt` |
| **Start at login** (Linux only) | Whether dinod runs as a systemd user service, so automations run with nobody logged in. `Enter` runs `dino service install`. | none: it's the CLI's `dino service` (crates/dino/src/systemd.rs) |

Left out, macOS-only: computer use, macOS privacy permissions, the default-terminal choice, and
`Power` (keeping agents running with the lid closed). Whether the machine has been set up is dinod's
`settings.machine.onboarded` (crates/dino-core/src/settings.rs), the same flag the app's Welcome reads
and sets, through `Settings` / `SetSettings`. So a machine set up once isn't asked again, from either UI.

## 3. Accounts, and real switching

**Today** (crates/dino-daemon/src/claude_accounts.rs, crates/dino-proxy/src/fallback.rs):
- Account 1 is Claude Code's own sign-in. dino doesn't hold its token: Claude Code's calls carry it
  and dinod's proxy passes them through.
- Accounts 2+ are tokens dinod holds: the Keychain on macOS, dino's mode-600 key file on Linux
  (`CLAUDE_ACCOUNT_<n>`, dino_core::account_store). The proxy tries them in order, only when the ones
  before are spent, and moves a session back when the first one's limit resets.
- `order` reorders 2+ only. So "switch to account 3" isn't possible: while account 1 has quota, it
  answers.

**The dinod change (spec, not landed):** a chosen account.
- `ClaudeAccounts { action: "use", account: N }` makes N the one Claude Code's calls go to, from each
  session's next turn, even while earlier ones have quota. The proxy signs with N's token instead of
  passing account 1's through. `use` with account 1 (or `None`) goes back to today's behaviour.
- When N is spent, the fallback order applies from N on, then wraps to the ones before it.
- dinod keeps the choice in its settings, so it survives a restart and the app and dino-tui show the
  same thing. `ClaudeAccountInfo` gets `chosen: bool`.
- Account 1 can't be chosen away from in one case: dino has no token for it, so if the user removes
  all others, it's account 1 again.
- The Mac app's Accounts settings get the same "Use this account" action; the sidebar's "in use"
  line already follows `answering`.
- **Expected, not verified:** a switch moves the conversation to another organization's prompt
  cache, so the first call after a switch reads the whole context uncached. The UI should say so
  once ("the next answer is slower and uses more of the new account's limit").

**dino-tui's Accounts screen** (v1's, plus): `Enter` uses the account (the `use` action), `J`/`K`
reorder the fallbacks, `a` adds, `r` renames, `dd` removes. Each row shows the account's name, its
plan, `in use` / `ready` / `back at 14:05`, and its 5h and 7d bars when Anthropic has reported them.

**Linux facts:**
- Verified in code: on Linux, accounts 2+ live in dino's key file, never Secret Service.
- Verified in code: `login_code` exists for sign-in without a browser.
- Not verified on a Linux machine: the whole sign-in round trip (`login` and `login_code`) and the
  proxy passing account 1 through. Claude Code on Linux keeps its own sign-in in a file under
  `~/.claude`, not a keychain; dinod doesn't read it, so this is expected to work, but nobody has run
  it. The Linux CI job runs dinod's tests, which use stand-in accounts.

## 4. Usage

- **The sidebar's usage panel** (section 1) is the always-on part, as in the app.
- **`:stats [7d|30d|all]`** becomes the app's Usage window (app/Sources/Dino/Stats.swift) in text:
  - the token total, with in / out / cache, and the split between the conversation, subagents and
    side requests as one bar
  - tokens per day as stacked bars of block characters, coloured by model (the top 4, then "other");
    ◂ ▸ picks a day and shows its numbers beside the chart
  - the activity heatmap: weeks as columns, weekdays as rows, four shades of green
  - per route: requests, errors, limit hits, and cost
  - the sessions that used the most
- **Data:** `Stats { range }`, as the app. No change to dinod.
- **Left out:** hover cards. A selected bar's numbers show on the line below the chart instead.

## 5. Getting dino onto Linux

What's there:
- dinod and the `dino` CLI build and pass their tests on Linux x64 and arm64 in CI.
- `dino` starts dinod on first use (it forks `dino daemon`), and `dino service install` makes it a
  systemd user service.
- dino-tui's release workflow builds linux-x64 and linux-arm64 tarballs on a v* tag.

What's missing:
- **Linux release assets for dino.** 0.1.12's release has only macOS assets.
  - Add `dino-<v>-linux-x64.tar.gz` and `-linux-arm64.tar.gz` (dino + dinod), built in the release
    workflow on ubuntu-22.04, as dino-tui's are, so glibc 2.35 and up runs them.
  - The CLI's self-update (`dino update`) has to pick the Linux tarball.
  - Signing: there's no Developer ID on Linux. The tarballs get the same EdDSA signature the update
    feed uses, and SHA256SUMS.
- **The installer.** scripts/install.sh fails on anything but Darwin. On Linux it should:
  1. download dino's and dino-tui's tarballs for the machine
  2. check them against SHA256SUMS
  3. install them to `~/.local/bin`
  4. ask whether to run `dino service install`
  One command: `curl -fsSL https://dino.dev/install.sh | sh`, as on the Mac.
- **dino-tui from dino.** `dino tui` runs dino-tui, and installs it first if it's missing. On Linux,
  `dino` with no arguments in an interactive terminal opens dino-tui, as opening the app does on the
  Mac. (On the Mac, `dino` keeps today's behaviour.)
- **Not verified:** which distributions this has been run on. None: no Linux machine or VM was used.
  The minimum test before shipping is the installer, dinod, a stand-in session, and dino-tui on
  Ubuntu 22.04 and 24.04 and one non-Debian (Fedora), x64 and arm64.

## dinod changes, all in dinod, not dino-tui

1. `ClaudeAccounts { action: "use" }` and `chosen` (section 3). Changes the Mac app too.
2. Linux release assets and the CLI's Linux self-update (section 5). That's release tooling and the
   CLI, not dinod itself.

## Performance budget for v2

Not measured. Before the sidebar ships, with the prototype, against v1's raw passthrough and
`dino attach`, on the same isolated test dinod (v1's harness, ~/priv/dino-ops/tui/harness/perf.py):

| Item | Budget |
|---|---|
| Idle CPU, attached with the sidebar | ~0, as v1: asleep in its long-polls and the attach read |
| Keystroke to echo, sidebar on | within 1 ms of `dino attach` (v1's budget) |
| 100 MB `cat`, sidebar on | to be set from the prototype's number. v1's "within 5% of `dino attach`" is likely out of reach once every byte goes through a parser; the honest budget is what the parser costs, measured, plus a frame-rate cap so a flood doesn't redraw faster than 60 fps |
| Sidebar off (`Ctrl-\ s`) | v1's budget exactly: it's v1's code path |

## Decisions

The founder approved the spec on 2026-10-11 with the recommendations below.

1. **How the agent is drawn:** alacritty_terminal (recommended) / vt100 + tui-term / tmux-only.
2. **Sidebar on by default:** yes when the terminal is 100+ columns (recommended); or off, with
   `Ctrl-\ s` to turn it on.
3. **`dino` with no arguments on Linux opens dino-tui:** yes (recommended), so there's one command to
   remember; or keep `dino` as the CLI's help and use `dino tui`.
4. **Real switching in dinod:** the `use` action above (recommended), or keep fallback-only order.
   This one also changes the Mac app.
5. **Linux distribution order:** the curl installer first (recommended), then Homebrew on Linux;
   apt/rpm packages later, if people ask.

## Not in v2

The PR flow, Changes/diff, previews and dev servers, automations, providers and plans management,
the build cache, computer use, and several agents side by side. dinod has most of them; each can be a
key or a command later, once v2 feels right.

## Previews

Mockups, made before v2 was built. They're in ~/priv/dino-ops/tui/v2/preview/, drawn
with v1's renderer (harness/ansi2png.py) at the sizes given.

1. `01-sidebar.png`: attached to a Claude session, sidebar on, usage panel open (140 x 36).
2. `02-setup.png`: the first-run setup card (110 x 30).
3. `03-accounts.png`: accounts with switching (110 x 26).
4. `04-usage.png`: `:stats 30d` (110 x 34).
5. `05-narrow.png`: a 90-column terminal, sidebar off, the 1-line status (90 x 24).

## Deviations in v2

Where the build differs from the spec above, and why.

- **The pin.** dino-core is pinned to dino's commit 59fd8fb, not a tag: it has 0.1.12, the input
  rate (#216) and choosing the account (#217), which no release has yet. CI builds its dinod
  there. Everything new degrades on an older dinod: the rate hides (0.1.11), the input rate shows
  as plain `tok/s` (0.1.12), and Enter on an account says this dinod can't switch yet, once, and
  the key leaves the screen's header.
- **The live rate** is the app's since #216: `2.0k out · 16.4k in tok/s`, whichever isn't 0. The
  input part is waited on only once dinod has said it (its answers carry the field).
- **The sidebar** is 28 columns and a separator; the session starts at column 29. The selected
  row's hint is two lines (as the mockup): agent · model · mode, then the branch, tokens and when
  it last wrote.
- **Grouping** by project or state is dino-tui's own option, `group_by` (`:set group_by state`):
  the app's View menu has no TUI counterpart. By project, plain folders come after repos (dinod's
  tree lists both; a folder has no branch). Rows keep their place as their status changes, as the
  app's sidebar; `n`/`p`/`N` still go in the list's order (v1's).
- **Keys added** with the sidebar:
  - the prefix and Page Up / Page Down scroll the session's history: the terminal's own scrollback
    can't reach a session drawn beside the sidebar
  - in the sidebar: `g`/`G`, `u` (the usage panel open or folded), `:` (the command line)
  - the prefix and Tab from the raw passthrough (100+ columns) turns the sidebar on with the
    keyboard in it.
- **What the session asks of its terminal**, beside the sidebar: modes that change what keys and
  the mouse send (cursor keys, keypad, bracketed paste, mouse reporting, the kitty keyboard
  protocol, modifyOtherKeys) and the cursor's shape go on to the real terminal; mouse reports come
  back moved past the sidebar. Queries are answered by dino-tui's screen (the cursor's position is
  the session's, not the terminal's), as dinod's own emulator answers them when no client is
  attached; colour queries go on to the real terminal, whose answer comes back as input. The
  wheel sends arrows (alternate scroll) only while the session's own alternate screen is up; the
  terminal's setting is saved and put back (XTSAVE/XTRESTORE, where the terminal has them).
- **Drawing:** a flood is drawn at most 60 times a second; up to 4 KB of output since the last
  frame (an echo, a prompt) is drawn at once, so typing isn't held to the frame rate. A
  synchronized update (`CSI ? 2026`) is drawn when it ends.
- **Not drawn as the terminal would** (not verified with an agent that uses them): OSC 8 links
  (the text shows, not the link), images (kitty graphics, sixel), underline styles and colours
  (one plain underline), blinking. Device attributes answer as alacritty_terminal's.
- **Setup:** agents not installed fold into one row (`N more`, Enter lists them), as the app's
  "Other agents". Model servers show name, version and port: `Providers` doesn't count models.
  "Start at login" is Linux's only. "No browser here" is Linux without `$DISPLAY` or
  `$WAYLAND_DISPLAY`; there, once claude.ai's link shows, the code line opens by itself. The
  default agent changes at once (the app's picker does too). An install or sign-in shell stays in
  the list when it's done, as the app leaves it.
- **Accounts:** a spent account says `back at 16:30` (or a day and time further off). The "in use"
  line follows dinod's `chosen`: account 1 is dinod's default, so it reads as Claude Code's own
  sign-in, never "chosen by you". The note after switching is said once, until the screen closes.
- **Usage:** the chart names the top four models and puts the rest in `other` (dinod's report names
  five); with more days than columns it shows the last ones. The heatmap is the last 13 weeks of
  the report's year, shaded by its quartiles as the app does. `7` / `3` / `a` change the range,
  `r` asks again. Top sessions are named as the list names them, else by their folder.
- **The status line** (narrow, `status_line = true`) is the mockup's: the session's mark and name,
  what needs you elsewhere, the rate, the account; the way back on the right. With several Claude
  accounts the account shows only its own windows, never Claude's quota as a whole.
- **Fixed on the way, v1's too:** after a size change, the next key waited for the one after it
  (crossterm answered its own resize event and lost the terminal's readiness).
- **Tests** put stand-ins named `pi` and `codex` on the test's `PATH` and its login shell's: dinod
  probes every agent it finds (`--version`), and looks in `/opt/homebrew/bin` and
  `/usr/local/bin` too.
