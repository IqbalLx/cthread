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
│ + New thread   ^]n ││ > _                           │┃ > _                             ┃
└────────────────────┘└───────────────────────────────┘┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
 ^]n new  ^]p pull  ^]x close  ^]h/l focus  ^]j/k next/prev  ^]? help
```

cthread runs the official `claude` binary in a PTY, so it uses your existing
Claude subscription and settings. It never calls the Anthropic API and never
reads your credentials.

## Install

Requires Rust 1.85+ and Claude Code (`claude` on your `PATH`). Runs on macOS and Linux.

```sh
cargo install --path .
# or build a release binary
cargo build --release   # → target/release/cthread
```

Then run `cthread` in a project folder. Use `cthread doctor` to check your setup.

## Using it

- Send at least one message in main, then press **`^]n`** and type a title.
  The thread forks main *as it is right now* and opens on the right.
- Tick **Isolate in a git worktree** (`Tab`) to give the thread its own
  branch (`cthread/<slug>`) and folder (`../<repo>-<slug>`). Uncommitted
  changes in main are not copied into the worktree; the prompt warns you
  when there are some.
- **`^]p`** summarizes the open thread in a throwaway fork and shows the
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

Every command uses a prefix (default **Ctrl-]**, like tmux). All other keys
go to the focused pane unchanged, so Claude Code's own shortcuts work.

| Keys | Action |
|---|---|
| `^] n` | New thread (forks main; disabled inside a thread) |
| `^] p` | Pull the open/selected thread into main |
| `^] x` | Close the thread pane |
| `^] h` / `l` | Focus left / right (sidebar, main, thread) |
| `^] j` / `k` | Open next / previous thread |
| `^] 1`…`9` | Open thread by position |
| `^] m` | Focus main |
| `^] r` | Rename thread |
| `^] J` / `K` | Move thread down / up |
| `^] a` / `A` | Archive thread / show archived |
| `^] D` | Delete thread (transcripts are never deleted) |
| `^] <` / `>` | Shrink / grow the thread pane |
| `^] b` | Toggle sidebar (narrow mode: expand it) |
| `^] ?` | Help |
| `^] q` | Quit |
| `^] ^]` | Send a literal Ctrl-] |

When the sidebar is focused, the plain keys `j/k`, `Enter`, `r`, `a`, `n`, `D`,
`J/K` and `Esc` work without the prefix. Mouse: click rows and panes, click `✕`
to close, drag the divider, scroll to see history.

Below 120 columns cthread switches to **narrow mode**: one pane at a time and a
3-column sidebar of glyphs (`^]b` expands it).

## Configuration

`~/.config/cthread/config.toml` is optional, and every key has a default.

```toml
[keys]
prefix = "C-]"

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
pull_command = "claude"                               # optional, for ^]p summaries
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

State is kept outside the repo, one JSON file per folder:
`~/Library/Application Support/cthread/` on macOS and `~/.local/state/cthread/` on Linux.
`cthread prune --older-than 30d` forgets old main sessions. It only touches
cthread's state, never transcripts or worktrees.

## luvus notes

cthread is an ordinary TUI, so it runs inside a luvus pane. Run one instance per
project folder; luvus handles switching between projects. The default prefix
avoids luvus's `Ctrl-Space`. If mouse capture interferes with luvus's text
selection, set `ui.mouse = false`.

## How it works

- Main runs `claude --session-id <uuid>` and later `claude --resume <id>`.
  A thread is created with `claude --resume <main> --fork-session --session-id <uuid>`,
  in a worktree folder if you asked for one.
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

### Manual release checklist (real Claude Code)

- [ ] Fork a thread and see main's history in it
- [ ] Parallel threads show correct glyphs; a permission prompt shows `!`
- [ ] Restore after discard
- [ ] Pull and insert (nothing is sent)
- [ ] Edits in a worktree thread stay isolated
- [ ] `/clear` and `/resume` in main
- [ ] All of the above inside a luvus pane
