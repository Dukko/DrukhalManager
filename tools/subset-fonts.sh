#!/bin/sh
# Rebuilds data/fonts from the upstream Google Fonts files.
#
#   tools/subset-fonts.sh <folder with the downloaded .ttf files>
#
# Sources (all SIL Open Font License 1.1, copied next to each font):
#   https://github.com/google/fonts/tree/main/ofl/googlesansflex
#   https://github.com/google/fonts/tree/main/ofl/firacode
#   https://github.com/google/fonts/tree/main/ofl/dmserifdisplay
#
# Needs fontTools (pip install fonttools brotli). The UI font keeps its
# weight and optical-size axes and drops the others; every font keeps the
# Latin, Latin Extended and Vietnamese ranges plus common punctuation and
# symbols. Text outside them falls back to the system's fonts.
set -eu
src=$1
out=$(dirname "$0")/../data/fonts
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
unicodes="U+0000-024F,U+0259,U+02B0-02FF,U+0300-036F,U+1E00-1EFF,U+2000-206F,U+20A0-20CF,U+2100-214F,U+2190-21FF,U+2200-22FF,U+2460-24FF,U+25A0-25FF,U+2600-26FF,U+2700-27BF,U+FEFF,U+FFFD"
fonttools varLib.instancer -q -o "$tmp/sans.ttf" "$src/GoogleSansFlex[ROND,opsz,slnt,wdth,wght].ttf" ROND=0 slnt=0 wdth=100
for pair in "$tmp/sans.ttf:GoogleSansFlex.ttf" "$src/FiraCode[wght].ttf:FiraCode.ttf" "$src/DMSerifDisplay-Regular.ttf:DMSerifDisplay.ttf"; do
    pyftsubset "${pair%%:*}" --unicodes="$unicodes" --layout-features='*' --name-IDs='*' --notdef-outline --output-file="$out/${pair#*:}"
done
