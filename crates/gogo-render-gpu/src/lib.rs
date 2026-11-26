//! The vello_gpu renderer on wgpu (design 28): the fast path. It draws the same
//! [`DisplayList`] as `gogo-render-cpu`, and its tests compare the two.
//!
//! [`GpuRenderer`] draws into a [`GpuSurface`]: one it made offscreen
//! ([`GpuRenderer::offscreen`], which can be read back) or a texture view someone else owns,
//! such as a window's current frame ([`GpuSurface::from_view`]).
//!
//! # How the ops are drawn
//!
//! - Effects follow design 28 §2, as native filter layers. No mask layer is used: vello_gpu
//!   panics on them (design 28 §5).
//! - A `BlurRect` with four different corner radii is drawn with the largest one.
//! - A [`Op::Backdrop`] is cut and blurred (design 28 §3): the ops before it are drawn into
//!   an offscreen texture, which is then painted blurred inside the shape, through the layers
//!   open around it. Each backdrop is one more surface-sized texture, outside the image
//!   cache's budget, and one more submission of every op before it, so the replayed ops
//!   grow with the square of the number of backdrops (#543).
//! - vello_gpu takes images only as textures, so each [`ImageKey`] is uploaded once and kept
//!   in a byte-budget cache (design 28 §4).
//! - Intermediate layers may be as large as the device allows: vello_gpu's own cap of 4096 px
//!   is too small for a blur over a large window.
//!
//! Errors from the GPU are returned, not panicked on.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use glifo::Glyph;
use gogo_ui_scene::{
    Brush, DEFAULT_IMAGE_BUDGET, DisplayList, Fit, GlyphRun, Image, ImageCache, ImageKey,
    ImageStore, Op, Renderer, Shape, Surface, blur_radius, filter_sigma, image_placement,
};
use vello_gpu::filter_effects::{EdgeMode, Filter, FilterPrimitive};
use vello_gpu::kurbo::{Affine, BezPath, Point, Rect, Shape as _};
use vello_gpu::peniko::{Extend, Fill, ImageAlphaType, ImageQuality, ImageSampler};
use vello_gpu::{
    ClearSettings, ImageSource, LayersConfig, Level, MemorySettings, PixelMetadata, Pixmap,
    RenderOptions, RenderSettings, RenderTarget, RenderTargetConfig, Resources, Scene, SizeU16,
    TargetInit, TextureBindings, TextureId, TextureRegion,
};

/// The format of offscreen surfaces.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Why there is no renderer or a frame could not be drawn.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GpuError {
    /// wgpu found no adapter.
    #[error("no GPU adapter: {0}")]
    NoAdapter(String),
    /// The adapter gave no device.
    #[error("no GPU device: {0}")]
    NoDevice(String),
    /// A side is 0 or above what the device can hold.
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
    /// vello_gpu refused the scene.
    #[error("vello_gpu could not draw the scene: {0}")]
    Vello(String),
    /// wgpu reported an error.
    #[error("the GPU reported an error: {0}")]
    Device(String),
    /// The surface's pixels cannot be read.
    #[error("only an offscreen surface can be read back")]
    NotReadable,
}

/// A texture to draw into.
#[derive(Debug)]
pub struct GpuSurface {
    view: wgpu::TextureView,
    width: u16,
    height: u16,
    /// The texture of an offscreen surface, for reading back.
    texture: Option<wgpu::Texture>,
}

impl GpuSurface {
    /// A surface over a view of a 2D texture in the renderer's target format, such as a
    /// window's current frame.
    ///
    /// # Errors
    /// [`GpuError::Size`] when a side of the texture is above 65535.
    pub fn from_view(view: wgpu::TextureView) -> Result<GpuSurface, GpuError> {
        let (width, height) = (view.texture().width(), view.texture().height());
        let (w, h) = sides(width, height)?;
        Ok(GpuSurface {
            view,
            width: w,
            height: h,
            texture: None,
        })
    }
}

impl Surface for GpuSurface {
    fn size(&self) -> (u32, u32) {
        (self.width.into(), self.height.into())
    }
}

fn sides(width: u32, height: u32) -> Result<(u16, u16), GpuError> {
    match (u16::try_from(width), u16::try_from(height)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(GpuError::Size { width, height }),
    }
}

