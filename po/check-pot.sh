#!/usr/bin/env bash
# Fails when po/drukhal-manager.pot does not match the strings in the
# sources. CI runs it; run it yourself before pushing.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

tmp=$(mktemp --suffix=.pot)
trap 'rm -f "$tmp"' EXIT

po/update-pot.sh "$tmp"

# The creation date changes on every run, so leave it out of the comparison.
filter() { grep -v '^"POT-Creation-Date:' "$1"; }
if ! diff -u <(filter po/drukhal-manager.pot) <(filter "$tmp"); then
    echo "po/drukhal-manager.pot is out of date: run po/update-pot.sh and commit the result" >&2
    exit 1
fi
