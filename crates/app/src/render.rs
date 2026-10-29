//! Draws the map with wgpu: inside egui through a paint callback, or offscreen for
//! `--render`.
//!
//! Tiles are drawn in zoom groups, lowest first (fallback parents under the wanted tiles),
//! and within a group pass by pass across all its tiles: each tile's land, then all land
//! cover, all water, every line casing under every line. Each tile is scissored to its own
//! square, with edges rounded the same way for neighbours, so the 64-unit buffers never
//! overlap and semi-transparent fills have no seams. A tile's land square hides the parent
//! drawn under it.
//!
//! A pass whose layers are capped below a tile's zoom (format 0.3, layer `maxzoom`) is drawn
//! in that tile from its source, the stored tile at the cap zoom, with the source's transform
//! and the tile's scissor.
//!
//! The route (phase 3 step 3.6) goes over the tiles and under the labels, so they stay
//! readable: its halo, then its line, as line segments in pieces placed like tiles
//! (routing/line.rs), in the paint after the style's.
//!
//! Labels go on top, as glyph quads from the distance field atlas (text.rs, `text.wgsl`).
//!
//! Pushpins (phase 4 step 4.7) go over the labels: one instanced quad each, a shape drawn by
//! its distance in the shader, from a buffer made when they change (pins.rs).

use std::collections::HashMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::{self, wgpu};
use wgpu::util::DeviceExt;

use crate::camera::Camera;
use crate::layers::Hidden;
use crate::mesh::{Mesh, Segment, Vertex};
use crate::pins::{self, PinDraw, PinInstance};
use crate::pool::{Alloc, Kind, Pool};
use crate::routing::{Look, RouteDraw, RouteSegment};
use crate::style::{FILL_PASSES, LINE_PASSES, MAX_PAINTS, PASSES, Pass, ROUTE_PAINTS, Rgba, Style};
use crate::text::{ATLAS, Upload};
use crate::tiles::TileId;

/// MSAA sample count, for the window (eframe's `multisampling`) and `--render`, unless
/// `config.toml` or `--msaa` turn it off.
pub const MSAA: u32 = 4;

/// The sample count to use for a requested one: 1 or [`MSAA`].
pub fn samples(requested: Option<u32>) -> u32 {
    if requested == Some(1) { 1 } else { MSAA }
}
const EXTENT: f64 = (1u32 << omp_core::tile::EXTENT_BITS) as f64;

/// The route's lines and their halo on each side, in points, and their colours.
const ROUTE_WIDTH: f32 = 5.0;
const ROUTE_HALO: f32 = 1.5;
const ROUTE_COLOR: Rgba = [36, 104, 224, 255];
const ROUTE_HALO_COLOR: Rgba = [255, 255, 255, 255];
const RESTRICTED_COLOR: Rgba = [150, 96, 200, 255];
const PICKED_WIDTH: f32 = 7.0;
const PICKED_COLOR: Rgba = [14, 44, 122, 255];
const CONNECTOR_WIDTH: f32 = 2.0;
const CONNECTOR_DASH: [f32; 2] = [4.0, 3.0];
const CONNECTOR_COLOR: Rgba = [70, 70, 70, 255];
const ALTERNATIVE_WIDTH: f32 = 4.5;
const ALTERNATIVE_COLOR: Rgba = [150, 156, 166, 255];

/// A tile's vertex data on the GPU, in ranges of the pool's slabs.
pub struct GpuMesh {
    /// Vertices and indices.
    fill: Option<(Alloc, Alloc)>,
    segments: Option<Alloc>,
    fills: [std::ops::Range<u32>; FILL_PASSES.len()],
    lines: [std::ops::Range<u32>; LINE_PASSES.len()],
    pub bytes: usize,
}

impl GpuMesh {
    pub fn upload(pool: &Arc<Pool>, m: &Mesh) -> Self {
        let fill = (!m.indices.is_empty()).then(|| {
            (
                pool.upload(Kind::Vertex, bytemuck::cast_slice(&m.vertices)),
                pool.upload(Kind::Index, bytemuck::cast_slice(&m.indices)),
            )
        });
        let segments = (!m.segments.is_empty())
            .then(|| pool.upload(Kind::Vertex, bytemuck::cast_slice(&m.segments)));
        Self {
            fill,
            segments,
            fills: m.fills.clone(),
            lines: m.lines.clone(),
            bytes: m.bytes(),
        }
    }
}

