---
name: nuntio website
description: A folded programming pocket reference in the lineage of DEC's VT cards, printed in black ink and one signal orange on warm card stock, with a night card in dark mode.
colors:
  desk: "#D9D3C4"
  stock: "#EDE9DF"
  stock-2: "#E3DED1"
  ink: "#141414"
  ink-2: "#4A4740"
  rule: "#B9B2A2"
  signal: "#EA5F1E"
  signal-ink: "#B23C0A"
  on-signal: "#141414"
  back-cover: "#141414"
  back-cover-ink: "#EDE9DF"
  desk-night: "#0A0A09"
  stock-night: "#121110"
  stock-2-night: "#1C1A17"
  ink-night: "#ECE6D8"
  ink-2-night: "#A8A194"
  rule-night: "#3A362F"
  signal-night: "#F26A2A"
  signal-ink-night: "#F58A4F"
  term-bg: "#1a1b26"
  term-fg: "#c0caf5"
  term-blue: "#7aa2f7"
  term-green: "#9ece6a"
  term-comment: "#565f89"
  term-strip: "#16161e"
  term-active: "#24283b"
  term-divider: "#3b4261"
  agent-blocked: "#e06c75"
  agent-done: "#98c379"
  agent-working: "#61afef"
  banner-info: "#1f5fa8"
typography:
  cover-name:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "clamp(5.5rem, 15vw, 13.5rem)"
    fontWeight: 900
    lineHeight: 0.82
    letterSpacing: "-0.01em"
    fontVariation: "'wdth' 62"
  cover-hook:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "clamp(1.75rem, 3vw, 2.75rem)"
    fontWeight: 700
    lineHeight: 1.05
    letterSpacing: "-0.01em"
    fontVariation: "'wdth' 87.5"
  panel-hook:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "clamp(2.25rem, 4.5vw, 4rem)"
    fontWeight: 800
    lineHeight: 0.95
    letterSpacing: "-0.005em"
    fontVariation: "'wdth' 75"
  prose-h1:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "clamp(2.5rem, 5vw, 4rem)"
    fontWeight: 800
    lineHeight: 0.95
    letterSpacing: "-0.01em"
    fontVariation: "'wdth' 62"
  prose-h2:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "1.75rem"
    fontWeight: 800
    lineHeight: 1.1
    fontVariation: "'wdth' 75"
  prose-h3:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "1.25rem"
    fontWeight: 700
    lineHeight: 1.2
    fontVariation: "'wdth' 87.5"
  brand:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "1.375rem"
    fontWeight: 800
    lineHeight: 1
    letterSpacing: "-0.01em"
    fontVariation: "'wdth' 75"
  body:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "1.0625rem"
    fontWeight: 400
    lineHeight: 1.6
  button:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "1.125rem"
    fontWeight: 700
    lineHeight: 1.15
    fontVariation: "'wdth' 87.5"
  caption:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.45
    fontVariation: "'wdth' 87.5"
  index-label:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "0.8125rem"
    fontWeight: 700
    letterSpacing: "0.05em"
    fontVariation: "'wdth' 75"
  table-head:
    fontFamily: "Archivo, system-ui, sans-serif"
    fontSize: "0.8rem"
    fontWeight: 800
    letterSpacing: "0.04em"
    fontVariation: "'wdth' 75"
  key:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, Consolas, monospace"
    fontSize: "0.8125rem"
    fontWeight: 600
    lineHeight: 1.3
  code:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, Consolas, monospace"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.55
  meta:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, Consolas, monospace"
    fontSize: "0.75rem"
    fontWeight: 400
    letterSpacing: "0.02em"
rounded:
  none: "0"
  keycap: "2px"
  tab: "8px"
  card: "20px"
  marker: "50%"
spacing:
  s1: "4px"
  s2: "8px"
  s3: "12px"
  s4: "16px"
  s5: "24px"
  s6: "32px"
  s7: "48px"
  s8: "64px"
  s9: "96px"
  s10: "128px"
