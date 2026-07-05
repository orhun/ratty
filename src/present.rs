//! 1:1 presentation of the terminal texture for flat 2D mode.
//!
//! Instead of drawing the terminal texture on a world-positioned sprite (whose
//! interpolated UVs resample the texture and make text blurry/jagged unless the
//! mapping happens to be exactly 1:1), this presents it with a fullscreen quad
//! whose fragment shader fetches each texel by physical pixel coordinate
//! (`textureLoad`). That is an identity sample — crisp at every font size and
//! DPI — modeled on linebender/bevy_vello's render-target present.
//!
//! The 3D plane path is unchanged; it still samples the texture as a material on
//! transformable geometry, where sampling is unavoidable.

use bevy::asset::{load_internal_asset, uuid_handle};
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology, VertexBufferLayout};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendState, RenderPipelineDescriptor, SpecializedMeshPipelineError, VertexFormat,
    VertexStepMode,
};
use bevy::shader::{Shader, ShaderRef};
use bevy::sprite_render::{Material2d, Material2dKey, Material2dPlugin};

/// Handle for the embedded terminal-present shader.
const TERMINAL_PRESENT_SHADER: Handle<Shader> =
    uuid_handle!("7e3b1c2a-5d6f-4a8b-9c0d-1e2f3a4b5c6d");

/// Material that presents the terminal texture 1:1 with physical pixels.
///
/// Sampled via `textureLoad` in the shader, so no sampler binding is needed.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct TerminalPresentMaterial {
    /// The terminal present texture (an `Rgba8Unorm` texture with an sRGB view).
    #[texture(0)]
    pub texture: Handle<Image>,
}

impl Material2d for TerminalPresentMaterial {
    fn vertex_shader() -> ShaderRef {
        TERMINAL_PRESENT_SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        TERMINAL_PRESENT_SHADER.into()
    }

    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Alpha-blend so the transparent area outside the terminal lets the
        // camera clear color show through.
        if let Some(fragment) = descriptor.fragment.as_mut()
            && let Some(Some(target)) = fragment.targets.first_mut()
        {
            target.blend = Some(BlendState::ALPHA_BLENDING);
        }
        // The present quad carries only clip-space positions.
        descriptor.vertex.buffers = vec![VertexBufferLayout::from_vertex_formats(
            VertexStepMode::Vertex,
            [VertexFormat::Float32x3],
        )];
        Ok(())
    }
}

/// A clip-space quad covering the whole viewport (positions only).
pub fn fullscreen_quad() -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::default(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![[-1.0, -1.0, 0.0], [3.0, -1.0, 0.0], [-1.0, 3.0, 0.0]],
    );
    mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
    mesh
}

/// Registers the terminal-present shader and material.
pub struct TerminalPresentPlugin;

impl Plugin for TerminalPresentPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            TERMINAL_PRESENT_SHADER,
            "shaders/terminal_present.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins(Material2dPlugin::<TerminalPresentMaterial>::default());
    }
}
