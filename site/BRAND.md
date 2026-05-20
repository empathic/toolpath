# Toolpath Brand Book

Provenance is forensic. Toolpath records who changed what, why, what
they tried that didn't work, and how to verify all of it — git blame
for everything that happens to code, including the parts git never
sees. The visual identity has to feel like a manual that earned its
precision through use, not aesthetic posturing.

The system holds two readings in tension. **Workshop** — warm, hand-marked,
material. The shop where the work is done. **Terminal** — UNIX man pages,
RFCs, `git log` output. The medium the tool actually lives in. Both
sensibilities want the same things: monospace type, sharp corners,
disciplined chrome, copper as the only mark that says "the system acted
here." Where they disagree, the workshop wins on color and the terminal
wins on layout.

Two themes. Light is the canonical workshop — copper on parchment.
Dark is the same shop after a long shift — copper on charred wood.
Never neutral gray, never cool black, never decorative gradients.

This document is the source of truth for site implementation. It
replaces an earlier serif/journal direction; monospace-only is now
load-bearing.

## Family

Toolpath sits in the Empathic family of three sibling brands:

| Sibling             | Voice                                       | Palette signal         |
| ------------------- | ------------------------------------------- | ---------------------- |
| `empathic/site`     | Parent. Reticle SVG hero, scope-and-target. | Cool warm-cast         |
| `empathic/pathbase` | Night Garden. Forest floor, growth.         | Emerald + gold         |
| **Toolpath**        | Workshop Terminal. Tools, marks, contour.   | **Copper + parchment** |

What's shared across the family: sharp corners, dark + light themes,
tracked-uppercase mono chrome, 940px container, 48px nav, the
`@layer reset, base, layout, components` cascade. What's distinct: the
palette, the iconography of the primary visual element, **and the body
type**. Toolpath is documentation-heavy in a way the other two aren't,
so its body prose is set in a serif (Source Serif 4) for long-form
readability while the chrome stays mono. Toolpath's distinctive marks
are **copper**, the **DAG-as-topographic-map**, the **FIG_NNN margin
label** convention, and the **mono-chrome / serif-body register split**.

## Canonical tokens

A single source of truth. Components reference tokens, never hex.

| Token              | Dark                        | Light                      | Purpose                                             |
| ------------------ | --------------------------- | -------------------------- | --------------------------------------------------- |
| `--accent`         | `#c97e3f`                   | `#b5652b`                  | Links, buttons, wordmark, DAG edges. The tool mark. |
| `--accent-dim`     | `rgba(201, 126, 63, 0.10)`  | `rgba(181, 101, 43, 0.08)` | Tinted surfaces (code blocks, hovered cards)        |
| `--bg`             | `#14110d`                   | `#f6f1eb`                  | Page background                                     |
| `--bg-surface`     | `#1c1813`                   | `#ece5db`                  | Cards, code blocks, panels                          |
| `--bg-elevated`    | `#251f18`                   | `#dfd6c8`                  | Hovered/active surfaces                             |
| `--text`           | `#ccc6bb`                   | `#2d2a26`                  | Primary body and headings                           |
| `--text-secondary` | `#8a7d6e`                   | `#6e655c`                  | Captions, labels, table headers, inactive chrome    |
| `--text-dim`       | `#5c5346`                   | `#9a8e80`                  | Hints, placeholders, contour visuals                |
| `--alert`          | `#c44030`                   | `#9a3020`                  | Errors, dead ends, destructive actions              |
| `--border`         | `rgba(255, 240, 220, 0.07)` | `rgba(45, 42, 38, 0.08)`   | Default 1px separators                              |
| `--border-strong`  | `rgba(255, 240, 220, 0.16)` | `rgba(45, 42, 38, 0.18)`   | Focus rings, emphasized separators                  |
| `--selection-bg`   | `#c97e3f`                   | `#b5652b`                  | Text selection background                           |
| `--selection-text` | `#14110d`                   | `#f6f1eb`                  | Text selection foreground                           |

Borders are derived from text color at low alpha so they stack cleanly
on `bg-surface` and `bg-elevated` without re-specification.

### Spacing

| Token         | Value           | Purpose                            |
| ------------- | --------------- | ---------------------------------- |
| `--space-xs`  | `0.25rem` (4px) | Inline gaps, icon-to-label spacing |
| `--space-sm`  | `0.5rem` (8px)  | Between related elements           |
| `--space-md`  | `1rem` (16px)   | Component internal padding         |
| `--space-lg`  | `1.5rem` (24px) | Container padding, section gutters |
| `--space-xl`  | `2.5rem` (40px) | Between major sections             |
| `--space-2xl` | `4rem` (64px)   | Hero / page-level breathing room   |

