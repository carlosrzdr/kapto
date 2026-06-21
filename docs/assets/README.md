# Kapto's logo

| File | Use |
|---|---|
| `kapto-logo-light.svg` · `kapto-logo-dark.svg` | The mark and the name, on a light or a dark background |
| `kapto-mark-light.svg` · `kapto-mark-dark.svg` | The mark alone, square |
| `social-preview.png` | The repository's preview when a link to it is shared (1280 × 640) |

The web UI draws the same mark in `ui/src/lib/Logo.svelte`, and uses it as
the page's icon in `ui/public/favicon.svg`, which follows the browser's
light or dark theme.

## How it is drawn

The mark is the capital K of Fira Sans ExtraBold (800), the typeface the
UI is set in, with its upper arm replaced by the record dot: the stem and
the leg are the glyph's own outline, so the mark has the name's
proportions. The dot is 1.32 times the stem's width across, centred on the
line the arm followed, and rises just above the cap height as round letters
do. In the files' units (the font's, y downwards, 608 wide and 707 tall):

- the stem: `M0 12H185V707H0Z`
- the leg: `M381 322L608 707H396L191 337Z`
- the dot: centre (439.9, 122.1), radius 122.1

The name is set in Fira Sans Bold (700) with the font's kerning, outlined
so the files need no font. Beside it the mark is 1.4 times the name's cap
height, centred on it, 0.42 cap heights away.

Colours: the mark is the UI's text colour (`#151718` on light,
`#f2f4f5` on dark) and the dot its accent green (`#22c55e`).
