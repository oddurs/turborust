---
id: 65
title: 'site: redraw illustrations, new type system, fix layout bugs'
type: bug
status: done
created: 2026-09-09
updated: 2026-09-09
priority: p2
---

## What happens

Rendered under Playwright at 1440 and 390, in both colour schemes, the site had
six defects — four of them invisible to anyone reading the source:

1. **The overlay panel list rendered one word per line.** `.steps li` declared a
   two-column grid and then handed it three children. The `<span>` wrapped onto
   row two, into the 34px number column, and every word broke.
2. **Every wave divider stopped at 64ch.** `pulldown-cmark` does not recognise
   `svg`, so a divider on its own line came back wrapped in a `<p>` — which
   inherits the prose measure. A full-bleed wave ended at 975px on a 1440px page.
3. **157px of horizontal overflow on mobile**, from `<pre>` blocks in a
   `1fr 1fr` grid. `1fr` will not shrink below its content's min-content width.
4. **The tide canvas was inset by one gutter on each side**, because the hero
   still matched the generic `section` padding rule after its own padding moved
   to `.hero-inner`. Water that does not reach the edge is a rectangle.
5. **The crab's cast shadow inverted in dark mode.** It was painted with `--ink`,
   which is near-white on the night beach, so the shadow lit up.
6. **The crab's eye whites took `--shell`**, also inverted, so in dark mode a
   dark pupil sat on a dark eye and the cursor-tracking was invisible.

Docs code blocks were also clipped: the column was `--measure`, which is a
reading width, and several config samples are wider than that.

## What should happen

All six fixed, and the two things the design was carrying badly replaced:

- **Typography.** Bricolage Grotesque + Instrument Sans out; **Archivo** (display)
  + **Source Sans 3** (body) + **IBM Plex Mono** (code) in. Hierarchy now runs
  through Archivo's width axis rather than through size alone, so the headline
  reads as signage instead of shouting — 66px at `wdth 112` where it was 92px
  at `wdth 82`.
- **Illustrations.** The crab was an ellipse, two circles and four stick legs.
  It is now drawn: a hexagonal carapace with a flat front margin, eight jointed
  legs, two chelae with an actual pincer gap, an underside tone and a cast
  shadow. The nav mark is redrawn to match and holds at 25px. The tide was three
  sine layers and a stroke; it is now a sheet of water bounded by two waves,
  with wet sand lagging behind it, drifting sun glitter and foam that breaks on
  the crests.

## Reproduction

1. `cargo run --manifest-path site/Cargo.toml && turborust up`
2. Load `/` at 390px wide — the page scrolls sideways by 157px.
3. Scroll to "A crab in the corner of your page" — the list body is one word wide.
4. Compare the wave divider's right edge against the viewport at 1440px.

## Verification

Screenshotted before and after at 1440/390 in light and dark, plus a
scripted check for document-level horizontal overflow, for any element wider
than the viewport, and for `pre` elements whose content is clipped rather than
scrollable. All clear; `scripts/agent check` green.
