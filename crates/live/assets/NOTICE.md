# Bundled assets

## tabler-icons.ttf

Tabler Icons — https://tabler.io/icons — **MIT License**, © Paweł Kuna.

Taken from `@tabler/icons-webfont` 3.31.0 and **subsetted** to only the
codepoints named in `src/icon.rs`: 12 KB here (39 glyphs) versus 2.4 MB for the
full 5,937-glyph font.

To add an icon: add its const to `src/icon.rs`, then regenerate this file. Don't
hand-write the `--unicodes` list — derive it from `icon.rs` so the subset can't
drift from the consts:

```sh
pip install fonttools
curl -sLO https://unpkg.com/@tabler/icons-webfont@3.31.0/dist/fonts/tabler-icons.ttf

# every `\u{XXXX}` in icon.rs, as a U+ list
UNICODES=$(grep -oE '\\u\{[0-9a-fA-F]+\}' crates/live/src/icon.rs \
  | sed -E 's/\\u\{(.*)\}/U+\1/' | tr 'a-f' 'A-F' | sort -u | paste -sd,)

python -m fontTools.subset tabler-icons.ttf \
  --unicodes="$UNICODES" \
  --output-file=crates/live/assets/tabler-icons.ttf \
  --no-hinting --desubroutinize --drop-tables+=GSUB,GPOS
```

To find a glyph's codepoint, look it up **by name** in the upstream font rather
than guessing (a wrong codepoint is tofu):

```sh
python -c "from fontTools.ttLib import TTFont; \
  cm=TTFont('tabler-icons.ttf').getBestCmap(); \
  print({n:hex(c) for c,n in cm.items() if n=='typography'})"
```

A codepoint that isn't in the subset renders as tofu, so a missed regeneration
shows up immediately rather than shipping a blank button. To check without
launching the app, compare the two sets directly — they should be equal in both
directions, since a glyph named but absent is tofu and one present but unnamed
is dead weight:

```sh
python -c '
import re; from fontTools.ttLib import TTFont
named = {int(m,16) for m in re.findall(r"u\{([0-9a-fA-F]+)\}",
    open("crates/live/src/icon.rs", encoding="utf-8").read())}
have = set(TTFont("crates/live/assets/tabler-icons.ttf").getBestCmap())
print("missing:", [hex(c) for c in sorted(named - have)])
print("unused :", [hex(c) for c in sorted(have - named)])
'
```

Single-quoted, and matching `u{…}` rather than the full `\u{…}`: inside double
quotes the shell eats the backslash and Python sees an invalid escape. Both
lists should come back empty — a named codepoint that's absent renders as tofu,
and one present but unnamed is dead weight in the file.

## material-symbols.ttf, mdi.ttf

The edit-tool icons (`src/material.rs`), **Apache License 2.0**:

- `material-symbols.ttf`: Google **Material Symbols Outlined**
  (google/material-design-icons, `variablefont/`), instanced at
  `wght=400 FILL=0 GRAD=0 opsz=24` and subsetted to select, razor, ripple.
- `mdi.ttf`: Pictogrammers **Material Design Icons** (`@mdi/font` 7.4.47),
  subsetted to roll and magnet, which Material Symbols doesn't have.

Regenerate with the codepoints in `src/material.rs` (the test
`every_material_icon_is_in_the_bundled_fonts` fails if one is missing):

```sh
python -m fontTools.varLib.instancer "MaterialSymbolsOutlined[FILL,GRAD,opsz,wght].ttf" \
  wght=400 FILL=0 GRAD=0 opsz=24 -o symbols_static.ttf
python -m fontTools.subset symbols_static.ttf --unicodes=U+F82F,U+E14E,U+E31C \
  --output-file=crates/live/assets/material-symbols.ttf \
  --no-hinting --desubroutinize --drop-tables+=GSUB,GPOS
python -m fontTools.subset materialdesignicons-webfont.ttf --unicodes=U+F093C,U+F0347 \
  --output-file=crates/live/assets/mdi.ttf \
  --no-hinting --desubroutinize --drop-tables+=GSUB,GPOS
```

Names → codepoints: Material Symbols' `.codepoints` file beside the font;
MDI's `css/materialdesignicons.css` (`.mdi-magnet::before { content: "\F0347" }`).
