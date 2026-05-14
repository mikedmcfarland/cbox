#!/usr/bin/env bash
# Apply repo rulesets from .github/rulesets/*.json idempotently.
#
# Each JSON file describes one ruleset (see GitHub REST API:
# https://docs.github.com/en/rest/repos/rules). The `name` field is the
# stable key — if a ruleset with the same name already exists on the
# repo, this script PUTs an update; otherwise it POSTs a new one.
#
# Usage: scripts/apply-rulesets.sh
# Requires: gh CLI authenticated with repo admin scope.

set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
rulesets_dir="$repo_dir/.github/rulesets"

# Detect owner/repo from origin or gh.
repo_slug="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"

existing="$(gh api "/repos/$repo_slug/rulesets" --jq '[.[] | {id, name}]')"

shopt -s nullglob
for file in "$rulesets_dir"/*.json; do
  name="$(jq -r .name "$file")"
  id="$(echo "$existing" | jq -r --arg n "$name" '.[] | select(.name == $n) | .id')"

  if [ -n "$id" ]; then
    echo "Updating ruleset '$name' (id $id) on $repo_slug"
    gh api --method PUT "/repos/$repo_slug/rulesets/$id" \
      --input "$file" >/dev/null
  else
    echo "Creating ruleset '$name' on $repo_slug"
    gh api --method POST "/repos/$repo_slug/rulesets" \
      --input "$file" >/dev/null
  fi
done

echo "Done."