/// An uploaded image.
#[derive(Debug)]
struct Cached {
    view: wgpu::TextureView,
    id: TextureId,
    width: u32,
    height: u32,
}

/// What makes textures: the cache's side of the renderer.
struct Uploader<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    next_id: &'a mut u64,
}

impl Uploader<'_> {
    /// A premultiplied RGBA8 texture of `image`; `None` when the device cannot hold it.
    fn upload(&mut self, image: Image) -> Option<(Cached, usize)> {
        let (width, height) = (image.width, image.height);
        if width.max(height) > self.device.limits().max_texture_dimension_2d {
            return None;
        }
        let (w, h) = sides(width, height).ok()?;
        let bytes = image.rgba.len();
        // Premultiplied by vello, so the CPU renderer sees the same numbers.
        let pixmap = Pixmap::from_parts(
            image.rgba.to_vec(),
            w,
            h,
            PixelMetadata::new(ImageAlphaType::Alpha, true),
        );
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gogo image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            pixmap.data_as_u8_slice(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            size,
        );
        Some((
            Cached {
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
                id: next(self.next_id),
                width,
                height,
            },
            bytes,
        ))
    }
}

fn next(id: &mut u64) -> TextureId {
    *id += 1;
    TextureId(*id)
}

/// The renderer, on one device and one target format.
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    adapter: Option<wgpu::AdapterInfo>,
    renderer: vello_gpu::Renderer,
    resources: Resources,
    images: ImageCache<Cached>,
    next_id: u64,
}

impl std::fmt::Debug for GpuRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuRenderer")
            .field("format", &self.format)
            .field("adapter", &self.adapter)
            .finish_non_exhaustive()
    }
}

