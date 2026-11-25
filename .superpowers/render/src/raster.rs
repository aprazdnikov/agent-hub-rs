//! Pure CPU triangle rasterizer for egui tessellated output.
//!
//! Color model verified against `egui_glow` 0.36.2: vertex colors are sRGBA with
//! premultiplied alpha (`epaint::mesh::Vertex`), the light-mode font atlas stores
//! premultiplied glyph coverage (`FontColorTransferFunction::Off` keeps the
//! rasterizer's premultiplied white), the fragment shader multiplies vertex and
//! texel in gamma space, and the blend state is `ONE, ONE_MINUS_SRC_ALPHA`
//! (premultiplied src-over). No linearization happens anywhere.

use anyhow::anyhow;
use epaint::{ClippedPrimitive, ColorImage, Mesh, Primitive, TextureId, Vertex};

pub struct Framebuffer {
    width: usize,
    height: usize,
    /// Premultiplied sRGBA, f32 in 0..=1.
    px: Vec<[f32; 4]>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize, background: epaint::Color32) -> Self {
        let fill = [
            f32::from(background.r()) / 255.0,
            f32::from(background.g()) / 255.0,
            f32::from(background.b()) / 255.0,
            f32::from(background.a()) / 255.0,
        ];
        Self { width, height, px: vec![fill; width * height] }
    }

    /// Unpremultiply and encode as opaque RGB8.
    pub fn into_rgb8(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 3);
        for p in &self.px {
            let a = p[3].clamp(0.0, 1.0);
            for channel in p.iter().take(3) {
                let straight = if a > 0.0 { channel / a } else { 0.0 };
                out.push((straight.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
        }
        out
    }

    fn blend(&mut self, x: usize, y: usize, src: [f32; 4]) {
        let inv = 1.0 - src[3];
        if let Some(dst) = self.px.get_mut(y * self.width + x) {
            for (d, s) in dst.iter_mut().zip(src) {
                *d = s + *d * inv;
            }
        }
    }
}

fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (bx - ax) * (py - ay) - (by - ay) * (px - ax)
}

/// Top-left fill rule for the normalized (positive-area, y-down) winding, so a
/// pixel exactly on a shared edge is drawn by exactly one triangle.
fn top_left(a: &Vertex, b: &Vertex) -> bool {
    (a.pos.y == b.pos.y && b.pos.x > a.pos.x) || b.pos.y < a.pos.y
}

fn channels(color: epaint::Color32) -> [f32; 4] {
    [
        f32::from(color.r()) / 255.0,
        f32::from(color.g()) / 255.0,
        f32::from(color.b()) / 255.0,
        f32::from(color.a()) / 255.0,
    ]
}

fn vertex_channels(v: &Vertex) -> ([f32; 4], [f32; 2]) {
    (channels(v.color), [v.uv.x, v.uv.y])
}

/// Component-wise linear interpolation of two channel arrays.
fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0; 4];
    for ((o, x), y) in out.iter_mut().zip(a).zip(b) {
        *o = x + (y - x) * t;
    }
    out
}

/// Barycentric interpolation of a per-vertex channel array.
fn bary<const N: usize>(a: [f32; N], b: [f32; N], c: [f32; N], l: [f32; 3]) -> [f32; N] {
    let [l0, l1, l2] = l;
    let mut out = [0.0; N];
    for (((o, x), y), z) in out.iter_mut().zip(a).zip(b).zip(c) {
        *o = x * l0 + y * l1 + z * l2;
    }
    out
}

struct Atlas<'a> {
    image: &'a ColorImage,
    width: f32,
    height: f32,
}

impl Atlas<'_> {
    fn texel(&self, col: i64, row: i64) -> [f32; 4] {
        let width = self.image.width() as i64;
        let height = self.image.height() as i64;
        let idx = (row.clamp(0, height - 1) * width + col.clamp(0, width - 1)) as usize;
        // Indices are clamped into the atlas above; the fallback is unreachable.
        let color = self.image.pixels.get(idx).copied().unwrap_or(epaint::Color32::TRANSPARENT);
        channels(color)
    }

    /// Bilinear sample with clamped edges, matching `GL_LINEAR` + clamp-to-edge.
    fn sample(&self, uv: [f32; 2]) -> [f32; 4] {
        let gx = uv[0] * self.width - 0.5;
        let gy = uv[1] * self.height - 0.5;
        let x0 = gx.floor();
        let y0 = gy.floor();
        let fx = gx - x0;
        let fy = gy - y0;
        let (x0, y0) = (x0 as i64, y0 as i64);
        let c00 = self.texel(x0, y0);
        let c10 = self.texel(x0 + 1, y0);
        let c01 = self.texel(x0, y0 + 1);
        let c11 = self.texel(x0 + 1, y0 + 1);
        mix(mix(c00, c10, fx), mix(c01, c11, fx), fy)
    }
}

