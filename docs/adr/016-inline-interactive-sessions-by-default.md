# ADR 016: interactive sessions run inline in the invoking pane

## Status
Accepted. Supersedes the "host tmux window per session" default established by [ADR 005](005-dtach-interactive-tmux-autonomous.md) for interactive sessions.

## Context
ADR 005 settled on dtach for inner persistence and host tmux for outer multiplexing — "containerized aspect barely noticeable, same keybindings, same workflow." The first cut implemented that by detecting `$TMUX` and unconditionally spawning a new host tmux window (`cbox:<name>`) for every `cbox <name>` invocation.

In practice that imposes a windowing convention on the user instead of getting out of the way: `cbox myfix cbox` from a working pane vanishes the prompt and replants the session somewhere else, leaving the original pane idle. Pane splits — the natural way to put a Claude session next to a shell — require manually re-running cbox after splitting, which itself spawns yet another window. The opposite of the ADR 005 goal.

## Decision
- `cbox <name>` runs `ssh ... dtach -A ...` inline in the current shell or tmux pane, regardless of whether host tmux is running. Subsequent `cbox <name>` invocations detect the existing dtach socket and reattach inline in whatever pane they're invoked from.
- Ancillary calls (`--shell` / `--claude` on a re-entry) and `--attach` (`SelectExisting`) follow the same rule — inline in the invoking pane.
- **Visual indicator** is an OSC 0 terminal-title escape (`\e]0;cbox:<session>\a`) emitted before the inner command runs. The title travels with the session regardless of which pane it lands in and works in any modern terminal — iTerm2, kitty, alacritty, WezTerm, tmux's own `set -g set-titles on`, etc. This replaces the never-shipped "different tmux status color per window" idea left as TBD in `docs/plans/v1.md` §"Host tmux integration".
- **Window composition is the user's job, not cbox's.** Users who want a Claude session in its own tmux window can run `tmux new-window cbox myfix` (or bind it); users who want splits run `tmux split-window cbox myfix`. cbox stays out of the windowing layer entirely.

## Alternatives considered

**Keep the tmux window as default; add `--inline` opt-out.** Reverses the friction direction — most users would set `alias cbox='cbox --inline'`, which defeats the ergonomic case for either default. Inline is the lower-friction default because it composes with how the user already organizes their terminal (splits, windows, sessions) rather than fighting it.

**Auto-detect "do I need a window?" heuristics** (e.g., current pane is busy, attached vs. detached). Adds a layer of mind-reading on top of a tool whose job is to be invisible. Explicit user-driven composition is clearer.

**Keep a `--window` opt-in for the legacy behavior.** Considered and rejected. It's trivial for users to open a tmux window themselves (`tmux new-window cbox myfix`), so the flag would only carry documentation and maintenance overhead. The codepath added an `inside_tmux` branch, two ancillary-suffix helpers, and a `kill_session_windows` cleanup hook on destroy for windows cbox no longer creates — all of which disappear by dropping the flag entirely. There is no installed user base depending on it (the flag never shipped outside this PR's diff).

**Visual indicator via prompt (PS1) injection.** Would require either editing rc files inside the container or wrapping the dtach command in a shell that re-exports PS1, which fights the user's own prompt config. Terminal title is rendered by the terminal/tmux without touching the user's shell.

## Consequences
- `cbox <name>` is a drop-in replacement for `claude` in whatever pane the user runs it from — no windowing side effects.
- Pane splits work naturally: split, run `cbox <name>` in the new pane, get an ancillary shell in the same workspace.
- Users who want named tmux windows compose them with normal tmux commands; cbox doesn't manage them.
- The terminal-title indicator is best-effort. Terminals without OSC 0 support (rare) show no indicator; the session still works.
- `src/tmux.rs` shrinks to just the `inside_tmux` probe and `create_window` helper used by `cbox auth <tier>` (which still opens a one-shot window for the OAuth handoff so it doesn't take over the user's pane). The per-session `window_name` / `select_window` / `kill_session_windows` machinery is gone.
- ADR 005's underlying decision (dtach for interactive, container tmux for autonomous) is unchanged — only the host-side multiplexing default flips for interactive sessions.
