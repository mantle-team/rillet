#!/usr/bin/env bash
set -euo pipefail
REPO="mantle-team/rillet"

gh api -X PATCH "repos/$REPO" \
  -F delete_branch_on_merge=true \
  -F allow_squash_merge=true \
  -F allow_merge_commit=false \
  -F allow_rebase_merge=false \
  -f squash_merge_commit_title=PR_TITLE \
  -f squash_merge_commit_message=COMMIT_MESSAGES

RULESET=$(cat <<'EOF'
{
  "name": "main",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": {
      "include": ["~DEFAULT_BRANCH"],
      "exclude": []
    }
  },
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" },
    {
      "type": "merge_queue",
      "parameters": {
        "merge_method": "SQUASH",
        "grouping_strategy": "ALLGREEN",
        "max_entries_to_build": 5,
        "min_entries_to_merge": 1,
        "max_entries_to_merge": 5,
        "min_entries_to_merge_wait_minutes": 5,
        "check_response_timeout_minutes": 60
      }
    },
    {
      "type": "required_status_checks",
      "parameters": {
        "strict_required_status_checks_policy": false,
        "required_status_checks": [
          { "context": "Checks" }
        ]
      }
    }
  ]
}
EOF
)

RULESET_ID=$(gh api "repos/$REPO/rulesets" --jq '.[] | select(.name == "main") | .id' | head -1)
if [ -n "$RULESET_ID" ]; then
  echo "$RULESET" | gh api -X PUT "repos/$REPO/rulesets/$RULESET_ID" --input -
else
  echo "$RULESET" | gh api -X POST "repos/$REPO/rulesets" --input -
fi
