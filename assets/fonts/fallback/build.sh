#!/bin/sh
# Rebuilds the bundled fallback fonts from Google Fonts' Noto sources.
# Requires: curl, fonttools (pip install fonttools). Output is committed;
# you only need this to change coverage or update the sources.
#
# All sources are under the SIL Open Font License 1.1 (see OFL.txt), which
# allows subsetting and redistribution; Noto declares no Reserved Font Name.
set -eu
COMMIT=2fe1e8c6b137fb13041a7cfca6fdcd8a910ea4dc   # github.com/google/fonts
BASE="https://raw.githubusercontent.com/google/fonts/$COMMIT/ofl"
OUT="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cd "$TMP"

get() { curl -sSf -o "$2" "$BASE/$1"; }
# Variable font -> static Regular, then keep only the given ranges. Layout
# tables are kept for future text shaping; hinting is dropped (not used).
build() { # src out unicodes axes...
  src=$1; out=$2; uni=$3; shift 3
  fonttools varLib.instancer "$src" "$@" -o static.ttf >/dev/null 2>&1 || cp "$src" static.ttf
  pyftsubset static.ttf --unicodes="$uni" --layout-features='*' --no-hinting \
    --name-IDs='*' --notdef-outline --output-file="$OUT/$out"
}

get "notosanssc/NotoSansSC%5Bwght%5D.ttf" sc.ttf
get "notosanskr/NotoSansKR%5Bwght%5D.ttf" kr.ttf
get "notosansdevanagari/NotoSansDevanagari%5Bwdth,wght%5D.ttf" deva.ttf
get "notosansthai/NotoSansThai%5Bwdth,wght%5D.ttf" thai.ttf
get "notoemoji/NotoEmoji%5Bwght%5D.ttf" emoji.ttf
get "notosanssymbols2/NotoSansSymbols2-Regular.ttf" sym.ttf
get "notosanssc/OFL.txt" "$OUT/OFL.txt"

# Chinese + Japanese: CJK punctuation, kana, the main ideograph block, fullwidth forms.
build sc.ttf NotoSansCJK-Subset.ttf "U+3000-303F,U+3040-30FF,U+31F0-31FF,U+3200-32FF,U+4E00-9FFF,U+FF00-FFEF" wght=400
# Korean: Hangul syllables and jamo.
build kr.ttf NotoSansKR-Subset.ttf "U+1100-11FF,U+3130-318F,U+AC00-D7A3" wght=400
build deva.ttf NotoSansDevanagari.ttf "U+0900-097F,U+A8E0-A8FF,U+1CD0-1CFF,U+200C-200D,U+25CC" wght=400 wdth=100
build thai.ttf NotoSansThai.ttf "U+0E00-0E7F,U+200C-200D,U+25CC" wght=400 wdth=100
build emoji.ttf NotoEmoji.ttf "U+00A9,U+00AE,U+203C-3299,U+1F000-1FAFF" wght=400
build sym.ttf NotoSansSymbols2.ttf "U+0000-10FFFF"
ls -l "$OUT"
