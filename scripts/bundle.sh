#!/bin/sh
# Builds Switchyard.app and installs it into ~/Applications so macOS offers it as a browser.
set -eu
cd "$(dirname "$0")/.."

cargo build --release

app=target/Switchyard.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp bundle/Info.plist "$app/Contents/"
cp bundle/Switchyard.icns "$app/Contents/Resources/"
cp target/release/switchyard "$app/Contents/MacOS/"
codesign --force --sign - "$app"

dest="${INSTALL_DIR:-$HOME/Applications}"
mkdir -p "$dest"
pkill -x switchyard 2>/dev/null || true
rm -rf "$dest/Switchyard.app"
cp -R "$app" "$dest/"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$dest/Switchyard.app"

echo "Installed $dest/Switchyard.app"
echo "Open it and choose General → Make default, or System Settings → Desktop & Dock → Default web browser."