components:
  card:
    backgroundColor: "{colors.stock}"
    textColor: "{colors.ink}"
    rounded: "{rounded.card}"
  masthead:
    backgroundColor: "{colors.stock}"
    textColor: "{colors.ink}"
    typography: "{typography.index-label}"
    padding: "16px 56px"
  button-primary:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.stock}"
    typography: "{typography.button}"
    rounded: "{rounded.none}"
    padding: "18px 24px"
  button-primary-hover:
    backgroundColor: "{colors.stock}"
    textColor: "{colors.ink}"
  button-primary-on-signal:
    backgroundColor: "{colors.back-cover}"
    textColor: "{colors.back-cover-ink}"
    rounded: "{rounded.none}"
  button-secondary:
    backgroundColor: "{colors.stock}"
    textColor: "{colors.ink}"
    typography: "{typography.button}"
    rounded: "{rounded.none}"
    padding: "18px 24px"
  button-secondary-hover:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.stock}"
  keycap:
    backgroundColor: "{colors.stock}"
    textColor: "{colors.ink}"
    typography: "{typography.key}"
    rounded: "{rounded.keycap}"
    padding: "2px 7px"
  chip:
    backgroundColor: "{colors.stock-2}"
    textColor: "{colors.ink}"
    typography: "{typography.key}"
    rounded: "{rounded.none}"
    padding: "3px 9px"
  thumb-index-tab:
    backgroundColor: "{colors.stock-2}"
    textColor: "{colors.ink}"
    typography: "{typography.index-label}"
    rounded: "{rounded.tab}"
    width: "44px"
    height: "88px"
  thumb-index-tab-current:
    backgroundColor: "{colors.signal}"
    textColor: "{colors.on-signal}"
  cover-field:
    backgroundColor: "{colors.signal}"
    textColor: "{colors.on-signal}"
    padding: "64px"
  marker:
    backgroundColor: "{colors.signal}"
    textColor: "{colors.on-signal}"
    typography: "{typography.key}"
    rounded: "{rounded.marker}"
    size: "26px"
  table-spec-head:
    backgroundColor: "{colors.stock-2}"
    textColor: "{colors.ink}"
    typography: "{typography.table-head}"
    padding: "10px 12px"
  table-matrix-head:
    backgroundColor: "{colors.signal}"
    textColor: "{colors.on-signal}"
    typography: "{typography.table-head}"
    padding: "10px 12px"
  code-card:
    backgroundColor: "{colors.stock-2}"
    textColor: "{colors.ink}"
    typography: "{typography.code}"
    rounded: "{rounded.none}"
    padding: "16px 24px"
  term:
    backgroundColor: "{colors.term-bg}"
    textColor: "{colors.term-fg}"
    typography: "{typography.code}"
    rounded: "{rounded.none}"
  download-insert:
    backgroundColor: "{colors.stock}"
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
  download-tab-current:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.stock}"
    typography: "{typography.index-label}"
    padding: "12px 24px"
  docs-nav-current:
    backgroundColor: "{colors.signal}"
    textColor: "{colors.on-signal}"
    padding: "8px 12px"
  back-cover:
    backgroundColor: "{colors.back-cover}"
    textColor: "{colors.back-cover-ink}"
    padding: "64px 56px 48px"
---

# Design System: nuntio website

## Overview

**Creative North Star: "The Pocket Reference"**

The site is a folded programming reference card, in the lineage of the DEC VT pocket references from which nuntio's CSI/OSC/DEC-private-mode vocabulary comes. It is warm card stock printed in black ink and one signal orange, with a rounded die-cut corner on the outside and everything inside it square. In dark mode the same card becomes a night card: near-black stock, cream ink, a slightly brighter orange. The page is a desk; the card is the site.

The card is read panel by panel. Each panel is a fold: a dashed crease with tick marks and a folio label (`N/8 · Name`) opens it, then a hook in condensed caps, then the exact key or setting on the right, then the body. Reference vocabulary does the work that a marketing site would give to decoration: keycaps, numbered callouts with legends, ruled tables, `FIG. N` captions, thumb-index tabs on the edge. Real screenshots and exact sequences are the evidence; HTML-built terminal figures replicate nuntio itself.

