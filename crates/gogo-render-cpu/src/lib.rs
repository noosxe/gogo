//! The vello_cpu renderer (design 28): the reference for golden images and the path when no
//! GPU works.
//!
//! [`CpuRenderer`] draws a [`DisplayList`] into a [`CpuSurface`]; [`CpuRenderer::render_to_rgba`]
//! is the same for a caller that only wants the pixels.
//!
//! # How the ops are drawn
//!
//! - Effects follow design 28 §2: blurred rounded rects, blur filter layers, gradients,
//!   image brushes. No mask layer is used.
//! - A `BlurRect` with four different corner radii is drawn with the **largest** one: vello's
//!   blurred rect takes a single radius (design 28 §5).
//! - A [`Op::Backdrop`] is drawn from an offscreen pixmap of the ops before it (design 28
//!   §3), through the layers open around it. Each backdrop is one extra render; earlier
//!   backdrops' pixmaps are reused, so the cost grows with their number, not its square.
//! - Images are cached as premultiplied pixmaps under their [`ImageKey`] (design 28 §4).
//!   There are no mipmaps: an image scaled far down aliases.
//! - It is **single-threaded**: vello_cpu's filters panic in its multithreading mode
//!   (design 28 §5).
//!
//! Drawing uses the SIMD level of the machine it runs on, so pixels can differ by a level or
//! two between architectures; golden tests compare with a tolerance.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;

use gogo_ui_scene::{
    Brush, DEFAULT_IMAGE_BUDGET, DisplayList, Fit, GlyphRun, Image, ImageCache, ImageKey,
    ImageStore, Op, Renderer, Shape, Surface, blur_radius, filter_sigma, image_placement,
};
use vello_cpu::filter_effects::{EdgeMode, Filter, FilterPrimitive};
use vello_cpu::kurbo::{Affine, BezPath, Point, Rect, Shape as _};
use vello_cpu::peniko::{Extend, Fill, ImageAlphaType, ImageQuality, ImageSampler};
use vello_cpu::{
    Glyph, ImageSource, Level, PixelMetadata, Pixmap, RasterizerSettings, RenderContext,
    RenderSettings, Resources, TargetInit,
};

/// Why a frame could not be drawn.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CpuError {
    /// A side is 0 or above 65535 device pixels.
    #[error("a surface of {width}x{height} device pixels is not possible")]
    Size {
        /// The width asked for.
        width: u32,
        /// The height asked for.
        height: u32,
    },
    /// The scale factor is not a positive number.
    #[error("the scale factor {0} is not a positive number")]
    Scale(f64),
    /// A glyph run could not be drawn.
    #[error("could not draw a glyph run: {0}")]
    Glyphs(String),
}

/// A pixel buffer in memory.
#[derive(Debug)]
pub struct CpuSurface {
    pixmap: Pixmap,
}

impl CpuSurface {
    /// A transparent surface of `width` by `height` device pixels.
    ///
    /// # Errors
    /// [`CpuError::Size`] when a side is 0 or above 65535.
    pub fn new(width: u32, height: u32) -> Result<CpuSurface, CpuError> {
        let (w, h) = sides(width, height)?;
        Ok(CpuSurface {
            pixmap: Pixmap::new(w, h),
        })
    }

    /// The pixels as premultiplied RGBA8, row by row from the top-left: what a window
    /// presents.
    pub fn premultiplied(&self) -> &[u8] {
        self.pixmap.data_as_u8_slice()
    }

    /// The pixels as a straight-alpha [`Image`].
    pub fn to_image(&self) -> Image {
        let (w, h) = self.size();
        let rgba = self.pixmap.clone().take_rgba8(ImageAlphaType::Alpha);
        match Image::new(w, h, rgba) {
            Some(i) => i,
            // A surface has no zero side and the pixmap holds exactly w * h pixels.
            None => unreachable!("a surface is always a valid image"),
        }
    }
}