### Layout primitives

| Token       | Value           | Purpose                            |
| ----------- | --------------- | ---------------------------------- |
| `--max-w`   | `940px`         | Content max-width (matches family) |
| `--nav-h`   | `48px`          | Navigation height                  |
| `--measure` | `40rem` (~62ch) | Prose column for long-form reading |

## Typography

Two registers, one role each. Monospace owns the chrome, the headings,
the code, and every label. A scholarly serif owns the body prose. The
split is principled, not decorative — and it is the one place where
Toolpath's type system diverges from its siblings.

**Why mono for chrome.** Toolpath lives at the CLI. Its primary outputs
are JSONL, JSON Schema, `git log`, and `path p render` ASCII trees. Its
primary inputs are conversation logs and diff hunks — already monospace
at rest. Nav, buttons, headings, table headers, code blocks, and
figure labels stay mono so the site never changes voice from the tool
it documents. Berkeley Mono is preferred where licensed (desktop app,
internal builds); IBM Plex Mono is the canonical free fallback for the
web site.

**Why serif for body.** Unlike `empathic/site` and `pathbase` — which
are mostly chrome — Toolpath publishes long-form documentation: the
RFC, the format spec, the FAQ, per-crate references. Reading several
thousand words of monospace at length is fatiguing. Source Serif 4 is
chosen for its mid-century technical-publishing lineage (closer to
Century Schoolbook than to a literary serif) — it reads as _journal
paper_, not as _novel_. Body prose, list items, blockquotes, and table
data cells are all serif.

```
/* Mono — chrome, headings, code, labels */
"Berkeley Mono", "IBM Plex Mono", "SF Mono", ui-monospace,
"Cascadia Code", "Source Code Pro", Menlo, Consolas, monospace;

/* Serif — body prose only */
"Source Serif 4", "Source Serif Pro", Georgia, "Times New Roman", serif;
```

### Canonical hierarchy

One table. Every text element pinned to family / size / weight /
tracking / color. Both themes are implicit — colors resolve through
tokens.

| Element                             | Family    | Size                       | Weight  | Tracking       | Color              |
| ----------------------------------- | --------- | -------------------------- | ------- | -------------- | ------------------ |
| Wordmark                            | mono      | `0.85rem`                  | 700     | `0.12em` upper | `--accent`         |
| Page heading (h1)                   | mono      | `1.5rem`                   | 700     | `0.08em` upper | `--accent`         |
| Hero heading                        | mono      | `clamp(1.8rem,5vw,2.6rem)` | 700     | `0.08em` upper | `--accent`         |
| Section (h2)                        | mono      | `1.1rem`                   | 600     | `0.06em` upper | `--text`           |
| Subsection (h3)                     | mono      | `0.95rem`                  | 600     | `0.05em` upper | `--text`           |
| Sub-subsection (h4)                 | mono      | `0.85rem`                  | 600     | `0.04em` upper | `--text-secondary` |
| **Body / list / blockquote / `td`** | **serif** | **`17px`**                 | **400** | **`0`**        | **`--text`**       |
| Tagline / subtitle                  | serif     | `1.05–1.1rem`              | 400     | `0`            | `--text-secondary` |
| Code                                | mono      | `0.85em`                   | 400     | `0`            | `--text`           |
| Caption / label                     | mono      | `0.72rem`                  | 600     | `0.10em` upper | `--text-secondary` |
| Page-title kicker                   | mono      | `0.625rem` (10px)          | 500     | `0.22em` upper | `--text-dim`       |
| Table header (`th`)                 | mono      | `0.72rem`                  | 600     | `0.08em` upper | `--text-secondary` |
| Nav link                            | mono      | `0.69rem` (11px)           | 500     | `0.05em` upper | `--text-secondary` |
| Footer                              | mono      | `0.72rem`                  | 400     | `0.04em`       | `--text-dim`       |
| Figure label (FIG_NNN)              | mono      | `0.62rem`                  | 600     | `0.16em` upper | `--accent`         |

Body line-height is **1.65**; headings are **1.2–1.3**. Body is
left-aligned; never justified, never centered. Hyphens off — clean
breaks. The `15px` `html` font-size is the **rem anchor for chrome**,
not the body display size; body explicitly overrides to `17px` serif.

