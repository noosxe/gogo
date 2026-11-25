# 27 — Skin format (#520)

Status: **accepted** (2026-11-25), not built yet (`gogo-skin`, design 24 §8 stack B).
This is the reference for skin authors.

A skin is a folder of [KDL](https://kdl.dev) v2 files: a declarative tree, styles and
tokens. It is written against the contract (design 26): it binds sources, places app
widgets, and calls actions by name. **It contains no code.** The grammar has nothing to
call except bound actions on input, and nothing to loop over except bounded `each`
(S4, S20).

**Why KDL, and our own tree.**
- KDL reads like a UI tree, has comments, and has a maintained Rust parser (`kdl` 6.7) with
  miette diagnostics.
- An own tree describes *our* model, not a toolkit's. Skins survive a renderer change, and
  every name can be checked against the contract ("unknown field `g.titel`, game.kdl:12").
- XML in the Kodi tradition was the fallback: same engine, wordier.
- A Slint subset was ruled out with Slint itself (design 24 §1).

## 1. Package layout

```
<name>/
  skin.kdl          manifest (§2)
  tokens.kdl        token sets per mode (§3)
  styles.kdl        named styles and state variants (§4)
  components/*.kdl  components (stock ones copied or rebuilt, or the skin's own)
  routes/*.kdl      one file per covered route: game.kdl, library.kdl, …
  assets/           png, svg, otf/ttf
```

Only these files are read. Asset references are relative to the skin root (S10).

## 2. Manifest

```kdl
skin "default" {
    title "Default"
    version "1.0.0"
    contract "1.0"
    modes "dark" "light"
    routes "shell" "library" "game" "queue" "settings"
    ui {
        var "tab" type="section" default="overview"
        var "view" type="enum(grid list)" default="grid"
    }
}
```

| Field | Meaning |
|---|---|
| `skin "<name>"` | The folder name and the id. Lowercase, `a-z0-9-` |
| `title` | Shown in the skin picker |
| `version` | The skin's own semver |
| `contract` | The contract version it needs (design 26 §8) |
| `modes` | Token sets it provides: `dark`, `light` |
| `routes` | The routes it covers. The rest fall back per route |
| `extends "<skin>"` | Optional. The skin it builds on: routes and components it doesn't define come from that skin before falling back to the default. Parsed by the prototype; the lookup arrives with binding (PR B4) |
| `ui { var … }` | Skin-local state (design 26 §6.4). Types: `bool`, `number`, `enum(a b …)`, `section` (the route's sections) |

## 3. Tokens

```kdl
tokens "dark" {
    bg0 "#121314"
    surface "#1f2023"
    accent "#7aa2ff"
    radius.md 8
    space.4 16
    font.body family="Noto Sans" size=14 weight=400
    font.mono family="JetBrains Mono" size=13 weight=400
    led-green "#3cff7a"          // a skin's own token: app widgets ignore it
}
```

- **Names.** The semantic names are in design 26 §6.1. Anything else is the skin's own.
- **References.** A token is referenced as `@name` (`@accent`, `@space.4`). Inside a string
  it is written `"{@warn alpha=.55}"`, which also adjusts alpha.
- **Colours** are `#rrggbb` or `#rrggbbaa`.
- **Lengths** are logical pixels. HiDPI scaling is the app's.
- **Font families are named as the font files name themselves** (`Noto Sans`,
  `JetBrains Mono`), never by a fontconfig alias. An unknown family is a load error.
- **The bundled families** are Noto Sans (variable) and JetBrains Mono. A skin may ship
  more OTF/TTF files in `assets/`.
- **A change from the prototype:** its skins used `JetBrainsMono Nerd Font`, a patched
  build with extra icon glyphs. The port bundles plain JetBrains Mono instead (decision 10
  in design 24), and the built-in skins' family names change with it.

## 4. Styles and states

```kdl
style "button" {
    h 40; radius 8; padding 0 16; gap 10; weight 600
    border 1 @outline; color @text-hi; fill @surface
    :hover   { fill @raised }
    :pressed { offset 0 1 }
    :focus   { ring 2 @focus }
}
style "primary" { fill @accent; color @on-accent; border 1 @accent }
```

- **Applying styles.** `style="button primary"` applies several styles left to right, and
  properties set on the element itself win.
- **States:** `:hover`, `:pressed`, `:focus`, `:on` (toggles, current tab, highlighted
  row) and `:disabled`.
- **Element defaults.** A style named after an element applies to every element of that
  kind (`style "button"`).

Properties:

| Group | Properties |
|---|---|
| Size | `w h min-w max-w min-h max-h size aspect` |
| Flex and grid | `grow shrink gap row-gap direction wrap align align-self justify columns rows col-span` |
| Box | `padding margin inset` (`inset` takes the node out of the flow: see §5.1) |
| Paint | `fill`, `gradient linear <angle> <colours…>` / `gradient radial <colours…>`, `border <w> <colour>`, `border-top/right/bottom/left`, `accent-edge <side> <w> <colour>`, `radius` (1 or 4 numbers), `opacity`, `shape circle\|rect`, `offset <x> <y>`, `ring <w> <colour>` |
| Effects | `shadow inner-shadow glow bloom-text noise scanlines reflection blur-behind` (§6); `effects none` clears every inherited box effect, `shadow none` only shadows, `bloom-text none` the bloom |
| Text | `font family font-size weight stretch tracking line-height color text-align upper text-wrap lines` |
| Hints | `key-hint #true` (show a button's `key=` as a hint chip), `mono-font "<family>"` (the family used for `mono=#true` text below) |

Layout follows CSS flexbox and grid as implemented by taffy:
- `align` takes `start center end stretch baseline`;
- `justify` takes `start center end between around`;
- **borders take layout space** (CSS `border-box`);
- **position:** every node is in the flow (taffy's static position) unless it sets
  `inset`, which makes it absolutely positioned against its parent. Insets on an in-flow
  node would be ignored, so the loader never produces one.

## 5. Elements

Only the elements below exist. Anything else is a load error.

### 5.1 Layout

| Element | Meaning |
|---|---|
| `column`, `box`, `form` | vertical flex container (`box` is a plain container) |
| `row` | horizontal flex container, items centred |
| `grid` | CSS grid: `columns="1fr 420"` or `columns=3` |
| `layer` | children stacked on top of each other: a one-cell grid (`1fr × 1fr`), every child in the same cell; `inset` children are placed absolutely |
| `scroll` | a clipping, scrolling container. **Planned:** not in the prototype and not validated yet; it arrives with scrolling (#545) |
| `outlet` | Only in the `shell` route: where the current route is drawn |
| `spacer`, `divider` | flexible space; a rule |

### 5.2 Content

| Element | Meaning |
|---|---|
| `text "<template>"` | One line, unless `text-wrap #true` (wraps at word boundaries) or `lines N` with N > 1 (wraps, at most N lines) |
| `icon "<name>"` | A vector icon shipped with the engine (`play`, `square`, `circle`, `chevron-down`, …). An unknown name is a load error |
| `art "{g.art.cover}"` | Game art by handle, with `fit="cover"` (default), `"contain"` or `"fill"` (stretch to the box); `label=` names it for screen readers |
| `image src="assets/x.png"` | An image from the skin folder, by the `src` property |
| `label "<template>"` | Text, like `text` |
| `chip`, `badge` | Small labelled marks, with `tone=` (a status tone or semantic colour) and `icon=`; read as a label |
| `display "<template>"` | Like `chip`, but read as a live status (an LCD-style readout) |
| `dot`, `led` | An 8 px circle; `on="{expr}"` puts it in its `:on` state. A `led` with `label=` is read as an on/off indicator |
| `card`, `module` | A titled panel (`title=`, `link=`, `link-action=`); `module` is the same element under a name that suits instrument-style skins |

- **Text properties are inherited.**
- **`stretch` (the `wdth` axis) applies only to variable fonts.** On a static face it is a
  load error. An inherited `stretch` stops at a child that switches to a static family.

### 5.3 Controls and actions

| Element | Input | Binding |
|---|---|---|
| `button "<label>"` | activate | `action="game.play" args="{$id}"`, plus `icon=`, `key="P"` (a key hint and shortcut) |
| `toggle` | on/off | `on="{expr}"`, `action=…`; `args` may use `on` (the state before the input) |
| `select` | choice | `value="{expr}" options="{list}"`, `action=…`; `args` may use `value` |
| `tabs` | section | `bind="ui.tab" sections="overview builds …"` |
| `menu "<label>" { item … }` | activate | each `item` has its own `action`/`args` |
| `field "<label>" { … }` | — | labels a control (also its accessible name) |

- `action` must name an action from design 26 §4, and `args` must type-check against it.
- Every element with an action needs an accessible name: its text, or `label=` (S14).
- `when=` and `ui.set` are the only ways a skin reacts. Nothing fires on load, hover or a
  timer (S4).

### 5.4 `data-table`

A built-in element: the skin owns the columns and cells, and the engine owns the table
structure, row virtualisation and keyboard handling.

```kdl
data-table label="Builds" rows="{game.builds($id) | take 4}" highlight="{row.this_machine}" {
    col "Build" "{row.version}" mono=#true w=100      // title, cell template
    col "" { badge "Newest" tone="warn" when="{row.newest}" }   // or a cell body
}
```

- Each row is bound to `row`.
- `highlight=` puts a row in its `:on` state.
- Columns without `w=` share the remaining width.
- It holds only `col` children, and `use "data-table"` is an error.

### 5.5 App widgets

App widgets (design 26 §5) are placed by name. Depth-B widgets take parts and variants:

```kdl
plan-card for="game.update" args="{$id}" placement="inline" labels="verb" layout="stacked" markers="number" {
    part "frame" style="plan-frame"
    part "badge" hidden=#true          // only optional parts can be hidden
}
env-table game="{$id}" row-actions="menu" title="Environment"
```

- **Checked against the contract.** Part names and variant values are checked, and an
  unknown one is a load error.
- **Variant names belong to their widget.** `layout` on `plan-card` (`stacked`, `side`)
  and `layout` on `picker` (`list`, `grid`, `keys`) are two different variants, each
  checked against its own widget's values.
- **What can't be hidden.** Hiding a non-optional part, the plan card's `footnote` or
  `confirm` included, is a load error (S5).
- **Depth-A widgets** (`confirm-destructive`, `secret-input`, `focus-ring`) take no parts.
  Skins reach them only through tokens.

### 5.6 Progress

`box style="bar-fill" progress="{job.fraction}"` sets the fill's width to that share of
its track.
- The value must be between 0 and 1.
- The node is marked, so live progress can find it without a full re-bind (design 25 §3).

### 5.7 Components

```kdl
component "summary-tile" params="mark tone title sub link action args" {
    row style="tile" { text "{title}" style="tile-title"; text "{sub}" when="{sub}" }
}
```

- **Using one.** `use "summary-tile" title="…"` instantiates a component, and parameters
  are bound like `let` values.
- **Borrowing from the default skin.** `use default "game.builds"` embeds a named fragment
  of the default skin (design 26 §8). The prototype only draws a labelled placeholder
  there; real borrowing arrives with binding (PR B4).
- **Stock components** (design 26 §5, depth C) ship with the default skin, and a skin may
  copy or replace them.

### 5.8 Binding and control flow

| Form | Meaning |
|---|---|
| `let g="{game($id)}"` | Names a source or value for the rest of the block |
| `when="{expr}"` | Draws the element only when the expression is true |
| `each "s" in="{list}" { … }` | One copy per item, at most the repeat limit (§8) |
| `switch "{expr}" { case "a" { … } else { … } }` | Picks the first `case` whose string equals the expression's value as text (exact, case-sensitive), else the `else` branch, else nothing |
| `id="…"` | A stable id for tests, the inspector and focus restore |

## 6. Effects

A fixed list that every renderer draws the same way (design 28). Skins can't supply
shaders.

| Effect | Syntax (example) | Units and rules |
|---|---|---|
| `shadow` | `shadow 0 6 18 "#00000066" [spread]` (x, y, blur, colour, optional spread in px; repeatable) | CSS blur radius r: a Gaussian with σ = r / 2 |
| `inner-shadow` | `inner-shadow 0 1 4 "#0008"` (x, y, blur, colour) | as `shadow`, drawn inside the box |
| `glow` | `glow 6 "{@led alpha=.8}" 18 "{@led alpha=.35}"` (radius and colour pairs) | each layer σ = r / 2 |
| `bloom-text` | `bloom-text 8 "{@accent alpha=.6}"` | Lights **text only**, for the text runs under the node that sets it; inherited like a text property. σ = r / 2 |
| `gradient` | `gradient linear 180 @bg1 @bg0` or `gradient radial @a @b` | a fill; stops evenly spaced, in order |
| `noise` | `noise .06 [seed]` | a tiled grain at that opacity; the optional integer seed (default 7) picks the pattern |
| `scanlines` | `scanlines 3 .18` (period in px, alpha) | horizontal black lines at that alpha, one every `period` px, clipped to the box |
| `reflection` | `reflection 120 .14` (angle in degrees, alpha) | a glass sheen over the box: a white linear gradient at that angle, from `alpha` fading to nothing before the middle. Not a mirror; a mirrored copy may be added later as a separate effect |
| `blur-behind` | `blur-behind 12` | CSS `backdrop-filter: blur(r)`: σ = r. Use sparingly: it is the most expensive effect |

- **Argument forms are provisional.** The units and rules above are fixed. The exact
  argument forms in the syntax column are settled when the loader lands (#525), and this
  table is updated then.
- **Reduced motion.** Under reduced motion, transforms and loops are removed and fades are
  capped at 150 ms (S15).
- **Motion declarations** (`motion "led" { on-change duration=120 easing="ease-out" }`):
  **planned**. The prototype doesn't handle them. When they land, they take only values
  from the motion tokens' ranges.

## 7. Expressions

A string with `{…}` inside is interpolated. A property value that is a single `{…}` is an
expression.

| Form | Example | Notes |
|---|---|---|
| field path | `g.title`, `run.galaxy.state` | names come from `let`, parameters and sources |
| optional | `g.update?.to else "—"` | `else` gives a default when the value is null |
| compare, logic | `g.status == ready and not g.running` | enum values are bare words |
| ternary | `s.kind == auto ? "auto" : "manual"` | |
| list pipes | `game.builds($id) \| sort -date \| take 4` | also `where`, `len`, `first`, `upper`, `lower`, `trim n` (at most n characters, an ellipsis last when cut), `before x`, `after x` |
| formatters | `fmt.bytes(x)`, `fmt.ago(t)` | the fixed list in design 26 §2 |
| status helpers | `status.word(s)`, `status.tone(s)` | |
| tokens | `@accent` | |
| route parameters | `$id` | |

- **`before x` / `after x`** give the item before or after `x` in the list, wrapping
  around at the ends (cycling through tabs or modes). If `x` isn't in the list they give
  the first item, and on an empty list null.
- **Braces in text.** Every `{` in a template opens an expression, up to the first `}`
  outside a quoted string. `{{` and `}}` stand for a literal `{` and `}`: `"{{x}}"` is the
  text `{x}`. A lone `}` outside an expression, or a `{` that is never closed, is a load
  error that points at the brace.
- **Parsed once.** Every template and expression is parsed at load (§9 step 3) and the
  tree is kept; evaluating never parses text. That includes action arguments.

Expressions are **total**:
- every expression ends;
- there is no assignment, no user function and no I/O;
- a type error is a load error, and a null is handled with `?.`/`else`.

## 8. Limits

**Static limits** are checked at load. A skin that exceeds one fails to load, and the
default skin is used (S12).

| Limit | Value |
|---|---|
| One KDL file | 256 KiB |
| Nodes per skin | 20 000 |
| Nesting depth | 48 |
| Expression length | 512 characters |
| Expression nesting depth | 32 |
| One PNG / SVG / font file | 4 MiB / 512 KiB / 8 MiB |
| Whole skin folder | 32 MiB |

The asset sizes are this design's proposal. The prototype loaded no skin assets; PR B3
adds the checks and confirms these numbers here.

**The row cap is data-dependent, so it truncates instead.** One `each` or `data-table`
draws at most **1 000** items. A longer list is cut at 1 000, silently, at bind time: data
can't make a valid skin fail to load. Virtualised lists, which draw only what is visible
and lift the cap, are planned with scrolling (#545).

## 9. Loading and validation

1. **Read the package.** Read the files (S10, S11) and parse them as KDL v2.
2. **Walk the tree into our AST and validate it:**
   - element and property names;
   - styles and tokens;
   - contract names (sources, fields, actions, routes, sections, parts, variants) and the
     contract version (S16);
   - accessible names (S14);
   - app-widget contrast and sizes (S7);
   - limits (§8).
3. **Parse templates and expressions once, here, and keep them in the cached skin.**
   Binding a route on a data change only evaluates (design 24 §6).
4. **Bind a route** against live data and the skin's UI state to produce the IR (design 24
   §2). A route whose binding fails falls back (design 26 §8). The loader reports why, with
   file and line.

Every finding is a miette diagnostic with the file, line and span:
- **errors** fail the route or the skin;
- **warnings** (such as S18) don't.

`gogo-gui --check-skin <dir>` prints them without opening a window.

## 10. Installing

- **Built-in skins** are embedded in the app and load through the same loader (S17).
- **Third-party skins** live in the skin folder (design 25 §9).
- **Installing** means importing a folder or a zip:
  - the app validates it;
  - rejects path escapes, symlinks and oversized files;
  - copies it into the skin folder.
- There is no online gallery and no auto-update.
- **Dev mode** (design 25 §8) watches a folder and re-loads it on every save.