impl GpuRenderer {
    /// A renderer without a window, on the adapter wgpu picks. `WGPU_BACKEND` and wgpu's
    /// other environment variables apply.
    ///
    /// # Errors
    /// [`GpuError::NoAdapter`] or [`GpuError::NoDevice`].
    pub fn headless() -> Result<GpuRenderer, GpuError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .map_err(|e| GpuError::NoAdapter(e.to_string()))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gogo"),
            required_limits: adapter.limits(),
            ..wgpu::DeviceDescriptor::default()
        }))
        .map_err(|e| GpuError::NoDevice(e.to_string()))?;
        let mut renderer = GpuRenderer::new(device, queue, OFFSCREEN_FORMAT);
        renderer.adapter = Some(adapter.get_info());
        Ok(renderer)
    }

    /// A renderer on a device someone else made, drawing into surfaces of `format`. Use a
    /// non-sRGB format: the colours are written as they are.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> GpuRenderer {
        let layers_config = LayersConfig {
            // Capped to the device's own limit.
            max_texture_size: SizeU16::new(u16::MAX),
            ..LayersConfig::default()
        };
        let (renderer, resources) = vello_gpu::Renderer::new_with(
            &device,
            &queue,
            // Only the first allocation: each frame takes its size from the surface.
            &RenderTargetConfig {
                format,
                width: 1,
                height: 1,
            },
            RenderSettings::default().with_memory_settings(MemorySettings {
                layers_config,
                ..MemorySettings::default()
            }),
        );
        GpuRenderer {
            device,
            queue,
            format,
            adapter: None,
            renderer,
            resources,
            images: ImageCache::new(DEFAULT_IMAGE_BUDGET),
            next_id: 0,
        }
    }

    /// The adapter of a [`GpuRenderer::headless`] renderer.
    pub fn adapter_info(&self) -> Option<&wgpu::AdapterInfo> {
        self.adapter.as_ref()
    }

    /// A surface of `width` by `height` device pixels that [`GpuRenderer::read_back`] can
    /// read.
    ///
    /// # Errors
    /// [`GpuError::Size`] when the device cannot hold it.
    pub fn offscreen(&self, width: u32, height: u32) -> Result<GpuSurface, GpuError> {
        let (w, h) = self.fits(width, height)?;
        catching(&self.device, || {
            let texture = self.target_texture(w, h, wgpu::TextureUsages::COPY_SRC);
            Ok(GpuSurface {
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
                width: w,
                height: h,
                texture: Some(texture),
            })
        })
    }

    /// Forgets the cached images. Needed when a key may now name other pixels, such as an
    /// asset path after a skin change or reload.
    pub fn clear_images(&mut self) {
        self.images.clear();
    }

    fn fits(&self, width: u32, height: u32) -> Result<(u16, u16), GpuError> {
        if width.max(height) > self.device.limits().max_texture_dimension_2d {
            return Err(GpuError::Size { width, height });
        }
        sides(width, height)
    }

    fn target_texture(&self, w: u16, h: u16, usage: wgpu::TextureUsages) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gogo target"),
            size: wgpu::Extent3d {
                width: w.into(),
                height: h.into(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | usage,
            view_formats: &[],
        })
    }

    /// The pixels of an offscreen surface as a straight-alpha [`Image`].
    ///
    /// # Errors
    /// [`GpuError::NotReadable`] for a surface from a view; [`GpuError::Device`] when the
    /// copy fails.
    pub fn read_back(&self, surface: &GpuSurface) -> Result<Image, GpuError> {
        let Some(texture) = &surface.texture else {
            return Err(GpuError::NotReadable);
        };
        let (width, height) = surface.size();
        let (buffer, padded) = catching(&self.device, || self.copy_out(texture))?;
        let row = width * 4;
        let (send, receive) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                // The receiver outlives the poll below.
                let _ = send.send(result);
            });
        let failed = |e: &dyn std::fmt::Display| GpuError::Device(e.to_string());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| failed(&e))?;
        receive
            .recv()
            .map_err(|e| failed(&e))?
            .map_err(|e| failed(&e))?;
        let mut data = Vec::with_capacity(row as usize * height as usize);
        for line in buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| failed(&e))?
            .chunks(padded as usize)
        {
            data.extend_from_slice(&line[..row as usize]);
        }
        buffer.unmap();
        if matches!(self.format, wgpu::TextureFormat::Bgra8Unorm) {
            for pixel in data.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
        }
        // The texture holds premultiplied pixels.
        let rgba = Pixmap::from_parts(data, surface.width, surface.height, Default::default())
            .take_rgba8(ImageAlphaType::Alpha);
        Image::new(width, height, rgba).ok_or(GpuError::Size { width, height })
    }

    /// A mappable buffer the texture is copied into, and the length of its rows.
    fn copy_out(&self, texture: &wgpu::Texture) -> Result<(wgpu::Buffer, u32), GpuError> {
        let (width, height) = (texture.width(), texture.height());
        let padded = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let size = u64::from(padded) * u64::from(height);
        if size > self.device.limits().max_buffer_size {
            return Err(GpuError::Size { width, height });
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gogo readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        Ok((buffer, padded))
    }

    /// Draws `list` at `scale` offscreen and reads the pixels back.
    ///
    /// # Errors
    /// [`GpuError`].
    pub fn render_to_rgba(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
    ) -> Result<Image, GpuError> {
        if !(scale.is_finite() && scale > 0.0) {
            return Err(GpuError::Scale(scale));
        }
        let px = |v: f64| (v * scale).round().clamp(0.0, f64::from(u32::MAX)) as u32;
        let mut surface = self.offscreen(px(list.width), px(list.height))?;
        self.render(list, images, scale, &mut surface)?;
        self.read_back(&surface)
    }

    fn frame(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        surface: &GpuSurface,
    ) -> Result<(), GpuError> {
        let (w, h) = (surface.width, surface.height);
        // The textures under the backdrops, in op order: each is the list up to its op.
        let mut below: Vec<(wgpu::TextureView, TextureId)> = Vec::new();
        for (i, op) in list.ops.iter().enumerate() {
            if matches!(op, Op::Backdrop { .. }) {
                let view = self
                    .target_texture(w, h, wgpu::TextureUsages::TEXTURE_BINDING)
                    .create_view(&wgpu::TextureViewDescriptor::default());
                self.pass(list, images, scale, (w, h), list.prefix(i), &below, &view)?;
                below.push((view, next(&mut self.next_id)));
            }
        }
        self.pass(
            list,
            images,
            scale,
            (w, h),
            &list.ops,
            &below,
            &surface.view,
        )
    }

    /// Draws `ops` over the list's background into `target` and submits. `below` holds the
    /// texture of each backdrop among them, in order.
    #[allow(clippy::too_many_arguments)]
    fn pass(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        (w, h): (u16, u16),
        ops: &[Op],
        below: &[(wgpu::TextureView, TextureId)],
        target: &wgpu::TextureView,
    ) -> Result<(), GpuError> {
        let mut recorder = Recorder {
            list,
            store: images,
            scale,
            scene: Scene::new_with(w, h, Level::try_detect().unwrap_or(Level::baseline())),
            bindings: TextureBindings::new(),
            images: &mut self.images,
            uploader: Uploader {
                device: &self.device,
                queue: &self.queue,
                next_id: &mut self.next_id,
            },
            resources: &mut self.resources,
        };
        recorder.record(ops, below)?;
        let Recorder {
            scene, bindings, ..
        } = recorder;

        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut options = RenderOptions::default();
        options.target_init = TargetInit::Clear(ClearSettings::Viewport {
            color: list.background,
        });
        self.renderer
            .render(
                &scene,
                &mut self.resources,
                &mut encoder,
                RenderTarget {
                    view: target,
                    depth_view: None,
                },
                &bindings,
                options,
            )
            .map_err(|e| GpuError::Vello(e.to_string()))?;
        // Submitted before the next pass: vello_gpu reuses its buffers.
        self.queue.submit([encoder.finish()]);
        Ok(())
    }
}

