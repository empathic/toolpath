# Toolpath Brand Book

---

## Design philosophy

**Technical manual meets shop floor.** Toolpath documents how code transforms through multiple actors. The visual identity should feel like a Bell Labs internal reference — the Bell System Technical Journal, UNIX programmer's manuals, research memoranda — printed on warm stock and annotated by hand.

Two sensibilities held in tension:

- **The technical reference** — condensed sans-serif headings, ruled tables, precise hierarchy. The authority of a published standard. Typography that says "this has been reviewed."
- **The workshop annotation** — warm paper, copper ink, topographic contour lines, hand-annotated margins. The tactile evidence that someone cared enough to mark it up.

The **provenance diagram** is Toolpath's native visual form. DAG diagrams of steps, actors, and dead ends are the core illustrations — rendered as topographic contour maps where each step is an elevation and dead ends are valleys that lead nowhere.

---

## Color palette

Warm and material. The palette comes from the workshop: graphite on kraft paper, copper tooling, wood grain, contour ink.

| Token            | Value       | Usage                                                        |
| ---------------- | ----------- | ------------------------------------------------------------ |
| `--graphite`     | `#2d2a26`   | Body text, primary foreground                                |
| `--copper`       | `#b5652b`   | Accent: links, annotations, diagram highlights, the wordmark |
| `--copper-light` | `#b5652b15` | Tinted backgrounds (code blocks, cards)                      |
| `--ground`       | `#f6f1eb`   | Page background (warm parchment)                             |
| `--grain`        | `#ece5db`   | Secondary surface (card backgrounds, code blocks)            |
| `--pencil`       | `#8a8078`   | Secondary text, captions, figure labels, contour lines       |
| `--white`        | `#ffffff`   | Bright surface when contrast is needed                       |

The copper is not decorative — it is the tool mark. It appears wherever the system acts: links, diagram edges, annotations, the logotype. Everything else is graphite on parchment.

### Actor colors (Toolpath-specific)

In DAG diagrams, actors get distinct contour fills while staying within the warm palette:

| Actor     | Fill        | Border           |
| --------- | ----------- | ---------------- |
| `human:*` | `#b5652b18` | `#b5652b`        |
| `agent:*` | `#b5652b30` | `#b5652b`        |
| `tool:*`  | `#8a807815` | `#8a8078`        |
| `ci:*`    | `#8a807815` | `#8a8078` dashed |
| Dead ends | `#c4403018` | `#c44030` dashed |

Humans and agents are copper (the active hands). Tools and CI are pencil-gray (the passive instruments). Dead ends break the palette with muted red — an abandoned cut line.

---

## Typography

Three registers in the tradition of Bell Labs technical manuals. Tight, mechanical, authoritative — the typography of the Bell System Technical Journal and UNIX reference manuals.

### Display: Condensed sans-serif

