#!/usr/bin/env bash
# Cut a release. The next version comes from the conventional commits
# since the last tag (git-cliff: breaking `!` -> major, `feat` -> minor,
# anything else -> patch); the changelog section and the GitHub release
# notes are generated from the same commits. CI (triggered by the tag)
# builds the DMG and publishes the release.
#
#   scripts/release.sh            bump as derived from the commits
#   scripts/release.sh --major    force a major bump (also --minor/--patch)
#
# The first release (no tags yet) ships the current Cargo.toml version.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v git-cliff >/dev/null || { echo "git-cliff missing: brew install git-cliff" >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "working tree is not clean" >&2; exit 1; }
git fetch --tags origin
[ "$(git rev-parse main)" = "$(git rev-parse origin/main)" ] || { echo "main is not in sync with origin/main" >&2; exit 1; }

current=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
latest=$(git tag --list 'v*' --sort=-v:refname | head -1)

if [ -n "$latest" ]; then
    next=$(git-cliff --bumped-version "$@")
    next=${next#v}
    [ "$next" != "$current" ] || { echo "nothing to release: no conventional commits since $latest" >&2; exit 1; }
else
    [ "$#" -eq 0 ] || { echo "no tags yet: the first release ships the current version ($current)" >&2; exit 1; }
    next=$current
fi

echo "releasing v$next (was ${latest:-untagged}, Cargo.toml has $current)"
# The workspace pins the version in one place; dependency tables are
# indented, so the unindented pattern matches only [workspace.package].
sed -i '' "s/^version = \"$current\"/version = \"$next\"/" Cargo.toml
cargo build -p gliff --quiet   # refresh Cargo.lock
if [ -n "$latest" ]; then
    git-cliff --bump "$@" -o CHANGELOG.md
else
    git-cliff --tag "v$next" -o CHANGELOG.md
fi

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore(release): v$next"
git tag "v$next"
git push origin main "v$next"
echo "tagged v$next; CI builds the DMG and publishes the GitHub release"