impl Renderer for GpuRenderer {
    type Surface = GpuSurface;
    type Error = GpuError;

    fn name(&self) -> &'static str {
        "gpu"
    }

    fn render(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        surface: &mut GpuSurface,
    ) -> Result<(), GpuError> {
        if !(scale.is_finite() && scale > 0.0) {
            return Err(GpuError::Scale(scale));
        }
        self.fits(surface.width.into(), surface.height.into())?;
        let device = self.device.clone();
        let drawn = catching(&device, || {
            self.images.begin_frame();
            let drawn = self.frame(list, images, scale, surface);
            self.images.trim();
            drawn
        });
        if drawn.is_err() {
            // An upload that failed left an invalid texture behind; upload again next time.
            self.images.clear();
        }
        drawn
    }
}

/// Runs `work` and returns what wgpu reported during it as an error: wgpu panics on an
/// error that nobody caught.
fn catching<T>(
    device: &wgpu::Device,
    work: impl FnOnce() -> Result<T, GpuError>,
) -> Result<T, GpuError> {
    let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let done = work();
    // Scopes close in the reverse order they were opened.
    let invalid = pollster::block_on(validation.pop());
    let full = pollster::block_on(memory.pop());
    match invalid.or(full) {
        Some(e) => Err(GpuError::Device(e.to_string())),
        None => done,
    }
}

/// Turns ops into a vello_gpu scene and the textures it samples.
struct Recorder<'a> {
    list: &'a DisplayList,
    store: &'a dyn ImageStore,
    scale: f64,
    scene: Scene,
    bindings: TextureBindings,
    images: &'a mut ImageCache<Cached>,
    uploader: Uploader<'a>,
    resources: &'a mut Resources,
}

