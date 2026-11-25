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

| Op | Meaning |
|---|---|
| `Fill { shape, brush }` | A path or rounded rect with a solid colour, gradient or image brush |
| `BlurRect { rect, radii, sigma, colour, inset }` | A blurred rounded rect (shadows, glow, inner shadow) |
| `Glyphs { run, brush }` | A shaped glyph run from `gogo-ui-text`: font blob, glyph ids, positions |
| `Image { id, rect, fit }` | An image from the store |
| `PushClip { shape }` / `PushOpacity { alpha }` / `PushFilter { blur sigma }` | Layers, closed by `Pop` |
| `Backdrop { shape, sigma }` | Blur whatever is already drawn under `shape` (§3) |

**Paint order per node:**
1. shadows;
2. glow;
3. backdrop;
4. fill;
5. noise;
6. scanlines;
7. reflection;
8. inner shadow;
9. border;
10. children;
11. focus ring.

App-owned widgets are appended after the skin's tree, so they are always on top (S7).

## 2. Effects per renderer

Units follow CSS (design 27 §6): a shadow, glow or bloom radius r is a Gaussian with
σ = r/2, and `blur-behind r` has σ = r.

| Effect | vello_cpu | vello_gpu |
|---|---|---|
| Shadow, glow | `BlurRect` (blurred rounded rect) | same |
| Inner shadow | `BlurRect` with `inset` (inverted, clipped to the box) | same |
| Text bloom | `PushFilter(blur)` over the glyph runs, then the sharp text | same (a native filter layer) |
| Gradient | peniko gradient brush | same |
| Noise | a tiled noise image brush | same |
| Scanlines | geometry (thin fills), clipped | same; **no mask layers** (§5) |
| Reflection | a mirrored, faded copy through clip + opacity | same; **no mask layers** |
| Backdrop blur | §3 | §3 |

- *Measured:* both renderers scored the same on an effects sheet and on the three prototype
  skins, within a few levels per pixel.
- **vello_cpu's blurred rounded rect is a little narrower than a true Gaussian** (an
  upstream bug). Goldens are taken from vello_cpu itself, so tests compare like with like.

## 3. Backdrop blur

No Vello renderer has a backdrop filter upstream, so `Backdrop` is ours on both renderers:

1. Render everything below the backdrop into an offscreen target (a pixmap, or a texture).
2. Draw that target back through a blur filter layer, clipped to the backdrop's shape.
3. Continue with the rest of the list on top.

Each backdrop re-renders everything below it. Sharing the below-target between backdrops
in one frame is an open optimisation (#543).

## 4. Images and caches

- **vello_gpu 0.4 takes images only as GPU textures**, not CPU pixmaps.
  - `gogo-render-gpu` uploads each `ImageId` once into a `TextureCache`.
  - It evicts least-recently-used textures beyond a **byte budget**, so a scrolling library
    of covers can't grow without bound (#541).
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
| vello_gpu panics on mask layers and on multi-primitive filters | No effect uses them. A new effect must avoid them, or wait for upstream |
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