Density is that of a manual: tight tables and captions, generous panel padding (`clamp(4rem, 8vw, 7.5rem)`), more space above a heading than below. Colour is rationed: orange appears as a field, a fill, a current-state tab or a marker, never as a gradient or a wash. The terminal figures keep Tokyo Night inside their frames and nowhere else.

**Key Characteristics:**
- One card on a desk; the card has the only large radius (20px), everything inside is square.
- Ink-on-stock hairlines and 1.5px ink rules carry structure; no shadows, no gradients, no glass.
- One accent, signal orange, used as fields and fills with ink text on it.
- Condensed Archivo caps for heads and index labels; JetBrains Mono for sequences, keys, code and meta.
- State and support are marks (● ◐ ○, numbered markers), not hues.
- Works without JavaScript; the signature paste demo and the OS tabs degrade to a static, complete state.

## Colors

A two-ink palette: warm stock, near-black ink, a muted hairline, and a single saturated orange. The dark scheme inverts stock and ink and lifts the orange slightly.

### Primary
- **Signal Orange** (`#EA5F1E` light, `#F26A2A` night): the cover field, the download panel, the current thumb-index tab, current docs entry, matrix header row, numbered markers. Text on it is always Ink (`--on-signal`, `#141414`), in both schemes.
- **Signal Ink** (`#B23C0A` light, `#F58A4F` night): orange as text on stock (panel labels, TOML keys, list markers); contrast about 4.8:1 or better. Not used for fills.

### Neutral
- **Desk** (`#D9D3C4` light, `#0A0A09` night): the page around the card; also the theme-color.
- **Card Stock** (`#EDE9DF` light, `#121110` night): the card ground, keycap face, inserts.
- **Stock Shade** (`#E3DED1` light, `#1C1A17` night): table headers, code cards, chips, inactive tabs, inline code.
- **Ink** (`#141414` light, `#ECE6D8` night): text, borders, rules, the primary button slab.
- **Ink Soft** (`#4A4740` light, `#A8A194` night): secondary text, captions, labels.
- **Crease Rule** (`#B9B2A2` light, `#3A362F` night): hairlines, creases, row rules, dividers between cells.
- **Back Cover** (`#141414` with `#EDE9DF` text in light; Desk night `#0A0A09` with Ink night `#ECE6D8` in dark): the closing panel.

### Terminal figures (inside `.term` only)
Tokyo Night: background `#1a1b26`, text `#c0caf5`, blue `#7aa2f7`, green `#9ece6a`, comment `#565f89`, tab strip `#16161e`, active tab `#24283b`, divider `#3b4261`. Agent states mirror the app: blocked `#e06c75`, done `#98c379`, working `#61afef`; the replica info banner is `#1f5fa8`. The download command blocks reuse the same terminal palette.

### Named Rules
**The Two-Ink Rule.** Surfaces are stock or signal; text is ink or ink-soft. A fill that is neither is a terminal replica and stays inside a `.term` frame or a command block.

**The Ink-On-Orange Rule.** Anything on a signal field is `#141414` text, `#141414` borders and `#141414` outlines, in light and dark alike. Never cream on orange, never orange on orange.

**The Mark-Not-Hue Rule.** State is carried by a glyph or a number: ● Yes, ◐ Partly, ○ No; marker ①②③. A colour may accompany a state only inside a terminal replica, where the app itself uses it.

## Typography

**Display / Body Font:** Archivo (variable, wdth 62–125, wght 100–900), fallback `system-ui, sans-serif`. Self-hosted subset, `font-display: swap`.
**Mono Font:** JetBrains Mono (variable, wght 100–800), fallback `ui-monospace, Menlo, Consolas, monospace`. Ligatures off.

**Character:** Archivo's width axis does the poster work: ultra-condensed black for the name, condensed caps for heads, normal width for reading. JetBrains Mono is the voice of the machine: only sequences, keys, settings, code and meta lines.