## The diagram

The DAG is Toolpath's primary visual artifact, not a decoration. A
provenance graph is what the project exists to render. Brand discipline
applies to the diagram **first**, and the chrome around it second.

### Reading

Read every Toolpath DAG as a **topographic map**. Each step is an
elevation. The active path is the ridge — the route that survived. Dead
ends are the spurs that lead nowhere. Edges are toolpath traces between
plunge points. This metaphor is the reason copper, contour, and
margin-labeled figures all belong together.

### Actor palette

Actor type drives node color. Active hands are copper; passive
instruments are pencil; abandonment is muted red.

| Actor     | Fill                | Stroke             | Stroke style | Reading                                 |
| --------- | ------------------- | ------------------ | ------------ | --------------------------------------- |
| `human:*` | `--accent` at 18%   | `--accent`         | solid        | The originating hand                    |
| `agent:*` | `--accent` at 30%   | `--accent`         | solid        | Heavier fill — agents do more per step  |
| `tool:*`  | `--text-dim` at 15% | `--text-secondary` | solid        | Passive instrument (rustfmt, prettier)  |
| `ci:*`    | `--text-dim` at 15% | `--text-secondary` | dashed       | Automation — the dash signals "no hand" |
| dead end  | `--alert` at 18%    | `--alert`          | dashed       | Off the ancestry of `path.head`         |

### Edges

| Edge        | Color              | Width | Reading                           |
| ----------- | ------------------ | ----- | --------------------------------- |
| Active path | `--text`           | 1.5px | The route that became `path.head` |
| Base        | `--accent`         | 1px   | Default — the toolpath line       |
| Inactive    | `--text-secondary` | 1px   | Branches that didn't survive      |

Arrowheads are small (8–10px), filled, in the same color as the edge.
No bezier curves — orthogonal or straight only. Right-angle bends echo
dimension callouts on a machinist's drawing.

### Labels

Step nodes carry a monospace `--text` label (the step ID, e.g.
`step-003`) and an optional secondary `--text-secondary` line (actor or
short summary). Edges may carry a small `--accent` label for tool name
or commit SHA prefix. All labels are uppercase; tracking matches the
table above.

### What "on-brand" means for a DAG render

A generic graphviz output is not on-brand even if the colors are right.
On-brand renders are: orthogonal edges, sharp-corner nodes, monospace
uppercase labels, FIG_NNN margin caption, no drop shadows, no rounded
corners, `--accent` reserved for the tool mark and never used as a
neutral fill.

## Visual elements

### Topographic divider

A row of small dashes in `--accent` at low opacity — contour ticks on a
milling chart. Used between **major** page sections only. Not a generic
`<hr>`.

```css
.divider {
  background: repeating-linear-gradient(
    to right,
    var(--accent) 0 6px,
    transparent 6px 12px
  );
  height: 6px;
  opacity: 0.4;
  margin: var(--space-xl) 0;
}
```

### Figure margin labels (FIG_NNN)

Every diagram, schematic, or rendered DAG on the site gets a label in
its left margin, rotated 90° counter-clockwise:

```
FIG_001   ┌──────────────────┐
          │   step-001       │
          │   step-002       │
          │   step-003 ◀─ HEAD │
          └──────────────────┘
```

Monospace, uppercase, tracked, `--accent`. Numbered sequentially within
a page. The convention is from a machinist's blueprint — every
illustration is named. This is one of two visual conventions (with the
divider) that signal "this is Toolpath, not pathbase."

### Contour motif (background)

Concentric contour lines in `--text-dim` at 8–12% opacity. Used
sparingly — section backgrounds, hero backdrops, never under body
text. Suggests the strata of a provenance chain. Implementation is an
inline SVG or a CSS `radial-gradient` stack; never a raster.

### No drop shadows. No gradients (except the contour radial). No drop caps.

Depth comes from **surface layering** (`bg` → `bg-surface` →
`bg-elevated`) and **border opacity**. The shop is well-lit and flat;
the depth lives in the work, not in the lighting.

## Components

### Navigation

Fixed, full-width, 48px tall. `--bg` background, 1px `--border` bottom.
Wordmark left, links right. Links are `--text-secondary` at 11px
uppercase tracked; hover shifts to `--text`. Theme toggle and menu
button live at the right edge.

### Hero

