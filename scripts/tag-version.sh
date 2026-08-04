#!/usr/bin/env bash
set -euo pipefail

version() { grep '^version' | head -1 | sed 's/.*"\(.*\)".*/\1/'; }
NEW=$(version < Cargo.toml)
OLD=$(git show HEAD^:Cargo.toml | version)
echo "old version: $OLD"
echo "new version: $NEW"

if [ "$NEW" = "$OLD" ]; then
    echo "versions match; no tag created"
    exit 0
fi

TAG="v$NEW"
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    echo "::error::tag $TAG already exists"
    exit 1
fi

echo "versions differ; tagging $TAG"
git config user.email "ci@getmantle.sh"
git config user.name "Rillet CI"
git tag "$TAG"
git push origin "$TAG"
