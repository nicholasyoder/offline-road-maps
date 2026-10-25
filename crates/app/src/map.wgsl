// The map: polygon fills, and lines widened to screen-width quads with round ends.
// Positions are tile-local (4096 per tile edge); `tile` places them in viewport pixels.

struct Globals {
    // Viewport size in pixels.
    size: vec2<f32>,
    _pad: vec2<f32>,
}

struct Paint {
    color: vec4<f32>,
    casing: vec4<f32>,
    // Line width and casing width, in pixels.
    width: vec2<f32>,
    // Dash and gap, in pixels: for the line, then for the casing. Zero for solid.
    dash: vec2<f32>,
    casing_dash: vec2<f32>,
    _pad: vec2<f32>,
}

struct TileTransform {
    origin: vec2<f32>,
    // Pixels per tile unit.
    scale: f32,
    _pad: f32,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<uniform> paints: array<Paint, 128>;
@group(1) @binding(0) var<uniform> tile: TileTransform;

fn to_clip(px: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(px.x / globals.size.x * 2.0 - 1.0, 1.0 - px.y / globals.size.y * 2.0, 0.0, 1.0);
}

fn premultiply(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

struct FillOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_fill(@location(0) p: vec2<i32>, @location(1) s: vec2<u32>) -> FillOut {
    var out: FillOut;
    out.pos = to_clip(tile.origin + vec2<f32>(p) * tile.scale);
    out.color = premultiply(paints[s.x].color);
    return out;
}

@fragment
fn fs_fill(in: FillOut) -> @location(0) vec4<f32> {
    return in.color;
}

struct LineOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Pixels along the segment from `a`, and across it from its middle.
    @location(1) local: vec2<f32>,
    // Segment length, half width, dash, gap.
    @location(2) shape: vec4<f32>,
    // Pixels along the whole line to `a`.
    @location(3) dist: f32,
}

fn line(vi: u32, a: vec2<i32>, b: vec2<i32>, s: u32, dist: f32, casing: bool) -> LineOut {
    let paint = paints[s];
    var w = paint.width.x;
    var color = paint.color;
    var dash = paint.dash;
    if casing {
        w = paint.width.y;
        color = paint.casing;
        dash = paint.casing_dash;
    }
    let pa = tile.origin + vec2<f32>(a) * tile.scale;
    let pb = tile.origin + vec2<f32>(b) * tile.scale;
    let d = pb - pa;
    let len = length(d);
    var dir = vec2<f32>(1.0, 0.0);
    if len > 1e-6 {
        dir = d / len;
    }
    let n = vec2<f32>(-dir.y, dir.x);
    // Lines thinner than a pixel are drawn a pixel wide. Not fainter: segments overlap at
    // their round ends, and a translucent line would darken at every vertex.
    let r = max(w, 1.0) * 0.5;
    // Half a pixel more on every side, for the antialiased edge; nothing at zero width.
    let h = select(r + 1.0, 0.0, w <= 0.0);
    // Two triangles: corners at the start (0) or end (1), on the right (-1) or left (1).
    let end = (22u >> vi) & 1u;
    let side = f32((52u >> vi) & 1u) * 2.0 - 1.0;
    let t = select(-h, len + h, end == 1u);
    let px = pa + dir * t + n * (side * h);
    var out: LineOut;
    out.pos = to_clip(px);
    out.color = premultiply(color);
    out.local = vec2<f32>(t, side * h);
    out.shape = vec4<f32>(len, r, dash);
    out.dist = dist * tile.scale;
    return out;
}

@vertex
fn vs_line(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<i32>,
    @location(1) b: vec2<i32>,
    @location(2) s: vec2<u32>,
    @location(3) dist: f32,
) -> LineOut {
    return line(vi, a, b, s.x, dist, false);
}

@vertex
fn vs_casing(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<i32>,
    @location(1) b: vec2<i32>,
    @location(2) s: vec2<u32>,
    @location(3) dist: f32,
) -> LineOut {
    return line(vi, a, b, s.x, dist, true);
}

@fragment
fn fs_line(in: LineOut) -> @location(0) vec4<f32> {
    let t = in.local.x;
    let len = in.shape.x;
    // Distance to the segment: round ends, and so round joins where segments meet.
    let d = length(vec2<f32>(t - clamp(t, 0.0, len), in.local.y));
    let cover = clamp(in.shape.y - d + 0.5, 0.0, 1.0);
    if cover <= 0.0 {
        discard;
    }
    let period = in.shape.z + in.shape.w;
    if in.shape.z > 0.0 {
        let along = in.dist + t;
        if along - floor(along / period) * period > in.shape.z {
            discard;
        }
    }
    return in.color * cover;
}
