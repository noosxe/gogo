# 28 — GUI rendering: the scene and the renderers (#520)

Status: **accepted** (2026-11-25), not built yet (design 24 §8 stack C). Facts marked
*measured* come from the prototype described in design 24.

The engine turns the IR (design 24 §2) into a **display list**. Each renderer draws that
same list. `gogo-ui-scene` owns the list and the effect lowering; `gogo-render-cpu`
(vello_cpu) and `gogo-render-gpu` (vello_gpu on wgpu) implement the `Renderer` trait.
Renderers never see the IR, a skin or the contract (design 24 §2.1).

## 1. The display list

- **Coordinates.** The list is built in logical pixels with kurbo geometry and peniko
  colours, brushes and gradients. The renderer applies the scale factor, so one list serves
  @1× and @2×.
- **Images are referenced, not embedded.** An `ImageId` points into an image store that
  the app fills (design 25 §7). Each renderer turns ids into its own form: a pixmap on the
  CPU, a texture on the GPU.

These are the final op names (settled in PR C3; `gogo-ui-scene`). `Shape` is a rect, a
rounded rect, an ellipse or a path with a fill rule; `Brush` is a solid colour, a gradient
or an image brush.

| Op | Meaning |
|---|---|
| `Fill { shape, brush }` | A shape with a solid colour, gradient or image brush. Borders and the focus ring are even-odd paths |
| `BlurRect { rect, radii, sigma, color, inset, bounds }` | A blurred rounded rect (shadows, glow, inner shadow), painted inside `bounds`. With `inset` it paints the inverse (`1 - coverage`) and always sits inside a `PushClip` of the box |
| `Glyphs { run, origin, brush }` | A shaped glyph run from `gogo-ui-text`: font, size, variation coordinates and positioned glyph ids, relative to `origin` |
| `Image { id, rect, fit }` | An image from the store, wrapped by the scene in a `PushClip` of the node's shape because `Cover` crops |
| `PushClip { shape }` / `PushOpacity { alpha }` / `PushFilter { sigma }` | Layers, closed by `Pop`. A `PushFilter` wraps exactly one primitive |
| `Backdrop { shape, sigma }` | Blur whatever is already drawn under `shape` (§3) |

The list also carries `images`, the table behind each `ImageId` (an `ImageKey`: a handle, an
asset path or a noise seed), and the frame's `background`. A renderer looks images up in an
`ImageStore` by key and caches under the key, because an `ImageId` is only valid for its own
list. `Renderer::render(&DisplayList, &dyn ImageStore, scale, &mut Surface)` draws a list.

**Paint order per node:**
1. shadows;
2. glow;
3. backdrop;
4. fill, and the node's image or icon;
5. noise;
6. scanlines;
7. reflection;
8. inner shadow;
9. border;
10. the node's own text runs (after the border, so a border never covers text);
11. children;
12. focus ring.

The effects of one node are drawn in this fixed order, not in the order the skin wrote them.
A scroll container's children are drawn inside a `PushClip` of its box. A node with
`opacity` below 1 is one `PushOpacity` layer with its children, and `Paint::offset` moves the
node and its descendants without touching layout.

App-owned widgets are appended after the skin's tree, so they are always on top (S7).

## 2. Effects per renderer

Units follow CSS (design 27 §6): a shadow, glow or bloom radius r is a Gaussian with
σ = r/2, and `blur-behind r` has σ = r.

| Effect | vello_cpu | vello_gpu |
|---|---|---|
| Shadow, glow | `BlurRect` (blurred rounded rect) | same |
| Inner shadow | `BlurRect` with `inset` (inverted, clipped to the box) | same |
| Text bloom | one `PushFilter(blur)` layer around *each* glyph run (one primitive per layer, §5), then the sharp text | same (a native filter layer) |
| Gradient | peniko gradient brush | same |
| Noise | a tiled noise image brush; the tile is `ImageKey::Noise { seed }`, made by `noise_tile(seed)` | same |
| Scanlines | thin black fills, each `period / 2` px tall, one every `period` px at the given alpha, clipped to the box (only the lines that can be seen) | same; **no mask layers** (§5) |
| Reflection | a glass sheen: a white linear gradient fill over the box at the given angle, fading out before the middle (not a mirror) | same |
| Backdrop blur | §3 | §3 |

