//! Status block printed at the top of `cbox auth <tier>` (a.k.a.
//! `tier login`) so the user can see what's already registered on the
//! per-tier `.claude` volume before driving an OAuth flow.
//!
//! Today this covers:
//! - **Anthropic auth** — parsed from `.credentials.json` on the
//!   per-tier `.claude` volume (ADR 014). Classified as
//!   `present` / `absent` / `expired` based on `oauthAccount.expiresAt`
//!   (epoch milliseconds) when that field is present; otherwise
//!   reported as `present (expiry unknown)`.
//! - **Registered MCPs** — enumerated from `.claude.json`'s
//!   `mcpServers` map (a Claude-Code-controlled key). For each entry
//!   we print the name and a best-effort auth state, falling back to
//!   `(state unknown)` when no cheap signal is available — explicit-
//!   not-misleading is the rule from issue #15.
//!
//! Strictly informational. Detection failures (missing volume,
//! malformed JSON, exec error) downgrade to a one-liner warning and
//! the agent still launches — `cbox auth` is never gated on detection.
//!
//! Once PR #22 (ADR 018, dual Anthropic auth) lands, the Anthropic
//! line should become aware of `tiers.<name>.auth` — `api-key` tiers
//! should say so plainly rather than probe `.credentials.json` (the
//! ADR specifies that file is unused for `api-key`). For now every
//! tier is treated as the implicit `api-key` default and we still
//! show the OAuth probe; on `main` there is no schema field to read.
//! See the issue body for the follow-up plan.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Classification of the per-tier Anthropic OAuth credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicStatus {
    /// `.credentials.json` is missing (volume empty / first-time login).
    Absent,
    /// File present, token present, expiry not yet reached (or expiry
    /// field absent — in which case `expiry_known` is `false`).
    Present { expiry_known: bool },
    /// File present, token present, expiry strictly in the past.
    Expired,
    /// File present but didn't parse / didn't contain a recognisable
    /// token shape. Treated as "unknown" rather than absent so the user
    /// notices something is off without us inventing detail.
    Malformed,
}

/// One row in the MCP section of the status block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStatus {
    pub name: String,
    /// Best-effort label: `"ready"`, `"oauth-pending"`, `"(state unknown)"`,
    /// etc. The exact set is unstable — Claude Code controls the
    /// `.claude.json` schema and we follow.
    pub state: String,
}

/// Classify `.credentials.json` content (or absence).
///
/// `raw == None` means the file is not present on the volume; that's
/// the first-time-login case from the issue.
pub fn classify_anthropic(raw: Option<&str>, now_unix_ms: i64) -> AnthropicStatus {
    let Some(raw) = raw else {
        return AnthropicStatus::Absent;
    };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return AnthropicStatus::Malformed;
    };

    // Claude Code's `.credentials.json` historically wraps the OAuth
    // material under a `claudeAiOauth` key. Tolerate either that
    // wrapper or a bare-token shape — we only need to find an access
    // token and (optionally) an expiry to classify.
    let token_obj = v
        .get("claudeAiOauth")
        .or_else(|| v.get("oauth"))
        .unwrap_or(&v);

    let has_token = token_obj
        .get("accessToken")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());

    if !has_token {
        return AnthropicStatus::Malformed;
    }

    // `expiresAt` is epoch milliseconds in Claude Code's stored
    // format. Accept ints or stringified ints; treat anything else as
    // "expiry unknown" rather than guessing.
    let expires_ms: Option<i64> = token_obj.get("expiresAt").and_then(|x| {
        x.as_i64()
            .or_else(|| x.as_str().and_then(|s| s.parse::<i64>().ok()))
    });

    match expires_ms {
        Some(exp) if exp <= now_unix_ms => AnthropicStatus::Expired,
        Some(_) => AnthropicStatus::Present { expiry_known: true },
        None => AnthropicStatus::Present {
            expiry_known: false,
        },
    }
}

/// Enumerate MCP servers from `.claude.json`.
///
/// Tolerates absence (`raw == None`) and malformed JSON — both yield
/// an empty list. A returned `Vec` of length zero means "no MCPs
/// registered" and the caller can print that explicitly.
pub fn classify_mcps(raw: Option<&str>) -> Vec<McpStatus> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };

    // Claude Code stores MCP server registrations under `mcpServers`.
    // If/when the schema moves, the volume-side detection moves with
    // it; the goal here is cheap-and-truthful, not exhaustive.
    let Some(map) = v.get("mcpServers").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut out: Vec<McpStatus> = map
        .iter()
        .map(|(name, entry)| McpStatus {
            name: name.clone(),
            state: best_effort_mcp_state(entry),
        })
        .collect();
    // Stable order for tests + human-readable output.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Cheap signals only. If the entry obviously carries an auth token
/// or marker we say so; otherwise `(state unknown)` per the issue's
/// explicit-not-misleading rule.
fn best_effort_mcp_state(entry: &Value) -> String {
    // OAuth-style MCPs usually have an `auth` / `oauth` / `tokens`
    // sub-object once a flow completes. We don't inspect contents —
    // presence of the key is the signal Claude Code's UI uses.
    for key in ["auth", "oauth", "tokens", "credentials"] {
        if entry.get(key).is_some_and(|v| !v.is_null()) {
            return "ready".to_string();
        }
    }
    // Stdio MCPs (registered via `claude mcp add ... -- cmd args`) have
    // a `command` key and no token store — they're "configured" but
    // their auth lives wherever the command itself looks.
    if entry.get("command").is_some() {
        return "configured (stdio)".to_string();
    }
    "(state unknown)".to_string()
}

