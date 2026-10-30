#!/usr/bin/env bash
# Lay out Patina.app around a universal binary and the libmpv it carries.
#
#   packaging/macos-bundle.sh <dir of libraries from macos-libmpv.sh>
#
# Expects release builds for both aarch64-apple-darwin and
# x86_64-apple-darwin. A .app is a directory of a particular shape with a
# plist naming the executable; nothing here needs Xcode beyond the tools
# every Mac has. The bundle is signed ad hoc and no more: that gives it a
# code identity (arm64 Macs run nothing unsigned) but proves nothing about
# who built it, so Gatekeeper still refuses it as a download.
set -euo pipefail

libs=${1:?usage: macos-bundle.sh <dir of libraries>}
root="$(cd "$(dirname "$0")/.." && pwd)"
app="$root/dist/Patina.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"

lipo -create \
    "$root/target/aarch64-apple-darwin/release/patina" \
    "$root/target/x86_64-apple-darwin/release/patina" \
    -output "$app/Contents/MacOS/patina"
chmod +x "$app/Contents/MacOS/patina"

# libmpv and what it links, where Patina looks first
# (<executable>/../Frameworks) and where the executable's rpath points.
cp "$libs"/*.dylib "$app/Contents/Frameworks/"

# The .icns, from the sizes packaging/icons.sh renders out of the SVG. The
# names are Apple's: iconutil refuses any it doesn't know, and a size serves
# both its own slot and the retina slot of the size below it.
iconset="$root/dist/patina.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
icon() { cp "$root/packaging/icons/patina-$1.png" "$iconset/$2"; }
icon 16 icon_16x16.png
icon 32 icon_16x16@2x.png
icon 32 icon_32x32.png
icon 64 icon_32x32@2x.png
icon 128 icon_128x128.png
icon 256 icon_128x128@2x.png
icon 256 icon_256x256.png
icon 512 icon_256x256@2x.png
icon 512 icon_512x512.png
icon 1024 icon_512x512@2x.png
iconutil --convert icns "$iconset" --output "$app/Contents/Resources/patina.icns"
rm -rf "$iconset"

cp "$root/LICENSE" "$app/Contents/Resources/LICENSE"
cp "$root/ui/icons/LICENSE" "$app/Contents/Resources/LICENSE-lucide"
cp "$root/packaging/THIRD-PARTY.md" "$app/Contents/Resources/THIRD-PARTY.md"

version="$(grep -m1 '^version' "$root/Cargo.toml" | cut -d'"' -f2)"

# The files Finder may open with Patina, one document type per kind, from
# the table Patina itself reads; and folders, for Open With only.
document_types() {
    local kind name
    for kind in video audio playlist; do
        case $kind in
            video) name=Video ;;
            audio) name=Audio ;;
            playlist) name=Playlist ;;
        esac
        printf '    <dict>\n'
        printf '      <key>CFBundleTypeName</key><string>%s</string>\n' "$name"
        printf '      <key>CFBundleTypeRole</key><string>Viewer</string>\n'
        printf '      <key>LSHandlerRank</key><string>Default</string>\n'
        printf '      <key>CFBundleTypeExtensions</key>\n      <array>\n'
        awk -F'\t' -v kind="$kind" \
            '!/^#/ && $2 == kind { printf "        <string>%s</string>\n", $1 }' \
            "$root/packaging/media-types.tsv"
        printf '      </array>\n    </dict>\n'
    done
    printf '    <dict>\n'
    printf '      <key>CFBundleTypeName</key><string>Folder</string>\n'
    printf '      <key>CFBundleTypeRole</key><string>Viewer</string>\n'
    printf '      <key>LSHandlerRank</key><string>Alternate</string>\n'
    printf '      <key>LSItemContentTypes</key><array><string>public.folder</string></array>\n'
    printf '    </dict>\n'
}

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Patina</string>
  <key>CFBundleDisplayName</key><string>Patina</string>
  <key>CFBundleIdentifier</key><string>video.patina.Patina</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleExecutable</key><string>patina</string>
  <key>CFBundleIconFile</key><string>patina</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <!-- What the libraries in Frameworks are built for. -->
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <!-- Video plays on the integrated GPU of a two-GPU Mac, as IINA's does. -->
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
$(document_types)
  </array>
  <!-- patina:// links, from browser extensions and web pages. -->
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>video.patina.Patina</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>CFBundleURLSchemes</key><array><string>patina</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# The libraries first: signing the bundle checks what is nested in it.
# Signing them ad hoc replaces IINA's own signatures, which a bundle signed
# by someone else could not keep anyway.
codesign --force --sign - "$app/Contents/Frameworks/"*.dylib
codesign --force --sign - "$app"
codesign --verify --strict "$app"

echo "built $app"
