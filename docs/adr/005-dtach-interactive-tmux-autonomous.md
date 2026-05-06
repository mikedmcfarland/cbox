# ADR 005: dtach for interactive sessions, tmux for autonomous

## Status
Accepted

## Context
Multiple sessions run in the same tier instance. Each needs a persistent terminal environment that survives SSH disconnects. cbox sessions should live naturally as panes and windows inside host tmux, with the containerized aspect barely noticeable — same keybindings, same workflow, different status bar color. Nested tmux (host tmux → container tmux) creates prefix key collisions that break that experience.

## Decision
**Interactive sessions** use `dtach` for inner persistence. No container tmux. Host tmux handles all multiplexing — splits, windows, copy mode, keybindings are all the user's normal host tmux.

**Autonomous sessions** (`cbox run`) use container tmux. Headless, no user interaction, no prefix conflict.

Both use socket files in `/run/cbox/` for stateless session tracking (see ADR 010).

```
Interactive:  ssh ... dtach -A /run/cbox/auth-fix.sock -z claude
Autonomous:   ssh ... tmux -S /run/cbox/auto-tests.sock new-session -d
```

## Alternatives considered

**Container tmux with different prefix** (e.g., `ctrl-a` inside, `ctrl-b` on host): Works, but the point is to make containerization invisible. Different keybindings break that.

**Container tmux with send-prefix** (`ctrl-b ctrl-b`): Double-tap is annoying for heavy use inside the container.

**No inner persistence at all**: If SSH drops, the process dies. Unacceptable for long-running Claude sessions.

**Separate tmux servers per session** (`tmux -L cbox-<name>`): Works but requires running from a bare terminal — host tmux + container tmux is the prefix-collision case. Rejected in favor of dtach for interactive sessions.

## Consequences
- Interactive sessions live seamlessly inside host tmux panes. Same prefix, same copy mode, same everything. The first `cbox <name>` starts Claude in dtach; subsequent calls open a shell in the same workspace (`--claude`/`--attach` to override).
- `cbox <name>` creates a host tmux window. Pane splits are normal host splits — run `cbox <name>` again for a container shell, or configure tmux `default-command` yourself for auto-SSH splits.
- If SSH drops, dtach keeps Claude alive. Reconnect via `cbox <name> --attach` or switch to the original host tmux window.
- Autonomous sessions are fully isolated with their own tmux server. `cbox run` output can be inspected by attaching to the container tmux.
- Session isolation preserved: dtach sockets are per-session, sessions don't see each other.
- **Future option**: a `nested` tmux mode (container tmux with separate prefix) is not blocked by this design. The socket-based tracking and API surface work identically — swap `dtach -A` for `tmux -S`.
