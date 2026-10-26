//! wgpu renderer for OpenKumite.
//!
//! Everything is in the converted assets' space: glTF metres, right-handed, y up.
//!
//! Draw kinds:
//! - [`SkinnedModel`]: characters, standard 4-joint linear blend skinning (the converter turned the engine's
//!   seam welding into skin weights). Skinning matrices are uploaded per frame with [`Renderer::update_bones`].
//! - [`StaticMesh`]: stage floor and props (unlit, texture × vertex colour, like the original hardware stage code).
//! - Sky meshes: the backdrop drum, drawn first, rotation-only, centred on the camera, without depth.
//! - Debug lines ([`LineVertex`]): drawn last, on top of everything.
//!
//! An [`Overlay`] (the debug UI) can draw into the frame after the scene, before it is presented.

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub use wgpu;

/// Debug line vertex: world position (metres), RGBA colour. Pairs of vertices form lines.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

/// Something drawn over the finished scene (e.g. a debug UI), into the same frame.
pub trait Overlay {
    /// 2D drawn behind the characters: after the sky and the stage, before the fighters (the original hardware game's
    /// HUD list B, linked behind the 3D characters). It goes into the 3D scene's target: `scale` is that target's size
    /// over the window's (the render scale).
    fn draw_under(&mut self, _device: &wgpu::Device, _queue: &wgpu::Queue, _encoder: &mut wgpu::CommandEncoder, _target: &wgpu::TextureView, _size: [u32; 2], _scale: f32) {}
    fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, size: [u32; 2]);
}

/// Per-frame numbers for the debug UI.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    /// The frame was not drawn (window hidden / surface lost).
    pub skipped: u32,
    pub draw_calls: u32,
    pub triangles: u32,
    pub lines: u32,
}

/// Effect particles for one frame: camera-facing (or floor) quads as triangles, textured from an atlas whose
/// alpha is the sprite's brightness. `alpha` quads are blended over, `additive` ones add light.
pub struct Particles<'a> {
    pub texture: &'a Texture,
    pub alpha: &'a [StaticVertex],
    pub additive: &'a [StaticVertex],
    /// Untextured triangles in their vertex colours, blended over / added (drawn first, in the order given:
    /// sort them back to front).
    pub flat_alpha: &'a [StaticVertex],
    pub flat_add: &'a [StaticVertex],
}

/// A captured frame: tightly packed RGBA8.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Vertex of a skinned character mesh (see the crate docs).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SkinVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [u16; 4],
    pub weights: [f32; 4],
    /// Bind-pose normal (zero: lit by the face's own normal).
    pub normal: [f32; 3],
}

/// The fighters' shadows (0x8003AFCC): each fighter's shadow mesh moved along the light onto the floor, drawn in
/// one opaque colour after the stage, before the fighters.
#[derive(Clone, Copy, Debug)]
pub struct Shadow {
    /// Towards the light (metres' axes, y up): the shadow falls the other way.
    pub toward: Vec3,
    /// The floor's height (metres).
    pub floor: f32,
    /// sRGB.
    pub colour: Vec3,
}

/// A stage's light for the fighters (the original's per-stage table 0x80096C78, NCCS): the ambient, the key light's
/// colour and the direction towards it (metres' axes), the material colour (128 = 1); a fill straight from above at
/// half the key colour.
#[derive(Clone, Copy, Debug)]
pub struct StageLight {
    pub ambient: Vec3,
    pub key: Vec3,
    pub toward: Vec3,
    pub material: Vec3,
}

/// Vertex of a static (stage) mesh: world position, atlas UV, RGBA modulation colour.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct StaticVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

/// Camera in metre space (y up), like the web viewer.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub fov_y: f32,
    /// The backdrop drum's placement (world, metres) and its view-space Y scale.
    pub sky: Mat4,
    pub sky_y_scale: f32,
    /// Draw the drum before the stage meshes (the stage stages.json names) instead of after them.
    pub sky_first: bool,
    /// The drum's panels seen from behind are not drawn (the original hardware's NCLIP: one of the original's drums,
    /// which the camera can stand outside of); off for a drum around the camera (an extra stage's, Ball's strip).
    pub sky_cull: bool,
    /// The stage's fill behind everything (sRGB 0..255), else the renderer's clear colour.
    pub background: Option<[u8; 3]>,
    /// The stage meshes' placement (the floor follows the fighters by whole map periods).
    pub floor: Mat4,
    /// The hit light on the fighters: colour × strength and its position (metres).
    pub hit_light: Option<([f32; 3], Vec3)>,
    /// The stage meshes cut to x0..x1, z0..z1 (metres): the original's finite floor patch, where it shows.
    pub floor_clip: Option<[f32; 4]>,
    /// The extra stages' floor lighting (export_stages / 0x80082E60, 0x8008364C), when the stage has it.
    pub floor_light: Option<FloorLight>,
    /// An arena with walls (the extra levels): both fighters (metres) and a margin; stage surfaces above
    /// the floor in a tube from the camera to either fighter, nearer the camera than that fighter by more than the
    /// margin, are cut away, so walls and props between the camera and the fight do not hide it.
    pub cutaway: Option<([Vec3; 2], f32)>,
    /// With the cutaway: cut everything above the floor nearer the camera than the nearer fighter (less the margin),
    /// not just the tubes (the camera was raised over the walls).
    pub cutaway_all: bool,
    /// Stage surfaces above the floor beyond this z (metres; the camera's side of a route) are not drawn.
    pub near_cut: Option<f32>,
    /// The stage's light for the fighters (None: a plain key light).
    pub light: Option<StageLight>,
    /// The fighters' shadows on the floor (None: none).
    pub shadow: Option<Shadow>,
}

