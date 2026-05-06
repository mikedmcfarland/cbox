# ADR 010: Stateless tracking via Docker labels and session sockets

## Status
Accepted

## Context
cbox needs to track two things: which tier instances exist and which sessions are alive. This state could live in a file (`~/.cbox/state.json`), a database, or be derived from the systems that already manage the underlying resources.

Interactive sessions use dtach and autonomous sessions use tmux (see ADR 005). Both use socket files, so tracking can be unified.

## Decision
No cbox-managed state files. Tier instances are tracked via Docker labels. Alive sessions are detected by the presence of socket files in `/run/cbox/` inside the tier instance.

## How it works

**Tier instances**: Every cbox tier instance gets Docker labels (`managed-by=cbox`, `cbox.tier=<name>`). `cbox list` queries Docker directly: `docker ps --filter "label=managed-by=cbox"`. Docker is the single source of truth.

**Sessions**: All session sockets live in `/run/cbox/` inside each tier instance:

```
/run/cbox/
  auth-fix.sock       # dtach socket (interactive session)
  experiment.sock     # dtach socket (interactive session)
  auto-tests.sock     # tmux socket (autonomous session)
```

Alive sessions are detected by listing this directory. The backend (dtach vs tmux) doesn't matter for detection — a socket file exists while the session is alive, and disappears when it ends. No sockets remaining → auto-pause the tier instance.

## Alternatives considered

**State file (`~/.cbox/state.json`)**: Track containers and sessions in a JSON file managed by cbox. Rejected because:

- State drift — if a tier instance is removed outside cbox (`docker rm`), or a session crashes, the state file becomes stale.
- Requires locking for concurrent access (multiple `cbox` invocations).
- Another file to manage, back up, and debug when things go wrong.

**PID files / lock files**: One file per session with the process ID. Rejected because PIDs can be recycled, stale PID files require cleanup logic, and we'd be reimplementing what socket files already provide.

**Database (SQLite)**: Overkill for tracking a handful of containers and sessions.

**Separate directories per backend** (e.g., tmux sockets in one place, dtach in another): Rejected — a single directory is simpler and makes detection backend-agnostic.

## Consequences
- No state to drift, corrupt, or clean up. Docker and socket files are the source of truth.
- `cbox list` is always accurate — it queries live state, not cached records.
- If a tier instance is removed outside cbox, it simply disappears from `cbox list`. No orphaned state.
- If a session crashes, its socket is gone and won't be counted. Auto-pause works correctly.
- Backend-agnostic: adding new persistence backends (e.g., nested tmux mode) requires no changes to tracking — just put the socket in `/run/cbox/`.
- Trade-off: querying Docker and SSH-ing into containers to check sockets is slightly slower than reading a local file. Acceptable given the small number of tier instances.