/// One tile in a frame.
pub struct TileDraw {
    /// `None`: an empty tile, drawn as bare land.
    pub mesh: Option<Arc<GpuMesh>>,
    pub id: TileId,
    /// Top-left corner in viewport pixels, and pixels per tile unit.
    pub origin: [f32; 2],
    pub scale: f32,
    /// The tile's square in viewport pixels, `[x0, y0, x1, y1]`, rounded.
    pub scissor: [i32; 4],
    /// Per pass (`Pass as usize`): drawn from this entry of [`Frame::sources`] instead of
    /// `mesh`, for capped layers.
    pub from: [Option<u32>; PASSES],
}

/// A tile whose capped passes are drawn in other tiles' squares.
pub struct SourceDraw {
    pub mesh: Arc<GpuMesh>,
    pub origin: [f32; 2],
    pub scale: f32,
}

/// Everything one frame draws. Tiles are sorted by zoom.
pub struct Frame {
    /// Viewport size in pixels.
    pub size: [f32; 2],
    pub paints: Vec<GpuPaint>,
    pub tiles: Vec<TileDraw>,
    pub sources: Vec<SourceDraw>,
    /// The route, drawn over the tiles and under the labels.
    pub route: Option<RouteDraw>,
    /// Label glyphs, drawn over the map.
    pub glyphs: Vec<GlyphInstance>,
    /// The pushpins, drawn over the labels.
    pub pins: Option<PinDraw>,
    /// Glyphs new to the atlas.
    pub atlas: Vec<Upload>,
}