/// The extra stages' floor lighting: mode 0 fades with the view depth (128 + T[(sz - 10000) * 6710 >> 16] * shade /
/// 128), mode 1 lights from each fighter (2^27 / d^2 each, at most 254), mode 2 from their midpoint (0x7A120000 / d^2,
/// at most 192); 128 = as drawn. Distances in game units (1 mm).
#[derive(Clone, Copy, Debug)]
pub struct FloorLight {
    pub mode: u32,
    pub shade: f32,
    /// The fighters' places on the floor (metres, x and z).
    pub fighters: [[f32; 2]; 2],
    /// The fade table 0x80056564, every 32nd of its 1024 bytes.
    pub table: [f32; 32],
}

impl Camera {
    /// A camera with the backdrop at the origin.
    pub fn new(eye: Vec3, target: Vec3, fov_y: f32) -> Self {
        Self {
            eye,
            target,
            fov_y,
            sky: Mat4::IDENTITY,
            sky_y_scale: 1.0,
            sky_first: false,
            sky_cull: true,
            background: None,
            floor: Mat4::IDENTITY,
            hit_light: None,
            floor_clip: None,
            floor_light: None,
            cutaway: None,
            cutaway_all: false,
            near_cut: None,
            light: None,
            shadow: None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],     // proj · view
    sky_view_proj: [[f32; 4]; 4], // proj · drum Y scale · view · drum placement
    floor: [[f32; 4]; 4],         // stage mesh placement
    light_dir: [f32; 4],
    hit_colour: [f32; 4],
    hit_pos: [f32; 4],
    floor_clip: [f32; 4],
    floor_light: [f32; 4], // mode (0 none, 1 depth fade, 2 fighters, 3 midpoint), shade, -, -
    fighters: [f32; 4],    // x, z of both fighters (metres)
    eye: [f32; 4],
    forward: [f32; 4],
    fade: [[f32; 4]; 8],
    cutaway: [f32; 4],   // x, y, z (metres) of fighter 1, margin (0 = off)
    cutaway2: [f32; 4],  // x, y, z of fighter 2, 1 = cut all nearer than the nearer fighter
    near_cut: [f32; 4],  // z beyond which stage surfaces above the floor go, 1 = on
    key_color: [f32; 4], // the stage light's key colour, w = 1 when there is one
    ambient: [f32; 4],
    material: [f32; 4],
    shadow: [f32; 4], // the light's x / y and z / y, the floor's height, 1 = on
    shadow_colour: [f32; 4],
}

/// RGBA texture with its bind group.
pub struct Texture {
    bind_group: wgpu::BindGroup,
    /// The same texture through the smooth (linear) sampler, for [`RenderSettings::smooth_textures`].
    bind_group_smooth: wgpu::BindGroup,
    texture: wgpu::Texture,
}

impl Texture {
    fn group(&self, smooth: bool) -> &wgpu::BindGroup {
        if smooth {
            &self.bind_group_smooth
        } else {
            &self.bind_group
        }
    }
}

/// How frames are drawn (the OPTION menu's GRAPHICS, or the command line).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderSettings {
    /// The 3D scene's resolution over the window's (0.25 .. 2: below 1 coarser, above 1 supersampled).
    pub scale: f32,
    /// Wait for the display's refresh.
    pub vsync: bool,
    /// Textures filtered (linear) instead of the original hardware's sharp texels.
    pub smooth_textures: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self { scale: 1.0, vsync: true, smooth_textures: false }
    }
}

/// The 3D scene's own target when the render scale is not 1: colour (with a non-sRGB view for the HUD drawn
/// under the fighters), depth, and the bind group the final pass samples it through.
struct SceneTarget {
    size: [u32; 2],
    colour: wgpu::Texture,
    depth: wgpu::TextureView,
    group: wgpu::BindGroup,
}

pub struct SkinnedModel {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    count: u32,
    bones: wgpu::Buffer,
    bones_group: wgpu::BindGroup,
    texture: Arc<Texture>,
    /// The shadow mesh (vertices, indices, count), on the same bones.
    shadow: Option<(wgpu::Buffer, wgpu::Buffer, u32)>,
}

impl SkinnedModel {
    /// The model's texture (its own: the face animation writes into it).
    pub fn texture(&self) -> &Texture {
        &self.texture
    }
}

pub struct StaticMesh {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    count: u32,
    texture: Arc<Texture>,
    /// Drawn without depth, sorted far to near each frame as the original hardware's ordering table does (the backdrop
    /// drum): its positions and triangles.
    sorted: Option<(Vec<Vec3>, Vec<[u32; 3]>)>,
}

pub const MAX_BONES: usize = 64;