Full-viewport height on the homepage only. Centered content, max-width
720px. Headline at the hero size from the type table; sub-copy at 13px
`--text-secondary`. Optional contour-motif backdrop. CTA is a bordered
copper button (see below).

### Buttons (CTA)

```
border: 1px solid var(--accent);
color: var(--accent);
background: transparent;
padding: 12px 28px;
font-size: 12px;
letter-spacing: 0.10em;
text-transform: uppercase;
```

Hover fills with `--accent`, text inverts to `--bg`. No rounded corners.
No box-shadow. No icon-with-button unless the icon is monospace text
(`→`, `▾`).

### Links (inline)

`--accent` color, underline 1px at 2–3px offset. Hover thickens the
underline to 2px; never adds a fill. External links may be suffixed
with a small monospace `↗` in `--text-secondary`.

### Code blocks

```
background: var(--bg-surface);
border-left: 3px solid var(--accent);
border-left-color: var(--accent-dim) ; /* falls back to ~30% */
padding: var(--space-md);
font-size: 0.85em;
line-height: 1.55;
overflow-x: auto;
```

No rounded corners. No copy button hovering inside. Syntax highlighting
stays inside the warm palette — see Prism token mapping in the
implementation appendix.

### Cards

```
background: var(--bg-surface);
border: 1px solid var(--border);
padding: var(--space-md);
```

No shadow, no radius. The card edge is a cut, not a lift.

### Tables

No outer border. Header row is uppercase tracked `--text-secondary` —
no fill, no underline. Row separators are 1px `--border`. Cells get
`var(--space-sm)` vertical padding. Numeric columns right-align.

### Prose container

Single column, max-width `--measure` (~62ch) for long-form reading.
Body left-aligned. Headings break the prose with `--space-xl` above.
Lists use the standard mono bullet (`•` or `-`); no custom markers.

### Footer

Small, monospace, `--text-dim`. Wordmark or "TOOLPATH" reprise on the
left, repo and license links on the right. 1px `--border` top. Same
horizontal rhythm as the nav.

## Tone of voice

### Stance

Direct, precise, material. Educational without being condescending —
the reader is smart, just unfamiliar. Concrete before abstract. State
the example first, derive the principle second.

### Material vocabulary

Reach for: **carve, layer, trace, mill, plunge, mark, cut, fixture,
machine.** Avoid: _journey, ecosystem, holistic, leverage, paradigm,
seamless._ The vocabulary is the vocabulary of making things; it
reinforces that Toolpath records the act of making.

### Body copy

| Good                                                                                                                          | Bad                                                                                 |
| ----------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| A step records a single change to one or more artifacts by one actor.                                                         | Steps are our way of representing the concept of changes in the system.             |
| Dead ends are implicit — steps not on the ancestry of `path.head`.                                                            | We have a flexible, opinionated approach to handling abandoned work.                |
| When Claude writes code, rustfmt reformats it, and a human refines it, git blame attributes everything to the human's commit. | In modern software development workflows, attribution can sometimes be challenging. |

### CLI voice

Toolpath is primarily a CLI. Three surfaces need brand discipline:

**Error messages.** Name the artifact, say what failed, suggest the
fix. One sentence each.

```
Good:  error: cache hit at ~/.toolpath/documents/claude-abc123.json (use --force to overwrite)
Bad:   Error: An issue was encountered while attempting to write the cache.
```

**`--help` text.** Imperative one-liners. Arguments before flags.
Always include one example invocation. No marketing.

```
Good:  Share an agent session to Pathbase via an interactive picker.
       Usage: path share [--harness NAME] [--session ID] [--url URL]
       Example: path share --harness claude

Bad:   The share command provides a powerful way to bring your
       conversational context into the Pathbase ecosystem.
```

**README rhythm.** One-sentence thesis, install, smallest useful
example, then escape hatches. The shape of a UNIX manual page, not a
landing page.

### Headings and labels

Imperative or noun-phrase, never sentence. "Build and test," not "How
to build and run the test suite." Uppercase tracked tokens are the
voice of the masthead — keep them short (1–3 words).

## Wordmark

`TOOLPATH` set in the display monospace, weight 700, tracked at
`0.12em`, colored `--accent`. Always uppercase. Always copper. Always
tracked wide enough that each letter reads as a coordinate on the
grid — the tracking _is_ the mark.

Preferred lockup: wordmark left, optional tagline right in regular
weight `--text-secondary`.

```
TOOLPATH                                              Know your tools.
```

