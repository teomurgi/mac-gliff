#!/bin/sh
# Regenerate the xkb keymap texts the macOS client ships (see
# crates/gliff/src/keymap.rs). Needs libxkbcommon + xkeyboard-config
# (`brew install libxkbcommon xkeyboard-config`); the output is
# committed so the client itself depends on neither.
set -eu

out="$(dirname "$0")/../crates/gliff/keymaps"
mkdir -p "$out"

# id layout variant — the ids keymap.rs maps macOS input sources to.
while read -r id layout variant; do
    [ "$id" = "#" ] && continue
    if [ -n "$variant" ]; then
        xkbcli compile-keymap --layout "$layout" --variant "$variant" > "$out/$id.xkb"
    else
        xkbcli compile-keymap --layout "$layout" > "$out/$id.xkb"
    fi
    echo "$id: $(wc -c < "$out/$id.xkb") bytes"
done <<'EOF'
us us
us-intl us intl
us-dvorak us dvorak
us-colemak us colemak
gb gb
de de
ch-de ch de
ch-fr ch fr
fr fr
ca-fr ca fr
it it
es es
latam latam
nl nl
be be
pt pt
br br
dk dk
no no
se se
fi fi
pl pl
cz cz
hu hu
tr tr
ru ru
ua ua
gr gr
ara ara
il il
bg bg
hr hr
sk sk
si si
ro ro
ee ee
lt lt
lv lv
EOF
