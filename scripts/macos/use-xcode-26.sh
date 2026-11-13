#!/bin/sh
# Selects Xcode 26 or later on a CI runner, whose actool compiles the Liquid Glass icon
# (assets/hydra.icon). The runner's default Xcode is kept when it's new enough.
set -eu

major=$(xcodebuild -version | awk 'NR == 1 { split($2, v, "."); print v[1] }')
if [ "$major" -lt 26 ]; then
    xcode=$(ls -d /Applications/Xcode_[2-9][0-9]*.app 2>/dev/null | sort -V | tail -1)
    [ -n "$xcode" ] || { echo "error: this runner has no Xcode 26 or later" >&2; exit 1; }
    sudo xcode-select -s "$xcode"
fi
xcodebuild -version