The wordmark never appears in `--text` or any neutral color. It is
either copper on `--bg` or `--bg` reversed on a copper field. There is
no monogram; the full word is the mark.

## What we're not

- **We are not Night Garden.** Pathbase's emerald-on-charcoal forest
  floor is its own brand. Don't import emerald, gold, or slate-green
  into Toolpath. The two sites are siblings, not skins.
- **We are not the parent empathic palette.** The parent site has its
  own warm cast at slightly cooler hues (`#bf7e48` accent, `#0a0a0c`
  bg). Toolpath's copper is warmer (`#b5652b` / `#c97e3f`); its dark
  ground is browner (`#14110d`). Family resemblance, not identity.
- **We are not a three-register journal.** An earlier direction
  leaned on IBM Plex Sans Condensed for headings and Source Serif for
  body. Headings have collapsed into the mono register that runs
  through the rest of the chrome; only the body kept its serif.
  Reaching for a third typeface, or putting the serif back into nav
  or buttons, is a regression.
- **We are not all-mono.** A tempting simplification is "monospace
  everywhere" to fully match the family. We tried it. Long-form prose
  in mono is hard on the eyes — and Toolpath publishes more long-form
  prose than its siblings. The hybrid is deliberate.
- **We are not decorative.** No illustrations of people, no stock
  photography, no abstract gradient compositions. The diagram is the
  illustration; the contour is the texture; the wordmark is the logo.

## Don'ts

Each rule comes with the reason it exists. A rule without a reason
breaks at the first edge case.

- **Don't introduce cool-cast colors.** The system is warm because
  warmth says "made by hand," and Toolpath records hands at work.
- **Don't round corners.** Sharp edges read as milled; rounded edges
  read as molded plastic. Toolpath is not molded.
- **Don't add drop shadows or gradients.** Depth comes from surface
  layering. Shadows imply elevation; Toolpath shows strata, not
  altitude.
- **Don't put serif in chrome, and don't put mono in body prose.**
  The register split is load-bearing. Nav, buttons, headings, table
  headers, code, and labels are mono because the content they wrap is
  mono. Body prose is serif because long-form reading at 17px in a
  monospace makes eyes tired. Mixing the two breaks the contract on
  both ends.
- **Don't hardcode hex in components.** Every color is a token. Themes
  flip cleanly only when nothing leaks below the token layer.
- **Don't use color alone to encode hierarchy.** Reach for size,
  weight, and tracking first. Color is the last differentiator —
  copper is for action, not for ranking.
- **Don't decorate the DAG.** The diagram is information. No animated
  edges, no glowing nodes, no curved beziers, no node icons. The
  topographic reading depends on the diagram looking technical.
- **Don't write marketing copy in `--help` text.** A CLI is read by
  someone who is already inside the door; selling them is noise.
- **Don't replace the FIG_NNN convention with auto-numbering or
  remove the margin label "for cleanliness."** It is one of the few
  marks that distinguish Toolpath from any other monospace site.

## Implementation appendix

### Cascade contract

CSS is organized in four layers, in this order:

```css
@layer reset, base, layout, components;
```

- `reset` — box-sizing, margin/padding zeroing, list/anchor/button
  resets. Nothing brand-specific.
- `base` — `:root` token definitions, `[data-theme="light"]` overrides,
  `body` defaults, `::selection`, keyframes.
- `layout` — `.page`, `.page-inner`, container utilities, page-title
  kicker. No component styling.
- `components` — everything in the Components section above. Every
  component file declares `@layer components { ... }`.

A component must never set a token-level variable; it consumes them. A
layout primitive must never touch component visuals.

### Theme switching

Default to `prefers-color-scheme`. Persist user override on `<html>`
via `data-theme="light"` or `data-theme="dark"`. Tri-state the toggle
control: system / dark / light. Read the active state from
`data-theme-pref` for icon swaps.

```js
const pref = localStorage.getItem("theme") ?? "system";
document.documentElement.dataset.themePref = pref;
if (pref !== "system") document.documentElement.dataset.theme = pref;
```

### Font loading

IBM Plex Mono from Google Fonts, weights 400 / 500 / 600 / 700, with
`font-display: swap`. Berkeley Mono, when present, loads from
`/fonts/BerkeleyMono-{Regular,Medium,Bold}.woff2` as the first stack
entry. The fallback chain in the type token is the contract — never
hardcode a different list at the component level.

### Prism token map

Syntax highlighting stays inside the warm palette. Token colors:

