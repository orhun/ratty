//! Direct Vello-to-Bevy texture rendering for the terminal surface.

use std::sync::{Arc, Mutex};

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    CommandEncoderDescriptor, Extent3d, TextureDimension, TextureFormat, TextureUsages,
    TextureViewDescriptor,
};
use bevy::render::renderer::{RenderDevice, RenderGraph, RenderGraphSystems, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::render::{Extract, ExtractSchedule, RenderApp};
use parley_ratatui::ratatui::buffer::Buffer;
use parley_ratatui::ratatui::layout::Position;
use parley_ratatui::vello::Scene;
use parley_ratatui::vello::peniko::Color as PenikoColor;
use parley_ratatui::{GpuRenderer, TerminalRenderer};

/// Plugin that renders terminal Vello scenes directly into Bevy GPU textures.
pub(crate) struct DirectTerminalRenderPlugin;

impl Plugin for DirectTerminalRenderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DirectTerminalSceneExchange>();
        let exchange = app
            .world()
            .resource::<DirectTerminalSceneExchange>()
            .clone();

        let render_app = app.sub_app_mut(RenderApp);
        render_app.init_resource::<DirectTerminalRenderState>();
        render_app.world_mut().insert_resource(exchange);
        render_app.init_resource::<ExtractedDirectTerminalFrame>();
        render_app.add_systems(ExtractSchedule, extract_terminal_frame);
        // Render inside the render-graph schedule so Vello's submission is
        // ordered with the frame's GPU work: `Begin` runs before the camera
        // passes that sample the terminal texture.
        render_app.add_systems(
            RenderGraph,
            render_terminal_frame.in_set(RenderGraphSystems::Begin),
        );
    }
}

/// The pair of textures a terminal frame is rendered through: Vello rasterizes
/// into `render` (a plain `Rgba8Unorm` storage texture) and it is copied into
/// `present` (sampled via an sRGB view) for the materials to sample.
pub(crate) struct TerminalImages {
    pub render: Handle<Image>,
    pub present: Handle<Image>,
}

struct DirectTerminalFrame {
    /// Plain `Rgba8Unorm` storage texture Vello rasterizes into.
    render_image: Handle<Image>,
    /// `Rgba8Unorm` texture (sampled via an sRGB view) the render image is
    /// copied into for the materials to sample.
    present_image: Handle<Image>,
    width: u32,
    height: u32,
    base_color: PenikoColor,
    scene: Scene,
}

/// Shared bounded exchange between the main world and Bevy render world.
#[derive(Resource, Clone, Default)]
pub(crate) struct DirectTerminalSceneExchange {
    inner: Arc<DirectTerminalSceneExchangeInner>,
}

#[derive(Default)]
struct DirectTerminalSceneExchangeInner {
    pending: Mutex<Option<DirectTerminalFrame>>,
    recycled: Mutex<Option<Scene>>,
}

/// Render-world Vello renderer state.
///
/// `vello::Renderer` is `Send` but not `Sync`, so the renderer lives in a
/// [`SyncCell`] to qualify as a regular [`Resource`]. A non-send resource
/// would pin [`render_terminal_frame`] to a specific thread, which deadlocks
/// the pipelined render app on its final update during shutdown.
#[derive(Resource)]
struct DirectTerminalRenderState {
    renderer: SyncCell<Option<GpuRenderer>>,
}

impl Default for DirectTerminalRenderState {
    fn default() -> Self {
        Self {
            renderer: SyncCell::new(None),
        }
    }
}

#[derive(Resource, Default)]
struct ExtractedDirectTerminalFrame(Option<DirectTerminalFrame>);

impl DirectTerminalSceneExchange {
    fn take_recycled_scene(&self) -> Scene {
        self.inner
            .recycled
            .lock()
            .expect("direct terminal recycled scene lock")
            .take()
            .unwrap_or_default()
    }

    fn recycle_scene(&self, mut scene: Scene) {
        scene.reset();

        let mut recycled = self
            .inner
            .recycled
            .lock()
            .expect("direct terminal recycled scene lock");
        if recycled.is_none() {
            *recycled = Some(scene);
        }
    }

    fn publish_frame(&self, frame: DirectTerminalFrame) {
        let previous = self
            .inner
            .pending
            .lock()
            .expect("direct terminal pending frame lock")
            .replace(frame);

        if let Some(previous) = previous {
            self.recycle_scene(previous.scene);
        }
    }

    fn take_pending_frame(&self) -> Option<DirectTerminalFrame> {
        self.inner
            .pending
            .lock()
            .expect("direct terminal pending frame lock")
            .take()
    }
}

/// Creates the terminal **present** texture that the plane material and sprite
/// sample.
///
/// Vello renders into the separate [`new_terminal_render_image`] storage
/// texture and writes sRGB-encoded, display-ready bytes; those bytes are copied
/// here each frame (same `Rgba8Unorm` format) and sampled through an
/// `Rgba8UnormSrgb` view so they are decoded on sample instead of being
/// re-encoded at the swapchain (which washes out colors). This texture has no
/// `STORAGE_BINDING`, so the sRGB view is valid on every backend — wgpu rejects
/// an sRGB view of a storage texture. The data is zero-filled so a frame
/// sampled before the first copy shows transparent black rather than
/// uninitialized memory.
pub(crate) fn new_terminal_image(width: u32, height: u32, label: &'static str) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.texture_descriptor.label = Some(label);
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    image.texture_descriptor.view_formats = &[TextureFormat::Rgba8UnormSrgb];
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        format: Some(TextureFormat::Rgba8UnormSrgb),
        ..Default::default()
    });
    // Nearest (point) filtering: the texture is authored at physical resolution
    // and the flat 2D view presents it 1:1 via `textureLoad` (no sampler), so the
    // 3D plane modes use point sampling too for a consistent, crisp pixel grid.
    // Cell-edge seams are prevented at authoring time in parley_ratatui
    // (pixel-snapped cell fills), not by linear blending.
    image.sampler = ImageSampler::nearest();
    image
}

