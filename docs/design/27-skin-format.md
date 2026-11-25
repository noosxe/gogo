# 27 — Skin format (#520)

Status: **accepted** (2026-11-25). The loader and structural validator are built
(`gogo-skin`, design 24 §8 stack B, PR B3b); resolving names against the contract and
binding are not yet. This is the reference for skin authors.

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

- **What each file holds.** `skin.kdl` one `skin` node; `tokens.kdl` one `tokens "<mode>"`
  block per mode; `styles.kdl` `style` nodes; each file in `components/` `component` nodes;
  `routes/<name>.kdl` exactly one `route "<name>" { … }` node, named like its file.
- **Assets** may sit in sub-folders of `assets/` (`assets/fonts/…`). A reference is the
  path from the skin root: `assets/logo.png`.
- **Anything else is an error** (S10): a file of another type (only KDL, PNG, SVG, OTF and
  TTF are allowed), a file outside the layout above, a symbolic link, a path with `..`, an
  absolute path, a backslash or a `:`, a path longer than 200 bytes, a file name that isn't
  UTF-8, and two paths that differ only in case.
- **Assets are checked by what they are, not by their name:**
  - a **PNG** has the PNG signature and a chunk list that starts with `IHDR` and ends
    exactly at `IEND`;
  - an **SVG** is static and refers to nothing outside itself (S10, S11): no `<script>`,
    no event attributes (`onclick`, …), no animation elements, no `<foreignObject>` or
    other embedding, no DOCTYPE or entity declaration. Every `href` and every `url(…)` points
    at a `#fragment` of the same file, and `@import` is refused;
  - an **OTF or TTF** has a TrueType or OpenType header with a table directory that fits.

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
| `skin "<name>"` | The folder name and the id. Lowercase, `a-z0-9-`, at most 64 characters. A folder loads only under the name its manifest gives |
| `title` | Shown in the skin picker |
| `version` | The skin's own semver (`MAJOR.MINOR.PATCH`, optional `-pre` and `+build`) |
| `contract` | The contract version it needs, `MAJOR.MINOR` (design 26 §8). It loads when the major is the app's and the minor is not newer (S16) |
| `modes` | Token sets it provides: `dark`, `light`, each at most once, at least one |
| `routes` | The routes it covers, each a route of design 26 §6.2 and each with its `routes/<name>.kdl`. The rest fall back per route. A route file that isn't listed isn't loaded, and the loader warns |
| `extends "<skin>"` | Optional. The skin it builds on: routes and components it doesn't define come from that skin before falling back to the default. Parsed by the prototype; the lookup arrives with binding (PR B4) |
| `ui { var … }` | Skin-local state (design 26 §6.4). Types: `bool`, `number`, `enum(a b …)`, `section` (the route's sections). Each `var` has a unique name, `type=` and a `default=` that fits the type: `#true`/`#false`, a number, one of the enum's words, or one of the contract's section names |

`title`, `version`, `contract` and `modes` are required, each is written once, and any
other field is an error.

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
- **Colours** are `#rrggbb` or `#rrggbbaa` (the short `#rgb` forms aren't accepted). In a
  style or an element's property the word `transparent` is a colour too.
- **Lengths** are logical pixels. HiDPI scaling is the app's. Shape and motion tokens
  (`radius`, `space`, `elevation`, `stroke`) are numbers; `motion.*` are milliseconds.
- **A token is** a colour, a number, a font (`family=`, `size=`, `weight=`, and for a
  variable family an optional `stretch=`) or an `@ref`. References are followed at load, so
  a reference to another token, or to a field of a type role (`@font.body.size`), reads
  that token's value. A reference to nothing, or a loop of references, is an error.
  `easing.<name>` tokens hold a curve name as text.
- **Every mode defines the same tokens**, with the same types. The tokens of design 26 §6.1
  are required in every mode, except `tone.queued` and `tone.paused`, which fall back to
  `text-lo` and `warn`. `font.<role>.size` and its siblings are fields of a role, not tokens
  to define.
- **Font families are named as the font files name themselves** (`Noto Sans`,
  `JetBrains Mono`), never by a fontconfig alias. An unknown family is a load error.
- **The bundled families** are Noto Sans (variable) and JetBrains Mono (static). A skin
  may ship OTF/TTF files in `assets/`, and the loader checks them as files. In this
  version a `family` must still name a bundled family: reading a family name out of a
  skin's own font needs a font parser, which the loader doesn't have, so a skin's own
  fonts can't be named yet.
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
  properties set on the element itself win. A style that is used and not defined is a load
  error.
- **Names.** A style name is lowercase letters, digits, `-`, `_` and `.`, and is defined
  once.
- **States:** `:hover`, `:pressed`, `:focus`, `:on` (toggles, current tab, highlighted
  row) and `:disabled`. A state holds properties, not states, and is set once per style.
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

**Values.** Each property takes the values below, and a value of another form or outside
its range is a load error that names the property.
- **Lengths** (`w h min-w … size`, `inset`) are a number, `auto` or a percentage (`50%`).
- **`padding`, `margin`** take 1 value (all sides), 2 (vertical, horizontal) or 4 (top,
  right, bottom, left); `radius` takes 1 or 4; `gap` takes 1 or 2 (row, column);
  `inset` takes 1, 2 or 4.
- **`columns` and `rows`** take a count (`columns 3`: three equal columns) or tracks: a
  number (pixels), `auto` or `<n>fr`. At most 64.
- **Ranges.** `opacity` 0–1; `weight` 1–1000; `stretch` 25–200 (100 is normal); `font-size`
  1–1024; `lines` and `col-span` are whole numbers of 1 or more.
- **`font @role`** applies a type role token (family, size and weight); `family` takes a
  bundled family name; `mono-font` the same.
- **Colours** are a literal, `transparent`, a colour token, or in a style one
  `"{@token alpha=.5}"`. A style is static: no other `{…}` expression is allowed in one.
  An element's inline properties may be expressions.
- **`stretch` on a static family** is an error when the same block sets both. An inherited
  `stretch` is not checked (§5.2).

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
| `icon "<name>"` | A vector icon shipped with the engine: `play`, `square`, `circle`, `chevron-down`, `dot`. An unknown name is a load error |
| `art "{g.art.cover}"` | Game art by handle, with `fit="cover"` (default), `"contain"` or `"fill"` (stretch to the box); `label=` names it for screen readers (S14) |
| `image src="assets/x.png"` | A PNG or SVG from the skin folder, by the `src` property: plain text, a path from the skin root that names an asset the skin has. A URL (`https:`, `data:`, any scheme), a path with `..`, or a file that isn't an image is a load error (S10, S11). Needs `label=` (S14) |
| `label "<template>"` | Text, like `text` |
| `chip`, `badge` | Small labelled marks, with `tone=` (a status tone or semantic colour) and `icon=`; read as a label |
| `display "<template>"` | Like `chip`, but read as a live status (an LCD-style readout) |
| `dot`, `led` | An 8 px circle; `on="{expr}"` puts it in its `:on` state, and `tone=` colours it like a `chip`'s. A `led` with `label=` is read as an on/off indicator |
| `card`, `module` | A titled panel (`title=`, `link=`, `link-action=`); `module` is the same element under a name that suits instrument-style skins |

- **Text properties are inherited.**
- **Arguments.** `text`, `label`, `icon`, `art`, `display`, `menu`, `item` and `field` take
  one argument; `chip`, `badge` and `button` at most one; `col` one or two; the other
  elements none. An argument is text (a template).
- **Properties.** An element takes the properties its row names, `id`, `style`, `when`,
  `label`, and the style properties of §4 inline. Any other property is a load error that
  names the closest known one.
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

- `action` must name an action from design 26 §4 that a skin may bind. `args` is split at
  whitespace outside `{…}` into terms; a term `name=value` names a parameter; each term is
  a template, parsed at load. That `args` type-check against the action is checked with the
  contract (PR B3c).
- **Inputs.** `value` belongs to `select` and `on` to `toggle` (and `dot`, `led`). Only
  `toggle` and `select` may use `value` and `on` in `args`; any other element that does is a
  load error.
- **Required.** `toggle` needs `on`, `select` needs `value` and `options`, `tabs` needs
  `bind` and `sections`, `image` needs `src`, `data-table` needs `rows`.
- Every element with an action needs an accessible name (S14), and so does every `art` and
  `image`. The name is the element's own text, a non-empty `label=` or `title=`, or an
  enclosing `field "<label>"`. A `card` or `module` with `link-action` needs `link=` text.
  `tabs` is named by its sections.
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
- It holds only `col` children, at least one, and `use "data-table"` is an error.
- A `col` has a title and either a cell template or a body, not both; `w=` and `mono=` are
  its properties.

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
  Skins reach them only through tokens, and writing one as an element is a load error.
- **Where parts go.** `part` goes only inside a depth-B widget, and a depth-B widget holds
  only `part`s. The loader keeps part names and variant values as written; the contract
  check above is PR B3c's.

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
  are bound like `let` values. A component the skin doesn't define, or a parameter it
  doesn't declare, is a load error; `when=` and `id=` work on any `use`. A parameter name
  is lowercase letters, digits and `_`, and `when` and `id` are reserved.
- **No loops.** Components that use each other, or themselves, are a load error (S20). A
  component or route whose uses, once expanded, pass the node or depth limits of §8 is too.
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
| `id="…"` | A stable id for tests, the inspector and focus restore: letters, digits and `_-.:` |

- **`each`** takes one variable name and `in="{expr}"`, and only an expression: a literal or
  a missing `in` is a load error (S20). It is the only loop in the format.
- **`case`** takes one plain string, written as a KDL string (`case "overview"`). It is
  compared with the `switch` value as text, so `case "1"` matches the number 1. A number or
  a template isn't a `case` value. `else` takes none and comes last, once. A `case` equal
  to an earlier one can never match, and the loader warns.
- **`let`** takes `name="{…}"` properties, each a lowercase name, bound once.

## 6. Effects

A fixed list that every renderer draws the same way (design 28). Skins can't supply
shaders.

| Effect | Syntax (example) | Units and rules |
|---|---|---|
| `shadow` | `shadow 0 6 18 "#00000066" [spread]` (x, y, blur, colour, optional spread in px; repeatable) | CSS blur radius r: a Gaussian with σ = r / 2. Blur is 0 or more |
| `inner-shadow` | `inner-shadow 0 1 4 "#00000088"` (x, y, blur, colour) | as `shadow`, drawn inside the box |
| `glow` | `glow 6 "{@led alpha=.8}" 18 "{@led alpha=.35}"` (radius and colour pairs, 1 to 4) | each layer σ = r / 2 |
| `bloom-text` | `bloom-text 8 "{@accent alpha=.6}"` | Lights **text only**, for the text runs under the node that sets it; inherited like a text property. σ = r / 2 |
| `gradient` | `gradient linear 180 @bg1 @bg0` or `gradient radial @a @b` | a fill; 2 to 8 colour stops evenly spaced, in order |
| `noise` | `noise 0.06 [seed]` | a tiled grain at that opacity (0 to 1); the optional whole-number seed (default 7) picks the pattern |
| `scanlines` | `scanlines 3 0.18` (period in px, alpha) | horizontal black lines at that alpha (0 to 1), one every `period` px (1 or more), clipped to the box |
| `reflection` | `reflection 120 0.14` (angle in degrees, alpha) | a glass sheen over the box: a white linear gradient at that angle, from `alpha` (0 to 1) fading to nothing before the middle. Not a mirror; a mirrored copy may be added later as a separate effect |
| `blur-behind` | `blur-behind 12` | CSS `backdrop-filter: blur(r)`: σ = r. Use sparingly: it is the most expensive effect |

- **Argument forms** are the ones in the table, checked at load: the count, the numbers and
  their ranges, and the colours (a literal, `transparent`, a colour token, or one
  `"{@token alpha=…}"`). Anything else is a load error that names the effect. A KDL number
  can't start with `.`, so write `0.06`, not `.06`; inside a `"{…}"` string `alpha=.55` is
  fine.
- **Clearing.** `effects none` clears every inherited box effect, `shadow none` only
  shadows, `bloom-text none` the bloom. Every effect but `shadow` is set once per block; a
  second one wins and the loader warns.
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
  tree is kept; evaluating never parses text. That includes action arguments. A syntax
  error in one is reported at its place inside the string.

### Grammar

Loosest binding first. Operators on one row bind equally; "left" and "right" give the
association.

| Level | Form | Association |
|---|---|---|
| 1 | `c ? a : b` | right: `a ? b : c ? d : e` is `a ? b : (c ? d : e)` |
| 2 | `a else b` | left |
| 3 | `x \| stage …` | left; a stage may be followed by `.field` |
| 4 | `a or b` | left |
| 5 | `a and b` | left |
| 6 | `not a` | prefix |
| 7 | `==` `!=` `<` `<=` `>` `>=` | none: `a < b < c` is a load error, write `(a < b) and (b < c)` |
| 8 | `-a` | prefix |
| 9 | `.field`, `?.field`, `name.path(args)` | left |
| 10 | literals, names, `$param`, `@token`, `( … )` | |

- A pipe is looser than `or`, so `a or b | len` is `(a or b) | len`. `not` is looser
  than a comparison, so `not a == b` is `not (a == b)`.
- Special syntax: `sort key.path` and `sort -key.path` (the key is a field path, not an
  expression), and `@token alpha=x`, where `x` is a number expression.
- `true`, `false` and `null` are literals. Numbers, `'…'` and `"…"` strings are literals
  too; a string has no escapes.

### Names

A bare name resolves in this order:

1. a **variable**, when one is bound by that name: a `let`, a parameter, an input
   variable (`value`, `on`), or, inside a `where` predicate, a field of the item;
2. otherwise it must be an **enum word of the contract**: a status (design 26 §2), an OS,
   or a word of the fixed set of a field in design 26 §3 (`ready`, `linux`, …). It is the
   text of that word, so `g.status == ready` compares the status with `ready`;
3. anything else is a load error, "unknown name", pointing at the name.

The loader checks every name of every expression (§9; `Expr::names` lists them) against
the variables in scope at that point. A `where` predicate also accepts the fields of the
list's items.

### Failures

`eval` returns a result. A failure at run time fails the binding of the route that
evaluated the expression, and the route falls back (design 26 §8). Once the loader's
checks pass, these can still fail, because they depend on the data:

- a field of a null, without `?.` (`g.update.to` when `update` is null);
- a field that a record does not have, or a field of a value that has none;
- a call that returns an error, or a source the app cannot answer;
- a type mismatch the load-time checks don't cover: `-x` or `fmt.bytes(x)` on text,
  `take n` with a non-number, `<` between a number and text, an `alpha=` that is not a
  number, a status helper on a word that is not a status;
- a list stage (`sort`, `where`, `take`, …) on a value that is not a list or null.

Every evaluation ends: there is no assignment, no user function and no I/O, and the only
loops are pipe stages over a list the app hands in.

## 8. Limits

**Static limits** are checked at load. A skin that exceeds one fails to load, and the
default skin is used (S12).

| Limit | Value |
|---|---|
| One KDL file | 256 KiB |
| Nodes per skin | 20 000 |
| KDL node nesting depth | 48 |
| Files in the package | 1 024 |
| Expression length | 512 characters |
| Expression depth | 32 |
| One PNG / SVG / font file | 4 MiB / 512 KiB / 8 MiB |
| Whole skin folder | 32 MiB |

- **Nodes** count over every KDL file of the skin. **KDL node nesting depth** counts the
  levels of nested nodes, a file's top-level node being level 1.
- **Expansion.** A `use` stands for its component's nodes and levels, so a component or a
  route whose expanded tree passes the node or depth limit fails the skin.
- **Paths** are at most 200 bytes. A file over its size limit is refused unread.

The asset sizes are the ones the loader checks. A PNG up to 4 MiB holds a full-screen
background, an SVG up to 512 KiB a detailed illustration, and a font up to 8 MiB a full CJK
family.

**Expression depth** counts the levels of one expression's tree, the root as level 1:
every operator, field access, call, pipe stage, `else` and `?:` adds a level over its
operands, so `a | len | len` and `a.b.c` are three levels deep, and a parenthesis counts
too. It is a different limit from the node nesting depth, which counts how deep KDL nodes
sit inside one another in a file. An expression deeper than 32 levels is a load error, so
the parser, the evaluator and everything that walks the tree stay within a known stack
use.

**The row cap is data-dependent, so it truncates instead.** One `each` or `data-table`
draws at most **1 000** items. A longer list is cut at 1 000, silently, at bind time: data
can't make a valid skin fail to load. Virtualised lists, which draw only what is visible
and lift the cap, are planned with scrolling (#545).

## 9. Loading and validation

1. **Read the package.** Admit each file by its path, type and size (§1, §8; S10, S11,
   S12), check each asset's content, and parse the KDL files as KDL v2. A folder is read
   with `SkinPackage::load`, the files of a zip with `SkinPackage::from_files`; both run
   the same checks.
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

The loader (PR B3b) does steps 1 and 3 and the structural part of step 2: names, shapes,
types, ranges, token references, limits and the accessible-name rule. Resolving a name to
the contract (an action's parameters, a route's sections, a widget's parts and variants),
the field and type checks of expressions, and S7 come with PR B3c, and binding with PR B4.

Every finding is a miette diagnostic with a stable code (`gogo_skin::<rule>`), the file,
line and span, and a help line:
- **skin errors** fail the skin, and the default skin is used (S12, S16): everything in
  `skin.kdl`, `tokens.kdl`, `styles.kdl`, a component or an asset, and every limit;
- **route errors** fail one route, which falls back: everything in `routes/<name>.kdl`;
- **warnings** don't fail anything: a status shown by colour alone (S18), a property set
  twice in a block, a `case` that can never match, and a route file the manifest doesn't
  list.

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
