# ADR 018: Dual Anthropic auth — OAuth alongside `ANTHROPIC_API_KEY`

## Status
Accepted (design); implementation deferred to follow-up issues (#15,
#12).

## Context

cbox currently has exactly one way to authenticate Claude Code to
Anthropic: resolve an `ANTHROPIC_API_KEY` env credential on the host
(via `op read` or equivalent) and inject it into the tier instance at
container start. The wiring lives in
`commands::common::build_run_config` and applies to any tier that lists
an env credential whose `env_var` is `ANTHROPIC_API_KEY`.

That works, but it bypasses Claude Code's `.credentials.json` OAuth
token store entirely. ADR 014 noted this as a current-state
observation, not a long-term design decision:

> `.credentials.json` (Anthropic OAuth token, when used — we currently
> bypass this with `ANTHROPIC_API_KEY`).

In the meantime Anthropic has moved OAuth login (the "Sign in with
Claude" subscription flow) to first-class status:

- It's the expected path for interactive humans on a Claude Code
  install.
- It pulls from a subscription rather than spending API credit.
- It produces a refreshable token in `~/.claude/.credentials.json`
  (mode `0600`) — exactly the file we currently route around.

The in-repo `cbox-dev` tier already trades on this: it ships with no
credentials and expects the user to run `cbox auth cbox-dev` to land
an OAuth token on the per-tier `.claude` volume from ADR 014. That
works *because* the credential resolver runs over the empty list — but
there's nothing in the schema declaring "this tier wants OAuth," and
no story for tiers that need to flip between OAuth and an API key.

API keys aren't going away. They remain the right answer for:

- Headless / CI sessions (`cbox run`), where there's no browser to
  complete an OAuth flow.
- Programmatic budgets and per-project spend tracking that an API key
  + console dashboard provide cleanly.
- Environments where the user explicitly wants to keep their
  subscription token off the tier volume (e.g., a `power` tier with
  network access running tools they don't fully trust).

Both paths need to be first-class, and the choice needs to be
**declarative in `cbox.yaml`** — not inferred from "is there an env
credential called `anthropic-key`?" — so the user (and project
authors) can reason about which auth a tier uses by reading its
config.

## Decision

### `tiers.<name>.auth` is the opt-in

Add an `auth` field to `TierConfig`. Two values:

- `oauth` — Claude Code uses its native `.credentials.json` on the
  per-tier `.claude` volume (ADR 014). The user runs `cbox auth
  <tier>` once to land a token; cbox never reads, mints, or rotates
  it. Refresh is Claude Code's responsibility.
- `api-key` — cbox resolves an env-shaped credential whose `env_var`
  is `ANTHROPIC_API_KEY` and injects it at container start. Same
  mechanism as today.

Default: **`api-key`**, for backwards compatibility (see Backwards
compatibility below). The default may flip to `oauth` in a future ADR
once the OAuth path has shipped and the dogfood tier has run on it for
long enough to be the obvious choice; that's not this ADR.

### Schema

```yaml
tiers:
  <name>:
    # ... existing fields ...
    auth: oauth | api-key   # default: api-key
```

Kebab-case matches the existing tier flag convention
(`dangerously-skip-permissions`). The serde rename for the api-key
variant uses the same kebab form.

### When each path applies

| Use case | Recommended auth | Why |
|---|---|---|
| Interactive `dev` / `cbox-dev` on the user's laptop | `oauth` | Browser is available; subscription is the human-friendly billing model; token survives `cbox build` rebuilds via the ADR 014 volume. |
| Autonomous `cbox run` on the user's laptop | `api-key` | No browser to complete an OAuth flow if the token is missing or revoked. Even when an OAuth token exists, headless runs benefit from the predictability of an env-injected key. |
| CI / scheduled jobs | `api-key` | Headless by definition. CI secret store ↔ `op read` ↔ env injection is the well-trodden path. |
| `auto` / sandboxed autonomous tier | `api-key` | Same headless argument as `cbox run`. |
| Tiers running on a remote backend (future) | `api-key` initially | ADR 012 already notes the remote OAuth-MCP story as an open question; the same caution applies to an OAuth Anthropic token landing on a remote disk. Revisit after the local OAuth path is shipping and the remote trust model is settled. |

These are recommendations the tier author makes in `cbox.yaml`, not
something cbox enforces by inspecting the call site. `cbox run` on an
`oauth` tier is allowed; it just relies on the user having completed
`cbox auth <tier>` first, exactly like an OAuth-MCP-using tier today.

### Example: project config with mixed auth

```yaml
tiers:
  dev:
    layers: [python, node]
    network: bridge
    auth: oauth                   # interactive, subscription-billed
    credentials: [github-ro]      # no anthropic-key needed
    settings: tiers/dev/settings.json

  auto:
    layers: [python, node]
    network: none
    auth: api-key                 # headless, API-billed
    credentials: [anthropic-key]  # ANTHROPIC_API_KEY env credential
    dangerously-skip-permissions: true
    settings: tiers/auto/settings.json
```

The `cbox-dev` tier currently in `.cbox/cbox.yaml` becomes:

```yaml
tiers:
  cbox-dev:
    layers: [rust]
    network: bridge
    auth: oauth                   # explicit; matches today's behaviour
    credentials: []
    settings: tiers/cbox-dev/settings.json
```

The behaviour doesn't change — `cbox-dev` already runs without an
Anthropic env credential — but the schema now says *why* out loud
instead of leaving it implicit in the empty `credentials:` list.

### Interaction with the per-tier `.claude` volume (ADR 014)

Both paths respect the per-tier trust boundary from ADR 012:

- **`oauth`**: `~/.claude/.credentials.json` lives on the
  `cbox-tier-<tier>-claude` named volume. Each tier has its own
  volume; one tier cannot read another tier's OAuth token. `cbox
  build <tier>` rebuilds preserve the token (the whole point of
  ADR 014). `cbox tier destroy` wipes it; `cbox tier reset` (per the
  in-flight ADR 017) wipes it; `cbox auth <tier>` re-mints.
- **`api-key`**: cbox injects `ANTHROPIC_API_KEY` as an env var at
  container start. The value is resolved on the host (`op read`) per
  tier-instance bring-up and never written to the `.claude` volume.
  This preserves the current-state property that nothing
  Anthropic-related lands on the volume for api-key tiers.

The two paths must not cross-contaminate:

- An `api-key` tier must not write `.credentials.json` (Claude Code
  won't, given a valid env var, per the auth docs — but cbox should
  not create the file either).
- An `oauth` tier must not have `ANTHROPIC_API_KEY` injected. If the
  tier lists an env credential whose `env_var` is
  `ANTHROPIC_API_KEY`, the credential resolver should skip the env
  injection (see "Precedence" below) so Claude Code falls back to
  `.credentials.json` rather than silently using the api key.

The per-tier `.claude` volume is unchanged by this ADR — same name,
same mount point, same lifecycle. What changes is *what cbox expects
to find on it* depending on the tier's `auth:` setting.

### Precedence

The intent surfaces in three places: `tiers.<name>.auth` in
`cbox.yaml`, an env credential listed in `tiers.<name>.credentials`,
and `ANTHROPIC_API_KEY` exported in the user's host shell.

Rules:

1. **`tiers.<name>.auth` is authoritative.** If a tier says
   `auth: oauth`, cbox does not inject an `ANTHROPIC_API_KEY` env
   var into the container, *even if* the tier lists an env
   credential called `anthropic-key` and *even if* the host has
   `ANTHROPIC_API_KEY` exported.

   The credential resolver still runs; it just skips the specific
   env-shaped credential whose `env_var` would land an Anthropic key.
   Mount credentials and non-Anthropic env credentials are
   unaffected.

   cbox emits a one-line warning when it suppresses an Anthropic env
   credential on an `oauth` tier (`"tier dev has auth: oauth;
   ignoring credential 'anthropic-key'"`), so the user notices a
   leftover credential rather than silently sending the key
   nowhere.

2. **For `auth: api-key`, behavior is identical to today.** cbox
   resolves the env credential and injects it. If no
   `ANTHROPIC_API_KEY` credential is listed, the container starts
   without one and Claude Code will fall back to
   `.credentials.json` if present, or fail with its own
   authentication error if not. cbox does not synthesize a
   credential from the host's `ANTHROPIC_API_KEY` env var.

3. **The host shell's `ANTHROPIC_API_KEY` env var is irrelevant.**
   cbox does not auto-forward host env vars into tier instances at
   any tier; everything goes through declared credentials. The host
   key being exported is convenient for the credential *resolver*
   (the user might write `source: env://ANTHROPIC_API_KEY` in
   future), but the value still only reaches the container via a
   declared credential.

**Justification.** Authoritative-config-over-implicit-host-state
matches how every other field in `cbox.yaml` works: tiers don't
inherit network settings, layers, or mounts from the host shell, and
auth shouldn't be the exception. It also makes the schema
self-describing: reading a tier's `auth:` line tells you what
Anthropic auth will be in effect without having to also know what env
vars the user has exported and what their personal-config credentials
declare.

### Backwards compatibility

The new world keeps env-key injection intact. Concretely:

- The `auth` field defaults to `api-key`. Every existing `cbox.yaml`
  with no `auth:` lines continues to work exactly as it does today.
- The current logic in
  `commands::common::build_run_config` — "look at
  `tier_cfg.credentials`, resolve each, inject env" — survives
  unchanged for `api-key` tiers. The change is a single early
  return: on `auth: oauth`, skip env credentials whose `env_var`
  is `ANTHROPIC_API_KEY`.
- The `examples/full-setup/cbox.yaml` corpus continues to parse and
  validate without modification. Adding an explicit `auth: api-key`
  to its tiers in a follow-up makes it self-documenting but isn't
  required for the ADR.
- The implicit-no-anthropic-credential pattern the `cbox-dev` tier
  uses today (no `anthropic-key` in `credentials:`) keeps working —
  Claude Code will read `.credentials.json` from the volume as it
  does now. The `auth: oauth` line just makes the intent explicit
  and unlocks the precedence rule above.

### Migration path

For a user who wants to flip an existing tier from api-key to OAuth:

1. Add `auth: oauth` to the tier in `cbox.yaml`.
2. Optionally remove the `anthropic-key` entry from the tier's
   `credentials:` list (or leave it — cbox will warn and skip).
3. Run `cbox auth <tier>` to land an OAuth token on the tier's
   `.claude` volume.
4. Existing sessions in the tier continue with their current
   process-memory api key until they restart; new container starts
   pick up the OAuth path.

No data migration is required — the env injection and the
`.credentials.json` file are independent stores, and the
`cbox-tier-<tier>-claude` volume already exists from ADR 014.

### What this ADR does NOT decide

- **Token refresh.** OAuth refresh is entirely Claude Code's
  responsibility; cbox never reads or rewrites `.credentials.json`.
  If refresh fails (subscription expired, token revoked, clock
  skew), the user re-runs `cbox auth <tier>` — same flow as the
  initial auth. cbox does not poll, monitor, or proxy the refresh.
- **Multi-account.** One Anthropic account per tier. A user wanting
  two accounts on one machine declares two tiers. We do not plan to
  thread an account selector through `cbox.yaml` in v1.
- **MCP OAuth flows.** Out of scope; tracked by issue #12 and
  governed by ADR 012's open questions. This ADR concerns the
  Anthropic account credential only.
- **`apiKeyHelper`-based remote credential broker.** Mentioned in
  ADR 012 as the planned remote-backend strategy for the Anthropic
  key on `api-key` tiers. The `auth` field defined here is
  orthogonal — when that broker lands, `auth: api-key` continues to
  describe "use the api-key path"; *how* the key reaches the
  container (env var on local, helper script on remote) is the
  backend's concern.
- **A third `auth: helper` (or similar) value** for the broker
  case. Not needed: from the tier's perspective, `apiKeyHelper`
  still results in Claude Code holding an api key — it just gets it
  from a script instead of an env var. Collapse if/when the
  distinction matters; don't pre-build the slot.

## Consequences

- `TierConfig` grows one field; serde default keeps every existing
  `cbox.yaml` valid.
- `build_run_config` gains an early-skip on Anthropic env credentials
  for `oauth` tiers, plus the warning. Implementation belongs in
  issue #15.
- The dogfood `.cbox/cbox.yaml` can declare its OAuth intent
  explicitly (`auth: oauth` on `cbox-dev`), removing the "no
  credentials" comment that currently has to explain it in prose.
- `cbox auth <tier>` keeps its current job — registering OAuth MCPs —
  and now also serves as the canonical command for landing an
  Anthropic OAuth token on an `oauth` tier. Its UX may need a small
  tweak (issue #15) to ensure the Anthropic login flow runs when the
  tier has `auth: oauth` and `.credentials.json` is absent.
- `cbox tier destroy` (and the in-flight `cbox tier reset` from
  ADR 017) already wipe the `.claude` volume, which is now the OAuth
  token's home. Both verbs continue to work without modification, but
  their docs should mention "destroys/resets the Anthropic OAuth
  token along with everything else on the volume" when this lands.
- Glossary picks up two terms: `oauth auth` and `api-key auth`
  (matching the schema values).
- ADR 014's "current-state bypass" footnote becomes stale; ADR 014 is
  updated with a one-line pointer to this ADR.

## Alternatives considered

**Infer auth from the credential list.** If the tier lists an
Anthropic env credential, treat it as api-key; otherwise OAuth. This
is essentially today's behaviour and was the path-of-least-resistance
default. Rejected because it conflates "I want OAuth" with "I forgot
to declare a credential," and it has no way to express "I want OAuth
on a tier that also happens to declare an `ANTHROPIC_API_KEY` env
credential for unrelated tooling." An explicit `auth:` field separates
intent from credential bookkeeping.

**A top-level `anthropic_auth:` block rather than a per-tier field.**
Centralised but wrong-shape — auth is a tier property in the same way
network mode and permissions are, since the trust boundary is the
tier (ADR 012). A global setting would force every tier to share
billing model and credential type. Rejected.

**An enum with three variants: `oauth`, `api-key`, `auto`.** Where
`auto` does the credential-list inference described above. Tempting
as a transitional default but pushes the inference complexity into
production code rather than removing it. Reject in favor of keeping
the rule simple ("default is `api-key`, you opt into `oauth`
explicitly") and letting the default flip happen in a future ADR with
its own justification.

**`auth: claude-code-default`** (let Claude Code pick whichever of
env-var, `.credentials.json`, or `apiKeyHelper` it finds first). This
is what Claude Code does internally anyway; explicit values let cbox
*control* which path is in effect and skip injecting an env var when
the tier wants OAuth. The control matters because it's the only way
to make the precedence rule above auditable from `cbox.yaml`.

**Rip out env-key injection entirely once OAuth ships.** Explicit
non-goal per the issue. CI / headless / programmatic-budget use cases
keep api-key first-class.

## References

- ADR 002 (per-tier instances — the trust-boundary model that scopes
  what "this tier's auth" means).
- ADR 011 (backend abstraction — `TierConfig` is the schema fed to
  the backend; the new `auth:` field flows the same path).
- ADR 012 (credentials and trust boundaries — load-bearing; both
  auth paths respect the per-tier boundary, and ADR 012's remote
  story interacts with both).
- ADR 014 (per-tier `.claude` volume — the storage for OAuth tokens
  under this ADR; ADR 014's bypass footnote is the proximate cause
  of this ADR).
- ADR 015 (config merge — `auth:` is a tier field, so the
  project-wins composition rule applies; a project's `auth: oauth`
  cannot be overridden by a personal-config tier of the same name).
- ADR 017 (in flight — `cbox tier reset` wipes the OAuth token along
  with the rest of the volume; both ADRs deliberately depend on
  ADR 014's storage location).
- Issue #15 — implementation: schema field, `build_run_config`
  branch, `cbox auth` UX tweak.
- Issue #12 — MCP OAuth, deliberately out of scope here.
- `src/commands/common.rs::build_run_config` — the current
  env-injection site; the implementation touch point.

## Open questions

- **Should `cbox auth <tier>` fail loudly on an `auth: api-key`
  tier?** Currently `cbox auth` is for OAuth MCPs; under this ADR
  it'd also drive Anthropic OAuth on `oauth` tiers. On an api-key
  tier, running `cbox auth` to register an OAuth MCP is still valid,
  but if the user is trying to land an Anthropic OAuth token they're
  in the wrong place. A pre-flight check ("this tier has
  `auth: api-key`; Anthropic OAuth won't take effect — proceed for
  MCP auth only?") is probably right; defer to issue #15.
- **Does Claude Code's `.credentials.json` survive an Anthropic-side
  token revocation cleanly?** I.e., does it surface a re-auth prompt
  the user can satisfy via `cbox auth <tier>` again, or does it sit
  in a broken state until the file is deleted? Worth verifying
  during issue #15 so the docs can tell the user what to expect.
- **A future `auth: helper` for the `apiKeyHelper` broker case.**
  Listed as a deliberate non-decision above. If the remote broker
  story ships and there's a real reason to distinguish "key from
  env" vs "key from helper" at the tier-config level, add the
  variant then.
