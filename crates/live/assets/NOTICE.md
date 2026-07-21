# Bundled assets

## tabler-icons.ttf

Tabler Icons — https://tabler.io/icons — **MIT License**, © Paweł Kuna.

Taken from `@tabler/icons-webfont` 3.31.0 and **subsetted** to only the
codepoints named in `src/icon.rs`: 10 KB here (32 glyphs) versus 2.4 MB for the
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
shows up immediately rather than shipping a blank button.
