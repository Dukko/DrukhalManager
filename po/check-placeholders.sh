#!/usr/bin/env bash
# Fails when a translation's {placeholders} don't match its English text, or
# when a plural translation has the wrong number of msgstr entries.
#
#     po/check-placeholders.sh [FILE.po ...]
#
# With no arguments it checks every po/*.po, so it can run in CI and by hand
# after pulling translations from Weblate. Named placeholders must survive
# translation exactly: `tf!` falls back to English at runtime when one is
# missing, but that hides the mistake from the translator.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ "$#" -eq 0 ]; then
    # The template too, to catch a plural whose English forms disagree.
    shopt -s nullglob
    files=(po/drukhal-manager.pot po/*.po)
    shopt -u nullglob
else
    files=("$@")
fi

status=0
for po in "${files[@]}"; do
    if ! awk -v file="$po" '
        # A sorted, comma-separated list of the named placeholders in `s`.
        function placeholders(s,   t, n, arr, vals, m, i, j, tmp, out) {
            t = s
            gsub(/\{[A-Za-z_][A-Za-z0-9_]*\}/, " & ", t)
            n = split(t, arr, " ")
            m = 0
            for (i = 1; i <= n; i++) {
                if (arr[i] ~ /^\{[A-Za-z_][A-Za-z0-9_]*\}$/) {
                    vals[++m] = arr[i]
                }
            }
            for (i = 1; i <= m; i++) {
                for (j = i + 1; j <= m; j++) {
                    if (vals[j] < vals[i]) {
                        tmp = vals[i]; vals[i] = vals[j]; vals[j] = tmp
                    }
                }
            }
            out = ""
            for (i = 1; i <= m; i++) {
                out = out (i > 1 ? "," : "") vals[i]
            }
            return out
        }

        # The text inside a `msgid "..."`-style line, unquoted.
        function quoted(line,   start) {
            start = index(line, "\"")
            if (start == 0) return ""
            return substr(line, start + 1, length(line) - start - 1)
        }

        function reset() {
            msgid = ""; plural = 0; have_str = 0; n_strs = 0
            delete str; field = ""; start = NR
        }

        function untranslated(   i) {
            for (i = 0; i < n_strs; i++) {
                if (str[i] != "") return 0
            }
            return 1
        }

        function flush(   expected, i, got) {
            if (msgid == "") return
            expected = placeholders(msgid)
            if (plural && placeholders(msgid_plural) != expected) {
                printf "%s:%d: the English plural has placeholders %s, the singular has %s\n", file, start, placeholders(msgid_plural), expected
                bad = 1
            }
            if (!have_str || untranslated()) return
            if (plural) {
                if (n_strs != nplurals) {
                    printf "%s:%d: plural entry has %d msgstr lines, the header says %d\n", file, start, n_strs, nplurals
                    bad = 1
                }
                for (i = 0; i < n_strs; i++) {
                    got = placeholders(str[i])
                    if (got != expected) {
                        printf "%s:%d: msgstr[%d] has placeholders %s, the English has %s\n", file, start, i, got, expected
                        bad = 1
                    }
                }
            } else {
                got = placeholders(str[0])
                if (got != expected) {
                    printf "%s:%d: translation has placeholders %s, the English has %s\n", file, start, got, expected
                    bad = 1
                }
            }
        }

        BEGIN { bad = 0; nplurals = 2; reset() }
        /^#/ { next }
        /^[ \t]*$/ { flush(); reset(); next }
        /^msgctxt / { flush(); reset(); msgctxt = quoted($0); start = NR; next }
        /^msgid / {
            flush(); reset()
            msgid = quoted($0); field = "id"; start = NR
            next
        }
        /^msgid_plural / { plural = 1; msgid_plural = quoted($0); field = "plural"; next }
        /^msgstr\[[0-9]+\] / {
            line = $0
            sub(/^msgstr\[/, "", line)
            sub(/\].*/, "", line)
            i = 0 + line
            str[i] = quoted($0)
            if (i >= n_strs) n_strs = i + 1
            have_str = 1; field = "str"; field_i = i
            next
        }
        /^msgstr / { str[0] = quoted($0); n_strs = 1; have_str = 1; field = "str"; field_i = 0; next }
        /^"/ {
            text = quoted($0)
            if (text ~ /Plural-Forms:/ && match(text, /nplurals=[0-9]+/)) {
                nplurals = substr(text, RSTART + 9, RLENGTH - 9) + 0
            }
            if (field == "id") msgid = msgid text
            else if (field == "plural") msgid_plural = msgid_plural text
            else if (field == "str") str[field_i] = str[field_i] text
            next
        }
        END { flush(); if (bad) exit 1 }
    ' "$po"; then
        status=1
    fi
done

if [ "$status" -ne 0 ]; then
    echo "Placeholders must match the English text: fix the .po files above." >&2
    exit 1
fi
echo "Placeholders match in every translation."
