#!/usr/bin/env bash
# Decides the next release version. Run from the repo root.
# Prints `skip` when nothing but docs or the site changed since the last vX.Y.Z tag, else the version to release:
#   no tags yet                         -> the workspace version in Cargo.toml
#   Cargo.toml version > last tag       -> the Cargo.toml version
#   otherwise                           -> the last tag with its patch number incremented
set -euo pipefail

last="$(git tag -l 'v*' | sort -V | tail -1)"
cargo="$(grep -m1 '^version' Cargo.toml | sed -E 's/^version *= *"([^"]*)".*/\1/' | tr -d '\r')"
if [ -z "$cargo" ]; then
    echo "can't read the workspace version from Cargo.toml" >&2
    exit 1
fi

if [ -z "$last" ]; then
    echo "$cargo"
    exit 0
fi

changed="$(git diff --name-only "$last" HEAD)"
if [ -z "$(printf '%s\n' "$changed" | grep -Ev '^docs/|^site/|\.md$' | grep -v '^$' || true)" ]; then
    echo skip
    exit 0
fi

last="${last#v}"
if [ "$cargo" != "$last" ] && [ "$(printf '%s\n%s\n' "$last" "$cargo" | sort -V | tail -1)" = "$cargo" ]; then
    echo "$cargo"
    exit 0
fi

IFS=. read -r major minor patch <<< "$last"
echo "$major.$minor.$((patch + 1))"