- *Measured:* both renderers scored the same on an effects sheet and on the three prototype
  skins, within a few levels per pixel.
- **vello_cpu's blurred rounded rect is a little narrower than a true Gaussian**
  (upstream bug [linebender/vello#2043](https://github.com/linebender/vello/issues/2043)). Goldens are taken from vello_cpu itself, so tests compare like with like.

## 3. Backdrop blur

No Vello renderer has a backdrop filter upstream, so `Backdrop` is ours on both renderers:

1. Render everything below the backdrop into an offscreen target (a pixmap, or a texture): the ops before it (`DisplayList::prefix`).
2. Draw that target back through a blur filter layer, clipped to the backdrop's shape.
3. Continue with the rest of the list on top.

A backdrop can sit inside open layers: its node's `PushOpacity`, an ancestor's `PushClip`
(a scroll container). Step 2 draws the blurred copy through those layers, which
`DisplayList::open_layers` lists outermost first, so the opacity and the clip still apply to it.
A backdrop with sigma 0 (radius 0) is not emitted, and neither are glow layers or bloom with radius 0.

Each backdrop re-renders everything below it. Sharing the below-target between backdrops
in one frame is an open optimisation (#543).

## 4. Images and caches

- **vello_gpu 0.4 takes images only as GPU textures**, not CPU pixmaps.
  - `gogo-render-gpu` uploads each `ImageId` once into a `TextureCache`.
  - **Today (prototype):** after each frame it drops every texture that frame didn't use.
    That is right for one page, but a scrolling library would re-upload covers constantly.
  - **Target (#541):** keep textures across frames and evict least-recently-used ones
    beyond a **byte budget**, so a library of covers neither re-uploads nor grows without
    bound.
  - vello_gpu has no image atlas, so the "image blank from the second frame" bug of compute
    vello can't happen.
- **The CPU path** keeps decoded pixmaps under the same budget.
- **Pipeline cache.** vello_gpu's pipelines are cached through wgpu's pipeline cache, in a
  file under the cache dir keyed with `wgpu::util::pipeline_cache_key`.
  - It is written atomically, and a corrupt or foreign file is ignored.
  - Creating the cache from bytes is an `unsafe` wgpu call. It is the one `unsafe` in the
    GUI crates, with a `// SAFETY:` note.
  - *Measured:* the cache took a cold NVIDIA start from 323 to 196 ms (#540).

## 5. Known upstream limits

| Limit | Effect on us |
|---|---|
| vello_gpu 0.4 is a beta. Mask layers are known to panic, so the prototype never uses them. A panic with filter layers that wrap more than one primitive came up while researching the library, but has no upstream issue we can cite, and the prototype never hit it | No effect uses mask layers. A new effect avoids them, and multi-primitive filter layers get a test on both renderers before they ship |
| vello_cpu filters panic in its multithreading mode | The CPU renderer is single-threaded (*measured* 70 ms for the heaviest page @2×) until upstream fixes it (#555) |
| No backdrop filter upstream | §3 |
| Hinting through parley → vello glyph runs is unconfirmed | Small text (10–11 px) is soft at @1× (#553) |
| wgpu's GL backend fails through NVIDIA's EGL vendor library ("Parent device is lost") | GL works through Mesa. On NVIDIA, Vulkan is used (#542) |

## 6. Damage and frame scheduling

- A frame is drawn only when a source slot changed, an input arrived, an animation is
  running, or the window was resized or rescaled (design 25 §3).
- Progress-only changes are capped at 10 frames a second.
- Partial redraws, which repaint only the dirty regions, come with damage tracking (#547).
  Until then a frame redraws the whole window.

## 7. Testing

- **Goldens.**
  - Small PNGs are drawn by vello_cpu at @1× and @2× and compared with a tolerance (08 §7):
    an effects sheet, and the built-in skins' game and shell routes on fixture data.
  - Fonts are the bundled set, and covers are procedural.
- **GPU vs CPU.** The same scenes are drawn on vello_gpu and diffed against vello_cpu within
  a tolerance. This runs on lavapipe in CI and is skipped without an adapter.
- **Structure.** Op counts and kinds per route catch lowering changes that don't move
  pixels much.
