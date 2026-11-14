#!/usr/bin/env bash
# Regenerates the translation template from the `t!`, `tf!`, `tn!`, `tnf!`,
# `tc!` and `tnc!` calls in the sources.
#
#     cargo install xtr --version 0.1.11 --locked
#     po/update-pot.sh [OUTPUT.pot]
#
# Run it after changing UI strings, then commit po/drukhal-manager.pot.
# `po/check-pot.sh` tells you when it is out of date.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v xtr >/dev/null; then
    echo "xtr is missing: cargo install xtr --version 0.1.11 --locked" >&2
    exit 1
fi

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
output=${1:-po/drukhal-manager.pot}

# The keywords mirror the macros in src/i18n.rs. Nothing else is extracted,
# so a string only becomes translatable by being wrapped in one of them.
xtr src/main.rs \
    --output "$output" \
    --package-name DrukhalManager \
    --package-version "$version" \
    --msgid-bugs-address "https://github.com/Dukko/DrukhalManager/issues" \
    --copyright-holder Dukko \
    -k t \
    -k tf \
    -k 'tn:1,2' \
    -k 'tnf:1,2' \
    -k 'tc:1c,2' \
    -k 'tnc:1c,2,3'
