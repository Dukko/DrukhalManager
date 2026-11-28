#!/usr/bin/env bash
# Builds DrukhalManager for Windows and gathers it with the DLLs it needs
# into one folder that runs on a PC without MSYS2. Run from the repository
# root in an MSYS2 UCRT64 shell with the packages the Windows CI job
# installs:
#
#     packaging/windows/bundle.sh [OUT_DIR]
#
# The interface (iced) draws itself with the system's graphics drivers and
# embeds its fonts and icons, so the bundle is the program, the runtime
# DLLs from MSYS2 (gettext's, the GCC runtime) and the translations. The
# folder is laid out like a prefix (bin/, share/locale/), where the app
# looks for its translations.

set -euo pipefail

out=${1:-target/windows-bundle/DrukhalManager}
prefix=${MINGW_PREFIX:?run this in an MSYS2 UCRT64 shell}

cargo build --release --locked

rm -rf "$out"
mkdir -p "$out/bin"

cp target/release/drukhal-manager.exe "$out/bin/DrukhalManager.exe"

# The UI translations, for the languages po/ has so far.
if [ -d target/i18n/locale ]; then
    mkdir -p "$out/share/locale"
    cp -r target/i18n/locale/. "$out/share/locale/"
fi

# Every DLL the app loads from the MSYS2 prefix (not Windows' own).
ldd "$out/bin/DrukhalManager.exe" \
    | awk -v bin="$(cygpath -u "$prefix")/bin/" 'index($3, bin) == 1 { print $3 }' \
    | sort -u | xargs -r cp -t "$out/bin/"

cp LICENSE NOTICE data/fonts/*-OFL.txt "$out/"
cp data/icons/material/LICENSE "$out/MaterialSymbols-LICENSE.txt"

echo "Bundled into $out ($(du -sh "$out" | cut -f1))"
