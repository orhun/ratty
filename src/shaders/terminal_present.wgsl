// Presents the terminal texture 1:1 with physical pixels.
//
// A fullscreen (clip-space) quad covers the viewport; each fragment samples the
// terminal texture by its own physical pixel coordinate via `textureLoad`, so
// every texel maps to exactly one screen pixel with no resampling — the crisp
// presentation pattern from linebender/bevy_vello, specialized to a centered,
// pixel-aligned sub-rect (the terminal grid does not always fill the window).
#import bevy_render::view::View
#import bevy_sprite::mesh2d_vertex_output::VertexOutput

@group(0) @binding(0) var<uniform> view: View;
@group(2) @binding(0) var terminal_texture: texture_2d<f32>;

struct Vertex {
    @location(0) position: vec3<f32>,
};

@vertex
fn vertex(in: Vertex) -> VertexOutput {
    var out: VertexOutput;
    // `position` is already in clip space (the quad spans the whole viewport).
    out.position = vec4<f32>(in.position, 1.0);
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_size = vec2<f32>(textureDimensions(terminal_texture));
    // Normally center the texture in the viewport. But when it is taller than
    // the viewport (only at the clamped 2-row minimum, when the window is too
    // short to show both rows), anchor to the bottom edge instead so the last
    // terminal row stays visible and the top row clips first. Snapped to a whole
    // physical pixel.
    let origin_x = view.viewport.x + floor((view.viewport.z - tex_size.x) * 0.5);
    let centered_y = view.viewport.y + floor((view.viewport.w - tex_size.y) * 0.5);
    let bottom_y = view.viewport.y + view.viewport.w - tex_size.y;
    let origin_y = select(centered_y, bottom_y, tex_size.y > view.viewport.w);
    let origin = vec2<f32>(origin_x, floor(origin_y));
    let p = in.position.xy - origin;
    if (p.x < 0.0 || p.y < 0.0 || p.x >= tex_size.x || p.y >= tex_size.y) {
        // Outside the terminal: transparent, so the camera clear shows through.
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    // Exact texel fetch (the texture's sRGB view decodes to linear on load).
    return textureLoad(terminal_texture, vec2<i32>(p), 0);
}
