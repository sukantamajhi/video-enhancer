//! A built-in sample picture, so the controls can be tried before opening a
//! video. It is deliberately flat and slightly blue, like untreated footage.

use crate::media::Frame;

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn ridge(x: f32, seed: f32, base: f32, height: f32) -> f32 {
    base - height
        * ((x * 5.1 + seed).sin() * 0.45
            + (x * 11.7 + seed * 2.3).sin() * 0.28
            + (x * 27.0 + seed * 0.7).sin() * 0.14
            + (x * 61.0 + seed * 1.9).sin() * 0.06)
}

fn smooth_step(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn frame() -> Frame {
    let (w, h) = (1280usize, 720usize);
    let aspect = w as f32 / h as f32;
    let mut rgb = Vec::with_capacity(w * h * 3);
    let (sun_x, sun_y) = (0.68f32, 0.40f32);
    let layers: [(f32, f32, f32, [f32; 3]); 3] = [
        (1.3, 0.50, 0.075, [0.46, 0.45, 0.60]),
        (4.2, 0.59, 0.085, [0.29, 0.31, 0.46]),
        (2.2, 0.68, 0.070, [0.15, 0.20, 0.30]),
    ];
    for py in 0..h {
        let y = py as f32 / h as f32;
        for px in 0..w {
            let x = px as f32 / w as f32;
            let d = (((x - sun_x) * aspect).powi(2) + (y - sun_y).powi(2)).sqrt();

            let mut c = mix([0.20, 0.34, 0.58], [0.93, 0.66, 0.46], (y / 0.55).powf(1.4));
            c = mix(c, [1.0, 0.86, 0.62], (0.42 - d).max(0.0) * 1.6);
            c = mix(c, [1.0, 0.95, 0.80], smooth_step(0.075, 0.062, d));

            for (seed, base, height, colour) in layers {
                let top = ridge(x, seed, base, height);
                if y > top {
                    let fog = ((y - top) * 5.0).min(1.0);
                    c = mix(colour, mix(colour, [0.95, 0.70, 0.55], 0.3), 1.0 - fog);
                }
            }

            // Lake with the sky's reflection, then a dark grassy foreground.
            if y > 0.74 {
                let ripple = ((y * 190.0 + (x * 19.0).sin() * 2.5).sin() * 0.5 + 0.5) * 0.09;
                let glow = smooth_step(0.28, 0.0, (x - sun_x).abs()) * 0.85;
                let reflected = mix([0.22, 0.32, 0.46], [0.90, 0.62, 0.42], glow);
                c = mix(reflected, [0.97, 0.82, 0.60], ripple);
            }
            let shore = 0.90 + (x * 8.0 + 1.0).sin() * 0.018 + (x * 37.0).sin() * 0.006;
            if y > shore {
                c = mix([0.13, 0.17, 0.10], [0.07, 0.09, 0.06], (y - shore) * 7.0);
            }

            // A walker on the shore with a warm jacket, for skin and clothing colour.
            let (cx, base) = (0.27f32, shore.min(0.905));
            let dx = (x - cx) * aspect;
            if y < base && y > base - 0.17 && dx.abs() < 0.024 - (y - (base - 0.08)).abs() * 0.05 {
                c = if y < base - 0.075 { [0.75, 0.30, 0.20] } else { [0.14, 0.17, 0.26] };
            }
            if (dx.powi(2) + (y - (base - 0.19)).powi(2)).sqrt() < 0.023 {
                c = [0.84, 0.62, 0.50];
            }

            // Make it look like flat, untreated footage: low contrast and a cool cast.
            let n = (((px * 13 + py * 7) % 17) as f32 / 17.0 - 0.5) * 0.012;
            rgb.extend([
                ((0.14 + c[0] * 0.62 + n) * 0.95 * 255.0).clamp(0.0, 255.0) as u8,
                ((0.14 + c[1] * 0.62 + n) * 255.0).clamp(0.0, 255.0) as u8,
                ((0.15 + c[2] * 0.62 + n) * 1.05 * 255.0).clamp(0.0, 255.0) as u8,
            ]);
        }
    }
    Frame { width: w, height: h, rgb }
}