/// The final pass: the scene's target stretched over the window (a full-screen triangle).
const BLIT_WGSL: &str = "
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
struct Out { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    let uv = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.pos = vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@fragment fn fs(i: Out) -> @location(0) vec4<f32> { return textureSample(tex, samp, i.uv); }
";
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

pub struct Renderer {
    /// What the window surfaces are made with (Android makes its window again after the background).
    instance: wgpu::Instance,
    /// Window surface, or `None` when rendering offscreen (tests, screenshots).
    surface: Option<wgpu::Surface<'static>>,
    /// Offscreen colour target (headless mode).
    offscreen: Option<wgpu::Texture>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// What the scene is drawn as: the surface's format, or its sRGB view when it has no sRGB format.
    frame_format: wgpu::TextureFormat,
    depth: wgpu::TextureView,
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    bones_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Linear, mipmapped, tiling both ways: textures that ask for it (the extra levels').
    sampler_filtered: wgpu::Sampler,
    /// Linear, the atlas's addressing: the smooth textures setting.
    sampler_smooth: wgpu::Sampler,
    /// The scene's upscale (nearest: a lower resolution shows its pixels) and downscale (linear) to the window.
    blit_nearest: wgpu::Sampler,
    blit_linear: wgpu::Sampler,
    blit_pipeline: wgpu::RenderPipeline,
    settings: RenderSettings,
    scene: Option<SceneTarget>,
    skinned_pipeline: wgpu::RenderPipeline,
    /// The fighters' shadows.
    shadow_pipeline: wgpu::RenderPipeline,
    /// Force's blob shadows: the floor darkened (the original hardware's subtraction).
    blob_pipeline: wgpu::RenderPipeline,
    /// This frame's blob shadows ([`Renderer::set_blob_shadows`]).
    blobs: Vec<StaticVertex>,
    static_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    particle_alpha: wgpu::RenderPipeline,
    particle_add: wgpu::RenderPipeline,
    /// A white texel for the untextured particles.
    white: Option<Arc<Texture>>,
    adapter_name: String,
    /// The GPU's kind (discrete, integrated, software; a browser does not tell).
    device_type: wgpu::DeviceType,
    /// Copy the next presented window frame back to memory (see [`Renderer::request_capture`]).
    capture_next: bool,
    captured: Option<Result<Image>>,
    pub clear: wgpu::Color,
}

impl Renderer {
    /// Create the renderer for a window (anything wgpu can make a surface from).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(target: impl Into<wgpu::SurfaceTarget<'static>>, width: u32, height: u32) -> Result<Self> {
        pollster::block_on(Self::new_async(target, width, height))
    }

    /// The same, waiting for the GPU without blocking (a browser cannot block).
    pub async fn new_async(target: impl Into<wgpu::SurfaceTarget<'static>>, width: u32, height: u32) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(target)?;
        Self::init(instance, Some(surface), width, height).await
    }

    /// Create an offscreen renderer of a fixed size; read frames back with [`Renderer::capture`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn headless(width: u32, height: u32) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        pollster::block_on(Self::init(instance, None, width, height))
    }

