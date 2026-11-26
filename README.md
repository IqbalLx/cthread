# cthread

Slack-style threads for [Claude Code](https://claude.com/claude-code), in your
terminal.

You get one **main** session, plus **threads** forked from it. Each thread starts
with everything main knew, runs in a pane to the right, and never leaks back
into main. When a thread reaches a conclusion, **pull** an editable summary into
main's input box and send it if you want to.

```
┌ THREADS ───────────┐┌ main ─────────────────────────┐┏ # stripe vs paddle pricing ━━━ ✕ ┓
│ ≡ main             ││ > let's design the pricing    │┃forked from main · today 14:32   ┃
│────────────────────││                               │┃ > which is cheaper in the EU?   ┃
│▌● stripe vs paddle…││ ⏺ Sure, here's a first pass…  │┃ ⏺ For EU sellers, Paddle acts…  ┃
│ ◐ auth refactor    ││                               │┃                                 ┃
│ ! fix flaky test   ││                               │┃                                 ┃
│ ◌ ⎇ db schema      ││                               │┃                                 ┃
│ + New thread   ^q n││ > _                           │┃ > _                             ┃
└────────────────────┘└───────────────────────────────┘┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
 ^q n new · p pull · x close · h/l focus · j/k next/prev · ? help
```

cthread runs the official `claude` binary in a PTY, so it uses your existing
Claude subscription and settings. It never calls the Anthropic API and never
reads your credentials.

## Quickstart

Install [Claude Code](https://claude.com/claude-code) first, then run:

```sh
curl -fsSL https://raw.githubusercontent.com/IqbalLx/cthread/main/install.sh | bash
```

Then run `cthread` in a project folder. Use `cthread doctor` to check your setup.

The script works on macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64). It
downloads the latest release binary, checks its SHA-256 checksum, and installs it into
`~/.local/bin`. If no prebuilt binary matches your machine, it builds from
source with `cargo`. To change what it does:

```sh
# a specific version, somewhere else
curl -fsSL https://raw.githubusercontent.com/IqbalLx/cthread/main/install.sh \
  | CTHREAD_VERSION=v0.1.0 CTHREAD_INSTALL_DIR=~/bin bash
```

| Variable | Default | Meaning |
|---|---|---|
| `CTHREAD_VERSION` | latest | Release tag to install |
| `CTHREAD_INSTALL_DIR` | `~/.local/bin` | Where the binary goes |
| `CTHREAD_FROM_SOURCE` | `0` | `1` builds with `cargo` instead of downloading |

To uninstall, delete `~/.local/bin/cthread` (and, if you want, the state folder
listed under [Configuration](#configuration)).

### From source

Requires Rust 1.85+.

```sh
cargo install --git https://github.com/IqbalLx/cthread --locked
# or, from a checkout
cargo install --path .
```

## Using it

- Send at least one message in main, then press **`Ctrl-Q`** then **`n`** and type a title.
  The thread forks main *as it is right now* and opens on the right.
- In the new-thread dialog, `Tab` moves to the options below the title and
  `Space` ticks the highlighted one:
  - **Isolate in a git worktree** gives the thread its own branch
    (`cthread/<slug>`) and folder (`../<repo>-<slug>`). Uncommitted changes in
    main are not copied into the worktree; the dialog warns you when there are
    some.
  - **Read-only tools** limits the thread to Read, Grep, Glob, WebFetch and
    WebSearch, with no MCP servers. Use it for threads that should only
    explore and answer. The thread header shows `read-only`.
- Every thread runs **without background tasks, subagents or Monitor**. That
  keeps a side conversation from starting long-running work of its own. Main
  keeps all of its tools.
- **`Ctrl-Q p`** summarizes the open thread in a throwaway fork and shows the
  summary. You can edit it (`e` in the app, `E` in `$EDITOR`), regenerate it (`r`),
  or press `Enter` to paste it into main's input. cthread never presses Enter for you.
- Sidebar glyphs: `◐` working · `●` unread · `!` needs you (a permission
  prompt) · `◌` discarded (unloaded) · `⎇` worktree.
- Idle threads are unloaded after 10 minutes, or when more than 5 are live,
  and come back when you open them. Threads, order and layout survive a
  restart.
- `/clear` in main starts a fresh, empty thread list. `/resume` to an older
  main session brings back that session's threads.

## Keys

Every command is a prefix key followed by a command key, like tmux: press
**Ctrl-Q**, let go, then press the command key. While cthread waits for the
command key, the status line shows `^q …`. **Ctrl-]** works as a prefix too.
All other keys go to the focused pane unchanged, so Claude Code's own
shortcuts work. Claude Code doesn't use Ctrl-Q or Ctrl-] by default.

Both the prefix and every command key can be changed; see
[Changing keys](#changing-keys). These are the defaults:

| Keys | Action | Config name |
|---|---|---|
| `^q n` | New thread (forks main; disabled inside a thread) | `new_thread` |
| `^q p` | Pull the open/selected thread into main | `pull` |
| `^q x` | Close the thread pane | `close` |
| `^q h` / `l` | Focus left / right (sidebar, main, thread) | `focus_left` / `focus_right` |
| `^q j` / `k` | Open next / previous thread | `next_thread` / `prev_thread` |
| `^q 1`…`9` | Open thread by position | (fixed) |
| `^q m` | Focus main | `focus_main` |
| `^q r` | Rename thread | `rename` |
| `^q J` / `K` | Move thread down / up | `move_down` / `move_up` |
| `^q a` / `A` | Archive thread / show archived | `archive` / `show_archived` |
| `^q D` | Delete thread (transcripts are never deleted) | `delete` |
| `^q <` / `>` | Shrink / grow the thread pane | `shrink` / `grow` |
| `^q b` | Toggle sidebar (narrow mode: expand it) | `toggle_sidebar` |
| `^q ?` | Help (shows your current keys) | `help` |
| `^q q` | Quit | `quit` |
| `^q ^q` | Send the prefix key itself to Claude Code | (fixed) |

When the sidebar is focused, `j/k`, `Enter` and `Esc` move, open and leave, and
the command keys for rename, archive, new thread, pull, delete and move work
without the prefix. Mouse: click rows and panes, click `✕`
to close, drag the divider, scroll to see history.

Below 120 columns cthread switches to **narrow mode**: one pane at a time and a
3-column sidebar of glyphs (`^q b` expands it).

## Configuration

`~/.config/cthread/config.toml` is optional, and every key has a default.

```toml
[keys]
prefix = ["C-q", "C-]"]   # one key or a list; see "Changing keys"

[ui]
sidebar_width = 22
narrow_breakpoint = 120   # columns
mouse = true              # turn off if it fights luvus / your terminal's selection
bell_on_needs_you = false

[threads]
discard_after_minutes = 10
max_live = 5

[worktree]
root = ".."               # relative to the repo root
branch_prefix = "cthread/"

[claude]
command = "claude"        # how to start Claude Code (see below)
# pull_command = "claude" # optional: different command for pull summaries
extra_args = []           # e.g. ["--model", "opus"]

[pull]
timeout_seconds = 120
prompt = "Summarize the conclusions, decisions, changed files and open questions of this conversation in under 200 words, as bullets."
```

### Using a custom Claude Code command

`[claude] command` is the program cthread runs for every session, plus any
leading arguments. cthread adds its own flags after them (`--session-id`,
`--resume … --fork-session`, `--settings …`), so the command must accept and
pass on the usual `claude` flags. Write it as a shell-style string or as an
array:

```toml
[claude]
command = "ccr code"                                  # program + fixed args
# command = "~/bin/claude-work --profile 'my work'"   # wrapper script; quotes work
# command = ["/opt/claude tools/claude", "--foo"]     # array form, no splitting
pull_command = "claude"                               # optional, for pull summaries
```

The command is run directly, not through your shell, so `~`, `$VARS`, aliases and
shell functions are **not** expanded. Use a full path, or a small script:

```sh
#!/bin/sh
export ANTHROPIC_BASE_URL=http://localhost:8080
exec claude "$@"
```

`cthread --claude "<command>"` overrides the config for one run (it works with
`cthread doctor` too). `doctor` shows the resolved command and its `--version`.
The old `path = "…"` key still works.

### Changing keys

The prefix can be one key or a list; any of them starts a command, and the first
is shown in hints. Write keys as `C-<key>` (Ctrl), `M-<key>` (Alt/Option, which
needs your terminal to send Option as Meta) or `C-Space`.

`[keys.commands]` changes the key pressed after the prefix. Each entry is an
action's config name (see the table under [Keys](#keys)) with one key or a list.
Plain keys are case-sensitive (`"N"` is Shift-n), and `C-`/`M-` keys work too.
An empty list unbinds an action.

```toml
[keys]
prefix = "C-a"

[keys.commands]
new_thread = "t"
close = ["w", "C-w"]
quit = []            # no quit key; use the terminal to close cthread
```

The help screen and status line show your keys. Mistakes are reported as
warnings at startup, and the affected action keeps its default:

- an unknown action name;
- a key that can't be parsed;
- `1`–`9`, which always open threads by position;
- a key already bound to another action.

A key you set wins over another action's default key. If two of your own
settings use the same key, the action listed first in the table gets it.

If a prefix seems to do nothing, your terminal or keyboard layout may be
swallowing it. For example, `Ctrl-]` can't be typed on many non-US layouts, and
`Ctrl-Space` is macOS's input-source shortcut. Pick a letter key instead.

State is kept outside the repo, one JSON file per folder:
`~/Library/Application Support/cthread/` on macOS and `~/.local/state/cthread/` on Linux.
`cthread prune --older-than 30d` forgets old main sessions. It only touches
cthread's state, never transcripts or worktrees.

## luvus notes

cthread is an ordinary TUI, so it runs inside a luvus pane. Run one instance per
project folder; luvus handles switching between projects. The default prefix,
Ctrl-Q, doesn't clash with luvus's `Ctrl-Space`. If mouse capture interferes
with luvus's text selection, set `ui.mouse = false`.

## How it works

- Main runs `claude --session-id <uuid>` and later `claude --resume <id>`.
  A thread is created with `claude --resume <main> --fork-session --session-id <uuid>`,
  in a worktree folder if you asked for one.
- Every thread start (the fork, and each later `--resume`) adds
  `--disallowedTools Agent,Monitor,Workflow` and sets
  `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1`. Read-only threads also get
  `--tools Read,Grep,Glob,WebFetch,WebSearch --strict-mcp-config`. Claude Code
  doesn't restore tool limits on resume, so cthread saves the choice per thread
  and passes the flags each time.
- Each child gets `--settings <generated.json>`, which adds hooks that run
  `cthread hook`. That command forwards the hook JSON over a Unix socket.
  This is how cthread knows what each session is doing (working, stopped,
  waiting on a permission prompt) and follows `/clear` and `/resume`. Your
  own Claude settings are not modified.
- Pull runs `claude -p --resume <thread> --fork-session --no-session-persistence`,
  so the thread is not modified and no extra session is saved.

Details about Claude Code internals that cthread depends on are recorded in
[docs/spikes.md](docs/spikes.md).

## Development

```sh
cargo test                     # unit, integration (fake claude), snapshot tests
cargo test --test real_claude -- --ignored --nocapture   # against real Claude Code
CTHREAD_LOG=/tmp/cthread.log cthread                     # debug log
```

The integration tests drive cthread headless (ratatui `TestBackend`) against
`tests/fake-claude`, a stand-in binary that accepts the same flags, writes
transcripts and fires hooks. CI needs no Claude subscription.

### Releasing

1. Bump `version` in `Cargo.toml`, commit, and push.
2. Tag the commit and push the tag: `git tag v0.1.0 && git push origin v0.1.0`.

The `release` workflow then:

- runs the tests;
- builds macOS (arm64, x86_64) and static Linux musl (x86_64, arm64) binaries;
- publishes `cthread-<target>.tar.gz` and its `.sha256` on a GitHub release;
- runs the `curl | bash` install against that release.

The tag must match the `Cargo.toml` version. To re-run a release for an existing
tag, start the workflow manually (Actions → release → Run workflow).

### Manual release checklist (real Claude Code)

- [ ] Fork a thread and see main's history in it
- [ ] Parallel threads show correct glyphs; a permission prompt shows `!`
- [ ] Restore after discard
- [ ] Pull and insert (nothing is sent)
- [ ] Edits in a worktree thread stay isolated
- [ ] `/clear` and `/resume` in main
- [ ] All of the above inside a luvus pane