### Hierarchy
- **Cover name** (900, width 62%, `clamp(5.5rem, 15vw, 13.5rem)`, line-height 0.82, tracking -0.01em, lowercase): the one huge word on the orange field.
- **Cover hook** (700, width 87.5%, `clamp(1.75rem, 3vw, 2.75rem)`, 1.05): "The terminal for AI agents."
- **Panel hook** (800, width 75%, `clamp(2.25rem, 4.5vw, 4rem)`, 0.95, uppercase, max 18ch): one per panel.
- **Docs title** (h1: 800, width 62%, `clamp(2.5rem, 5vw, 4rem)`, 0.95), **h2** (800, width 75%, 1.75rem, 2px ink top rule, 64px above), **h3** (700, width 87.5%, 1.25rem).
- **Body** (400, 1.0625rem / 1.6, 62ch in panels, 72ch in prose).
- **Caption** (400, width 87.5%, 0.875rem / 1.45, Ink Soft, hairline above); the `FIG. N` prefix is 800, width 75%, uppercase, tracking 0.04em, in Ink.
- **Index label** (700, width 75%, uppercase, tracking 0.05em, 0.8125–0.875rem): masthead nav, thumb-index tabs, download tabs, spec-band labels, docs sidebar label, crease folio.
- **Table head** (800, width 75%, 0.8rem, uppercase, tracking 0.04em).
- **Key / chip / marker** (mono 600, 0.8125rem), **code** (mono 400, 0.875rem / 1.55), **meta** (mono 0.75rem, tracking 0.02em): release line, file sizes, download button sub-line.

### Named Rules
**The Mono-Means-Machine Rule.** Mono is for things the reader types or the terminal emits: keys, settings, sequences, code, file names, version and size meta. Never for headings, nav or prose.

**The Lowercase Name Rule.** The brand is "nuntio", always lowercase, including at the start of a headline; the `lc` class opts the name out of uppercase heads.

**The Width-Is-Rank Rule.** Rank is set by the width axis before size: name at 62%, heads at 75%, subheads and buttons at 87.5%, reading text at 100%.

## Layout

A single centred card on a desk. The desk pads `clamp(0px, 3vw, 40px)`; the card is at most `min(1280px, 100vw - 112px)` from 1100px up, full-bleed with no border or radius at 720px and below. Panels inside are padded `clamp(4rem, 8vw, 7.5rem)` block and `clamp(20px, 4vw, 56px)` inline.

Panel bodies sit on a 12-column grid (gap 48px / 32px): text in columns 1–5 and figure in 6–12, or mirrored (text 8–12, figure 1–7). Protocols and Download panels run full width. The cover is a 12-column grid: a full-height orange field in columns 1–6 (padding 64px), the paste figure on stock in 7–12 (padding 48px), and a six-cell spec band along the bottom (ink top rule, label over value) that reflows to 3×2 below 1100px and 2×3 below 720px.

The thumb index sits fixed on the right edge from 1100px, vertically centred; below that it becomes a sticky horizontal row under the masthead. Docs pages use a 260px sticky sidebar plus a prose column (max 78ch); below 900px the sidebar is a horizontal tab row above the article.

Breakpoints: 1100px, 900px, 720px. Spacing scale (px): 4, 8, 12, 16, 24, 32, 48, 64, 96, 128 (`s1`–`s10`). Anchors scroll with a 72px offset.

**Panel head grammar.** Every panel from 2 to 8 has fixed tiers: the crease with folio `N/8 · Name`, the hook, and at the end of the head row the key slot with the exact key, chip or release line. On mobile the key slot drops below the hook.

## Elevation & Depth

Flat. There are no shadows, gradients, blur or glass; depth comes from ruled lines, stacked tonal stock (Stock vs Stock Shade), ink borders (1.5px), the thumb-index tabs offsetting toward the page (`translateX(-4px)`, current `-6px`) and the card's rounded die-cut corner. Pressed buttons shift 1px down.

