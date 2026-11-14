#!/usr/bin/env bash
# Builds DrukhalManager for Windows and gathers it with the GTK it runs on
# into one folder that runs on a PC without MSYS2. Run from the repository
# root in an MSYS2 UCRT64 shell with the packages the Windows CI job
# installs (plus librsvg and adwaita-icon-theme):
#
#     packaging/windows/bundle.sh [OUT_DIR]
#
# The folder is laid out like an MSYS2 prefix (bin/, lib/, share/), so GLib,
# GTK and gdk-pixbuf find their data next to their DLLs on their own.

set -euo pipefail

out=${1:-target/windows-bundle/DrukhalManager}
prefix=${MINGW_PREFIX:?run this in an MSYS2 UCRT64 shell}

cargo build --release --locked

rm -rf "$out"
mkdir -p "$out/bin" "$out/lib" "$out/share/glib-2.0/schemas" "$out/share/icons"

cp target/release/drukhal-manager.exe "$out/bin/DrukhalManager.exe"

# The image loaders: the app icon is an SVG, and mod thumbnails can be
# anything gdk-pixbuf reads.
pixbuf=lib/gdk-pixbuf-2.0/2.10.0
mkdir -p "$out/$pixbuf"
cp -r "$prefix/$pixbuf/loaders" "$out/$pixbuf/"
rm -f "$out/$pixbuf"/loaders/*.a
# The cache lists loaders by absolute path; gdk-pixbuf on Windows takes
# relative ones from its install folder.
GDK_PIXBUF_MODULEDIR="$(cygpath -m "$out/$pixbuf/loaders")" gdk-pixbuf-query-loaders \
    | sed -E 's#^"[^"]*/(lib/gdk-pixbuf-2\.0/[^"]*)"#"\1"#' > "$out/$pixbuf/loaders.cache"
if grep -q '^"[A-Za-z]:' "$out/$pixbuf/loaders.cache"; then
    echo "loaders.cache still has absolute paths" >&2
    exit 1
fi

# Every DLL the app and the loaders load, from the MSYS2 prefix (not
# Windows' own).
dlls() {
    ldd "$@" | awk -v bin="$(cygpath -u "$prefix")/bin/" 'index($3, bin) == 1 { print $3 }' | sort -u
}
dlls "$out/bin/DrukhalManager.exe" "$out/$pixbuf"/loaders/*.dll | xargs -r cp -t "$out/bin/"
# What GLib needs to start other programs.
cp "$prefix"/bin/gspawn-win64-helper*.exe "$out/bin/" 2>/dev/null || true

# GSettings schemas: GTK's own, read by the file chooser.
cp "$prefix"/share/glib-2.0/schemas/*.xml "$out/share/glib-2.0/schemas/"
glib-compile-schemas "$out/share/glib-2.0/schemas"

# Icons: the Adwaita theme, hicolor (where the app's own icons go) and the
# app's icons.
cp -r "$prefix/share/icons/Adwaita" "$prefix/share/icons/hicolor" "$out/share/icons/"
mkdir -p "$out/share/icons/hicolor/scalable/apps" "$out/share/icons/hicolor/symbolic/apps"
cp data/icons/io.github.dukko.DrukhalManager.svg "$out/share/icons/hicolor/scalable/apps/"
cp data/icons/hicolor/symbolic/apps/*.svg "$out/share/icons/hicolor/symbolic/apps/"
for theme in "$out"/share/icons/*/; do
    gtk4-update-icon-cache -q -t -f "$theme"
done

cp LICENSE NOTICE "$out/"

echo "Bundled into $out ($(du -sh "$out" | cut -f1))"
