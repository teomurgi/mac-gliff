#!/bin/sh
# Regenerate the xkb keymap texts the macOS client ships (see
# crates/gliff/src/keymap.rs). Needs libxkbcommon + xkeyboard-config
# (`brew install libxkbcommon xkeyboard-config`); the output is
# committed so the client itself depends on neither.
set -eu

out="$(dirname "$0")/../crates/gliff/keymaps"
mkdir -p "$out"

# id layout variant — the ids keymap.rs maps macOS input sources to.
# The ids without a macOS counterpart in keymap.rs are still shipped:
# scripts may request them by id later; keep the two lists in sync.
while read -r id layout variant; do
    [ "$id" = "#" ] && continue
    if [ -n "$variant" ]; then
        xkbcli compile-keymap --layout "$layout" --variant "$variant" > "$out/$id.xkb"
    else
        xkbcli compile-keymap --layout "$layout" > "$out/$id.xkb"
    fi
done <<'EOF'
af af 
af-ps af ps
af-uz af uz
al al 
am am 
ara ara
ara-azerty ara azerty
at at 
au au 
az az 
be be
bg bg
bg-phonetic bg phonetic
br br
bt bt 
by by 
ca-fr ca fr
ch-de ch de
ch-fr ch fr
cn-ug cn ug
cz cz
cz-qwerty cz qwerty
de de
dk dk
dz dz 
ee ee
es es
et et 
fi fi
fo fo 
fr fr
gb gb
ge ge 
gr gr
gr-polytonic gr polytonic
hr hr
hu hu
hu-qwerty hu qwerty
ie ie 
il il
iq-ku iq ku
ir ir 
is is 
it it
jp-kana jp kana
kg kg 
kh kh 
kr kr 
kz kz 
la la 
latam latam
lt lt
lv lv
ma-tifinagh ma tifinagh
mk mk 
mm mm 
mn mn 
mt mt 
ng ng 
nl nl
no no
no-smi no smi
np np 
nz nz 
pk pk 
pl pl
pt pt
ro ro
rs rs 
rs-latin rs latin
ru ru
ru-cv ru cv
ru-phonetic ru phonetic
ru-sah ru sah
se se
si si
sk sk
sk-qwerty sk qwerty
th th 
tj tj 
tm tm 
tr tr
tr-f tr f
tr-ku tr ku
ua ua
us us
us-colemak us colemak
us-dvorak us dvorak
us-intl us intl
uz uz 
vn vn 
EOF
echo "generated $(ls "$out" | wc -l | tr -d ' ') keymaps"
