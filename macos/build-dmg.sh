#!/bin/bash
# Build mac-gliff.app and wrap it in a DMG.
#
# The bundle carries the whole brew GTK stack inside Contents/Frameworks
# (load commands rewritten to @executable_path), so it runs on machines
# without Homebrew. Ad-hoc signed only — no Developer ID, no notarization;
# Gatekeeper will ask on the first launch of a downloaded copy.
#
# Needs on the build machine: Rust >= 1.92, `brew install pkg-config
# gtk4 libadwaita`, and the build-time tools gdk-pixbuf-query-loaders /
# glib-compile-schemas (they come with gtk4's deps).
#
# Written for macOS's bash 3.2: no associative arrays, no mapfile.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
BREW=${BREW_PREFIX:-$(brew --prefix)}

APP="mac-gliff.app"
BUNDLE="dist/$APP"
FW="$BUNDLE/Contents/Frameworks"
RES="$BUNDLE/Contents/Resources"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
: > "$WORK/machos"   # every Mach-O that needs load-command rewrites
: > "$WORK/seen"     # "<resolved src path> <bundled name>" per copied dylib

echo "== build"
cargo build --release --locked -p gliff

echo "== bundle"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$FW" "$RES"

cp target/release/mac-gliff "$BUNDLE/Contents/MacOS/mac-gliff.bin"
cp macos/AppIcon.icns "$RES/AppIcon.icns"

cat > "$BUNDLE/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>          <string>mac-gliff</string>
    <key>CFBundleIdentifier</key>          <string>io.github.teomurgi.mac-gliff</string>
    <key>CFBundleName</key>                <string>mac-gliff</string>
    <key>CFBundleDisplayName</key>         <string>mac-gliff</string>
    <key>CFBundlePackageType</key>         <string>APPL</string>
    <key>CFBundleShortVersionString</key>  <string>$VERSION</string>
    <key>CFBundleVersion</key>             <string>$VERSION</string>
    <key>CFBundleIconFile</key>            <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>      <string>12.0</string>
    <key>LSApplicationCategoryType</key>   <string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key>     <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key> <true/>
</dict>
</plist>
EOF

cat > "$BUNDLE/Contents/MacOS/mac-gliff" <<'EOF'
#!/bin/sh
# Point GTK at the resources carried inside the bundle.
HERE="$(cd "$(dirname "$0")" && pwd)"
RES="$HERE/../Resources"
export GDK_PIXBUF_MODULE_FILE="$RES/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
export GSETTINGS_SCHEMA_DIR="$RES/share/glib-2.0/schemas"
export XDG_DATA_DIRS="$RES/share${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
exec "$HERE/mac-gliff.bin" "$@"
EOF
chmod +x "$BUNDLE/Contents/MacOS/mac-gliff"

# --- dylib relocation ------------------------------------------------------
# Collect every brew dylib reachable from the seeds (the binary, and the
# gdk-pixbuf loaders which are dlopen'd rather than linked), copy them
# into Frameworks, and rewrite every load command to
# @executable_path/../Frameworks/<name>.

rpaths_of() { # file
    otool -l "$1" | awk '/LC_RPATH/{getline; getline; print $2}'
}

resolve_dep() { # dep macho -> prints the resolved brew path, or nothing
    local dep=$1 macho=$2 rp
    case "$dep" in
        /usr/lib/*|/System/*) return ;;
        @rpath/*)
            for rp in $(rpaths_of "$macho"); do
                rp=${rp/@loader_path/$(dirname "$macho")}
                rp=${rp/@executable_path/$BUNDLE/Contents/MacOS}
                if [ -e "$rp/${dep#@rpath/}" ]; then
                    dep="$rp/${dep#@rpath/}"
                    break
                fi
            done
            ;;
    esac
    case "$dep" in
        "$BREW"/*) realpath "$dep" || true ;;
    esac
}

deps_of() { # macho -> brew deps (resolved), one per line
    # Record the BUNDLED counterpart for rewrites — never rewrite the
    # brew originals we only read.
    case "$1" in
        "$BUNDLE"/*) echo "$1" >> "$WORK/machos" ;;
        *) echo "$FW/$(basename "$1")" >> "$WORK/machos" ;;
    esac
    otool -L "$1" | tail -n +2 | sed 's/(.*//;s/^[[:space:]]*//;s/[[:space:]]*$//' | while IFS= read -r dep; do
        [ -n "$dep" ] || continue
        resolved=$(resolve_dep "$dep" "$1")
        if [ -n "${resolved:-}" ]; then echo "$resolved"; fi
    done
}

collect_from() { # seed files...
    printf '%s\n' "$@" > "$WORK/frontier"
    while [ -s "$WORK/frontier" ]; do
        while IFS= read -r m; do
            deps_of "$m"
        done < "$WORK/frontier" > "$WORK/queue"
        sort -u "$WORK/queue" > "$WORK/queue.u"
        # drop already-copied ones
        cut -d' ' -f1 "$WORK/seen" > "$WORK/seen.paths"
        grep -v -x -F -f "$WORK/seen.paths" "$WORK/queue.u" > "$WORK/new" || true
        : > "$WORK/frontier"
        if [ -s "$WORK/new" ]; then
            while IFS= read -r src; do
                cp "$src" "$FW/$(basename "$src")"
                echo "$src $(basename "$src")" >> "$WORK/seen"
                echo "$src" >> "$WORK/frontier"
            done < "$WORK/new"
        fi
    done
}

collect_from "$BUNDLE/Contents/MacOS/mac-gliff.bin"

