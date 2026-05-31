#!/usr/bin/env bash
# Apply repo-level settings from .github/repo-settings.json.
#
# PATCHes https://api.github.com/repos/{owner}/{repo} — see
# https://docs.github.com/en/rest/repos/repos#update-a-repository
# for the accepted fields. PATCH is naturally idempotent, so unlike
# rulesets there's no name lookup or create-vs-update branching.
#
# Usage: scripts/apply-repo-settings.sh
# Requires: gh CLI authenticated with repo admin scope.

set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
settings_file="$repo_dir/.github/repo-settings.json"

repo_slug="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"

echo "Applying repo settings to $repo_slug from .github/repo-settings.json"
gh api --method PATCH "/repos/$repo_slug" \
  --input "$settings_file" >/dev/null

echo "Done."