/// One label glyph, as `text.wgsl` draws it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GlyphInstance {
    /// The quad's centre in viewport pixels.
    pub center: [f32; 2],
    /// The text's direction, a unit vector; the quad's other axis is a quarter turn on.
    pub axis: [f32; 2],
    /// Half the quad's width and height, in pixels.
    pub half: [f32; 2],
    /// Its atlas rectangle, 0–1: `[x0, y0, x1, y1]`.
    pub uv: [f32; 4],
    /// Straight alpha, in the target's colour space.
    pub color: [u8; 4],
    pub halo: [u8; 4],
    /// Pixels per atlas pixel, and the halo's width in pixels.
    pub scale: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct GpuPaint {
    color: [f32; 4],
    casing: [f32; 4],
    width: [f32; 2],
    dash: [f32; 2],
    casing_dash: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    size: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileTransform {
    origin: [f32; 2],
    scale: f32,
    /// A route piece's paint; unused by tiles.
    paint: f32,
}

/// A style colour for the GPU, 0–1 with straight alpha: linear for an sRGB target.
pub fn gpu_color(c: Rgba, srgb_target: bool) -> [f32; 4] {
    let f = |v: u8| {
        let v = f32::from(v) / 255.0;
        match srgb_target {
            true if v <= 0.04045 => v / 12.92,
            true => ((v + 0.055) / 1.055).powf(2.4),
            false => v,
        }
    };
    [f(c[0]), f(c[1]), f(c[2]), f32::from(c[3]) / 255.0]
}

/// The style's paints at a zoom, in pixels, then the route's from [`ROUTE_PAINTS`].
pub fn paints(
    style: &Style,
    hidden: &Hidden,
    zoom: f64,
    pixels_per_point: f32,
    srgb_target: bool,
) -> Vec<GpuPaint> {
    let z = zoom as f32;
    let ppp = pixels_per_point;
    let color = |c: Rgba| gpu_color(c, srgb_target);
    style
        .paints
        .iter()
        .enumerate()
        .map(|(i, p)| {
            // Outside its zoom range or toggled off: transparent, and lines of no width are
            // not drawn.
            if !p.shown_at(z) || hidden.paint(i) {
                return GpuPaint::default();
            }
            let width = p.width.at(z);
            let casing = p.casing_width.at(z);
            GpuPaint {
                color: color(p.color),
                casing: color(p.casing),
                width: [
                    width * ppp,
                    if casing > 0.0 {
                        (width + casing) * ppp
                    } else {
                        0.0
                    },
                ],
                dash: p.dash.map(|d| d * ppp),
                casing_dash: p.casing_dash.map(|d| d * ppp),
                _pad: [0.0; 2],
            }
        })
        .chain(std::iter::repeat(GpuPaint::default()))
        .take(ROUTE_PAINTS)
        .chain(Look::ALL.map(|look| {
            let (c, w, dash) = match look {
                Look::Alternative => (ALTERNATIVE_COLOR, ALTERNATIVE_WIDTH, [0.0; 2]),
                Look::Connector => (CONNECTOR_COLOR, CONNECTOR_WIDTH, CONNECTOR_DASH),
                Look::Route => (ROUTE_COLOR, ROUTE_WIDTH, [0.0; 2]),
                Look::Restricted => (RESTRICTED_COLOR, ROUTE_WIDTH, [0.0; 2]),
                Look::Picked => (PICKED_COLOR, PICKED_WIDTH, [0.0; 2]),
            };
            GpuPaint {
                color: color(c),
                casing: color(ROUTE_HALO_COLOR),
                width: [w * ppp, (w + 2.0 * ROUTE_HALO) * ppp],
                dash: dash.map(|d| d * ppp),
                ..GpuPaint::default()
            }
        }))
        .chain(pins::Colour::ALL.map(|c| GpuPaint {
            color: color(c.rgba()),
            casing: color([255; 4]),
            width: [pins::EDGE * ppp, 0.0],
            ..GpuPaint::default()
        }))
        .collect()
}

/// Places `tiles` (sorted by zoom) for `cam`, in pixels. `capped` gives each tile's source
/// per cap zoom, `(tile, cap, source, its mesh)`, and `pass_cap` which passes are capped
/// where (the style's).
pub fn layout(
    cam: &Camera,
    pixels_per_point: f32,
    paints: Vec<GpuPaint>,
    tiles: impl IntoIterator<Item = (TileId, Option<Arc<GpuMesh>>)>,
    capped: impl IntoIterator<Item = (TileId, u8, TileId, Arc<GpuMesh>)>,
    pass_cap: &[Option<u8>; PASSES],
) -> Frame {
    let ppp = f64::from(pixels_per_point);
    let px = |w: [f64; 2]| {
        let p = cam.world_to_screen(w);
        [p[0] * ppp, p[1] * ppp]
    };
    let place = |id: TileId| {
        let [x0, y0, x1, y1] = id.world_rect();
        let a = px([x0, y0]);
        let b = px([x1, y1]);
        let scale = ((b[0] - a[0]) / EXTENT) as f32;
        ([a[0] as f32, a[1] as f32], scale, [a[0], a[1], b[0], b[1]])
    };
    let mut sources = Vec::new();
    let mut source_ids: HashMap<TileId, u32> = HashMap::new();
    let mut from: HashMap<(TileId, u8), u32> = HashMap::new();
    for (t, cap, s, mesh) in capped {
        let k = *source_ids.entry(s).or_insert_with(|| {
            let (origin, scale, _) = place(s);
            sources.push(SourceDraw {
                mesh,
                origin,
                scale,
            });
            (sources.len() - 1) as u32
        });
        from.insert((t, cap), k);
    }
    let tiles = tiles
        .into_iter()
        .map(|(id, mesh)| {
            let (origin, scale, r) = place(id);
            let pass_from = pass_cap.map(|cap| {
                cap.filter(|&m| id.z > m)
                    .and_then(|m| from.get(&(id, m)).copied())
            });
            TileDraw {
                mesh,
                id,
                origin,
                scale,
                scissor: r.map(|v| v.round().clamp(-1e9, 1e9) as i32),
                from: pass_from,
            }
        })
        .collect();
    Frame {
        size: [
            (cam.size[0] * ppp).round() as f32,
            (cam.size[1] * ppp).round() as f32,
        ],
        paints,
        tiles,
        sources,
        route: None,
        glyphs: Vec::new(),
        pins: None,
        atlas: Vec::new(),
    }
}

/// Pipelines and per-frame buffers; one per render target format.
pub struct Renderer {
    fill: wgpu::RenderPipeline,
    line: wgpu::RenderPipeline,
    casing: wgpu::RenderPipeline,
    route: wgpu::RenderPipeline,
    route_halo: wgpu::RenderPipeline,
    pin: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    paints: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    tile_layout: wgpu::BindGroupLayout,
    transforms: wgpu::Buffer,
    transforms_bg: wgpu::BindGroup,
    /// Bytes between tile transforms: the device's dynamic offset alignment.
    stride: u64,
    capacity: usize,
    /// The land square every tile is drawn on, style ID 0.
    square: (wgpu::Buffer, wgpu::Buffer),
    glyph: wgpu::RenderPipeline,
    halo: wgpu::RenderPipeline,
    atlas: wgpu::Texture,
    text_bg: wgpu::BindGroup,
    glyphs: wgpu::Buffer,
    pub srgb: bool,
    pub samples: u32,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, samples: u32) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("map.wgsl"));
        let uniform = |binding, size: u64, dynamic| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: wgpu::BufferSize::new(size),
            },
            count: None,
        };
        let paints_size = (MAX_PAINTS * std::mem::size_of::<GpuPaint>()) as u64;
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("map globals"),
            entries: &[uniform(0, 16, false), uniform(1, paints_size, false)],
        });
        let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("map tile"),
            entries: &[uniform(0, 16, true)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("map"),
            bind_group_layouts: &[Some(&globals_layout), Some(&tile_layout)],
            immediate_size: 0,
        });
        let fill_attrs = wgpu::vertex_attr_array![0 => Sint16x2, 1 => Uint16x2];
        let line_attrs =
            wgpu::vertex_attr_array![0 => Sint16x2, 1 => Sint16x2, 2 => Uint16x2, 3 => Float32];
        let pipeline = |label, vs, buffer: wgpu::VertexBufferLayout<'_>, fs| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(buffer)],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let fill_buf = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &fill_attrs,
        };
        let line_buf = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Segment>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &line_attrs,
        };
        let fill = pipeline("map fill", "vs_fill", fill_buf, "fs_fill");
        let line = pipeline("map line", "vs_line", line_buf.clone(), "fs_line");
        let casing = pipeline("map casing", "vs_casing", line_buf, "fs_line");
        let route_attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];
        let route_buf = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<RouteSegment>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &route_attrs,
        };
        let route = pipeline("route line", "vs_route", route_buf.clone(), "fs_line");
        let route_halo = pipeline("route halo", "vs_route_halo", route_buf, "fs_line");
        let pin_attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Uint16x2];
        let pin_buf = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<PinInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &pin_attrs,
        };
        let pin = pipeline("pushpins", "vs_pin", pin_buf, "fs_pin");

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("map globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let paints = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("map paints"),
            size: paints_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("map globals"),
            layout: &globals_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: paints.as_entire_binding(),
                },
            ],
        });
        let stride = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(16);
        let (transforms, transforms_bg) = Self::transforms(device, &tile_layout, stride, 64);
        // Out into the buffer, so its antialiased edge falls outside the scissor rectangle.
        let (a, b) = (-64, EXTENT as i16 + 64);
        let square_v = [[a, a], [b, a], [b, b], [a, b]].map(|pos| Vertex { pos, style: [0; 2] });
        let square = (
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("tile square"),
                contents: bytemuck::cast_slice(&square_v),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("tile square indices"),
                contents: bytemuck::cast_slice(&[0u32, 1, 2, 0, 2, 3]),
                usage: wgpu::BufferUsages::INDEX,
            }),
        );
        let (glyph, halo, atlas, text_bg) = Self::text(device, format, samples, &globals);
        let glyphs = Self::glyph_buffer(device, 1024);
        Self {
            fill,
            line,
            casing,
            route,
            route_halo,
            pin,
            glyph,
            halo,
            atlas,
            text_bg,
            glyphs,
            globals,
            paints,
            globals_bg,
            tile_layout,
            transforms,
            transforms_bg,
            stride,
            capacity: 64,
            square,
            srgb: format.is_srgb(),
            samples,
        }
    }

    /// The label pipelines (halos, then glyphs), the atlas and their bind group.
    fn text(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        samples: u32,
        globals: &wgpu::Buffer,
    ) -> (
        wgpu::RenderPipeline,
        wgpu::RenderPipeline,
        wgpu::Texture,
        wgpu::BindGroup,
    ) {
        let shader = device.create_shader_module(wgpu::include_wgsl!("text.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS,
                height: ATLAS,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let view = atlas.create_view(&Default::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![
            0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x4,
            4 => Unorm8x4, 5 => Unorm8x4, 6 => Float32x2
        ];
        let pipeline = |label, vs| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attrs,
                    })],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_glyph"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        (
            pipeline("label glyphs", "vs_glyph"),
            pipeline("label halos", "vs_halo"),
            atlas,
            bg,
        )
    }

    fn glyph_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("label glyphs"),
            size: (capacity * std::mem::size_of::<GlyphInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn transforms(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        stride: u64,
        capacity: usize,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("map tile transforms"),
            size: stride * capacity as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("map tile transforms"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(16),
                }),
            }],
        });
        (buf, bg)
    }

    /// Uploads the frame's uniforms.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &Frame) {
        let route: Vec<([f32; 2], f32, f32)> = frame
            .route
            .iter()
            .flat_map(|r| &r.lines)
            .flat_map(|l| {
                let paint = l.look.paint() as f32;
                l.pieces.iter().map(move |p| (p.1, p.2, paint))
            })
            .collect();
        let pins = frame.pins.as_ref().map(|p| (p.origin, p.scale, p.half));
        let n_transforms =
            frame.tiles.len() + frame.sources.len() + route.len() + usize::from(pins.is_some());
        if n_transforms > self.capacity {
            self.capacity = n_transforms.next_power_of_two();
            (self.transforms, self.transforms_bg) =
                Self::transforms(device, &self.tile_layout, self.stride, self.capacity);
        }
        let globals = Globals {
            size: frame.size,
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        let n = frame.paints.len().min(MAX_PAINTS);
        queue.write_buffer(&self.paints, 0, bytemuck::cast_slice(&frame.paints[..n]));
        for u in &frame.atlas {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: u.x,
                        y: u.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &u.sdf.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(u32::from(u.sdf.w)),
                    rows_per_image: Some(u32::from(u.sdf.h)),
                },
                wgpu::Extent3d {
                    width: u32::from(u.sdf.w),
                    height: u32::from(u.sdf.h),
                    depth_or_array_layers: 1,
                },
            );
        }
        let glyph_bytes = std::mem::size_of_val(frame.glyphs.as_slice()) as u64;
        if glyph_bytes > self.glyphs.size() {
            self.glyphs = Self::glyph_buffer(device, frame.glyphs.len().next_power_of_two());
        }
        if glyph_bytes > 0 {
            queue.write_buffer(&self.glyphs, 0, bytemuck::cast_slice(&frame.glyphs));
        }
        if n_transforms == 0 {
            return;
        }
        // The tiles' transforms, then the sources', then the route pieces', then the pins'
        // (their half size in `paint`).
        let mut data = vec![0u8; self.stride as usize * n_transforms];
        let placed = frame.tiles.iter().map(|t| (t.origin, t.scale, 0.0));
        let placed = placed.chain(frame.sources.iter().map(|s| (s.origin, s.scale, 0.0)));
        let placed = placed.chain(route).chain(pins);
        for (i, (origin, scale, paint)) in placed.enumerate() {
            let tr = TileTransform {
                origin,
                scale,
                paint,
            };
            let at = i * self.stride as usize;
            data[at..at + 16].copy_from_slice(bytemuck::bytes_of(&tr));
        }
        queue.write_buffer(&self.transforms, 0, &data);
    }

    /// Draws the frame. `offset` is where the viewport's top-left is in the render target, and
    /// `clip` (`[x0, y0, x1, y1]`, target pixels) what may be drawn on.
    pub fn paint(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        frame: &Frame,
        offset: [i32; 2],
        clip: [u32; 4],
    ) {
        pass.set_bind_group(0, &self.globals_bg, &[]);
        let scissor = |pass: &mut wgpu::RenderPass<'_>, t: &TileDraw| {
            let [x0, y0, x1, y1] = t.scissor;
            let x0 = (x0 + offset[0]).max(clip[0] as i32);
            let y0 = (y0 + offset[1]).max(clip[1] as i32);
            let x1 = (x1 + offset[0]).min(clip[2] as i32);
            let y1 = (y1 + offset[1]).min(clip[3] as i32);
            if x1 <= x0 || y1 <= y0 {
                return false;
            }
            pass.set_scissor_rect(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32);
            true
        };
        let mut start = 0;
        while start < frame.tiles.len() {
            let z = frame.tiles[start].id.z;
            let end = frame.tiles[start..]
                .iter()
                .position(|t| t.id.z != z)
                .map_or(frame.tiles.len(), |n| start + n);
            let group = start..end;
            start = end;
            let stride = self.stride;
            let bind = |pass: &mut wgpu::RenderPass<'_>, slot: usize| {
                pass.set_bind_group(1, &self.transforms_bg, &[(slot as u64 * stride) as u32]);
            };
            let each =
                |pass: &mut wgpu::RenderPass<'_>,
                 f: &mut dyn FnMut(&mut wgpu::RenderPass<'_>, usize, &TileDraw)| {
                    for i in group.clone() {
                        let t = &frame.tiles[i];
                        if scissor(pass, t) {
                            f(pass, i, t);
                        }
                    }
                };
            // The mesh pass `p` of tile `i` is drawn from, bound to its transform.
            let mesh_for = |pass: &mut wgpu::RenderPass<'_>, i: usize, t: &TileDraw, p: Pass| {
                let (mesh, slot) = match t.from[p as usize] {
                    Some(k) => (
                        Some(&frame.sources[k as usize].mesh),
                        frame.tiles.len() + k as usize,
                    ),
                    None => (t.mesh.as_ref(), i),
                };
                let mesh = mesh?;
                bind(pass, slot);
                Some(mesh.clone())
            };
            pass.set_pipeline(&self.fill);
            each(pass, &mut |pass, i, _| {
                bind(pass, i);
                pass.set_vertex_buffer(0, self.square.0.slice(..));
                pass.set_index_buffer(self.square.1.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..6, 0, 0..1);
            });
            for p in FILL_PASSES {
                each(pass, &mut |pass, i, t| {
                    let Some(m) = mesh_for(pass, i, t, p) else {
                        return;
                    };
                    let Some((v, ix)) = &m.fill else { return };
                    let r = m.fills[p.slot()].clone();
                    if !r.is_empty() {
                        pass.set_vertex_buffer(0, v.all());
                        pass.set_index_buffer(ix.all(), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(r, 0, 0..1);
                    }
                });
            }
            for p in LINE_PASSES {
                let cased = matches!(p, Pass::Tunnels | Pass::Roads | Pass::Bridges);
                let pipelines: &[&wgpu::RenderPipeline] = if cased {
                    &[&self.casing, &self.line]
                } else {
                    &[&self.line]
                };
                for pl in pipelines {
                    pass.set_pipeline(pl);
                    each(pass, &mut |pass, i, t| {
                        let Some(m) = mesh_for(pass, i, t, p) else {
                            return;
                        };
                        let Some(s) = &m.segments else { return };
                        let r = m.lines[p.slot()].clone();
                        if !r.is_empty() {
                            // An offset slice rather than a first instance, which GLES lacks.
                            let size = std::mem::size_of::<Segment>() as u64;
                            pass.set_vertex_buffer(
                                0,
                                s.slice(u64::from(r.start) * size..u64::from(r.end) * size),
                            );
                            pass.draw(0..6, 0..r.len() as u32);
                        }
                    });
                }
            }
        }
        if let Some(route) = &frame.route
            && clip[2] > clip[0]
            && clip[3] > clip[1]
        {
            pass.set_scissor_rect(clip[0], clip[1], clip[2] - clip[0], clip[3] - clip[1]);
            let mut first = frame.tiles.len() + frame.sources.len();
            let size = std::mem::size_of::<RouteSegment>() as u64;
            for line in &route.lines {
                let pipelines = match line.look.halo() {
                    true => &[&self.route_halo, &self.route][..],
                    false => &[&self.route][..],
                };
                for pl in pipelines {
                    pass.set_pipeline(pl);
                    for (k, (r, _, _)) in line.pieces.iter().enumerate() {
                        let offset = ((first + k) as u64 * self.stride) as u32;
                        pass.set_bind_group(1, &self.transforms_bg, &[offset]);
                        let bytes = u64::from(r.start) * size..u64::from(r.end) * size;
                        pass.set_vertex_buffer(0, line.band.segments.slice(bytes));
                        pass.draw(0..6, 0..r.len() as u32);
                    }
                }
                first += line.pieces.len();
            }
        }
        if !frame.glyphs.is_empty() && clip[2] > clip[0] && clip[3] > clip[1] {
            pass.set_scissor_rect(clip[0], clip[1], clip[2] - clip[0], clip[3] - clip[1]);
            let n = frame.glyphs.len() as u32;
            let bytes = u64::from(n) * std::mem::size_of::<GlyphInstance>() as u64;
            pass.set_bind_group(0, &self.text_bg, &[]);
            pass.set_vertex_buffer(0, self.glyphs.slice(..bytes));
            for pl in [&self.halo, &self.glyph] {
                pass.set_pipeline(pl);
                pass.draw(0..6, 0..n);
            }
        }
        if let Some(p) = &frame.pins
            && clip[2] > clip[0]
            && clip[3] > clip[1]
        {
            pass.set_scissor_rect(clip[0], clip[1], clip[2] - clip[0], clip[3] - clip[1]);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            let route: usize = frame.route.as_ref().map_or(0, |r| r.pieces());
            let slot = frame.tiles.len() + frame.sources.len() + route;
            pass.set_bind_group(
                1,
                &self.transforms_bg,
                &[(slot as u64 * self.stride) as u32],
            );
            let bytes = u64::from(p.gpu.n) * std::mem::size_of::<PinInstance>() as u64;
            pass.set_pipeline(&self.pin);
            pass.set_vertex_buffer(0, p.gpu.instances.slice(0..bytes));
            pass.draw(0..6, 0..p.gpu.n);
        }
    }
}