- **Font**: [IBM Plex Sans Condensed](https://fonts.google.com/specimen/IBM+Plex+Sans+Condensed) (fallback: Helvetica Neue, Arial Narrow)
- **Usage**: Logo/wordmark ("TOOLPATH"), page headings (h1–h3), navigation, table headers, footer
- **Style**: All uppercase, tracked, bold (700) for h1/wordmark, semibold (600) for h2–h3
- **Color**: `--copper` for h1/wordmark, `--graphite` for h2–h3

The condensed sans is the voice of the technical manual masthead. IBM Plex Sans Condensed carries the direct lineage of Helvetica — the typeface that defined Bell Labs publications — through IBM's own technical heritage. Narrow letterforms, wide tracking: precision instrument typography.

### Body: Scholarly serif

- **Font**: [Source Serif 4](https://fonts.google.com/specimen/Source+Serif+4) (fallback: Georgia, Times New Roman)
- **Usage**: Body text, paragraphs, long-form explanation, taglines/subtitles (italic)
- **Style**: Regular weight, left-aligned, moderate line height (1.5–1.55)
- **Size**: 17px base
- **Color**: `--graphite`

Source Serif is closer to Century Schoolbook — the canonical text face of mid-century technical publishing — than literary serifs like Newsreader. Crisper stroke contrast, more mechanical terminals. It reads as "journal paper," not "novel."

### Labels: Monospace / technical

- **Font**: [IBM Plex Mono](https://fonts.google.com/specimen/IBM+Plex+Mono) (fallback: SF Mono, Menlo, Consolas)
- **Usage**: Code blocks, inline code, figure labels, diagram annotations, metadata, actor strings, h4 headings
- **Style**: All uppercase for labels (FIG_001, STEP-003), regular case for code
- **Size**: 0.85em relative to body
- **Color**: `--copper` for labels, `--graphite` for code
- **Letter-spacing**: Slightly tracked for uppercase labels (0.04em)

IBM Plex Mono completes the type system. Tighter than JetBrains Mono, more evocative of keypunch output and teletype listings. The machine's voice, rendered in the same design language as the display face.

### Hierarchy

| Element              | Font              | Size        | Weight | Color                   |
| -------------------- | ----------------- | ----------- | ------ | ----------------------- |
| Wordmark             | Display           | 0.85rem     | 700    | `--copper`              |
| Page heading (h1)    | Display           | 1.5rem      | 700    | `--copper`              |
| Hero heading         | Display           | 2.4rem      | 700    | `--copper`              |
| Section heading (h2) | Display           | 1.1rem      | 600    | `--graphite`            |
| Subsection (h3)      | Display           | 0.95rem     | 600    | `--graphite`            |
| Sub-subsection (h4)  | Monospace         | 0.85rem     | 600    | `--pencil`              |
| Body                 | Serif             | 1rem (17px) | 400    | `--graphite`            |
| Caption / label      | Display uppercase | 0.8rem      | 600    | `--pencil`              |
| Table header         | Display uppercase | 0.8rem      | 600    | `--pencil`              |
| Code                 | Monospace         | 0.85rem     | 400    | `--graphite`            |
| Nav links            | Display uppercase | 0.78rem     | 500    | `--pencil` / `--copper` |

---

## Layout

### Spatial principles

- **Wide container**: 72rem max-width (the reference manual is a big book)
- **Generous margins**: Breathing room around all content — like a sketchbook page with wide borders
- **Two-column affinity**: Text on the left, diagrams/figures on the right — when the content calls for it. Single-column for pure prose.
- **Justified body text**: With proper hyphenation (`hyphens: auto`). Left-aligned is acceptable fallback.
- **Drop caps**: First paragraph of major sections gets a drop cap in the pixel font or the serif at display weight

### Grid

```
|  margin  |  text column (40–45ch)  |  gutter  |  figure column  |  margin  |
```

On narrow screens, stack to single column (text above figure).

### Spacing scale

Multiples of 0.5rem:

| Token        | Value    | Usage                                   |
| ------------ | -------- | --------------------------------------- |
| `--space-xs` | `0.5rem` | Inline spacing, tight gaps              |
| `--space-sm` | `1rem`   | Between related elements                |
| `--space-md` | `2rem`   | Between sections                        |
| `--space-lg` | `4rem`   | Major section breaks                    |
| `--space-xl` | `6rem`   | Hero padding, page-level breathing room |

---

## Visual elements

### Decorative divider

A topographic contour line — a single wavy horizontal rule that suggests layered terrain. Used sparingly:

- Below the hero/header
- Between major page sections (never between every section)

```css
.divider {
  height: 3px;
  background: var(--copper);
  opacity: 0.3;
  mask-image: url("data:image/svg+xml,..."); /* wavy contour line */
}
```

Alternatively, a row of small squares in the pixel-grid motif (echoing the CNC coordinate grid):

```css
.divider-grid {
  background: repeating-linear-gradient(
    to right,
    var(--copper) 0 6px,
    transparent 6px 12px
  );
  height: 6px;
  opacity: 0.4;
}
```

### Code blocks

- Background: `--grain` (warm paper)
- Border-left: 3px solid `--copper` at ~30% opacity
- Font: Monospace, `--graphite`
- No rounded corners (sharp, milled edges)
- Generous padding (1.25rem)

### Cards

- Background: `--grain`
- Border: 1px solid `--copper` at ~15% opacity
- No shadow, no border-radius
- The card edge should feel like a cut — clean, precise

### Tables

- No outer border
- Header row: Monospace uppercase, `--pencil`, letter-spaced
- Row borders: 1px solid `--copper` at ~10% opacity
- Clean, technical, scannable

### Links

- Color: `--copper`
- Underline: 1px, offset 2px
- Hover: heavier underline or slight opacity shift
- The copper is always the copper

### Figure labels

Rotated 90° counter-clockwise in the left margin of diagrams:

```
FIG_001  [  STEP DAG  ]
```

Monospace, uppercase, tracked, `--pencil`. The convention of a technical drawing — every illustration is labeled and numbered, like a machinist's blueprint.

---

## Illustration style

### Diagrams (Toolpath-native)

DAG diagrams are the core visual. Render them as topographic contour maps:

- **Nodes**: Rectangles with 1px `--copper` border, faint warm fill. Each step is an elevation — active path nodes are "higher" (darker fill), dead ends are "lower" (lighter, dashed)
- **Edges**: 1px `--copper` lines with small arrowheads — the toolpath connecting plunge points
- **Labels**: Monospace, uppercase, `--copper`
- **Dead ends**: `#c44030` dashed border, faint red fill — the abandoned cut
- **Annotation arrows**: Thin lines with right-angle bends, like dimension callouts on an industrial drawing

### Sketch-style illustrations (aspirational)

For conceptual illustrations (the three-object model, signature flow, etc.):

- Loose but confident linework — as if drawn with a felt-tip pen on tracing paper
- Line weight: 1–2px in `--graphite`, key features highlighted in `--copper`
- Callout annotations in monospace, connected by leader lines
- Hatching for shading (not gradients) — cross-hatch in `--pencil`
- No drop shadows

### Topographic / contour motifs

For decorative or background elements:

- Concentric contour lines in `--pencil` at low opacity
- Suggest depth and layering — the strata of a provenance chain
- Can be used as subtle page backgrounds or section illustrations
- Evokes CNC-milled wood topographic maps: precision carving revealing layers beneath

---

## Tone of voice

### Writing style

- **Direct and precise.** No hedging, no filler.
- **Educational but not condescending.** Assume the reader is smart but unfamiliar.
- **Conversational authority.** Like a well-written textbook, not a blog post.
- **Concrete before abstract.** Show the example, then explain the principle.
- **Material metaphors welcome.** "Carve," "layer," "trace," "mill" — the vocabulary of making things.

### Examples

Good: "A step records a single change to one or more artifacts by one actor."

Bad: "Steps are our way of representing the concept of changes in the system."

Good: "When Claude writes code, rustfmt reformats it, and a human refines it, git blame attributes everything to the human's commit."

Bad: "In modern software development workflows, attribution can sometimes be challenging."

---

## Logo / wordmark

The Toolpath wordmark is "TOOLPATH" set in IBM Plex Sans Condensed Bold, colored `--copper`, tracked at 0.12em.

- Always uppercase
- Always in the display font (IBM Plex Sans Condensed), bold weight
- Always in `--copper` on light backgrounds, `--ground` reversed on dark
- Wide letter-spacing (0.12em) — the tracking conveys mechanical precision
- Preferred lockup: wordmark left, tagline right in serif italic

```
TOOLPATH                    Know your tools.
```

The condensed sans reads as a technical manual masthead. The wide tracking transforms a workhorse typeface into a mark — each letter spaced like coordinates on a grid.

---

## Don'ts

- Don't introduce cold colors. The palette is warm: graphite, copper, parchment.
- Don't round corners. Sharp edges — milled, not molded.
- Don't use drop shadows. Depth comes from layering and contour, not elevation.
- Don't use stock photography. All visuals are diagrams, sketches, or contour illustrations.
- Don't use the display font for body text. It's headings and UI only.
- Don't use color to distinguish content hierarchy. Use type size, weight, and font register instead.
- Don't center body text. Left-align or justify.
- Don't use gradients. Hatching, contour lines, and opacity steps create depth instead.

---

## Summary

| Attribute     | Choice                                                        |
| ------------- | ------------------------------------------------------------- |
| Palette       | Warm monotone: copper accent, graphite text, parchment ground |
| Display font  | IBM Plex Sans Condensed (technical manual masthead)           |
| Body font     | Source Serif 4 (scholarly journal text)                       |
| Label font    | IBM Plex Mono (keypunch / teletype)                           |
| Layout        | Wide, two-column affinity, generous spacing                   |
| Illustrations | Topographic contour maps, sketch-style callouts, hatching     |
| Dividers      | Ruled lines or pixel-grid pattern                             |
| Corners       | Sharp (no border-radius) — milled edges                       |
| Shadows       | None — depth via contour and layering                         |
| Texture       | Warm paper grain, not flat white                              |
| Tone          | Direct, precise, educational, material                        |
