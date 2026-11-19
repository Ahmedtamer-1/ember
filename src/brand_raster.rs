// Procedural rasterizer for the Ember mark ("orbiting ember": a tapered comet trail
// circling a glowing core, ending in a bright spark).
//
// This file is shared verbatim by `build.rs` (exe icon) and `src/brand.rs` (window icon)
// through `include!`, so it must only use `std` and contain no inner attributes or docs.
// The geometry mirrors `assets/logo.svg` (256x256 design space); keep them in sync.

/// Center of the orbit and of the glowing core.
const MARK_CENTER: (f32, f32) = (134.0, 131.0);
/// Radius of the orbit (trail centerline).
const MARK_ORBIT_RADIUS: f32 = 84.0;
/// Angle of the spark (trail head) in degrees, clockwise from +x (SVG/y-down convention).
const MARK_HEAD_DEG: f32 = -40.0;
/// How far the trail sweeps behind the spark, in degrees.
const MARK_SWEEP_DEG: f32 = 290.0;
/// Trail width at the tail and at the head.
const MARK_TAIL_WIDTH: f32 = 11.0;
const MARK_HEAD_WIDTH: f32 = 44.0;
/// Radii of the spark and of the core.
const MARK_SPARK_RADIUS: f32 = 29.0;
const MARK_CORE_RADIUS: f32 = 33.0;
/// Trail gradient axis (bottom-left magenta to top-right orange).
const MARK_TRAIL_AXIS: [f32; 4] = [46.0, 219.0, 222.0, 43.0];
const MARK_TRAIL_STOPS: [(f32, [u8; 3]); 3] = [
    (0.0, [0xC2, 0x18, 0x7F]),
    (0.55, [0xFF, 0x3D, 0x5E]),
    (1.0, [0xFF, 0x9A, 0x1F]),
];
const MARK_CORE_STOPS: [(f32, [u8; 3]); 3] = [
    (0.0, [0xFF, 0xF7, 0xDB]),
    (0.5, [0xFF, 0xD0, 0x45]),
    (1.0, [0xFF, 0x8F, 0x1F]),
];
const MARK_SPARK_STOPS: [(f32, [u8; 3]); 3] = [
    (0.0, [0xFF, 0xF7, 0xDB]),
    (0.5, [0xFF, 0xD0, 0x45]),
    (1.0, [0xFF, 0x9F, 0x1C]),
];
/// Supersampling factor per axis (on top of SDF anti-aliasing).
const MARK_SUPERSAMPLE: u32 = 3;

fn mark_gradient(stops: &[(f32, [u8; 3])], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    let mut prev = stops[0];
    for &stop in stops {
        if t <= stop.0 {
            let span = stop.0 - prev.0;
            let k = if span > 0.0 { (t - prev.0) / span } else { 0.0 };
            let mut out = [0.0; 3];
            for (i, o) in out.iter_mut().enumerate() {
                let a = f32::from(prev.1[i]);
                let b = f32::from(stop.1[i]);
                *o = (a + (b - a) * k) / 255.0;
            }
            return out;
        }
        prev = stop;
    }
    let last = prev.1;
    [
        f32::from(last[0]) / 255.0,
        f32::from(last[1]) / 255.0,
        f32::from(last[2]) / 255.0,
    ]
}

fn mark_point_on_orbit(deg: f32) -> (f32, f32) {
    let a = deg.to_radians();
    (
        MARK_CENTER.0 + MARK_ORBIT_RADIUS * a.cos(),
        MARK_CENTER.1 + MARK_ORBIT_RADIUS * a.sin(),
    )
}

fn mark_dist(x: f32, y: f32, p: (f32, f32)) -> f32 {
    ((x - p.0) * (x - p.0) + (y - p.1) * (y - p.1)).sqrt()
}