/// The map as an egui paint callback. The [`Renderer`] lives in egui's callback resources.
pub struct MapCallback(pub Frame);

impl egui_wgpu::CallbackTrait for MapCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(r) = resources.get_mut::<Renderer>() {
            r.prepare(device, queue, &self.0);
        }
        Vec::new()
    }

    fn paint(
        &self,
        info: eframe::egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(r) = resources.get::<Renderer>() else {
            return;
        };
        let vp = info.viewport_in_pixels();
        let clip = info.clip_rect_in_pixels();
        let clip = [
            clip.left_px.max(0) as u32,
            clip.top_px.max(0) as u32,
            (clip.left_px + clip.width_px).max(0) as u32,
            (clip.top_px + clip.height_px).max(0) as u32,
        ];
        r.paint(pass, &self.0, [vp.left_px, vp.top_px], clip);
    }
}

/// A device without a window, for `--render`.
pub fn headless_device() -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), String> {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    // As the window does (main.rs): Intel's Vulkan driver crashes on Windows.
    #[cfg(windows)]
    if wgpu::Backends::from_env().is_none() {
        desc.backends = wgpu::Backends::DX12 | wgpu::Backends::GL;
    }
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        ..Default::default()
    }))
    .map_err(|e| format!("no GPU adapter: {e}"))?;
    let limits = if adapter.get_info().backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("omp headless"),
        required_limits: wgpu::Limits {
            max_texture_dimension_2d: 8192,
            ..limits
        },
        ..Default::default()
    }))
    .map_err(|e| format!("no GPU device: {e}"))?;
    Ok((adapter, device, queue))
}

