#!/usr/bin/env bash
set -euo pipefail

version_pattern='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
if [[ $# -ne 1 || ! ${1:-} =~ $version_pattern ]]; then
    printf 'Usage: %s X.Y.Z\n' "$0" >&2
    exit 2
fi
version=$1
tag="v$version"

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
status=$(git status --porcelain)
if [[ -n $status ]]; then
    printf 'Commit or stash your changes before preparing a release.\n' >&2
    exit 1
fi
if git show-ref --verify --quiet "refs/tags/$tag"; then
    printf 'Tag %s already exists.\n' "$tag" >&2
    exit 1
fi

manifest=$(awk -v version="$version" '
    /^[[:space:]]*\[/ { package = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*(#.*)?$/) }
    package && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ {
        sub(/"[^"]*"/, "\"" version "\"")
        count++
    }
    { print }
    END { if (count != 1) exit 1 }
' Cargo.toml) || {
    printf 'Could not find the package version in Cargo.toml.\n' >&2
    exit 1
}

# Restore the initially clean release files if updating or committing them fails.
trap 'git restore --source=HEAD --staged --worktree -- Cargo.toml Cargo.lock' EXIT
printf '%s\n' "$manifest" > Cargo.toml
cargo update -p clipserve

if ! git diff --quiet -- Cargo.toml Cargo.lock; then
    git commit -m "build: bump version to $version" -- Cargo.toml Cargo.lock
fi
trap - EXIT

release_commit=$(git rev-parse HEAD)
printf 'Release commit: %s\n' "$(git rev-parse --short "$release_commit")"
git tag -a -m "Release $tag" "$tag" "$release_commit"
printf 'Created tag %s. After CI passes, publish it with: git push origin %s\n' "$tag" "$tag"