impl Recorder<'_> {
    /// Records `ops`. Layers they leave open are closed.
    fn record(
        &mut self,
        ops: &[Op],
        below: &[(wgpu::TextureView, TextureId)],
    ) -> Result<(), GpuError> {
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
                    self.scene.set_paint(*color);
                    self.scene.fill_blurred_rounded_rect_in(
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
                    self.scene.push_clip_layer(&path);
                    depth += 1;
                }
                Op::PushOpacity { alpha } => {
                    self.scene.push_opacity_layer(*alpha);
                    depth += 1;
                }
                Op::PushFilter { sigma } => {
                    self.scene
                        .push_filter_layer(blur(filter_sigma(*sigma, self.scale)));
                    depth += 1;
                }
                Op::Pop => {
                    if depth > 0 {
                        self.scene.pop_layer();
                        depth -= 1;
                    }
                }
                Op::Backdrop { shape, sigma } => {
                    if let Some((view, id)) = below.next() {
                        self.bindings.insert(*id, view.clone());
                        self.backdrop(shape, *sigma, *id);
                    }
                }
                // `Op` is non-exhaustive: an op this renderer does not know draws nothing.
                _ => {}
            }
        }
        for _ in 0..depth {
            self.scene.pop_layer();
        }
        Ok(())
    }

    /// The state every op starts from: logical px scaled to the surface.
    fn reset_paint(&mut self) {
        self.scene.set_transform(Affine::scale(self.scale));
        self.scene.reset_paint_transform();
        self.scene.set_fill_rule(Fill::NonZero);
    }

    /// The shape's path, with its fill rule set on the scene.
    fn path(&mut self, shape: &Shape) -> BezPath {
        let (path, even_odd) = shape.to_path();
        if even_odd {
            self.scene.set_fill_rule(Fill::EvenOdd);
        }
        path
    }

    /// The cached texture of `key`, bound for this scene: its id and size.
    fn texture(&mut self, key: &ImageKey) -> Option<(TextureId, u32, u32)> {
        let uploader = &mut self.uploader;
        let c = self
            .images
            .get_or_insert(key, self.store, |image| uploader.upload(image))?;
        self.bindings.insert(c.id, c.view.clone());
        Some((c.id, c.width, c.height))
    }

    /// Sets `brush` as the paint. `false`: there is nothing to paint with.
    fn brush(&mut self, brush: &Brush) -> bool {
        match brush {
            Brush::Solid(c) => self.scene.set_paint(*c),
            Brush::Gradient(g) => self.scene.set_paint(g.clone()),
            Brush::Image {
                id,
                transform,
                repeat,
                alpha,
            } => {
                let list = self.list;
                let Some((texture, ..)) = list.image(*id).and_then(|key| self.texture(key)) else {
                    return false;
                };
                let extend = if *repeat { Extend::Repeat } else { Extend::Pad };
                self.scene.set_paint(image_paint(texture, extend, *alpha));
                self.scene.set_paint_transform(*transform);
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
            Shape::Rect(r) => self.scene.fill_rect(r),
            _ => {
                let path = self.path(shape);
                self.scene.fill_path(&path);
            }
        }
    }

    fn glyphs(&mut self, run: &GlyphRun, origin: Point, brush: &Brush) -> Result<(), GpuError> {
        if !self.brush(brush) {
            return Ok(());
        }
        self.scene
            .set_transform(Affine::scale(self.scale) * Affine::translate(origin.to_vec2()));
        // A gradient or image brush is placed in logical px, not from the text's origin.
        let placed = match brush {
            Brush::Image { transform, .. } => *transform,
            _ => Affine::IDENTITY,
        };
        self.scene
            .set_paint_transform(Affine::translate(-origin.to_vec2()) * placed);
        self.scene
            .glyph_run(self.resources, &run.font)
            .font_size(run.size)
            .normalized_coords(&run.coords)
            .fill_glyphs(run.glyphs.iter().map(|g| Glyph {
                id: g.id,
                x: g.x,
                y: g.y,
            }))
            .map_err(|e| GpuError::Glyphs(format!("{e:?}")))
    }

    fn image(&mut self, key: &ImageKey, rect: Rect, fit: Fit) {
        let Some((texture, width, height)) = self.texture(key) else {
            return;
        };
        let Some((transform, drawn)) = image_placement(rect, fit, width, height) else {
            return;
        };
        self.scene.set_paint(image_paint(texture, Extend::Pad, 1.0));
        self.scene.set_paint_transform(transform);
        self.scene.fill_rect(&drawn);
    }

    /// The blurred copy of the texture inside `shape`, in whatever layers are open.
    fn backdrop(&mut self, shape: &Shape, sigma: f64, under: TextureId) {
        let sigma = filter_sigma(sigma, self.scale);
        let clip = self.path(shape);
        self.scene.push_clip_layer(&clip);
        self.scene.push_filter_layer(blur(sigma));
        self.scene.set_fill_rule(Fill::NonZero);
        self.scene.set_paint(image_paint(under, Extend::Pad, 1.0));
        // The texture is in device px; the scene draws in logical px.
        self.scene
            .set_paint_transform(Affine::scale(1.0 / self.scale));
        // Only what the blur can reach: 3 sigma around the shape.
        self.scene
            .fill_rect(&shape.bounds().inflate(3.0 * sigma, 3.0 * sigma));
        self.scene.pop_layer();
        self.scene.pop_layer();
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

fn image_paint(texture: TextureId, extend: Extend, alpha: f32) -> vello_gpu::Image {
    vello_gpu::Image {
        image: ImageSource::external_texture(texture, TextureRegion::Full, true),
        sampler: ImageSampler {
            x_extend: extend,
            y_extend: extend,
            quality: ImageQuality::Medium,
            alpha,
        },
    }
}
