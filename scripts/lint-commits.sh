#!/usr/bin/env bash
# Fail if any commit subject in the given range is not a conventional
# commit: type(scope)!: description. CI runs this over the new commits
# of every pull request and push to main.
set -uo pipefail
cd "$(dirname "$0")/.."

range=${1:?usage: lint-commits.sh <base>..HEAD}
pattern='^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\([a-zA-Z0-9._/-]+\))?(!)?: .+'

status=0
while IFS= read -r subject; do
    [ -n "$subject" ] || continue
    if ! [[ $subject =~ $pattern ]]; then
        echo "not a conventional commit: $subject" >&2
        status=1
    fi
done < <(git log --format=%s $range)

if [ "$status" -ne 0 ]; then
    echo 'Subjects must match: type(scope)!: description' >&2
    echo 'types: feat fix docs style refactor perf test build ci chore revert' >&2
fi
exit "$status"
