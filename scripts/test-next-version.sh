#!/usr/bin/env bash
# Checks scripts/next-version.sh against throwaway git repos. Run from Git Bash:
#   bash scripts/test-next-version.sh
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/next-version.sh"
fails=0
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

# new_repo <cargo version> -> prints the path of a repo with one commit
new_repo() {
    local dir
    dir="$(mktemp -d "$root/repo.XXXXXX")"
    (
        cd "$dir"
        git init -q
        git config user.email test@example.com
        git config user.name test
        git config commit.gpgsign false
        git config core.autocrlf false
        printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\n' "$1" > Cargo.toml
        mkdir -p src docs
        echo 'fn main() {}' > src/main.rs
        git add -A
        git commit -qm init
    )
    echo "$dir"
}

commit_file() { # <repo> <file> <content>
    (cd "$1" && mkdir -p "$(dirname "$2")" && echo "$3" > "$2" && git add -A && git commit -qm "change $2")
}

check() { # <name> <repo> <expected>
    local got
    got="$(cd "$2" && bash "$script")" || got="<exit $?>"
    if [ "$got" = "$3" ]; then
        echo "ok   - $1: $got"
    else
        echo "FAIL - $1: expected '$3', got '$got'"
        fails=$((fails + 1))
    fi
}

r="$(new_repo 0.4.0)"
check "no tags gives the Cargo version" "$r" "0.4.0"

r="$(new_repo 0.4.0)"
(cd "$r" && git tag v0.4.0)
commit_file "$r" docs/guide.md "more docs"
commit_file "$r" README.md "readme"
check "docs-only changes skip" "$r" "skip"

r="$(new_repo 0.4.0)"
(cd "$r" && git tag v0.4.0)
check "no changes since the tag skip" "$r" "skip"

r="$(new_repo 0.4.0)"
(cd "$r" && git tag v0.4.0)
commit_file "$r" src/main.rs "fn main() { println!(); }"
check "code change, Cargo == last tag gives patch+1" "$r" "0.4.1"

r="$(new_repo 0.4.0)"
(cd "$r" && git tag v0.3.0 && git tag v0.3.7)
commit_file "$r" src/main.rs "fn main() { println!(); }"
check "Cargo higher than last tag gives the Cargo version" "$r" "0.4.0"

r="$(new_repo 0.4.0)"
(cd "$r" && git tag v0.4.8 && git tag v0.4.9)
commit_file "$r" src/lib.rs "pub fn f() {}"
check "v0.4.9 gives 0.4.10 (numeric)" "$r" "0.4.10"

r="$(new_repo 0.4.0)"
(cd "$r" && git tag v0.4.9 && git tag v0.4.10)
commit_file "$r" src/lib.rs "pub fn f() {}"
check "v0.4.10 sorts above v0.4.9" "$r" "0.4.11"

if [ "$fails" -ne 0 ]; then
    echo "$fails failed"
    exit 1
fi
echo "all passed"
