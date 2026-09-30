# CSS in the browser

`browse` styles pages with `crates/css` and lays them out with
`crates/layout`. Both are `no_std` crates with host unit tests; the same
layout code serves the text browser (character cells) and is written in
CSS px so that a pixel renderer can use it too.

## Pipeline

1. **Style sheets.** The HTML user-agent sheet (`crates/css/src/ua.css`,
   after the HTML standard's rendering section), a small terminal sheet
   (no side margin on `body`), then the page's `<style>` and
   `<link rel="stylesheet">` sheets in document order. `@import`s are
   fetched (up to three levels, sixteen downloads per page) and cascade
   before the importing sheet. Sheets whose `media` does not match are
   skipped. `-nocss` keeps only the built-in sheets.
2. **Cascade** (`css::cascade`): rules are indexed by the rightmost
   compound selector (id, class, tag, universal); matching declarations
   are sorted by origin and importance, cascade layer (`@layer`, with
   `!important` reversing layer order), specificity and source order. The
   `style` attribute and presentational hints (`bgcolor`, `width`,
   `align`, `cellpadding`, `<font>`, ...) take part as in the HTML
   standard. Custom properties are resolved first, `var()` is substituted
   before a declaration is parsed (an unusable result behaves like
   `unset`), `font-size` is computed before the other properties so `em`
   works.
3. **Box tree** (`layout::tree`): `display` decides the boxes;
   `::before`/`::after`/`::marker` content with counters (`counter-reset`,
   `-increment`, `-set`, `counter()`, `counters()`), quotes and `attr()`;
   anonymous block, table and flex/grid item boxes; `display: contents`;
   form controls and images are replaced boxes.
4. **Layout** (`layout::flow`, `inline`, `flex`, `grid`, `table`): block
   flow with margin collapsing, floats and clearance, block formatting
   contexts; inline formatting with white-space processing, break
   opportunities (including between ideographs), `text-align`, `text-indent`,
   outside list markers, atomic inline boxes; the full flexbox algorithm;
   grid with `repeat()`/`auto-fill`/`auto-fit`, named lines and areas,
   auto-placement (`dense` too), `fr`/`minmax()`/`fit-content()` tracks;
   tables with row/column spans and the automatic and fixed algorithms;
   relative, absolute, fixed and sticky (as relative) positioning.
5. **Painting** (`layout::cells`): backgrounds, text, markers, form
   widgets and `<hr>` rules onto a grid of cells, positioned boxes in
   `z-index` order, `overflow: hidden` clipping.

## Supported CSS

* **Syntax**: CSS Syntax 3 tokenizer and parser, comments, escapes,
  `!important`, CSS Nesting (`&`, nested rules and nested `@media`).
* **Selectors 4**: type, universal, class, id, attribute (`= ~= |= ^= $= *=`,
  `i` flag), all combinators, `:is()`, `:where()`, `:not()`, `:has()`,
  `:nth-child(An+B of S)`, `:nth-last-child()`, `:nth-of-type()`,
  `:first/last/only-child`, `:first/last/only-of-type`, `:root`, `:empty`,
  `:link`, `:visited`, `:any-link`, `:hover`, `:focus`, `:focus-within`,
  `:focus-visible`, `:active`, `:target`, `:checked`, `:disabled`,
  `:enabled`, `:required`, `:optional`, `:read-only`, `:read-write`,
  `:placeholder-shown`, `:default`, `:valid`, `:invalid`, `:open`,
  `:lang()`, `:scope`; `::before`, `::after`, `::marker` (also
  `::first-line`, `::first-letter`, `::placeholder`, `::selection`,
  `::backdrop` parse but are not rendered).
* **At-rules**: `@media` (Media Queries 4 including range syntax, `and`/
  `or`/`not`, `prefers-color-scheme`, `hover`, `pointer`, `orientation`,
  `scripting`, ...), `@supports` (declarations and `selector()`),
  `@layer` (blocks and statements), `@import` (with media), `@font-face`
  (collected for the graphical browser); `@container`, `@scope` and
  `@starting-style` apply their rules unconditionally; `@keyframes` and
  `@page` are ignored.
* **Values**: lengths in `px em rem ex ch lh vw vh vmin vmax pt pc in cm mm q`
  (and the `s/l/d` viewport variants), percentages, `calc()`, `min()`,
  `max()`, `clamp()`; colors as names, `#rgb[a]`, `#rrggbb[aa]`, `rgb()`,
  `hsl()`, `hwb()`, `lab()`, `lch()`, `oklab()`, `oklch()`, system colors,
  `currentColor`; `inherit`, `initial`, `unset`, `revert`; custom
  properties and `var()` with fallbacks.
* **Properties**: display (all values), position and insets, float,
  clear, z-index, the box model (margin, padding, border, box-sizing,
  width/height with min/max, `min-content`/`max-content`/`fit-content`,
  aspect-ratio), overflow, visibility, opacity, color and backgrounds
  (colors, images, gradients, position, size, repeat), fonts (size,
  weight, style, family, `font` shorthand, small caps), line-height,
  text (`text-align`, `text-indent`, `text-transform`, `text-decoration`,
  `white-space`, `word-break`, `overflow-wrap`, `text-overflow`,
  letter/word spacing, `vertical-align`), lists (`list-style*`), content
  and quotes, counters, flexbox (all properties and shorthands), grid
  (templates, areas, auto rows/columns/flow, placement shorthands), gaps,
  alignment (`justify-*`, `align-*`, `place-*`), tables
  (`border-collapse`, `border-spacing`, `table-layout`, `caption-side`,
  `empty-cells`), transforms (parsed), box-shadow, outline, cursor,
  pointer-events, object-fit, user-select, appearance. Animations,
  transitions, filters and similar are accepted and ignored.

## Character cells

In the text browser a column is 8 px and a row 16 px. Every line of text
is one row whatever its font size; vertical space rounds to whole rows
(less than about 10 px takes none). Borders take no cells (except
`<hr>`, drawn as a rule); padding under half a cell is dropped. Scrolling
boxes (`overflow: auto/scroll`) show all their content instead of a fixed
height. List bullets are `*`, `o` and `+`, as in lynx. A background color
is shown with the exact text color; on the terminal's own background dark
text uses the terminal's default color.
