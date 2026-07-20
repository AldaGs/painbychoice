# Bundled assets

## tabler-icons.ttf

Tabler Icons — https://tabler.io/icons — **MIT License**, © Paweł Kuna.

Taken from `@tabler/icons-webfont` 3.31.0 and **subsetted** to only the
codepoints named in `src/icon.rs`: 8 KB here versus 2.4 MB for the full
5,937-glyph font.

To add an icon: add its const to `src/icon.rs`, then regenerate this file with
that codepoint included —

```sh
pip install fonttools
curl -sLO https://unpkg.com/@tabler/icons-webfont@3.31.0/dist/fonts/tabler-icons.ttf
python -m fontTools.subset tabler-icons.ttf \
  --unicodes=U+ED46,U+ED45,...   # every codepoint in icon.rs
  --output-file=crates/live/assets/tabler-icons.ttf \
  --no-hinting --desubroutinize --drop-tables+=GSUB,GPOS
```

A codepoint that isn't in the subset renders as tofu, so a missed regeneration
shows up immediately rather than shipping a blank button.
