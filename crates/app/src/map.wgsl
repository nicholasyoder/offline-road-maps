// The map: polygon fills, and lines widened to screen-width quads with round ends.
// Positions are tile-local (4096 per tile edge); `tile` places them in viewport pixels.
// The route is drawn with the same lines, from float positions local to a piece of it,
// which `tile` places the same way, in the paint `tile` names (one of the route's, after the
// style's). Pushpins are quads round points placed the same way, `tile.paint` holding their
// half size in pixels, each shape drawn from its distance with its paint's colour and a
// white edge of its line width.

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
    // The route's pieces: their paint (style.rs, ROUTE_PAINTS).
    paint: f32,
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

// A segment from `pa` to `pb`, in viewport pixels.
fn line(vi: u32, pa: vec2<f32>, pb: vec2<f32>, s: u32, dist: f32, casing: bool) -> LineOut {
    let paint = paints[s];
    var w = paint.width.x;
    var color = paint.color;
    var dash = paint.dash;
    if casing {
        w = paint.width.y;
        color = paint.casing;
        dash = paint.casing_dash;
    }
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

fn place(p: vec2<i32>) -> vec2<f32> {
    return tile.origin + vec2<f32>(p) * tile.scale;
}

@vertex
fn vs_line(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<i32>,
    @location(1) b: vec2<i32>,
    @location(2) s: vec2<u32>,
    @location(3) dist: f32,
) -> LineOut {
    return line(vi, place(a), place(b), s.x, dist, false);
}

@vertex
fn vs_casing(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<i32>,
    @location(1) b: vec2<i32>,
    @location(2) s: vec2<u32>,
    @location(3) dist: f32,
) -> LineOut {
    return line(vi, place(a), place(b), s.x, dist, true);
}

@vertex
fn vs_route(@builtin(vertex_index) vi: u32, @location(0) a: vec2<f32>, @location(1) b: vec2<f32>) -> LineOut {
    return line(vi, tile.origin + a * tile.scale, tile.origin + b * tile.scale, u32(tile.paint), 0.0, false);
}

@vertex
fn vs_route_halo(@builtin(vertex_index) vi: u32, @location(0) a: vec2<f32>, @location(1) b: vec2<f32>) -> LineOut {
    return line(vi, tile.origin + a * tile.scale, tile.origin + b * tile.scale, u32(tile.paint), 0.0, true);
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

struct PinOut {
    @builtin(position) pos: vec4<f32>,
    // Pixels from the pin's centre.
    @location(0) local: vec2<f32>,
    @location(1) fill: vec4<f32>,
    @location(2) edge: vec4<f32>,
    // Half size and edge width in pixels, and the shape.
    @location(3) @interpolate(flat) size: vec2<f32>,
    @location(4) @interpolate(flat) shape: u32,
}

@vertex
fn vs_pin(@builtin(vertex_index) vi: u32, @location(0) p: vec2<f32>, @location(1) look: vec2<u32>) -> PinOut {
    // Two triangles: corners (0,0) (1,0) (1,1), (0,0) (1,1) (0,1).
    let corner = vec2<f32>(f32((22u >> vi) & 1u), f32((52u >> vi) & 1u));
    let half = tile.paint;
    // Room for the diamond's and triangle's points and the antialiased edge.
    let local = (corner * 2.0 - 1.0) * (half * 1.2 + 1.0);
    let paint = paints[look.x];
    var out: PinOut;
    out.pos = to_clip(tile.origin + p * tile.scale + local);
    out.local = local;
    out.fill = premultiply(paint.color);
    out.edge = premultiply(paint.casing);
    out.size = vec2<f32>(half, paint.width.x);
    out.shape = look.y;
    return out;
}

// pins.rs, Shape: circle, square, diamond, triangle (pointing up; y is down on screen).
fn pin_distance(p: vec2<f32>, r: f32, shape: u32) -> f32 {
    switch shape {
        case 1u: {
            let q = abs(p) - vec2<f32>(r * 0.85);
            return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
        }
        case 2u: {
            return (abs(p.x) + abs(p.y) - r * 1.15) * 0.70710678;
        }
        case 3u: {
            // Inigo Quilez's equilateral triangle, side 2a, centred on its centroid.
            let k = sqrt(3.0);
            let a = r * 1.15 * 0.95;
            var q = vec2<f32>(abs(p.x) - a, -(p.y - r * 0.2) + a / k);
            if q.x + k * q.y > 0.0 {
                q = vec2<f32>(q.x - k * q.y, -k * q.x - q.y) / 2.0;
            }
            q.x -= clamp(q.x, -2.0 * a, 0.0);
            return -length(q) * sign(q.y);
        }
        default: {
            return length(p) - r;
        }
    }
}

@fragment
fn fs_pin(in: PinOut) -> @location(0) vec4<f32> {
    let d = pin_distance(in.local, in.size.x, in.shape);
    let outer = clamp(0.5 - d, 0.0, 1.0);
    if outer <= 0.0 {
        discard;
    }
    let inner = clamp(0.5 - (d + in.size.y), 0.0, 1.0);
    return mix(in.edge, in.fill, inner) * outer;
}
