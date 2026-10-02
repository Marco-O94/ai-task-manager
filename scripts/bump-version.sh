#!/usr/bin/env bash
# New release version (ARCHITECTURE.md «Versione e aggiornamenti»): sets the workspace version
# (the bundle's, tauri.conf.json has none), updates the workspace's own entries in Cargo.lock,
# commits "Release vX.Y.Z" and tags it. Pushing is left to you: the tag starts
# .github/workflows/release.yml.
#
#   scripts/bump-version.sh 0.2.0
#   git push origin main v0.2.0
#
# Under 0.x a new minor (0.1.x -> 0.2.0) is a REQUIRED update for the installed apps (blocking
# modal), a new patch an optional one; from 1.0 on only a new major is required.
set -euo pipefail
cd "$(dirname "$0")/.."

new=${1:-}
if [[ ! $new =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "usage: scripts/bump-version.sh X.Y.Z" >&2
    exit 2
fi
if [[ -n $(git status --porcelain) ]]; then
    echo "bump-version: the working tree is not clean" >&2
    exit 1
fi
old=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[[ -n $old ]] || {
    echo "bump-version: no [workspace.package] version in Cargo.toml" >&2
    exit 1
}
if git rev-parse -q --verify "refs/tags/v$new" >/dev/null; then
    echo "bump-version: tag v$new already exists" >&2
    exit 1
fi
# Only the line inside [workspace.package].
sed -i '' "/^\[workspace.package\]/,/^\[/s/^version = \"$old\"/version = \"$new\"/" Cargo.toml
# The workspace members' versions in the lock, no dependency touched.
cargo update --workspace --offline
git diff --stat
cargo check --locked -q -p atm-types
git commit -qam "Release v$new"
git tag -a "v$new" -m "AI Task Manager v$new"
echo "bump-version: $old -> $new, committed and tagged v$new. Publish with:"
echo "  git push origin $(git rev-parse --abbrev-ref HEAD) v$new"