| Prism token               | Color                                             |
| ------------------------- | ------------------------------------------------- |
| `comment`                 | `--text-secondary` italic                         |
| `keyword`                 | `#7a4b8a` (warm violet, dark) / `#5b3268` (light) |
| `string`, `url`           | `#6e7d3a` (moss, dark) / `#4d5a26` (light)        |
| `number`                  | `#8b5e3c` (dark) / `#7a4520` (light)              |
| `function`, `class-name`  | `--accent`                                        |
| `property`                | `#9e5019` (dark) / `#8a3f10` (light)              |
| `operator`, `punctuation` | `--text-secondary`                                |
| `namespace`               | `--text-dim`                                      |

These are the only non-token colors permitted in the site, and they
appear only inside `<code>` blocks.

### Accessibility floor

- All token pairs meet **WCAG AA** for body text (4.5:1) on their
  intended background. Verify before changing any token value.
- Focus states use `--border-strong` as a 2px outline at `2px` offset.
  Never rely on color alone (the outline change is structural).
- `prefers-reduced-motion: reduce` disables the contour-motif
  animation (if any) and the smooth-scroll on anchor jumps.
- Selection contrast is enforced by `--selection-text` against
  `--selection-bg` — both themes ship a tested pair.

### Asset inventory

Fonts and JS libraries are self-hosted — no third-party CDNs at runtime.
Sources are pulled from npm at build time and copied to `_site/` via
`eleventyConfig.addPassthroughCopy`. Filenames are stable so
`<link rel="preload">` and `@font-face` URLs match across builds.

| Asset                               | Source (build-time)                          | Output path                                          |
| ----------------------------------- | -------------------------------------------- | ---------------------------------------------------- |
| IBM Plex Mono 400/500/600/700       | `@fontsource/ibm-plex-mono` (latin subset)   | `/fonts/plex-mono-{w}.woff2`                         |
| Source Serif 4 400/600 + 400 italic | `@fontsource/source-serif-4` (latin)         | `/fonts/source-serif-{w}[-italic].woff2`             |
| Berkeley Mono (preferred, licensed) | drop-in at `site/fonts/BerkeleyMono-*.woff2` | `/fonts/BerkeleyMono-*.woff2` (gitignored)           |
| d3 v7                               | `d3` (npm)                                   | `/vendor/d3.min.js`                                  |
| dagre-d3 v0.6                       | `dagre-d3` (npm)                             | `/vendor/dagre-d3.min.js`                            |
| xterm v6 + addon-fit                | `@xterm/xterm`, `@xterm/addon-fit`           | `/vendor/xterm{,-addon-fit}.js`, `/vendor/xterm.css` |
| prismjs (core+json+diff)            | `prismjs`                                    | `/vendor/prism{,-json,-diff}.js`                     |
| Wordmark SVG                        | (TBD)                                        | `/assets/wordmark.svg`                               |
| Favicon (16 / 32)                   | (TBD)                                        | `/favicon-{16,32}.png`                               |
| Favicon SVG                         | (TBD)                                        | `/favicon.svg`                                       |
| Apple touch icon                    | (TBD)                                        | `/apple-touch-icon.png` (180×180)                    |
| OG image template                   | (TBD)                                        | `/assets/og-template.svg` (1200×630)                 |

**Preload contract.** The four most-used font weights (Source Serif 400,
Plex Mono 400/600/700) are emitted as `<link rel="preload" as="font"
crossorigin>` in `<head>` so the browser starts fetching them in
parallel with the stylesheet — no late swap on hard refresh.

## Summary

| Attribute    | Choice                                                                   |
| ------------ | ------------------------------------------------------------------------ |
| Palette      | Warm copper accent, charred-wood / parchment grounds                     |
| Themes       | Dark + light, both warm-cast, token-driven                               |
| Typography   | Mono chrome / mono headings (IBM Plex Mono); serif body (Source Serif 4) |
| Hierarchy    | Tracked uppercase chrome (10–12px); 17px / 1.65 serif body               |
| Container    | 940px max-width, 48px nav, 24px gutter, sharp corners                    |
| Diagram      | Topographic DAG; copper actors, pencil tools, red dead ends              |
| Distinctives | Topographic divider, FIG_NNN margin labels, contour motif                |
| Voice        | Direct, precise, material — UNIX man page on warm stock                  |
| Cascade      | `@layer reset, base, layout, components` — non-negotiable                |
| Don'ts       | Cool colors, rounded corners, shadows, gradients, hex in components      |