impl Surface for CpuSurface {
    fn size(&self) -> (u32, u32) {
        (self.pixmap.width().into(), self.pixmap.height().into())
    }
}

fn sides(width: u32, height: u32) -> Result<(u16, u16), CpuError> {
    match (u16::try_from(width), u16::try_from(height)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(CpuError::Size { width, height }),
    }
}

/// An image ready to paint.
#[derive(Debug)]
struct Cached {
    pixmap: Arc<Pixmap>,
    width: u32,
    height: u32,
}

fn cached(image: Image) -> Option<(Cached, usize)> {
    let w = u16::try_from(image.width).ok()?;
    let h = u16::try_from(image.height).ok()?;
    let bytes = image.rgba.len();
    let pixmap = Pixmap::from_parts(
        image.rgba.to_vec(),
        w,
        h,
        PixelMetadata::new(ImageAlphaType::Alpha, true),
    );
    Some((
        Cached {
            pixmap: Arc::new(pixmap),
            width: w.into(),
            height: h.into(),
        },
        bytes,
    ))
}

/// The renderer. It keeps its image cache and glyph resources between frames.
#[derive(Debug)]
pub struct CpuRenderer {
    images: ImageCache<Cached>,
    resources: Resources,
}

impl Default for CpuRenderer {
    fn default() -> Self {
        CpuRenderer::new()
    }
}

impl CpuRenderer {
    /// A renderer with the default image budget.
    pub fn new() -> CpuRenderer {
        CpuRenderer::with_image_budget(DEFAULT_IMAGE_BUDGET)
    }

    /// A renderer whose image cache holds about `bytes`.
    pub fn with_image_budget(bytes: usize) -> CpuRenderer {
        CpuRenderer {
            images: ImageCache::new(bytes),
            resources: Resources::new(),
        }
    }

    /// Forgets the cached images. Needed when a key may now name other pixels, such as an
    /// asset path after a skin change or reload.
    pub fn clear_images(&mut self) {
        self.images.clear();
    }

    /// Draws `list` at `scale` into a new image of the list's size times `scale`.
    ///
    /// # Errors
    /// [`CpuError`], for example when that size is not possible.
    pub fn render_to_rgba(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
    ) -> Result<Image, CpuError> {
        let mut surface = surface_for(list, scale)?;
        self.render(list, images, scale, &mut surface)?;
        Ok(surface.to_image())
    }
}

fn surface_for(list: &DisplayList, scale: f64) -> Result<CpuSurface, CpuError> {
    if !(scale.is_finite() && scale > 0.0) {
        return Err(CpuError::Scale(scale));
    }
    let px = |v: f64| (v * scale).round().clamp(0.0, f64::from(u32::MAX)) as u32;
    CpuSurface::new(px(list.width), px(list.height))
}

impl Renderer for CpuRenderer {
    type Surface = CpuSurface;
    type Error = CpuError;

    fn name(&self) -> &'static str {
        "cpu"
    }

    fn render(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        surface: &mut CpuSurface,
    ) -> Result<(), CpuError> {
        if !(scale.is_finite() && scale > 0.0) {
            return Err(CpuError::Scale(scale));
        }
        let (width, height) = surface.size();
        let (w, h) = sides(width, height)?;
        let mut frame = Frame {
            list,
            store: images,
            scale,
            ctx: RenderContext::new_with(
                w,
                h,
                RenderSettings {
                    level: Level::try_detect().unwrap_or(Level::baseline()),
                    num_threads: 0,
                },
            ),
            images: &mut self.images,
            resources: &mut self.resources,
        };
        frame.images.begin_frame();
        // The pixmaps under the backdrops, in op order: each is the list up to its op.
        let mut below: Vec<Arc<Pixmap>> = Vec::new();
        for (i, op) in list.ops.iter().enumerate() {
            if matches!(op, Op::Backdrop { .. }) {
                let mut under = Pixmap::new(w, h);
                frame.draw(list.prefix(i), &below, &mut under)?;
                below.push(Arc::new(under));
            }
        }
        frame.draw(&list.ops, &below, &mut surface.pixmap)?;
        self.images.trim();
        Ok(())
    }
}