/// The format `--render` draws in: not sRGB, like egui's usual target, so colours match.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Draws `frame` offscreen on `background` and returns the RGBA pixels, row by row.
pub fn draw_offscreen(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    frame: &Frame,
    background: Rgba,
) -> Result<Vec<u8>, String> {
    let [w, h] = frame.size.map(|v| v as u32);
    let texture = |samples, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | usage,
            view_formats: &[],
        })
    };
    let resolved = texture(1, wgpu::TextureUsages::COPY_SRC);
    let msaa =
        (renderer.samples > 1).then(|| texture(renderer.samples, wgpu::TextureUsages::empty()));
    let resolved_view = resolved.create_view(&Default::default());
    let msaa_view = msaa.as_ref().map(|t| t.create_view(&Default::default()));
    renderer.prepare(device, queue, frame);
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let c = |v: u8| f64::from(v) / 255.0;
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("offscreen map"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: msaa_view.as_ref().unwrap_or(&resolved_view),
                depth_slice: None,
                resolve_target: msaa_view.as_ref().map(|_| &resolved_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: c(background[0]),
                        g: c(background[1]),
                        b: c(background[2]),
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        renderer.paint(&mut pass, frame, [0, 0], [0, 0, w, h]);
    }
    read_texture((device, queue), enc, &resolved, [w, h], Vec::new())
}

/// Reads back `texture` (`size` pixels, [`OFFSCREEN_FORMAT`]) after the commands in `enc`
/// and `extra` have run: RGBA, row by row.
pub fn read_texture(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    mut enc: wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    [w, h]: [u32; 2],
    extra: Vec<wgpu::CommandBuffer>,
) -> Result<Vec<u8>, String> {
    let row =
        (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let out = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("offscreen readback"),
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &out,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(extra.into_iter().chain([enc.finish()]));
    let slice = out.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("GPU: {e}"))?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| format!("reading back the image: {e}"))?;
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let at = y * row as usize;
        pixels.extend_from_slice(&data[at..at + w as usize * 4]);
    }
    Ok(pixels)
}
