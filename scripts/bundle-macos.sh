#!/bin/sh
# Empacota o binário num harness.app — no macOS o ícone do Dock vem do bundle,
# não do `with_icon` do eframe (que só vale para Windows/Linux).
#
#   scripts/bundle-macos.sh [debug|release]   (padrão: release)
set -eu

PROFILE="${1:-release}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/$PROFILE/harness"
APP="$ROOT/target/harness.app"

[ "$PROFILE" = "release" ] && cargo build --release --manifest-path "$ROOT/Cargo.toml"
[ -x "$BIN" ] || { echo "binário não encontrado: $BIN"; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/harness"
cp "$ROOT/assets/icon/harness.icns" "$APP/Contents/Resources/harness.icns"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>harness</string>
  <key>CFBundleDisplayName</key><string>harness</string>
  <key>CFBundleExecutable</key><string>harness</string>
  <key>CFBundleIdentifier</key><string>sh.harness.harness</string>
  <key>CFBundleIconFile</key><string>harness</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
PLIST

touch "$APP"
echo "$APP"
