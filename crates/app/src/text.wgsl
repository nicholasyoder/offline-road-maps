// Label glyphs from the signed distance field atlas (text.rs): one instanced quad per glyph,
// drawn twice, first every halo and then every glyph, so no halo covers a letter.

struct Globals {
    // Viewport size in pixels.
    size: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

// text.rs: SDF_EDGE and SDF_RANGE.
const EDGE: f32 = 0.75;
const RANGE: f32 = 8.0;

struct GlyphOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    // Pixels per atlas pixel, and the halo width in pixels (0 for the glyph itself).
    @location(2) scale: vec2<f32>,
}

fn glyph(
    vi: u32,
    center: vec2<f32>,
    axis: vec2<f32>,
    half: vec2<f32>,
    uv: vec4<f32>,
    color: vec4<f32>,
    scale: vec2<f32>,
) -> GlyphOut {
    // Two triangles: corners (0,0) (1,0) (1,1), (0,0) (1,1) (0,1).
    let corner = vec2<f32>(f32((22u >> vi) & 1u), f32((52u >> vi) & 1u));
    let local = (corner * 2.0 - 1.0) * half;
    let px = center + axis * local.x + vec2<f32>(-axis.y, axis.x) * local.y;
    var out: GlyphOut;
    out.pos = vec4<f32>(px.x / globals.size.x * 2.0 - 1.0, 1.0 - px.y / globals.size.y * 2.0, 0.0, 1.0);
    out.uv = mix(uv.xy, uv.zw, corner);
    out.color = vec4<f32>(color.rgb * color.a, color.a);
    out.scale = scale;
    return out;
}

@vertex
fn vs_glyph(
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2<f32>,
    @location(1) axis: vec2<f32>,
    @location(2) half: vec2<f32>,
    @location(3) uv: vec4<f32>,
    @location(4) color: vec4<f32>,
    @location(5) halo: vec4<f32>,
    @location(6) scale: vec2<f32>,
) -> GlyphOut {
    return glyph(vi, center, axis, half, uv, color, vec2<f32>(scale.x, 0.0));
}

@vertex
fn vs_halo(
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2<f32>,
    @location(1) axis: vec2<f32>,
    @location(2) half: vec2<f32>,
    @location(3) uv: vec4<f32>,
    @location(4) color: vec4<f32>,
    @location(5) halo: vec4<f32>,
    @location(6) scale: vec2<f32>,
) -> GlyphOut {
    // The field reaches EDGE · RANGE atlas pixels out; keep the halo's edge inside it.
    let width = min(scale.y, (EDGE * RANGE - 0.5) * scale.x);
    return glyph(vi, center, axis, half, uv, halo, vec2<f32>(scale.x, width));
}

@fragment
fn fs_glyph(in: GlyphOut) -> @location(0) vec4<f32> {
    let v = textureSampleLevel(atlas, atlas_sampler, in.uv, 0.0).r;
    // Distance outside the outline, in pixels; the edge is antialiased over one pixel.
    let d = (EDGE - v) * RANGE * in.scale.x - in.scale.y;
    let cover = clamp(0.5 - d, 0.0, 1.0);
    if cover <= 0.0 {
        discard;
    }
    return in.color * cover;
}
