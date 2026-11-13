#!/bin/sh
# Builds Hydra.app from release builds of hydraui and hydra, and zips it for a release.
#
#   scripts/macos/make-app.sh <hydraui> <hydra> <version> <out dir>
#
# Writes <out dir>/hydra-macos-aarch64.zip (Hydra.app inside) and its .sha256. Both programs
# sit in Contents/MacOS, so hydraui finds hydra beside it and its updater replaces both there.
# The app isn't signed as a bundle: each program keeps the signature its build gave it, so an
# update that swaps one in place doesn't break a seal over the rest.
set -eu

if [ "$#" -ne 4 ]; then
    echo "usage: $0 <hydraui> <hydra> <version> <out dir>" >&2
    exit 2
fi
hydraui=$1
hydra=$2
version=$3
out=$4
here=$(cd "$(dirname "$0")" && pwd)
icon_png="$here/../../assets/hydra.png"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/Hydra.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$hydraui" "$app/Contents/MacOS/hydraui"
cp "$hydra" "$app/Contents/MacOS/hydra"
chmod 755 "$app/Contents/MacOS/hydraui" "$app/Contents/MacOS/hydra"

# The icon: every size macOS asks for, from the 512 px PNG.
set_dir="$work/hydra.iconset"
mkdir -p "$set_dir"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$icon_png" --out "$set_dir/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    if [ "$double" -le 512 ]; then
        sips -z "$double" "$double" "$icon_png" --out "$set_dir/icon_${size}x${size}@2x.png" >/dev/null
    fi
done
cp "$icon_png" "$set_dir/icon_512x512@2x.png"
iconutil -c icns "$set_dir" -o "$app/Contents/Resources/hydra.icns"

cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Hydra</string>
    <key>CFBundleDisplayName</key>
    <string>Hydra</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.davojc.hydra</string>
    <key>CFBundleExecutable</key>
    <string>hydraui</string>
    <key>CFBundleIconFile</key>
    <string>hydra</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null

mkdir -p "$out"
zip="hydra-macos-aarch64.zip"
rm -f "$out/$zip"
ditto -c -k --keepParent "$app" "$out/$zip"
(cd "$out" && shasum -a 256 "$zip" >"$zip.sha256" && cat "$zip.sha256")