/// Creates the terminal **render** texture that Vello rasterizes into.
///
/// Vello binds it as a compute storage target, so it must be a plain
/// `Rgba8Unorm` texture with no sRGB view: wgpu rejects an sRGB view of a
/// storage texture (`STORAGE_BINDING`), which crashes bind-group creation on
/// strict backends. Its sRGB-encoded contents are copied into the
/// [`new_terminal_image`] present texture for sampling, so this texture is only
/// ever a copy source and is never sampled directly.
pub(crate) fn new_terminal_render_image(width: u32, height: u32, label: &'static str) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.texture_descriptor.label = Some(label);
    image.texture_descriptor.usage =
        TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC | TextureUsages::COPY_DST;
    image
}

pub(crate) fn resize_terminal_image(image: &mut Image, width: u32, height: u32) {
    if image.texture_descriptor.size.width == width
        && image.texture_descriptor.size.height == height
    {
        return;
    }

    image.resize(Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    });
}

pub(crate) fn update_direct_terminal_frame(
    exchange: &DirectTerminalSceneExchange,
    images: TerminalImages,
    terminal_renderer: &mut TerminalRenderer,
    buffer: &Buffer,
    cursor: Option<Position>,
    cursor_visible: bool,
    elapsed_seconds: f32,
) {
    let build_scene = exchange.take_recycled_scene();
    let spare_scene = terminal_renderer.replace_scene(build_scene);
    let (width, height) = terminal_renderer.texture_size_for_buffer(buffer);
    let base_color = terminal_renderer.theme().background.to_peniko();
    terminal_renderer.build_scene_with_elapsed(buffer, cursor, cursor_visible, elapsed_seconds);
    let scene = terminal_renderer.replace_scene(spare_scene);

    exchange.publish_frame(DirectTerminalFrame {
        render_image: images.render,
        present_image: images.present,
        width,
        height,
        base_color,
        scene,
    });
}

fn extract_terminal_frame(
    mut frame: ResMut<ExtractedDirectTerminalFrame>,
    exchange: Extract<Res<DirectTerminalSceneExchange>>,
) {
    if let Some(next_frame) = exchange.take_pending_frame()
        && let Some(previous_frame) = frame.0.replace(next_frame)
    {
        exchange.recycle_scene(previous_frame.scene);
    }
}

fn render_terminal_frame(
    mut state: ResMut<DirectTerminalRenderState>,
    exchange: Res<DirectTerminalSceneExchange>,
    mut frame: ResMut<ExtractedDirectTerminalFrame>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
) {
    let Some(current) = frame.0.take() else {
        return;
    };
    // Retain undrawable frames and retry on the next render frame; a newer
    // published frame supersedes a retained one during extraction.
    let (Some(render_image), Some(present_image)) = (
        gpu_images.get(&current.render_image),
        gpu_images.get(&current.present_image),
    ) else {
        debug!("retaining terminal frame: GPU image not yet prepared");
        frame.0 = Some(current);
        return;
    };

    let render_size = render_image.texture_descriptor.size;
    let present_size = present_image.texture_descriptor.size;
    if render_size.width != current.width
        || render_size.height != current.height
        || present_size.width != current.width
        || present_size.height != current.height
    {
        debug!(
            "retaining terminal frame: render {}x{}, present {}x{}, frame {}x{}",
            render_size.width,
            render_size.height,
            present_size.width,
            present_size.height,
            current.width,
            current.height
        );
        frame.0 = Some(current);
        return;
    }

    let device = render_device.wgpu_device();
    let renderer = state
        .renderer
        .get()
        .get_or_insert_with(|| GpuRenderer::new(device).expect("vello renderer"));

    // Vello rasterizes into the plain `Rgba8Unorm` storage texture; its default
    // view is storage-compatible (no sRGB reinterpretation).
    renderer
        .render_scene_to_texture_view(
            device,
            &render_queue,
            &render_image.texture_view,
            current.width,
            current.height,
            current.base_color,
            &current.scene,
        )
        .expect("render terminal scene into Bevy texture");

    // Copy the rendered, sRGB-encoded bytes into the present texture, which the
    // materials sample through an `Rgba8UnormSrgb` view to decode them. Both
    // textures are `Rgba8Unorm`, so this is a plain same-format texel copy.
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("terminal present copy"),
    });
    encoder.copy_texture_to_texture(
        render_image.texture.as_image_copy(),
        present_image.texture.as_image_copy(),
        Extent3d {
            width: current.width,
            height: current.height,
            depth_or_array_layers: 1,
        },
    );
    render_queue.submit([encoder.finish()]);

    exchange.recycle_scene(current.scene);
}
