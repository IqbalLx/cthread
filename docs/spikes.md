# M0 spikes: findings

Run against Claude Code **2.1.331** on macOS (arm64), 2026-11-24.

## S1: fork flags

`claude --resume <main> --fork-session --session-id <uuid>` is accepted, and the
fork uses exactly that UUID: the SessionStart hook reported it, and the
transcript file was `<uuid>.jsonl`. cthread pre-assigns thread IDs this way,
but it still treats the SessionStart hook as the source of truth
(`pending_session_id` until it is confirmed).

## S2: hooks via `--settings`

A `--settings <file>` with a `hooks` block added hooks that fired for
SessionStart, UserPromptSubmit, Stop and SessionEnd, in both interactive and
`-p` runs. We did not separately confirm that the user's own hooks still run
alongside ours. The docs say settings sources merge. Re-check this when
upgrading Claude Code.

Real payloads (anonymized) are in `tests/fixtures/hooks/`. Findings beyond the PRD:

- A forked session's SessionStart has **`source: "fork"`** (plus
  `context_tokens`, `estimated_cache_write_usd`, …). cthread treats `fork`
  like `startup`.
- Stop includes `last_assistant_message`, which cthread does not use yet.
- SessionEnd's `reason` was `other` for `-p` runs. `/clear` sends
  SessionEnd `reason=clear` and then SessionStart `source=clear`, so cthread
  ignores SessionEnd and detects exits from the PTY instead.
- The **Notification** fixtures are synthesized from the documented schema
  (`notification_type`: `permission_prompt`, `idle_prompt`, …), not captured:
  a permission prompt can't be triggered from `-p`. `idle_prompt` (the
  60-second "waiting for input" reminder) is deliberately *not* treated as
  "Needs you"; otherwise every idle thread would show `!`.

## S3: transcript paths

`~/.claude/projects/<encoded cwd>/<session>.jsonl`, where the encoding replaces
every non-`[A-Za-z0-9]` character with `-` (verified with `.`, `_`, `-` and `/`).
The encoded cwd is the *resolved* path (`/private/tmp/...` on macOS).
Implemented in `paths::encode_project_dir` and unit tested.

`--resume <main> --fork-session` run from a **git worktree** of the same repo
worked in 2.1.331 *without* copying the transcript (issue #58591 seems
addressed for worktrees). It also worked with the copy, so cthread copies
the transcript anyway, to stay compatible with versions that still need it.

## Summarizer flags

`claude -p --resume <id> --fork-session --no-session-persistence "<prompt>"`
works and leaves **no** transcript behind (file count unchanged), so pulls
don't clutter `/resume`.

## S4: rendering fidelity (vt100 0.18, own renderer)

`tui-term` pins vt100 0.16, so cthread renders the vt100 screen itself
(`ui::draw_pane`). On startup Claude Code sends these terminal queries and
modes:

| Sequence | Meaning | cthread |
|---|---|---|
| `CSI c` | DA1 | answered `CSI ?62;22c` |
| `CSI 6n` | cursor position | answered |
| `CSI > 0 q` | XTVERSION | not answered (optional) |
| `CSI ? u`, `CSI > 5 u` | kitty keyboard query/push | not answered → legacy keys |
| `OSC 7501;?` | Claude-specific query | not answered |
| `?2026h` | synchronized output | ignored (harmless) |
| `?2004h` | bracketed paste | honoured (paste + pull insert) |
| `?1004h`, `?2031h` | focus / color-scheme reports | not sent |

Because the kitty protocol is unanswered, Claude Code falls back to legacy
key encoding, which is what `keys::encode_key` produces. Shift+Enter is sent
as `ESC CR`, which Claude Code treats as a newline.

A fresh folder (including a new worktree) shows Claude Code's **folder-trust
prompt** first. It renders and works inside the pane like any other screen.

**End-to-end with real Claude Code passes** (`tests/real_claude.rs`): trust
prompt → prompt in main → fork a thread (it shows main's history) → prompt in
the thread → pull → insert into main (it lands as `[Pasted text #1 +N lines]`,
not sent) → quit. Hook statuses (`◐` working, idle reminder ignored) behaved
as designed.

Findings:

- Claude Code 2.1.331 switches to its **full-screen renderer** inside the pane
  (`?1049h` alternate screen + mouse tracking `?1000/1002/1003/1006h`), and polls
  `CSI ?6n` (DECXCPR) about every 200 ms; cthread answers it. Because the child enables
  mouse reporting, cthread forwards the scroll wheel to Claude Code, which
  scrolls its own history.
- cthread must **not** pass on Claude Code's session-marker variables
  (`CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, …) when it is launched from
  inside a Claude Code session. Otherwise children run with "Transcript saving
  is off" and threads can't fork. They are stripped (`pane::SESSION_MARKERS`).
- `--resume` on a main session that never saved a message fails ("No
  conversation found"), so cthread starts main fresh in that case.
- **Open:** after main is resized (the thread pane opening or closing), the
  full-screen renderer redraws the header and input box straight away but
  leaves the message area blank for a few seconds, until its next repaint.
  Parsing the redraw at the new size (parser resized before SIGWINCH) did not
  change this, and neither did `CLAUDE_CODE_ALT_SCREEN_FULL_REPAINT=1`. Next
  steps: compare with `alacritty_terminal`, and check whether forwarding focus
  events (`?1004`) triggers an immediate repaint.

## S5: inside luvus

**Not run**: luvus is not available in this environment. Spike S5 still needs
doing on a machine with luvus. The prefix (`Ctrl-]`) avoids luvus's
`Ctrl-Space`, and mouse capture can be turned off with `ui.mouse = false`.