fn triangle(fb: &mut Framebuffer, clip: [i64; 4], atlas: &Atlas<'_>, tri: [&Vertex; 3]) {
    let [v0, v1, v2] = tri;
    let area = edge(v0.pos.x, v0.pos.y, v1.pos.x, v1.pos.y, v2.pos.x, v2.pos.y);
    if area.abs() < 1e-9 {
        return;
    }
    // Normalize to positive area; egui emits both windings.
    let (v1, v2) = if area < 0.0 { (v2, v1) } else { (v1, v2) };
    let area = area.abs();

    let min_x = v0.pos.x.min(v1.pos.x).min(v2.pos.x).floor() as i64;
    let max_x = v0.pos.x.max(v1.pos.x).max(v2.pos.x).ceil() as i64;
    let min_y = v0.pos.y.min(v1.pos.y).min(v2.pos.y).floor() as i64;
    let max_y = v0.pos.y.max(v1.pos.y).max(v2.pos.y).ceil() as i64;
    let x_start = min_x.max(clip[0]);
    let x_end = max_x.min(clip[1]);
    let y_start = min_y.max(clip[2]);
    let y_end = max_y.min(clip[3]);

    let (c0, uv0) = vertex_channels(v0);
    let (c1, uv1) = vertex_channels(v1);
    let (c2, uv2) = vertex_channels(v2);

    for y in y_start..y_end {
        let py = y as f32 + 0.5;
        for x in x_start..x_end {
            let px = x as f32 + 0.5;
            let w0 = edge(v1.pos.x, v1.pos.y, v2.pos.x, v2.pos.y, px, py);
            let w1 = edge(v2.pos.x, v2.pos.y, v0.pos.x, v0.pos.y, px, py);
            let w2 = edge(v0.pos.x, v0.pos.y, v1.pos.x, v1.pos.y, px, py);
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            if (w0 == 0.0 && !top_left(v1, v2))
                || (w1 == 0.0 && !top_left(v2, v0))
                || (w2 == 0.0 && !top_left(v0, v1))
            {
                continue;
            }
            let weights = [w0 / area, w1 / area, w2 / area];
            let uv = bary(uv0, uv1, uv2, weights);
            let tex = atlas.sample(uv);
            let mut src = bary(c0, c1, c2, weights);
            // Gamma-space multiply, exactly like the glow fragment shader.
            for (s, t) in src.iter_mut().zip(tex) {
                *s *= t;
            }
            fb.blend(x as usize, y as usize, src);
        }
    }
}

fn draw_mesh(
    fb: &mut Framebuffer,
    clip: [i64; 4],
    atlas: &Atlas<'_>,
    mesh: &Mesh,
) -> anyhow::Result<()> {
    match mesh.texture_id {
        TextureId::Managed(0) => {}
        TextureId::Managed(other) => {
            return Err(anyhow!("unexpected managed texture {other}, only the font atlas is supported"));
        }
        TextureId::User(id) => {
            return Err(anyhow!("unexpected user texture {id}, the demo GUI uses none"));
        }
    }
    for idx in mesh.indices.chunks_exact(3) {
        let [i0, i1, i2] = idx else { continue };
        let v0 = mesh.vertices.get(*i0 as usize).ok_or_else(|| anyhow!("mesh index out of range"))?;
        let v1 = mesh.vertices.get(*i1 as usize).ok_or_else(|| anyhow!("mesh index out of range"))?;
        let v2 = mesh.vertices.get(*i2 as usize).ok_or_else(|| anyhow!("mesh index out of range"))?;
        triangle(fb, clip, atlas, [v0, v1, v2]);
    }
    Ok(())
}

/// Rasterize tessellated egui primitives into the framebuffer.
pub fn rasterize(
    primitives: &[ClippedPrimitive],
    atlas_image: &ColorImage,
    fb: &mut Framebuffer,
) -> anyhow::Result<()> {
    let atlas = Atlas {
        image: atlas_image,
        width: atlas_image.width() as f32,
        height: atlas_image.height() as f32,
    };
    let screen = [0_i64, fb.width as i64, 0_i64, fb.height as i64];
    for clipped in primitives {
        let rect = clipped.clip_rect;
        let clip = [
            (rect.min.x.floor() as i64).max(screen[0]),
            (rect.max.x.ceil() as i64).min(screen[1]),
            (rect.min.y.floor() as i64).max(screen[2]),
            (rect.max.y.ceil() as i64).min(screen[3]),
        ];
        if clip[0] >= clip[1] || clip[2] >= clip[3] {
            continue;
        }
        match &clipped.primitive {
            Primitive::Mesh(mesh) => draw_mesh(fb, clip, &atlas, mesh)?,
            Primitive::Callback(_) => {
                return Err(anyhow!("paint callbacks are not supported by the CPU rasterizer"));
            }
        }
    }
    Ok(())
}
