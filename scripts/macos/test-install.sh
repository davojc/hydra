#!/bin/sh
# Runs install.sh against a release folder on disk, in a throwaway HOME, and checks what it
# did: the app, the hydra link, PATH added once, `hydra init`, and a bad checksum refused.
#
#   scripts/macos/test-install.sh <dist dir holding hydra-macos-aarch64.zip and its .sha256>
set -eu

dist=$(cd "$1" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
home=$(mktemp -d)
trap 'rm -rf "$home"' EXIT

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

run_install() {
    env -i HOME="$home" PATH=/usr/bin:/bin:/usr/sbin:/sbin SHELL=/bin/zsh \
        HYDRA_RELEASE_URL="file://$1" sh "$here/install.sh"
}

run_install "$dist" >"$home/out1.txt" 2>&1 || { cat "$home/out1.txt"; fail "install failed"; }
app="$home/Applications/Hydra.app"
[ -x "$app/Contents/MacOS/hydraui" ] || fail "no hydraui in $app"
[ -L "$home/.local/bin/hydra" ] || fail "no hydra link in ~/.local/bin"
[ "$(readlink "$home/.local/bin/hydra")" = "$app/Contents/MacOS/hydra" ] ||
    fail "the hydra link doesn't point into the app"
HOME="$home" "$home/.local/bin/hydra" --version | grep -q '^hydra ' ||
    fail "hydra --version through the link didn't answer"
[ -d "$home/.hydra" ] || fail "hydra init didn't make ~/.hydra"
[ "$(grep -c 'local/bin' "$home/.zprofile")" = 1 ] || fail "PATH wasn't added to ~/.zprofile"
plutil -lint "$app/Contents/Info.plist" >/dev/null || fail "Info.plist isn't valid"

# Again: reinstalls, and adds PATH no second time.
run_install "$dist" >"$home/out2.txt" 2>&1 || { cat "$home/out2.txt"; fail "reinstall failed"; }
[ "$(grep -c 'local/bin' "$home/.zprofile")" = 1 ] || fail "PATH was added twice"

# A download that doesn't match its checksum installs nothing.
bad=$(mktemp -d)
cp "$dist/hydra-macos-aarch64.zip" "$bad/"
printf '%s  hydra-macos-aarch64.zip\n' "$(printf '0%.0s' $(seq 64))" >"$bad/hydra-macos-aarch64.zip.sha256"
rm -rf "$app"
if run_install "$bad" >"$home/out3.txt" 2>&1; then
    fail "a bad checksum was installed"
fi
grep -q "doesn't match its checksum" "$home/out3.txt" || fail "a bad checksum didn't say why"
[ ! -e "$app" ] || fail "a bad checksum left an app behind"
rm -rf "$bad"

echo "install.sh: all checks passed"