Motion is limited to 160ms ease-out colour/transform transitions, smooth anchor scrolling, and the crease drawing in on scroll (`clip-path` 0% to 100% across `entry 0% entry 40%` of the panel's view timeline, behind `@supports (animation-timeline: view())`). All of it is off under `prefers-reduced-motion`; content is never hidden by motion.

### Named Rules
**The Flat Card Rule.** Nothing casts a shadow. If a thing needs to stand out it gets a 1.5px ink border, a stock-shade fill or the orange.

## Shapes

Square by default (radius 0): buttons, tables, code cards, chips, terminal frames, inserts, images. The exceptions are deliberate and rare: the card itself (20px, the die-cut corner), keycaps (2px), thumb-index tabs (8px on the outer side only, like a physical index tab) and the numbered markers (full circle, 26px, 1.5px ink ring). Borders are 1.5px ink for structural frames, 1px Crease Rule or ink for hairlines, and a 1px dashed Crease Rule for folds. List bullets are 9px orange squares with a 1px ink border.

### Named Rules
**The Die-Cut Rule.** The card's corner is the only large radius on the site; nothing inside it may be rounded to echo it.

## Components

### Card shell and masthead
The card is stock with a 1.5px ink border, 20px radius, clipped. The masthead is a flex row under an ink 1.5px rule: brand (28px icon + lowercase "nuntio", Archivo width 75% / 800 / 1.375rem), index-label nav (Docs, Protocols, WSL, Config, GitHub; current page gets a 2px ink underline, hover a 2px orange one), and a small primary Download button at the end. Below 720px the nav wraps to a second row and drops Config.

### Buttons
- **Primary:** ink slab, stock text, radius 0, 1.5px ink border, padding 18px 24px (small variant 10px 16px), Archivo width 87.5% / 700 / 1.125rem; an optional mono sub-line (0.75rem) carries file type and size.
- **Hover / Active:** inverts to stock with ink text and ink border; active shifts 1px down. 160ms ease-out.
- **On a signal field:** background and border `#141414`, text `#EDE9DF`; hover turns transparent with ink text.
- **Secondary:** transparent with a 1.5px ink outline; hover fills ink.
- **Focus:** 3px solid ink outline, offset 3px; on signal fields the outline is `#141414` (`--on-signal`), on the back cover cream.
- **Links:** ink with a 2px orange underline at offset 3px; hover turns the underline ink.

### Keycaps and chips
- **Keycap:** mono 600, 0.8125rem, stock face, 1.5px ink border with a 3px bottom border, 2px radius, padding 2px 7px; consecutive keycaps are 3px apart. Chords are written as separate keycaps (`Ctrl` `Shift` `V`), the macOS alternative after a `/`.
- **Chip:** the same mono face for a setting (`herdr.enabled = true`), on Stock Shade with a 1.5px ink border, square, padding 3px 9px.

### Thumb index
A column of tabs on the right card edge, ≥1100px: 44px wide, min 88px high, vertical text (`02 IMAGES`), Stock Shade, 1.5px ink border without the right side, 8px outer radius. Hover moves left 4px; the current section is orange with ink text and moves left 6px. Below 1100px a sticky horizontal strip with 10px 14px padding and 1px rules between tabs; current is orange. The tab for the section in view is set by an IntersectionObserver; on the cover none is current.

### Panel head with crease
The crease is a 1px dashed Crease Rule line across the card with 8px × 1.5px ink ticks at both edges and the folio label `N/8 · Name` in index-label type on stock, offset by the gutter. It draws in on scroll. Below it, the hook (uppercase condensed) at left and the key slot at right in mono Ink Soft.

### Tables
- **Spec table** (`table.spec`): 1.5px ink rules top and bottom, Stock Shade header with a 1.5px ink underline, 1px Crease Rule between rows, first column mono 600 and never wraps.
- **Support matrix** (`table.matrix`): same ruling inside a 1.5px ink frame; header row on orange with ink condensed caps; the Support column is a mono mark and word (● Yes, ◐ Partly, ○ No); Notes in Ink Soft. Below 720px rows become two-line stacks and the header hides.
- **Prose tables:** ink rules top and bottom, Stock Shade header, cell padding 10px 12px, horizontally scrollable.

### Figures, markers and legends
A figure is evidence plus a caption. Screenshots get a 1px ink border (prose) or 1.5px ink (panels). Numbered markers (26px orange circles, ink mono digit, 1.5px ink ring and a stock halo) are positioned with `--x`/`--y` percentages over the image; below the figure an `ol.legend` repeats each marker inline (22px) with its explanation. The caption starts with `FIG. N` and has a hairline above it. Touch devices swap the paste hint.

### Terminal figures (`.term`)
HTML replicas of nuntio: Tokyo Night inside a 1.5px ink frame, square, mono, container-sized type (`min(.8125rem, (100cqw - 24px)/20)`), a `#16161e` tab strip with the active tab `#24283b`, panes split by a 1px `#3b4261` divider (stacked below 720px). `aria-hidden`; the caption and legend carry the meaning. Fig. 1 (paste demo) has three states (`path`, `attached`, `done`) toggled by `data-state`, hiding later steps with `visibility` so nothing shifts. Fig. 3 (herdr) shows tab dots, an info banner and a status bar in the app's own colours.

### Code card
Stock Shade, 1.5px ink border, square, mono 0.875rem / 1.55, padding 16px 24px, horizontal scroll. TOML section keys in Signal Ink 600, string values in Ink Soft. In prose, `pre` uses the same face with the ink border.

### Download insert
On the full orange panel, a stock insert with a 1.5px `#141414` border holds the radio-tab mechanism (no JS needed): index-label tabs on Stock Shade, current tab ink with stock text; files as 1.5px ink bordered rows (the primary file is an ink slab), mono size meta, hints in Ink Soft, and a Tokyo Night command block.

### Docs sidebar and prose
The sidebar has a label "Manual" (index-label type, Ink Soft, ink rule beneath), numbered entries (`01` mono Ink Soft + title, 1px rule between), the current entry on orange with its table of contents indented beneath. Prose: h1/h2/h3 as in Typography, inline code on Stock Shade, `pre` with ink border, images with a 1px ink border, blockquote with a 1px ink left rule, list markers in Signal Ink, tables as above.

### Back cover
Closing ink panel (Desk night/Ink night in dark): licence line, links (Source, Issues, Releases, Privacy), the statement "nuntio collects no data." in condensed caps 1.5rem, and the font credit at 75% opacity.

## Do's and Don'ts

### Do:
- **Do** keep every panel on the head grammar: folio crease, hook, key slot at the end of the head row.
- **Do** put the exact key, setting or sequence in mono and the claim in Archivo; cite the real thing (`OSC 1337`, `herdr.enabled = true`).
- **Do** show support as a mark plus a word (● Yes, ◐ Partly, ○ No) and say what is not supported as plainly as what is.
- **Do** put ink (`#141414`) text, borders and focus outlines on every orange field, in both schemes.
- **Do** back screenshots with numbered markers and a legend, and give HTML terminal replicas `aria-hidden` plus a caption.
- **Do** make every enhancement degrade: radio tabs without JS, the paste demo static in `done`, motion off under reduced motion.
- **Do** write the brand as lowercase "nuntio".

### Don't:
- **Don't** use shadows, gradients, blur, glass or glow anywhere; depth is ink lines and stock tone.
- **Don't** round anything except the card (20px), keycaps (2px), thumb-index outer edge (8px) and markers.
- **Don't** introduce a second accent, or carry Tokyo Night colours outside a terminal figure or command block.
- **Don't** encode state in colour alone, or use cream-on-orange or orange-on-orange text.
- **Don't** set headings, nav or running prose in mono, or body text wider than 62ch (panels) / 72ch (prose).
- **Don't** load anything from a third party: fonts, scripts and figures are served from the site.
- **Don't** hide content behind scroll animation; reveals only draw rules.

### Not canonized (carried by the build, do not copy to new surfaces)
- Numbered markers draw a stock `box-shadow` ring (0 0 0 2px) as a halo, which breaks the Flat Card Rule; new markers should use a border only.
- Orange-field buttons hard-code `#141414` / `#EDE9DF` instead of tokens.
- Prose blockquote rule is 1px (the build), not the 3px of the original plan.