struct Frame<'a> {
    list: &'a DisplayList,
    store: &'a dyn ImageStore,
    scale: f64,
    ctx: RenderContext,
    images: &'a mut ImageCache<Cached>,
    resources: &'a mut Resources,
}

impl Frame<'_> {
    /// Draws `ops` over the list's background into `target`. `below` holds the pixmap of
    /// each backdrop among them, in order. Layers the ops leave open are closed.
    fn draw(
        &mut self,
        ops: &[Op],
        below: &[Arc<Pixmap>],
        target: &mut Pixmap,
    ) -> Result<(), CpuError> {
        self.ctx.reset();
        let mut below = below.iter();
        let mut depth = 0usize;
        for op in ops {
            self.reset_paint();
            match op {
                Op::Fill { shape, brush } => self.fill(shape, brush),
                Op::BlurRect {
                    rect,
                    radii,
                    sigma,
                    color,
                    inset,
                    bounds,
                } => {
                    self.ctx.set_paint(*color);
                    self.ctx.fill_blurred_rounded_rect_in(
                        &bounds.to_path(0.1),
                        rect,
                        blur_radius(*radii) as f32,
                        *sigma as f32,
                        *inset,
                    );
                }
                Op::Glyphs { run, origin, brush } => self.glyphs(run, *origin, brush)?,
                Op::Image { id, rect, fit } => {
                    let list = self.list;
                    if let Some(key) = list.image(*id) {
                        self.image(key, *rect, *fit);
                    }
                }
                Op::PushClip { shape } => {
                    let path = self.path(shape);
                    self.ctx.push_clip_layer(&path);
                    depth += 1;
                }
                Op::PushOpacity { alpha } => {
                    self.ctx.push_opacity_layer(*alpha);
                    depth += 1;
                }
                Op::PushFilter { sigma } => {
                    self.ctx
                        .push_filter_layer(blur(filter_sigma(*sigma, self.scale)));
                    depth += 1;
                }
                Op::Pop => {
                    if depth > 0 {
                        self.ctx.pop_layer();
                        depth -= 1;
                    }
                }
                Op::Backdrop { shape, sigma } => {
                    if let Some(under) = below.next() {
                        self.backdrop(shape, *sigma, Arc::clone(under));
                    }
                }
                // `Op` is non-exhaustive: an op this renderer does not know draws nothing.
                _ => {}
            }
        }
        for _ in 0..depth {
            self.ctx.pop_layer();
        }
        self.ctx.flush();
        self.ctx.render_with(
            target,
            self.resources,
            RasterizerSettings {
                target_init: TargetInit::Clear(self.list.background),
                ..RasterizerSettings::default()
            },
        );
        Ok(())
    }

    /// The state every op starts from: logical px scaled to the surface.
    fn reset_paint(&mut self) {
        self.ctx.set_transform(Affine::scale(self.scale));
        self.ctx.reset_paint_transform();
        self.ctx.set_fill_rule(Fill::NonZero);
    }

    /// The shape's path, with its fill rule set on the context.
    fn path(&mut self, shape: &Shape) -> BezPath {
        let (path, even_odd) = shape.to_path();
        if even_odd {
            self.ctx.set_fill_rule(Fill::EvenOdd);
        }
        path
    }

    /// Sets `brush` as the paint. `false`: there is nothing to paint with.
    fn brush(&mut self, brush: &Brush) -> bool {
        match brush {
            Brush::Solid(c) => self.ctx.set_paint(*c),
            Brush::Gradient(g) => self.ctx.set_paint(g.clone()),
            Brush::Image {
                id,
                transform,
                repeat,
                alpha,
            } => {
                let list = self.list;
                let Some(key) = list.image(*id) else {
                    return false;
                };
                let Some(c) = self.images.get_or_insert(key, self.store, cached) else {
                    return false;
                };
                let extend = if *repeat { Extend::Repeat } else { Extend::Pad };
                self.ctx
                    .set_paint(image_paint(Arc::clone(&c.pixmap), extend, *alpha));
                self.ctx.set_paint_transform(*transform);
            }
            _ => return false,
        }
        true
    }

    fn fill(&mut self, shape: &Shape, brush: &Brush) {
        if !self.brush(brush) {
            return;
        }
        match shape {
            Shape::Rect(r) => self.ctx.fill_rect(r),
            _ => {
                let path = self.path(shape);
                self.ctx.fill_path(&path);
            }
        }
    }

    fn glyphs(&mut self, run: &GlyphRun, origin: Point, brush: &Brush) -> Result<(), CpuError> {
        if !self.brush(brush) {
            return Ok(());
        }
        self.ctx
            .set_transform(Affine::scale(self.scale) * Affine::translate(origin.to_vec2()));
        // A gradient or image brush is placed in logical px, not from the text's origin.
        let placed = match brush {
            Brush::Image { transform, .. } => *transform,
            _ => Affine::IDENTITY,
        };
        self.ctx
            .set_paint_transform(Affine::translate(-origin.to_vec2()) * placed);
        self.ctx
            .glyph_run(self.resources, &run.font)
            .font_size(run.size)
            .normalized_coords(&run.coords)
            .fill_glyphs(run.glyphs.iter().map(|g| Glyph {
                id: g.id,
                x: g.x,
                y: g.y,
            }))
            .map_err(|e| CpuError::Glyphs(format!("{e:?}")))
    }

    fn image(&mut self, key: &ImageKey, rect: Rect, fit: Fit) {
        let Some(c) = self.images.get_or_insert(key, self.store, cached) else {
            return;
        };
        let Some((transform, drawn)) = image_placement(rect, fit, c.width, c.height) else {
            return;
        };
        self.ctx
            .set_paint(image_paint(Arc::clone(&c.pixmap), Extend::Pad, 1.0));
        self.ctx.set_paint_transform(transform);
        self.ctx.fill_rect(&drawn);
    }

    /// The blurred copy of `under` inside `shape`, in whatever layers are open.
    fn backdrop(&mut self, shape: &Shape, sigma: f64, under: Arc<Pixmap>) {
        let sigma = filter_sigma(sigma, self.scale);
        let clip = self.path(shape);
        self.ctx.push_clip_layer(&clip);
        self.ctx.push_filter_layer(blur(sigma));
        self.ctx.set_fill_rule(Fill::NonZero);
        self.ctx.set_paint(image_paint(under, Extend::Pad, 1.0));
        // The pixmap is in device px; the context draws in logical px.
        self.ctx
            .set_paint_transform(Affine::scale(1.0 / self.scale));
        // Only what the blur can reach: 3 sigma around the shape.
        self.ctx
            .fill_rect(&shape.bounds().inflate(3.0 * sigma, 3.0 * sigma));
        self.ctx.pop_layer();
        self.ctx.pop_layer();
    }
}

/// A Gaussian blur filter; vello scales sigma with the transform in force.
fn blur(sigma: f64) -> Filter {
    Filter::from_primitive(FilterPrimitive::GaussianBlur {
        std_deviation_x: sigma as f32,
        std_deviation_y: sigma as f32,
        edge_mode: EdgeMode::None,
    })
}

fn image_paint(pixmap: Arc<Pixmap>, extend: Extend, alpha: f32) -> vello_cpu::Image {
    vello_cpu::Image {
        image: ImageSource::Pixmap(pixmap),
        sampler: ImageSampler {
            x_extend: extend,
            y_extend: extend,
            quality: ImageQuality::Medium,
            alpha,
        },
    }
}