    async fn init(instance: wgpu::Instance, surface: Option<wgpu::Surface<'static>>, width: u32, height: u32) -> Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface.as_ref(),
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .context("no GPU adapter")?;
        let info = adapter.get_info();
        log::info!("GPU: {info:?}");
        let adapter_name = format!("{} ({:?})", info.name, info.backend);
        // (in a browser, WebGL2's limits so it runs on either WebGPU or WebGL2)
        let base = if cfg!(target_arch = "wasm32") { wgpu::Limits::downlevel_webgl2_defaults() } else { wgpu::Limits::downlevel_defaults() };
        let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor { label: Some("openkumite"), required_limits: base.using_resolution(adapter.limits()), ..Default::default() }).await?;
        let max = device.limits().max_texture_dimension_2d;
        let (width, height) = (width.clamp(1, max), height.clamp(1, max));
        let config = match &surface {
            Some(surface) => {
                let mut c = surface.get_default_config(&adapter, width, height).context("surface not supported by the adapter")?;
                c.present_mode = wgpu::PresentMode::AutoVsync;
                // allow copying window frames back (screenshots), when the surface supports it
                if surface.get_capabilities(&adapter).usages.contains(wgpu::TextureUsages::COPY_SRC) {
                    c.usage |= wgpu::TextureUsages::COPY_SRC;
                }
                // overlays (egui) blend in gamma space: they draw through a non-sRGB view of the frame, where the
                // device allows other view formats (WebGL 2 does not: there they draw into the frame as it is)
                let views = adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::SURFACE_VIEW_FORMATS);
                if views && c.format.remove_srgb_suffix() != c.format {
                    c.view_formats.push(c.format.remove_srgb_suffix());
                }
                // a surface without an sRGB format (a WebGPU canvas: bgra8unorm): the frame is drawn through an
                // sRGB view of it, so its linear colours are encoded as everywhere else
                if views && !c.format.is_srgb() && c.format.add_srgb_suffix() != c.format {
                    c.view_formats.push(c.format.add_srgb_suffix());
                }
                surface.configure(&device, &c);
                c
            }
            None => wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                color_space: Default::default(),
                width,
                height,
                present_mode: wgpu::PresentMode::AutoVsync,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: vec![wgpu::TextureFormat::Rgba8Unorm],
            },
        };
        let offscreen = surface.is_none().then(|| make_offscreen(&device, &config));
        let srgb = config.format.add_srgb_suffix();
        let frame_format = if config.view_formats.contains(&srgb) { srgb } else { config.format };
        let depth = make_depth(&device, &config);

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let bones_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bones"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        // original-hardware look: nearest filtering, clamped atlas.
        let sampler =
            device.create_sampler(&wgpu::SamplerDescriptor { label: Some("nearest"), address_mode_u: wgpu::AddressMode::Repeat, address_mode_v: wgpu::AddressMode::ClampToEdge, ..Default::default() });

        let sampler_filtered = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("filtered"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let sampler_smooth = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("smooth"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blit_nearest = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("blit nearest"), ..Default::default() });
        let blit_linear =
            device.create_sampler(&wgpu::SamplerDescriptor { label: Some("blit linear"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("blit"), source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()) });
        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("blit"), bind_group_layouts: &[Some(&texture_layout)], immediate_size: 0 });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&blit_layout),
            vertex: wgpu::VertexState { module: &blit_shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: frame_format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("openkumite"), source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()) });
        let layout = |groups: &[Option<&wgpu::BindGroupLayout>]| device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: groups, immediate_size: 0 });
        let skinned_layout = layout(&[Some(&globals_layout), Some(&texture_layout), Some(&bones_layout)]);
        let static_layout = layout(&[Some(&globals_layout), Some(&texture_layout)]);

        let skin_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint16x4, 3 => Float32x4, 4 => Float32x3];
        let static_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4];
        let make = |label: &str,
                    layout: &wgpu::PipelineLayout,
                    vs: &str,
                    fs: &str,
                    buffers: &[wgpu::VertexBufferLayout],
                    depth_write: bool,
                    depth_test: bool,
                    topology: wgpu::PrimitiveTopology,
                    blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some(vs), compilation_options: Default::default(), buffers: &buffers.iter().cloned().map(Some).collect::<Vec<_>>() },
                primitive: wgpu::PrimitiveState { topology, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(if depth_test { wgpu::CompareFunction::Less } else { wgpu::CompareFunction::Always }),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: frame_format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let skin_vb = [wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<SkinVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &skin_attrs }];
        let static_vb = [wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<StaticVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &static_attrs }];
        let line_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
        let line_vb = [wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<LineVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &line_attrs }];
        let line_layout = layout(&[Some(&globals_layout)]);
        let tri = wgpu::PrimitiveTopology::TriangleList;
        let skinned_pipeline = make("skinned", &skinned_layout, "vs_skinned", "fs_lit", &skin_vb, true, true, tri, None);
        let static_pipeline = make("static", &static_layout, "vs_static", "fs_static", &static_vb, true, true, tri, None);
        let sky_pipeline = make("sky", &static_layout, "vs_sky", "fs_unlit", &static_vb, false, false, tri, None);
        let line_pipeline = make("lines", &line_layout, "vs_line", "fs_line", &line_vb, false, false, wgpu::PrimitiveTopology::LineList, None);
        // particles: depth-tested against the scene, never written
        let particle_alpha = make("particles alpha", &static_layout, "vs_static", "fs_particle_alpha", &static_vb, false, true, tri, Some(wgpu::BlendState::ALPHA_BLENDING));
        let add = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add };
        let keep = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add };
        let particle_add = make("particles add", &static_layout, "vs_static", "fs_particle_add", &static_vb, false, true, tri, Some(wgpu::BlendState { color: add, alpha: keep }));
        // shadows: opaque, depth-tested against the stage (the original sorts them behind what stands nearer), never
        // written
        let shadow_pipeline = make("shadows", &skinned_layout, "vs_shadow", "fs_shadow", &skin_vb, false, true, tri, None);
        // blobs: the frame times the fragment's factor
        let times = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::Src, operation: wgpu::BlendOperation::Add };
        let blob_pipeline = make("blob shadows", &static_layout, "vs_world", "fs_blob", &static_vb, false, true, tri, Some(wgpu::BlendState { color: times, alpha: keep }));

        Ok(Self {
            instance,
            surface,
            offscreen,
            device,
            queue,
            config,
            frame_format,
            depth,
            globals,
            globals_group,
            texture_layout,
            bones_layout,
            sampler,
            sampler_filtered,
            sampler_smooth,
            blit_nearest,
            blit_linear,
            blit_pipeline,
            settings: RenderSettings::default(),
            scene: None,
            skinned_pipeline,
            shadow_pipeline,
            blob_pipeline,
            blobs: Vec::new(),
            static_pipeline,
            sky_pipeline,
            line_pipeline,
            particle_alpha,
            particle_add,
            white: None,
            adapter_name,
            device_type: info.device_type,
            capture_next: false,
            captured: None,
            clear: wgpu::Color { r: 0.06, g: 0.067, b: 0.085, a: 1.0 },
        }
        .with_white())
    }

    fn with_white(mut self) -> Self {
        self.white = Some(self.texture(1, 1, &[255; 4]));
        self
    }

    fn bind(&self, view: &wgpu::TextureView, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.texture_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) }],
        })
    }

    /// Change how frames are drawn: the render scale, vsync (the surface reconfigured), the texture filter.
    pub fn set_settings(&mut self, s: RenderSettings) {
        let s = RenderSettings { scale: s.scale.clamp(0.1, 4.0), ..s };
        if s.vsync != self.settings.vsync {
            self.config.present_mode = if s.vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync };
            if let Some(surface) = &self.surface {
                surface.configure(&self.device, &self.config);
            }
        }
        self.settings = s;
    }

    /// The scene's target for the render scale (None at 1: the frame itself), made or remade for its size.
    fn scene_target(&mut self) -> Option<&SceneTarget> {
        let scale = self.settings.scale;
        if (scale - 1.0).abs() < 1e-3 {
            self.scene = None;
            return None;
        }
        let max = self.max_texture();
        let size = [((self.config.width as f32 * scale).round() as u32).clamp(1, max), ((self.config.height as f32 * scale).round() as u32).clamp(1, max)];
        if self.scene.as_ref().is_none_or(|t| t.size != size) {
            let overlay = [self.overlay_format()];
            let views: &[wgpu::TextureFormat] = if overlay[0] != self.frame_format { &overlay } else { &[] };
            let colour = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("scene"),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.frame_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: views,
            });
            let depth = self
                .device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("scene depth"),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: DEPTH,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            // a lower resolution shows its pixels (nearest); a higher one is averaged down (linear)
            let sampler = if scale < 1.0 { &self.blit_nearest } else { &self.blit_linear };
            let group = self.bind(&colour.create_view(&Default::default()), sampler);
            self.scene = Some(SceneTarget { size, colour, depth, group });
        }
        self.scene.as_ref()
    }

    /// The GPU's largest texture side: frames and the 3D scene's target are at most this.
    pub fn max_texture(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        // (a window larger than the GPU's textures: the frame at the limit, stretched)
        let (width, height) = (width.min(self.max_texture()), height.min(self.max_texture()));
        self.config.width = width;
        self.config.height = height;
        match &self.surface {
            Some(s) => s.configure(&self.device, &self.config),
            None => self.offscreen = Some(make_offscreen(&self.device, &self.config)),
        }
        self.depth = make_depth(&self.device, &self.config);
    }

    /// The window's surface dropped (None: Android took the window away, the app in the background; draw nothing
    /// meanwhile) or made again for its new window, at its size. The GPU's resources stay.
    pub fn set_surface(&mut self, target: Option<wgpu::SurfaceTarget<'static>>, width: u32, height: u32) -> Result<()> {
        self.surface = None;
        if let Some(t) = target {
            self.surface = Some(self.instance.create_surface(t)?);
            self.resize(width, height);
        }
        Ok(())
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Colour format an [`Overlay`] renders into: the frame's format without sRGB encoding (UI libraries
    /// such as egui blend in gamma space).
    pub fn overlay_format(&self) -> wgpu::TextureFormat {
        let plain = self.config.format.remove_srgb_suffix();
        if plain == self.config.format || self.config.view_formats.contains(&plain) {
            plain
        } else {
            self.config.format
        }
    }

    pub fn size(&self) -> [u32; 2] {
        [self.config.width, self.config.height]
    }

    /// GPU name and backend.
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// The GPU's kind (discrete, integrated, software; `Other` in a browser).
    pub fn device_type(&self) -> wgpu::DeviceType {
        self.device_type
    }

    /// Copy the next window frame back; collect it with [`Renderer::take_capture`].
    pub fn request_capture(&mut self) {
        self.capture_next = true;
    }

    pub fn take_capture(&mut self) -> Option<Result<Image>> {
        self.captured.take()
    }

    pub fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height as f32
    }

    /// Upload an RGBA8 texture (sRGB colours, alpha 0 = transparent texel).
    pub fn texture(&self, width: u32, height: u32, rgba: &[u8]) -> Arc<Texture> {
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: Default::default(), aspect: Default::default() },
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * width), rows_per_image: Some(height) },
            size,
        );
        let view = tex.create_view(&Default::default());
        let bind_group = self.bind(&view, &self.sampler);
        let bind_group_smooth = self.bind(&view, &self.sampler_smooth);
        Arc::new(Texture { bind_group, bind_group_smooth, texture: tex })
    }

    /// Upload an RGBA8 texture for linear filtering with mipmaps (box-filtered on the CPU), tiling both ways.
    /// Transparent texels keep their alpha; the mip levels average the opaque texels' colours so cut-outs do not
    /// darken at their edges.
    pub fn texture_filtered(&self, width: u32, height: u32, rgba: &[u8]) -> Arc<Texture> {
        let mut levels = vec![(width, height, rgba.to_vec())];
        while let Some((w, h, px)) = levels.last().filter(|l| l.0 > 1 || l.1 > 1) {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let mut out = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    let (mut c, mut a, mut n) = ([0u32; 3], 0u32, 0u32);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (sx, sy) = ((2 * x + dx).min(w - 1), (2 * y + dy).min(h - 1));
                        let i = ((sy * w + sx) * 4) as usize;
                        a += px[i + 3] as u32;
                        if px[i + 3] >= 128 {
                            n += 1;
                            for k in 0..3 {
                                c[k] += px[i + k] as u32;
                            }
                        }
                    }
                    let o = ((y * nw + x) * 4) as usize;
                    for k in 0..3 {
                        out[o + k] = c[k].checked_div(n).unwrap_or(0) as u8;
                    }
                    out[o + 3] = (a / 4) as u8;
                }
            }
            levels.push((nw, nh, out));
        }
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, (w, h, px)) in levels.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: level as u32, origin: Default::default(), aspect: Default::default() },
                px,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(*h) },
                wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 },
            );
        }
        let view = tex.create_view(&Default::default());
        let bind_group = self.bind(&view, &self.sampler_filtered);
        let bind_group_smooth = self.bind(&view, &self.sampler_filtered);
        Arc::new(Texture { bind_group, bind_group_smooth, texture: tex })
    }

    /// Replace the texels of rectangle (x, y, w, h) of `texture` with `rgba` (RGBA8 rows): the face animation's
    /// copies.
    pub fn update_texture(&self, texture: &Texture, [x, y, w, h]: [u32; 4], rgba: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture.texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: Default::default() },
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    pub fn skinned(&self, vertices: &[SkinVertex], indices: &[u32], texture: Arc<Texture>) -> SkinnedModel {
        let vbuf = self.buffer(bytemuck::cast_slice(vertices), wgpu::BufferUsages::VERTEX);
        let ibuf = self.buffer(bytemuck::cast_slice(indices), wgpu::BufferUsages::INDEX);
        let bones = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bones"),
            size: (MAX_BONES * 64) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bones_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bones"),
            layout: &self.bones_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: bones.as_entire_binding() }],
        });
        SkinnedModel { vbuf, ibuf, count: indices.len() as u32, bones, bones_group, texture, shadow: None }
    }

    /// Give a model its shadow mesh (skinned to the model's joints).
    pub fn set_shadow(&self, model: &mut SkinnedModel, vertices: &[SkinVertex], indices: &[u32]) {
        let v = self.buffer(bytemuck::cast_slice(vertices), wgpu::BufferUsages::VERTEX);
        let i = self.buffer(bytemuck::cast_slice(indices), wgpu::BufferUsages::INDEX);
        model.shadow = (!indices.is_empty()).then_some((v, i, indices.len() as u32));
    }

    /// The next frame's blob shadows (Force, 0x800793B8): triangles on the floor (metres), each vertex's
    /// colour the amount (of 1) taken off the floor's displayed colour.
    pub fn set_blob_shadows(&mut self, vertices: Vec<StaticVertex>) {
        self.blobs = vertices;
    }

    /// Skinning matrices (joint world × inverse bind, including the fighter's placement), index = joint.
    pub fn update_bones(&self, model: &SkinnedModel, bones: &[Mat4]) {
        let n = bones.len().min(MAX_BONES);
        self.queue.write_buffer(&model.bones, 0, bytemuck::cast_slice(&bones[..n]));
    }

    pub fn static_mesh(&self, vertices: &[StaticVertex], indices: &[u32], texture: Arc<Texture>) -> StaticMesh {
        StaticMesh {
            vbuf: self.buffer(bytemuck::cast_slice(vertices), wgpu::BufferUsages::VERTEX),
            ibuf: self.buffer(bytemuck::cast_slice(indices), wgpu::BufferUsages::INDEX),
            count: indices.len() as u32,
            texture,
            sorted: None,
        }
    }

    /// A mesh drawn without depth whose triangles are put in order each frame, the farthest first (the original
    /// hardware's ordering table, by each polygon's average depth): the backdrop drum, whose panels overlap.
    pub fn sorted_mesh(&self, vertices: &[StaticVertex], indices: &[u32], texture: Arc<Texture>) -> StaticMesh {
        let positions = vertices.iter().map(|v| Vec3::from_array(v.pos)).collect();
        let tris = indices.as_chunks::<3>().0.to_vec();
        StaticMesh {
            vbuf: self.buffer(bytemuck::cast_slice(vertices), wgpu::BufferUsages::VERTEX),
            ibuf: self.buffer(bytemuck::cast_slice(indices), wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST),
            count: indices.len() as u32,
            texture,
            sorted: Some((positions, tris)),
        }
    }

    /// A sorted mesh's triangles far to near under `view_proj` (stable: ties keep their order).
    fn sort_mesh(&self, m: &StaticMesh, view_proj: Mat4, cull: bool) {
        let Some((pos, tris)) = &m.sorted else { return };
        let clip: Vec<glam::Vec4> = pos.iter().map(|p| view_proj * p.extend(1.0)).collect();
        // the original hardware's NCLIP: a panel seen from behind (reversed on screen) is not drawn, so from outside
        // the drum its near side does not cover the far one
        let facing = |t: &[u32; 3]| {
            let [a, b, c] = t.map(|i| clip[i as usize]);
            if !cull || a.w <= 0.0 || b.w <= 0.0 || c.w <= 0.0 {
                return true;
            }
            let (a, b, c) = (a.truncate().truncate() / a.w, b.truncate().truncate() / b.w, c.truncate().truncate() / c.w);
            (b - a).perp_dot(c - a) >= 0.0
        };
        let mut order: Vec<(f32, [u32; 3])> = tris.iter().filter(|t| facing(t)).map(|t| (t.iter().map(|&i| clip[i as usize].w).sum::<f32>(), *t)).collect();
        order.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut indices: Vec<u32> = order.into_iter().flat_map(|(_, t)| t).collect();
        // (culled ones: degenerate triangles in their places)
        indices.resize(m.count as usize, 0);
        self.queue.write_buffer(&m.ibuf, 0, bytemuck::cast_slice(&indices));
    }

    fn buffer(&self, data: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
        self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: data, usage })
    }

    /// Draw one frame: sky meshes first (camera-centred, no depth), then static meshes, the overlay's "under"
    /// part, then characters, particles and the debug `lines`; then the `overlay`, and present.
    #[allow(clippy::too_many_arguments)] // one frame's independent parts
    pub fn render_with(
        &mut self,
        cam: &Camera,
        sky: &[&StaticMesh],
        statics: &[&StaticMesh],
        models: &[&SkinnedModel],
        particles: Option<&Particles>,
        lines: &[LineVertex],
        mut overlay: Option<&mut dyn Overlay>,
    ) -> Result<RenderStats> {
        let mut stats = RenderStats::default();
        let proj = glam::camera::rh::proj::directx::perspective(cam.fov_y, self.aspect(), 0.05, 400.0);
        let view = glam::camera::rh::view::look_at_mat4(cam.eye, cam.target, Vec3::Y);
        let sky_view_proj = proj * Mat4::from_scale(Vec3::new(1.0, cam.sky_y_scale, 1.0)) * view * cam.sky;
        // the drum's panels in the order the game's ordering table draws them: far first
        for m in sky {
            self.sort_mesh(m, sky_view_proj, cam.sky_cull);
        }
        let g = Globals {
            view_proj: (proj * view).to_cols_array_2d(),
            sky_view_proj: sky_view_proj.to_cols_array_2d(),
            floor: cam.floor.to_cols_array_2d(),
            hit_colour: cam.hit_light.map_or([0.0; 4], |(c, _)| [c[0], c[1], c[2], 0.0]),
            hit_pos: cam.hit_light.map_or([0.0; 4], |(_, p)| [p.x, p.y, p.z, 1.0]),
            light_dir: cam.light.map_or(Vec3::new(0.35, 0.8, 0.5), |l| l.toward).normalize().extend(0.0).to_array(),
            floor_clip: cam.floor_clip.unwrap_or([0.0; 4]),
            floor_light: cam.floor_light.map_or([0.0; 4], |l| {
                [
                    if l.mode == 0 {
                        if l.shade != 0.0 {
                            1.0
                        } else {
                            0.0
                        }
                    } else {
                        l.mode as f32 + 1.0
                    },
                    l.shade,
                    0.0,
                    0.0,
                ]
            }),
            fighters: cam.floor_light.map_or([0.0; 4], |l| [l.fighters[0][0], l.fighters[0][1], l.fighters[1][0], l.fighters[1][1]]),
            eye: cam.eye.extend(1.0).to_array(),
            forward: (cam.target - cam.eye).normalize_or_zero().extend(0.0).to_array(),
            fade: cam.floor_light.map_or([[0.0; 4]; 8], |l| std::array::from_fn(|i| std::array::from_fn(|k| l.table[i * 4 + k]))),
            cutaway: cam.cutaway.map_or([0.0; 4], |(p, m)| [p[0].x, p[0].y, p[0].z, m]),
            cutaway2: cam.cutaway.map_or([0.0; 4], |(p, _)| [p[1].x, p[1].y, p[1].z, f32::from(u8::from(cam.cutaway_all))]),
            near_cut: cam.near_cut.map_or([0.0; 4], |z| [z, 1.0, 0.0, 0.0]),
            key_color: cam.light.map_or([0.0; 4], |l| [l.key.x, l.key.y, l.key.z, 1.0]),
            ambient: cam.light.map_or([0.0; 4], |l| l.ambient.extend(0.0).to_array()),
            material: cam.light.map_or([0.0; 4], |l| l.material.extend(0.0).to_array()),
            shadow: cam.shadow.filter(|s| s.toward.y > 0.05).map_or([0.0; 4], |s| [s.toward.x / s.toward.y, s.toward.z / s.toward.y, s.floor, 1.0]),
            shadow_colour: cam.shadow.map_or([0.0; 4], |s| {
                // sRGB to the frame's linear colour
                let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
                [lin(s.colour.x), lin(s.colour.y), lin(s.colour.z), 1.0]
            }),
        };
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&g));

        let frame = match &self.surface {
            Some(surface) => match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
                wgpu::CurrentSurfaceTexture::Occluded => return Ok(RenderStats { skipped: 1, ..stats }), // window hidden: skip the frame
                other => {
                    log::warn!("surface: {other:?}; reconfiguring");
                    surface.configure(&self.device, &self.config);
                    return Ok(stats);
                }
            },
            None => None,
        };
        let frame_view = match &frame {
            Some(f) => f.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(self.frame_format), ..Default::default() }),
            None => self.offscreen.as_ref().context("no render target")?.create_view(&Default::default()),
        };
        // the 3D (and the HUD under the fighters) into the scene's target at the render scale, or the frame
        let overlay_format = self.overlay_format();
        let smooth = self.settings.smooth_textures;
        let scene = self.scene_target().map(|t| {
            let ov = wgpu::TextureViewDescriptor { format: Some(overlay_format), ..Default::default() };
            (t.colour.create_view(&Default::default()), t.colour.create_view(&ov), t.depth.clone(), t.size)
        });
        let view_tex = scene.as_ref().map_or_else(|| frame_view.clone(), |s| s.0.clone());
        let depth_view = scene.as_ref().map_or_else(|| self.depth.clone(), |s| s.2.clone());
        let line_buf = (!lines.is_empty()).then(|| self.buffer(bytemuck::cast_slice(lines), wgpu::BufferUsages::VERTEX));
        let blobs = std::mem::take(&mut self.blobs);
        let blob_buf = (!blobs.is_empty()).then(|| (self.buffer(bytemuck::cast_slice(&blobs), wgpu::BufferUsages::VERTEX), blobs.len() as u32));
        let flat_bufs: Vec<(&wgpu::RenderPipeline, wgpu::Buffer, u32)> = particles
            .map(|p| {
                [(&self.particle_alpha, p.flat_alpha), (&self.particle_add, p.flat_add)]
                    .into_iter()
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(pipe, v)| (pipe, self.buffer(bytemuck::cast_slice(v), wgpu::BufferUsages::VERTEX), v.len() as u32))
                    .collect()
            })
            .unwrap_or_default();
        let particle_bufs: Vec<(&wgpu::RenderPipeline, wgpu::Buffer, u32)> = particles
            .map(|p| {
                [(&self.particle_alpha, p.alpha), (&self.particle_add, p.additive)]
                    .into_iter()
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(pipe, v)| (pipe, self.buffer(bytemuck::cast_slice(v), wgpu::BufferUsages::VERTEX), v.len() as u32))
                    .collect()
            })
            .unwrap_or_default();
        let overlay_view = || -> Result<wgpu::TextureView> {
            let desc = wgpu::TextureViewDescriptor { format: Some(self.overlay_format()), ..Default::default() };
            Ok(match &frame {
                Some(f) => f.texture.create_view(&desc),
                None => self.offscreen.as_ref().context("no render target")?.create_view(&desc),
            })
        };
        let mut enc = self.device.create_command_encoder(&Default::default());
        // the stage's fill (sRGB) in the frame's linear colour, else the default
        let fill = cam.background.map_or(self.clear, |c| {
            let lin = |v: u8| {
                let v = v as f64 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            wgpu::Color { r: lin(c[0]), g: lin(c[1]), b: lin(c[2]), a: 1.0 }
        });
        let pass_desc = |label, clear: bool| {
            let colour = if clear { wgpu::LoadOp::Clear(fill) } else { wgpu::LoadOp::Load };
            let depth = if clear { wgpu::LoadOp::Clear(1.0) } else { wgpu::LoadOp::Load };
            (label, colour, depth)
        };
        let begin = |enc: &mut wgpu::CommandEncoder, (label, colour, depth): (&'static str, wgpu::LoadOp<wgpu::Color>, wgpu::LoadOp<f32>)| {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view_tex,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: colour, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations { load: depth, store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime()
        };
        {
            let mut pass = begin(&mut enc, pass_desc("stage", true));
            pass.set_bind_group(0, &self.globals_group, &[]);
            // the drum goes after the floor without depth (the game's OT 1010 vs 1021), or first on the stage
            // stages.json names
            let order = if cam.sky_first { [(&self.sky_pipeline, sky), (&self.static_pipeline, statics)] } else { [(&self.static_pipeline, statics), (&self.sky_pipeline, sky)] };
            for (pipeline, meshes) in order {
                pass.set_pipeline(pipeline);
                for m in meshes {
                    pass.set_bind_group(1, m.texture.group(smooth), &[]);
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..m.count, 0, 0..1);
                    stats.draw_calls += 1;
                    stats.triangles += m.count / 3;
                }
            }
        }
        // the 2D drawn behind the fighters, then the fighters over it
        if let Some(o) = overlay.as_deref_mut() {
            match &scene {
                Some((_, ov, _, size)) => o.draw_under(&self.device, &self.queue, &mut enc, ov, *size, size[0] as f32 / self.config.width as f32),
                None => o.draw_under(&self.device, &self.queue, &mut enc, &overlay_view()?, [self.config.width, self.config.height], 1.0),
            }
        }
        {
            let mut pass = begin(&mut enc, pass_desc("characters", false));
            pass.set_bind_group(0, &self.globals_group, &[]);
            // the shadows first, on the floor under the fighters
            if cam.shadow.is_some_and(|s| s.toward.y > 0.05) {
                pass.set_pipeline(&self.shadow_pipeline);
                for m in models {
                    let Some((vbuf, ibuf, count)) = &m.shadow else { continue };
                    pass.set_bind_group(1, m.texture.group(smooth), &[]);
                    pass.set_bind_group(2, &m.bones_group, &[]);
                    pass.set_vertex_buffer(0, vbuf.slice(..));
                    pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..*count, 0, 0..1);
                    stats.draw_calls += 1;
                }
            }
            if let (Some((buf, n)), Some(white)) = (&blob_buf, self.white.as_ref()) {
                pass.set_pipeline(&self.blob_pipeline);
                pass.set_bind_group(1, &white.bind_group, &[]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..*n, 0..1);
                stats.draw_calls += 1;
            }
            pass.set_pipeline(&self.skinned_pipeline);
            for m in models {
                pass.set_bind_group(1, m.texture.group(smooth), &[]);
                pass.set_bind_group(2, &m.bones_group, &[]);
                pass.set_vertex_buffer(0, m.vbuf.slice(..));
                pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..m.count, 0, 0..1);
                stats.draw_calls += 1;
                stats.triangles += m.count / 3;
            }
            if let (false, Some(white)) = (flat_bufs.is_empty(), self.white.as_ref()) {
                pass.set_bind_group(1, &white.bind_group, &[]);
                for (pipe, buf, n) in &flat_bufs {
                    pass.set_pipeline(pipe);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..*n, 0..1);
                    stats.draw_calls += 1;
                    stats.triangles += n / 3;
                }
            }
            if let Some(p) = particles {
                pass.set_bind_group(1, &p.texture.bind_group, &[]);
                for (pipe, buf, n) in &particle_bufs {
                    pass.set_pipeline(pipe);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..*n, 0..1);
                    stats.draw_calls += 1;
                    stats.triangles += n / 3;
                }
            }
            if let Some(buf) = &line_buf {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..lines.len() as u32, 0..1);
                stats.draw_calls += 1;
                stats.lines = lines.len() as u32 / 2;
            }
        }
        // the scene to the window
        if let (Some(t), Some(_)) = (self.scene.as_ref(), &scene) {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &frame_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(self.clear), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blit_pipeline);
            pass.set_bind_group(0, &t.group, &[]);
            pass.draw(0..3, 0..1);
        }
        if let Some(o) = overlay {
            o.draw(&self.device, &self.queue, &mut enc, &overlay_view()?, [self.config.width, self.config.height]);
        }
        let readback = match (&frame, self.capture_next) {
            (Some(f), true) if self.config.usage.contains(wgpu::TextureUsages::COPY_SRC) => Some(self.copy_out(&mut enc, &f.texture)),
            (Some(_), true) => {
                self.captured = Some(Err(anyhow::anyhow!("this window's frames cannot be read back")));
                None
            }
            _ => None,
        };
        self.capture_next = false;
        self.queue.submit([enc.finish()]);
        // (shown before the readback: a failed capture does not drop the frame)
        if let Some(f) = frame {
            self.queue.present(f);
        }
        if let Some((buf, padded)) = readback {
            self.captured = Some(self.read_back(&buf, padded));
        }
        Ok(stats)
    }

    /// Queue a copy of `tex` into a mappable buffer (rows padded to 256 bytes).
    fn copy_out(&self, enc: &mut wgpu::CommandEncoder, tex: &wgpu::Texture) -> (wgpu::Buffer, u32) {
        let (w, h) = (self.config.width, self.config.height);
        let padded = (4 * w).div_ceil(256) * 256; // rows must be 256-byte aligned for copies
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: Default::default(), aspect: Default::default() },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        (buf, padded)
    }

    /// Map a buffer from [`Renderer::copy_out`] and unpack it to RGBA8 (BGRA surfaces are swizzled).
    fn read_back(&self, buf: &wgpu::Buffer, padded: u32) -> Result<Image> {
        let (w, h) = (self.config.width, self.config.height);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        let data = slice.get_mapped_range().map_err(|e| anyhow::anyhow!("map: {e:?}"))?;
        let mut rgba = Vec::with_capacity((4 * w * h) as usize);
        for row in 0..h as usize {
            rgba.extend_from_slice(&data[row * padded as usize..row * padded as usize + 4 * w as usize]);
        }
        if matches!(self.config.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) {
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        Ok(Image { width: w, height: h, rgba })
    }

    /// Read back the last offscreen frame as tightly packed RGBA8 (headless mode only).
    pub fn capture(&self) -> Result<Image> {
        let tex = self.offscreen.as_ref().context("capture needs a headless renderer")?;
        let mut enc = self.device.create_command_encoder(&Default::default());
        let (buf, padded) = self.copy_out(&mut enc, tex);
        self.queue.submit([enc.finish()]);
        self.read_back(&buf, padded)
    }
}

fn make_offscreen(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen"),
        size: wgpu::Extent3d { width: config.width, height: config.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: config.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &config.view_formats,
    })
}

fn make_depth(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d { width: config.width, height: config.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}
