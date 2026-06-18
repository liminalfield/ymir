#!/usr/bin/env bash
#
# gh-setup-structure.sh
#
# Bootstraps the Ymir tracking structure on GitHub. Idempotent: safe to
# re-run. Creates NO issues. It only sets up the empty scaffolding:
#
#   * track:* labels and a core-touching label
#   * an optional custom "Node" issue type on the org
#   * a "Ymir Roadmap" Project (v2) with a single-select "Track" field
#
# Issues (the per-track epics and their sub-issues) are a separate step,
# written once their bodies exist.
#
# Requirements:
#   * gh >= 2.94, authenticated against the org (gh auth login)
#   * scopes: project and read:org for the Project and labels; admin:org
#     only for the optional issue-type block. Grant with:
#       gh auth refresh -s project,read:org,admin:org
#
# Usage:
#   ./scripts/gh-setup-structure.sh

set -euo pipefail

ORG="liminalfield"
REPO="liminalfield/ymir"
PROJECT_TITLE="Ymir Roadmap"

command -v gh >/dev/null || { echo "gh not found on PATH"; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "not authenticated; run: gh auth login"; exit 1; }

echo "==> Labels"
# Format: name|hex-color|description
labels=(
  "track:nodes|1d76db|Node library: generators, modifiers, masks, combiners"
  "track:erosion|b60205|Erosion and resolution-dependent terrain physics"
  "track:gui|0e8a16|ymir-gui: viewport and node editor"
  "track:io|5319e7|Serialization, export formats, file-format stability"
  "track:tiling|fbca04|Tiled and large builds, seam handling"
  "track:engine|0052cc|Core engine: cache policy, benches, test coverage"
  "track:cli|006b75|ymir-cli: headless batch runner"
  "track:docs|c5def5|Documentation and project hygiene"
  "core-touching|d93f0b|Changes ymir-core or a downstream contract; do deliberately"
)
for entry in "${labels[@]}"; do
  IFS="|" read -r name color desc <<< "$entry"
  # --force updates the label if it already exists, so re-runs are clean.
  gh label create "$name" --color "$color" --description "$desc" --force --repo "$REPO"
done

echo "==> Issue type 'Node' (optional; needs admin:org)"
# Orgs ship Bug/Feature/Task by default. This adds a "Node" type for the
# additive operator work. Skipped without complaint if it already exists or
# the token lacks admin:org.
if gh api "orgs/$ORG/issue-types" --jq '.[].name' 2>/dev/null | grep -qx "Node"; then
  echo "    'Node' issue type already present"
elif gh api --method POST "orgs/$ORG/issue-types" \
       -f name="Node" -F is_enabled=true -f color="green" \
       -f description="A concrete operator: one Operator impl plus one inventory::submit!" \
       >/dev/null 2>&1; then
  echo "    created 'Node' issue type"
else
  echo "    skipped (already exists, or token lacks admin:org)"
fi

echo "==> Project"
num="$(gh project list --owner "$ORG" --format json \
        --jq ".projects[] | select(.title==\"$PROJECT_TITLE\") | .number" 2>/dev/null | head -n1)"
if [ -z "${num:-}" ]; then
  num="$(gh project create --owner "$ORG" --title "$PROJECT_TITLE" --format json --jq '.number')"
  echo "    created project #$num"
else
  echo "    reusing existing project #$num"
fi

echo "==> Track field"
if gh project field-list "$num" --owner "$ORG" --format json --jq '.fields[].name' \
     | grep -qx "Track"; then
  echo "    'Track' field already present"
else
  gh project field-create "$num" --owner "$ORG" --name "Track" \
    --data-type SINGLE_SELECT \
    --single-select-options "Nodes,Erosion,GUI,IO,Tiling,Engine,CLI,Docs" \
    >/dev/null
  echo "    created 'Track' field"
fi

echo
echo "Done."
echo "  Project: https://github.com/orgs/$ORG/projects/$num"
echo "  Next:    create the eight epics and their sub-issues (step 2)."
