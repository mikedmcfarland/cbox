# ADR 019: Forward OAuth callback port over SSH for `cbox auth`

## Status
Accepted

> **Concurrent ADR-number claim:** PRs #22 (016/017/018 territory) and
> companions may also be reaching for the next free integer. If 019 has
> been taken by the time this PR lands, the merger should rename to
> the next free slot — the content is self-contained.

## Context

`cbox auth <tier>` (a.k.a. the planned `cbox tier login`) opens an
interactive Claude Code session in a tier instance so the user can
walk through Anthropic `/login` and any OAuth-MCP setup flows
(Notion, Linear, Slack). Tokens land on the per-tier `.claude` named
volume (ADR 014).

Today the OAuth flow falls back to **manual code paste**:

1. Claude prints a URL and asks the user to open it in a browser.
2. The user opens the URL on the host.
3. The OAuth provider redirects to `http://localhost:<port>/callback?...`.
4. With nothing listening on the host's `localhost:<port>`, the
   browser shows an error page. The user copies the resulting
   `code=...` value out of the URL and pastes it back into the
   terminal where Claude is waiting.

It works, but it's clunky — three context switches per login, and
it's the kind of friction that makes people skip refreshing their
auth state.

The "happy path" is to forward the loopback port from the host into
the container so the browser's redirect lands directly on Claude's
in-container HTTP listener. SSH already gives us `-L` for this.
What the existing `src/ssh.rs` lacks is *any* port-forwarding wiring.

### Investigation: what port does the callback use?

Several constraints on what we can answer in-repo:

- Claude Code is a closed-source npm package; we can't pin the port
  from this repo's source tree.
- The agent environment couldn't reach the host filesystem to inspect
  an installed `cli.js`, and we are explicitly **not** authorised to
  run a real OAuth flow that would mint persistent credentials. So
  observation-by-running was off the table for this PR.

What we know from outside this codebase:

- Anthropic's OAuth implementation for the CLI uses a **fixed loopback
  port** in the published `redirect_uri`. Community write-ups and the
  observable shape of the `/login` URL (which includes the
  `redirect_uri=http://localhost:<PORT>/callback` query parameter,
  visible in plain text on screen before any token is minted) make
  this verifiable by anyone running `/login` — no token paste needed.
- The historical/observed value is **54545**. We treat this as the
  default but do *not* hard-code it as the only allowed value:
  Anthropic could move it, and OAuth-MCP servers run their own
  loopback listeners on their own ports.

The callback is a plain HTTP GET to `/callback?code=...`. A vanilla
`ssh -L` is sufficient — no proxy, no path rewriting, no header
fiddling required.

OAuth MCPs (Notion, Linear, Slack...) follow the same loopback
pattern but each picks its own port — sometimes fixed per-server,
sometimes from a small range. Forwarding *all* of them statically
isn't safe (collisions on the host, port-already-in-use noise), so
this ADR scopes MCP support to "user passes the port(s) explicitly
via the same flag as Anthropic /login". A future revision can move
to on-demand `ssh -O forward` via a control master if the MCP
ecosystem demands it.

## Decision

Add **opt-in OAuth callback forwarding** to `cbox auth` via a
`--forward-port <PORT>` flag (repeatable) on the `auth` subcommand.

- The flag defaults to **`54545`** when not supplied (Anthropic
  `/login`'s known callback port). Pass `--forward-port 0` (or the
  explicit `--no-forward-port` switch) to disable.
- Each `--forward-port N` adds `-L 127.0.0.1:N:127.0.0.1:N` to the
  SSH command. Loopback-bound on **both** ends (ADR-012 trust
  boundary): we never expose the forwarded port on a non-loopback
  interface, and we never reach for the container's external IP.
- Forwarding is wired in `auth` only. Bare `cbox <name>` sessions
  and `cbox run` keep no port forwarding by default — they have no
  OAuth flow to complete and shouldn't expose extra surface.
- Logging: print a one-line `==> forwarding localhost:NNN -> tier
  loopback (for OAuth callback)` per forwarded port so the user
  knows what's happening and which ports to keep free on the host.

### Failure mode

If the requested host port is already in use, OpenSSH prints
`bind [127.0.0.1]:<port>: Address already in use` and (by default)
**continues running the session** — the OAuth manual-paste fallback
still works. We rely on this behavior rather than pre-probing the
port from Rust: a pre-probe race would lie, and OpenSSH's behavior
is already correct.

We deliberately do **not** pass `ExitOnForwardFailure=yes`. Failing
the whole `cbox auth` session because port 54545 is taken would be
strictly worse than the status quo.

### MCP scope

In-scope for this ADR: any OAuth callback the user knows the port
of, via `--forward-port`.

Out of scope (deferred to a follow-up):

- Auto-discovery of MCP callback ports.
- `ssh -O forward` over a control master to add forwards mid-session.
- Reverse-direction forwarding for in-container browsers reaching
  out to host services.

A follow-up issue captures this.

## Consequences

- **Friendlier `/login`.** When the host port is free, the browser
  redirect completes silently, no code paste.
- **No regression** when forwarding fails: SSH continues, manual
  paste still works. We trade a one-line `bind: Address already in
  use` warning for the chance of a fully-automated flow.
- **No change to long-running sessions.** Regular `cbox <name>`
  shells are unaffected — the new SshConn field defaults to "no
  forwards" and only `auth` populates it.
- **Trust boundary preserved.** Forwarded ports are loopback-bound
  on both sides; the container's loopback HTTP listener never
  becomes reachable from off-host.
- **Future MCP work has a clear shape.** The same flag/Vec wiring
  in `SshConn` already accepts arbitrary ports, so MCP support is
  additive rather than a redesign.

## Alternatives considered

**Forward a range (`-L 8000-8100:...`).** OpenSSH supports
multi-port ranges only via repeated `-L` flags, not a literal
`N-M` syntax. We'd be enumerating ports anyway; defaulting to a
single known-good port with the option to add more is cleaner.

**Control-master + `ssh -O forward` on demand.** Significantly
more moving parts (master socket lifetime, error handling, race
with sshd ready-state). Only justified if we need to add forwards
*after* the session starts — true for some MCP flows, not true
for Anthropic `/login`. Deferred.

**Wrap the auth flow with an in-container proxy.** Would let us
forward a single known port (e.g., 7474) and re-emit requests to
whatever port the inner OAuth helper actually uses, decoupling host
configuration from Claude version drift. Way more code than the
problem warrants today; revisit if Anthropic's port turns out to
churn.

**Always forward by default in every session.** Violates the
least-privilege boundary in ADR 012 — sessions don't need OAuth
callback ports open. Auth is the only command that does.

## References

- ADR 004 — ssh-not-docker-exec (why we use ssh at all).
- ADR 012 — credentials and trust boundaries (loopback-only).
- ADR 014 — claude state volume (where the tokens end up).
- Issue #12 — OAuth callback forwarding.