/// Signed distance to the tapered trail (negative inside).
fn mark_trail_sdf(x: f32, y: f32) -> f32 {
    let dx = x - MARK_CENTER.0;
    let dy = y - MARK_CENTER.1;
    let r = (dx * dx + dy * dy).sqrt();
    let tail_deg = MARK_HEAD_DEG - MARK_SWEEP_DEG;
    let along = (dy.atan2(dx).to_degrees() - tail_deg).rem_euclid(360.0);
    if along <= MARK_SWEEP_DEG {
        let t = along / MARK_SWEEP_DEG;
        let width = MARK_TAIL_WIDTH + (MARK_HEAD_WIDTH - MARK_TAIL_WIDTH) * t.powf(0.9);
        (r - MARK_ORBIT_RADIUS).abs() - width / 2.0
    } else {
        let tail = mark_dist(x, y, mark_point_on_orbit(tail_deg)) - MARK_TAIL_WIDTH / 2.0;
        let head = mark_dist(x, y, mark_point_on_orbit(MARK_HEAD_DEG)) - MARK_HEAD_WIDTH / 2.0;
        tail.min(head)
    }
}

/// Composites premultiplied `rgb` with coverage `a` over the premultiplied accumulator.
fn mark_over(acc: &mut [f32; 4], rgb: [f32; 3], a: f32) {
    if a <= 0.0 {
        return;
    }
    let inv = 1.0 - a;
    acc[0] = rgb[0] * a + acc[0] * inv;
    acc[1] = rgb[1] * a + acc[1] * inv;
    acc[2] = rgb[2] * a + acc[2] * inv;
    acc[3] = a + acc[3] * inv;
}

/// Shades one sample in design space; `aa` is the sample footprint in design units.
fn mark_sample(x: f32, y: f32, aa: f32) -> [f32; 4] {
    let cover = |d: f32| (0.5 - d / aa).clamp(0.0, 1.0);
    let mut acc = [0.0f32; 4];

    let trail = cover(mark_trail_sdf(x, y));
    if trail > 0.0 {
        let [x0, y0, x1, y1] = MARK_TRAIL_AXIS;
        let (ax, ay) = (x1 - x0, y1 - y0);
        let t = ((x - x0) * ax + (y - y0) * ay) / (ax * ax + ay * ay);
        let rgb = mark_gradient(&MARK_TRAIL_STOPS, t);
        mark_over(&mut acc, rgb, trail);
    }

    let core_d = mark_dist(x, y, MARK_CENTER);
    let core = cover(core_d - MARK_CORE_RADIUS);
    if core > 0.0 {
        let rgb = mark_gradient(&MARK_CORE_STOPS, core_d / MARK_CORE_RADIUS);
        mark_over(&mut acc, rgb, core);
    }

    let spark_d = mark_dist(x, y, mark_point_on_orbit(MARK_HEAD_DEG));
    let spark = cover(spark_d - MARK_SPARK_RADIUS);
    if spark > 0.0 {
        let rgb = mark_gradient(&MARK_SPARK_STOPS, spark_d / MARK_SPARK_RADIUS);
        mark_over(&mut acc, rgb, spark);
    }
    acc
}

fn mark_render(size: u32) -> Vec<u8> {
    let size = size.max(1);
    let n = MARK_SUPERSAMPLE;
    let scale = 256.0 / size as f32;
    let aa = scale / n as f32;
    let samples = (n * n) as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut sum = [0.0f32; 4];
            for sy in 0..n {
                for sx in 0..n {
                    let x = (px as f32 + (sx as f32 + 0.5) / n as f32) * scale;
                    let y = (py as f32 + (sy as f32 + 0.5) / n as f32) * scale;
                    let s = mark_sample(x, y, aa);
                    for (acc, v) in sum.iter_mut().zip(s) {
                        *acc += v;
                    }
                }
            }
            let a = sum[3] / samples;
            let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            if a > 0.0 {
                out.push(to_u8(sum[0] / samples / a));
                out.push(to_u8(sum[1] / samples / a));
                out.push(to_u8(sum[2] / samples / a));
                out.push(to_u8(a));
            } else {
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    out
}

/// Renders the full-color Ember mark as a `size` x `size` straight-alpha RGBA8 buffer.
pub fn render_logo_rgba(size: u32) -> Vec<u8> {
    mark_render(size)
}

