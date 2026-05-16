#!/usr/bin/env bash
# Register the GitHub MCP server with Claude Code if a token is present.
#
# cbox runs every /cbox/init.d/*.sh script on container start (see
# base/cbox-init). Scripts must be idempotent: this one checks whether
# the `github` server is already registered and exits cleanly if so.
#
# The token comes from a credential the tier opted into in cbox.yaml
# (e.g. credentials: [github-ro]). cbox resolves it via 1Password on the
# host and injects it as GITHUB_TOKEN at container start.

set -euo pipefail

# No token, no MCP — most tiers (e.g. `auto`) don't grant GitHub access.
if [ -z "${GITHUB_TOKEN:-}" ]; then
    exit 0
fi

# Idempotency: bail if the user-scope github server is already present.
# `claude mcp list` exits 0 even with no matches, so grep -q is enough.
if sudo -u cbox -i claude mcp list 2>/dev/null | grep -q '^github\b'; then
    exit 0
fi

# `-s user` writes into ~/.claude.json, which lives on the per-tier
# .claude named volume — the registration survives image rebuilds.
sudo -u cbox -i \
    GITHUB_TOKEN="$GITHUB_TOKEN" \
    claude mcp add github \
        https://api.githubcopilot.com/mcp/ \
        -t http \
        -s user \
        -H "Authorization: Bearer $GITHUB_TOKEN"