# gdk-pixbuf loaders are dlopen'd, not linked: seed them too. Brew's
# own loaders are .so; librsvg ships its svg loader as both .so and
# .dylib — seed every module file so nothing keeps brew load commands.
LOADERS_DIR=$(echo "$BREW"/lib/gdk-pixbuf-2.0/*/loaders)
mkdir -p "$RES/lib/gdk-pixbuf-2.0/2.10.0/loaders"
# Copy the loaders' contents into the mkdir'd directory: /bin/cp's
# create-the-destination path for `cp -R src dst` fails with ENOENT
# on this machine, while copying into an existing directory works.
cp -RLL "$LOADERS_DIR"/. "$RES/lib/gdk-pixbuf-2.0/2.10.0/loaders"
LOADERS=("$RES"/lib/gdk-pixbuf-2.0/2.10.0/loaders/*)
collect_from "${LOADERS[@]}"

echo "collected $(wc -l < "$WORK/seen" | tr -d ' ') libraries"

lookup() { # resolved-or-literal dep path -> bundled name
    awk -v k="$1" '$1 == k {print $2; exit}' "$WORK/seen"
}
# Rewrite load commands on every collected Mach-O.
sort -u "$WORK/machos" | while IFS= read -r m; do
    [ -n "$m" ] || continue
    case "$m" in "$BUNDLE"/*) ;; *) echo "skip non-bundle $m" >&2; continue ;; esac
    otool -L "$m" | tail -n +2 | sed 's/(.*//;s/^[[:space:]]*//;s/[[:space:]]*$//' | sort -u | while IFS= read -r dep; do
        [ -n "$dep" ] || continue
        resolved=$(resolve_dep "$dep" "$m")
        name=$(lookup "${resolved:-$dep}")
        # @rpath deps resolve against the reader's rpaths, which differ
        # between the Cellar original and the bundled copy: fall back to
        # a collected library with the same basename.
        if [ -z "$name" ]; then
            name=$(awk -v b="$(basename "$dep")" '
                BEGIN { sub(/\.dylib$/, "", b) }
                {
                    c = $2; sub(/\.dylib$/, "", c)
                    if (c == b || index(c, b ".") == 1) { print $2; exit }
                }' "$WORK/seen")
        fi
        if [ -n "$name" ]; then
            install_name_tool -change "$dep" "@executable_path/../Frameworks/$name" "$m"
        fi
    done
done


# Rewrite install ids of the bundled dylibs.
while IFS= read -r pair; do
    set -- $pair
    install_name_tool -id "@executable_path/../Frameworks/$2" "$FW/$2"
done < "$WORK/seen"


# Brew builds some libs with an rpath into their own keg; every load
# command now resolves through @executable_path, so strip the leftover
# Cellar paths instead of shipping the build machine's layout.
sort -u "$WORK/machos" | while IFS= read -r m; do
    [ -n "$m" ] || continue
    for rp in $(rpaths_of "$m"); do
        case "$rp" in "$BREW"/*) install_name_tool -delete_rpath "$rp" "$m" ;; esac
    done
done

# install_name_tool invalidated the signatures; re-sign before anything
# loads the rewritten libraries (the kernel kills unsigned loads).
find "$FW" "$RES/lib" "$BUNDLE/Contents/MacOS" -type f \
    \( -name '*.dylib' -o -name '*.so' -o -name '*.bin' \) \
    -exec codesign --force --sign - {} +
# --- GTK resources ---------------------------------------------------------
echo "== resources"

# pixbuf loaders cache, pointing at the bundled loaders.
# Query the brew originals (their deps load there), then point the
# cache at the bundled copies — never load the rewritten loaders.
# dlopen expands @executable_path in the module paths, so the cache
# stays valid wherever the app bundle is installed.
"$BREW/bin/gdk-pixbuf-query-loaders" "$LOADERS_DIR"/* \
    | sed "s|^\"$LOADERS_DIR|\"@executable_path/../Resources/lib/gdk-pixbuf-2.0/2.10.0/loaders|" \
    > "$RES/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"

# gsettings schemas (gtk4, libadwaita, gsettings-desktop-schemas, ...)
mkdir -p "$RES/share/glib-2.0/schemas"
cp -RL "$BREW"/share/glib-2.0/schemas/*.xml "$RES/share/glib-2.0/schemas/" 2>/dev/null || true
"$BREW/bin/glib-compile-schemas" "$RES/share/glib-2.0/schemas"

# icon themes and the shared-mime-info database (clipboard mime types)
mkdir -p "$RES/share/icons" "$RES/share/mime"
cp -RLL "$BREW/share/icons/hicolor" "$RES/share/icons/" 2>/dev/null || true
[ -d "$BREW/share/icons/Adwaita" ] && cp -RLL "$BREW/share/icons/Adwaita" "$RES/share/icons/" || true
cp -RLL "$BREW/share/mime/." "$RES/share/mime/" 2>/dev/null || true

# --- sign + dmg ------------------------------------------------------------
echo "== sign"
codesign --force --sign - "$BUNDLE"
codesign --verify --strict "$BUNDLE"

echo "== dmg"
STAGE=$(mktemp -d)
cp -R "$BUNDLE" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
mkdir -p dist
hdiutil create -volname mac-gliff -srcfolder "$STAGE" -format UDZO -ov \
    "dist/mac-gliff-$VERSION-$ARCH.dmg" >/dev/null
rm -rf "$STAGE"

echo
echo "OK: dist/mac-gliff-$VERSION-$ARCH.dmg ($(du -h "dist/mac-gliff-$VERSION-$ARCH.dmg" | cut -f1))"
