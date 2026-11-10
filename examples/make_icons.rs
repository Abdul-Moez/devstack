//! Draws the devstack logo and writes every icon asset:
//!
//!   cargo run --example make_icons
//!
//! The mark: three stacked rack units on a black tile, each with a status light. The app icon
//! uses Signal orange lights; the tray uses green (something is running) or unlit (nothing is).
//! Geometry lives on a 64-unit grid; small sizes use a bolder variant so the lights stay
//! visible at 16-32 px. Shapes are rasterized with 4x4 supersampling; PNGs are
//! compressed with flate2.

use std::fs;
use std::path::Path;

type Rgba = [f32; 4]; // straight alpha, 0..1

const TILE: [u8; 3] = [14, 14, 16];
const TILE_EDGE: [u8; 3] = [58, 58, 64];
const UNIT: [u8; 3] = [34, 34, 39];
const UNIT_EDGE: [u8; 3] = [70, 70, 78];
const VENT: [u8; 3] = [84, 84, 92];
const SIGNAL: [u8; 3] = [255, 138, 61];
const LED_ON: [u8; 3] = [74, 222, 128];
const LED_OFF: [u8; 3] = [72, 72, 80];

#[derive(Clone, Copy)]
enum Lights {
    Brand,
    Running,
    Stopped,
}

fn color(c: [u8; 3], a: f32) -> Rgba {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a]
}

/// Signed distance to a rounded rectangle (negative inside).
fn rounded_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let hx = (x1 - x0) / 2.0 - r;
    let hy = (y1 - y0) / 2.0 - r;
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    outside + dx.max(dy).min(0.0) - r
}

fn circle(px: f32, py: f32, cx: f32, cy: f32, r: f32) -> f32 {
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - r
}

/// Color of one sample point (in 64-unit coordinates) on top of transparency.
fn sample(x: f32, y: f32, lights: Lights, bold: bool) -> Rgba {
    let mut out: Rgba = [0.0; 4];
    let mut over = |c: Rgba| {
        // straight-alpha "over"
        let a = c[3] + out[3] * (1.0 - c[3]);
        if a > 0.0 {
            for i in 0..3 {
                out[i] = (c[i] * c[3] + out[i] * out[3] * (1.0 - c[3])) / a;
            }
        }
        out[3] = a;
    };
    let tile = rounded_rect(x, y, 0.5, 0.5, 63.5, 63.5, 14.0);
    if tile > 0.0 {
        return out;
    }
    over(color(TILE, 1.0));
    let edge = if bold { 2.0 } else { 1.5 };
    if tile > -edge {
        over(color(TILE_EDGE, 1.0));
    }
    // Three rack units.
    let (x0, x1, h, ys, led_r, led_x) = if bold {
        (9.0, 55.0, 13.0, [9.5, 25.5, 41.5], 4.4, 17.5)
    } else {
        (12.0, 52.0, 10.0, [13.0, 27.0, 41.0], 3.0, 19.5)
    };
    let led = match lights {
        Lights::Brand => SIGNAL,
        Lights::Running => LED_ON,
        Lights::Stopped => LED_OFF,
    };
    let lit = !matches!(lights, Lights::Stopped);
    for y0 in ys {
        let y1 = y0 + h;
        let d = rounded_rect(x, y, x0, y0, x1, y1, if bold { 3.5 } else { 3.0 });
        if d <= 0.0 {
            over(color(if d > -1.2 { UNIT_EDGE } else { UNIT }, 1.0));
            let cy = (y0 + y1) / 2.0;
            // vents: two short slots on the right half
            if !bold {
                for vy in [cy - 1.6, cy + 1.6] {
                    if rounded_rect(x, y, 30.0, vy - 0.6, 46.0, vy + 0.6, 0.6) <= 0.0 {
                        over(color(VENT, 1.0));
                    }
                }
            }
            // the light, with a soft glow when lit
            let dl = circle(x, y, led_x, cy, led_r);
            if lit && dl > 0.0 && dl < led_r * 1.6 {
                over(color(led, 0.42 * (1.0 - dl / (led_r * 1.6)).powi(2)));
            }
            if dl <= 0.0 {
                over(color(led, 1.0));
            }
        }
    }
    out
}