/// Render the status block as plain text (no leading/trailing newline).
///
/// Output budget per the issue: ~5–10 lines. We stay inside that for
/// any realistic MCP count.
pub fn render(tier: &str, anthropic: &AnthropicStatus, mcps: &[McpStatus]) -> String {
    let mut out = String::new();
    out.push_str(&format!("==> tier {tier:?} login state:\n"));
    out.push_str(&format!("    Anthropic: {}\n", render_anthropic(anthropic)));
    if mcps.is_empty() {
        out.push_str("    MCPs: none registered\n");
    } else {
        out.push_str("    MCPs:\n");
        for m in mcps {
            out.push_str(&format!("      - {} — {}\n", m.name, m.state));
        }
    }
    out
}

fn render_anthropic(s: &AnthropicStatus) -> &'static str {
    match s {
        AnthropicStatus::Absent => "absent (no .credentials.json on tier volume)",
        AnthropicStatus::Present { expiry_known: true } => "present",
        AnthropicStatus::Present {
            expiry_known: false,
        } => "present (expiry unknown)",
        AnthropicStatus::Expired => "expired",
        AnthropicStatus::Malformed => ".credentials.json present but unparseable",
    }
}

/// Current unix time in milliseconds. Tests pass an explicit
/// `now_unix_ms` to [`classify_anthropic`]; production callers use
/// this.
pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        // Clock-before-epoch (CI VMs do strange things) — pick 0
        // rather than panic. "Expired" only fires when expiry < now,
        // so this errs on the side of "present".
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000; // 2023-11-14 in epoch ms.

    #[test]
    fn absent_credentials_file_is_absent() {
        assert_eq!(classify_anthropic(None, NOW), AnthropicStatus::Absent);
    }

    #[test]
    fn unparseable_credentials_file_is_malformed() {
        assert_eq!(
            classify_anthropic(Some("not json"), NOW),
            AnthropicStatus::Malformed
        );
    }

    #[test]
    fn present_with_future_expiry() {
        let raw = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"abc","expiresAt":{}}}}}"#,
            NOW + 60_000
        );
        assert_eq!(
            classify_anthropic(Some(&raw), NOW),
            AnthropicStatus::Present { expiry_known: true }
        );
    }

    #[test]
    fn present_without_expiry_field_says_so() {
        let raw = r#"{"claudeAiOauth":{"accessToken":"abc"}}"#;
        assert_eq!(
            classify_anthropic(Some(raw), NOW),
            AnthropicStatus::Present {
                expiry_known: false
            }
        );
    }

    #[test]
    fn past_expiry_is_expired() {
        let raw = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"abc","expiresAt":{}}}}}"#,
            NOW - 1
        );
        assert_eq!(
            classify_anthropic(Some(&raw), NOW),
            AnthropicStatus::Expired
        );
    }

    #[test]
    fn bare_token_shape_also_parses() {
        // Tolerate a shape without the `claudeAiOauth` wrapper — the
        // file format is Claude Code's, not ours, so we don't want to
        // over-assume.
        let raw = format!(r#"{{"accessToken":"abc","expiresAt":{}}}"#, NOW + 60_000);
        assert_eq!(
            classify_anthropic(Some(&raw), NOW),
            AnthropicStatus::Present { expiry_known: true }
        );
    }

    #[test]
    fn missing_token_is_malformed() {
        let raw = r#"{"claudeAiOauth":{"other":"thing"}}"#;
        assert_eq!(
            classify_anthropic(Some(raw), NOW),
            AnthropicStatus::Malformed
        );
    }

    #[test]
    fn no_mcps_when_file_absent() {
        assert!(classify_mcps(None).is_empty());
    }

    #[test]
    fn no_mcps_when_key_missing() {
        let raw = r#"{"other": {}}"#;
        assert!(classify_mcps(Some(raw)).is_empty());
    }

    #[test]
    fn mcps_enumerated_and_sorted() {
        let raw = r#"{
            "mcpServers": {
                "notion": {"auth": {"token": "x"}},
                "linear": {"command": "linear-mcp"},
                "weird":  {"transport": "sse"}
            }
        }"#;
        let mcps = classify_mcps(Some(raw));
        assert_eq!(mcps.len(), 3);
        assert_eq!(mcps[0].name, "linear");
        assert_eq!(mcps[0].state, "configured (stdio)");
        assert_eq!(mcps[1].name, "notion");
        assert_eq!(mcps[1].state, "ready");
        assert_eq!(mcps[2].name, "weird");
        assert_eq!(mcps[2].state, "(state unknown)");
    }

    #[test]
    fn malformed_mcps_file_yields_empty_list() {
        // We don't surface "malformed" for the MCP file — the
        // Anthropic line already covers "something's wrong with the
        // volume." Empty list keeps the block short.
        assert!(classify_mcps(Some("garbage")).is_empty());
    }

    #[test]
    fn render_includes_tier_name_and_each_section() {
        let block = render(
            "dev",
            &AnthropicStatus::Present { expiry_known: true },
            &[
                McpStatus {
                    name: "notion".into(),
                    state: "ready".into(),
                },
                McpStatus {
                    name: "linear".into(),
                    state: "(state unknown)".into(),
                },
            ],
        );
        assert!(block.contains("tier \"dev\""), "{block}");
        assert!(block.contains("Anthropic: present"), "{block}");
        assert!(block.contains("notion — ready"), "{block}");
        assert!(block.contains("linear — (state unknown)"), "{block}");
    }

    #[test]
    fn render_no_mcps_says_none() {
        let block = render("dev", &AnthropicStatus::Absent, &[]);
        assert!(block.contains("MCPs: none registered"), "{block}");
        assert!(block.contains("Anthropic: absent"), "{block}");
    }
}
