#!/usr/bin/env bash
# Step 9 nightly check: has the Dory PR merged, or has its head moved since we pinned it?
# Read-only: it only queries GitHub. Exit status 0 means "unchanged", 1 means "look at it".
set -uo pipefail
PIN="1c3cdd23787750814aa21a070dc4d6dc451af871"
json=$(gh pr view 30306 --repo ggml-org/llama.cpp --json state,mergedAt,headRefOid 2>/dev/null) || { echo "could not query GitHub (is gh logged in?)"; exit 2; }
state=$(jq -r .state <<<"$json"); merged=$(jq -r .mergedAt <<<"$json"); head=$(jq -r .headRefOid <<<"$json")
echo "PR #30306: state=$state merged_at=$merged head=$head pinned=$PIN"
if [ "$merged" != "null" ] || [ "$head" != "$PIN" ]; then
  echo "CHANGED: re-run the comparison suite and update the pin (step 10)"; exit 1
fi
echo "unchanged"