fn render(size: u32, lights: Lights) -> Vec<u8> {
    let bold = size <= 32;
    let ss = 4;
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for j in 0..size {
        for i in 0..size {
            let mut acc = [0.0f32; 4];
            for sj in 0..ss {
                for si in 0..ss {
                    let x = (i as f32 + (si as f32 + 0.5) / ss as f32) * 64.0 / size as f32;
                    let y = (j as f32 + (sj as f32 + 0.5) / ss as f32) * 64.0 / size as f32;
                    let c = sample(x, y, lights, bold);
                    for k in 0..3 {
                        acc[k] += c[k] * c[3]; // premultiply while averaging
                    }
                    acc[3] += c[3];
                }
            }
            let n = (ss * ss) as f32;
            let a = acc[3] / n;
            for channel in &acc[..3] {
                let v = if a > 0.0 { channel / n / a } else { 0.0 };
                px.push((v * 255.0).round().clamp(0.0, 255.0) as u8);
            }
            px.push((a * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    px
}

// ------------------------------------------------------------------ PNG / ICO writers
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
}

fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut raw = Vec::new();
    for row in rgba.chunks((width * 4) as usize) {
        raw.push(0); // filter: none
        raw.extend(row);
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&raw).unwrap();
    let z = enc.finish().unwrap();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0];
    out.extend((images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in images {
        let s = if *size >= 256 { 0 } else { *size as u8 };
        out.extend([s, s, 0, 0]);
        out.extend(1u16.to_le_bytes()); // planes
        out.extend(32u16.to_le_bytes()); // bits per pixel
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in images {
        out.extend(data);
    }
    out
}

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    fs::write(assets.join("icon-brand.rgba"), render(64, Lights::Brand)).unwrap();
    fs::write(assets.join("icon-running.rgba"), render(32, Lights::Running)).unwrap();
    fs::write(assets.join("icon-stopped.rgba"), render(32, Lights::Stopped)).unwrap();
    let sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256];
    let images: Vec<(u32, Vec<u8>)> = sizes.iter().map(|&s| (s, png(s, s, &render(s, Lights::Brand)))).collect();
    fs::write(assets.join("devstack.ico"), ico(&images)).unwrap();
    fs::write(assets.join("logo.png"), png(256, 256, &render(256, Lights::Brand))).unwrap();
    // A contact sheet for judging small sizes by eye: 16/24/32/64 px, each blown up 4x with
    // hard pixel edges, on a dark and a light strip (like the taskbar). Not used by the app.
    let preview = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("icon-preview.png");
    fs::create_dir_all(preview.parent().unwrap()).unwrap();
    let (scale, gap, sizes) = (4u32, 16u32, [16u32, 24, 32, 64]);
    let width = sizes.iter().map(|s| s * scale + gap).sum::<u32>() + gap;
    let row = 64 * scale + gap;
    let rows = [Lights::Brand, Lights::Running, Lights::Stopped];
    let height = row * rows.len() as u32 * 2 + gap;
    let mut sheet = vec![0u8; (width * height * 4) as usize];
    for (strip, bg) in [[32u8, 32, 32], [238, 238, 238]].iter().enumerate() {
        for y in 0..height / 2 {
            for x in 0..width {
                let i = (((strip as u32 * height / 2 + y) * width + x) * 4) as usize;
                sheet[i..i + 4].copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
            }
        }
        for (r, lights) in rows.iter().enumerate() {
            let mut ox = gap;
            for s in sizes {
                let icon = render(s, *lights);
                let oy = strip as u32 * height / 2 + gap + r as u32 * row;
                for y in 0..s * scale {
                    for x in 0..s * scale {
                        let src = (((y / scale) * s + x / scale) * 4) as usize;
                        let a = icon[src + 3] as u32;
                        let dst = (((oy + y) * width + ox + x) * 4) as usize;
                        for k in 0..3 {
                            sheet[dst + k] = ((icon[src + k] as u32 * a + sheet[dst + k] as u32 * (255 - a)) / 255) as u8;
                        }
                    }
                }
                ox += s * scale + gap;
            }
        }
    }
    fs::write(&preview, png(width, height, &sheet)).unwrap();
    println!("wrote assets/icon-*.rgba, assets/devstack.ico, assets/logo.png (preview: target/icon-preview.png)");
}
