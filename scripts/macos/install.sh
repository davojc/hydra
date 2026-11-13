#!/bin/sh
# Installs hydra on a Mac: Hydra.app (hydraui) in ~/Applications, and the hydra command.
#
#   curl -fsSL https://github.com/davojc/hydra/releases/latest/download/install.sh | sh
#
# It downloads the latest release's Hydra.app, checks its SHA-256, puts it in ~/Applications,
# links ~/.local/bin/hydra to the hydra inside it (adding ~/.local/bin to your PATH when it
# isn't), and runs `hydra init`. Run it again to reinstall; later updates come from hydraui's
# Update button or `hydra update`. Nothing needs sudo, and files curl downloads aren't
# quarantined, so macOS opens the app without the unsigned-app prompt.
#
# For testing, these change where it reads and writes:
#   HYDRA_RELEASE_URL  the release's download folder (default: the latest GitHub release)
#   HYDRA_APP_DIR      where Hydra.app goes (default: ~/Applications)
#   HYDRA_BIN_DIR      where the hydra link goes (default: ~/.local/bin)
#   HYDRA_PROFILE      the shell profile PATH is added to (default: by $SHELL)
set -eu

release=${HYDRA_RELEASE_URL:-https://github.com/davojc/hydra/releases/latest/download}
app_dir=${HYDRA_APP_DIR:-$HOME/Applications}
bin_dir=${HYDRA_BIN_DIR:-$HOME/.local/bin}
zip=hydra-macos-aarch64.zip

say() { printf '%s\n' "$*"; }
fail() {
    printf 'hydra install: %s\n' "$*" >&2
    exit 1
}

[ "$(uname -s)" = Darwin ] || fail "this installer is for macOS; on Windows run hydra-setup.exe"
[ "$(uname -m)" = arm64 ] || fail "hydra's Mac build is for Apple silicon (M1 and later) only"

# Only the hydraui this would replace: another copy (a dev build) can keep running.
if pgrep -f "$app_dir/Hydra.app/Contents/MacOS/hydraui" >/dev/null 2>&1; then
    fail "Hydra is open: quit it first, then run this again"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

say "Downloading Hydra.app…"
curl -fsSL "$release/$zip" -o "$work/$zip" || fail "couldn't download $release/$zip"
curl -fsSL "$release/$zip.sha256" -o "$work/$zip.sha256" ||
    fail "couldn't download $release/$zip.sha256"
want=$(awk '{print tolower($1)}' "$work/$zip.sha256")
got=$(shasum -a 256 "$work/$zip" | awk '{print $1}')
[ -n "$want" ] && [ "$want" = "$got" ] ||
    fail "the download doesn't match its checksum (expected $want, got $got): not installing"

mkdir -p "$app_dir"
ditto -x -k "$work/$zip" "$work/unzipped"
[ -x "$work/unzipped/Hydra.app/Contents/MacOS/hydraui" ] ||
    fail "the download has no Hydra.app in it: not installing"
rm -rf "$app_dir/Hydra.app"
mv "$work/unzipped/Hydra.app" "$app_dir/Hydra.app"
# Only a browser marks downloads; clear it in case this zip came by one.
xattr -dr com.apple.quarantine "$app_dir/Hydra.app" 2>/dev/null || true

mkdir -p "$bin_dir"
ln -sf "$app_dir/Hydra.app/Contents/MacOS/hydra" "$bin_dir/hydra"
if [ -f "$bin_dir/hydraui" ] && [ ! -L "$bin_dir/hydraui" ]; then
    say "Note: $bin_dir/hydraui is an older copy, no longer needed: open Hydra from Applications."
fi

case ":$PATH:" in
*":$bin_dir:"*) ;;
*)
    if [ -n "${HYDRA_PROFILE:-}" ]; then
        profile=$HYDRA_PROFILE
    else
        case "${SHELL:-}" in
        */bash) profile=$HOME/.bash_profile ;;
        *) profile=$HOME/.zprofile ;;
        esac
    fi
    line="export PATH=\"$bin_dir:\$PATH\""
    if ! grep -qsF "$line" "$profile"; then
        printf '\n# hydra\n%s\n' "$line" >>"$profile"
        say "Added $bin_dir to your PATH in $profile (new terminals pick it up)."
    fi
    ;;
esac

"$bin_dir/hydra" init

say ""
say "Hydra is installed: open it from Applications, Launchpad or Spotlight."
say "The hydra command is $bin_dir/hydra."
