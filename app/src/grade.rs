//! The colour engine. Every grade, whether from a look, the sliders or an AI
//! model, becomes one 3D colour table. The preview and the exported video both
//! use that table, so what you see is what you get.

use std::fmt::Write as _;
use std::path::Path;

/// Everything the sliders control. All neutral values leave the picture untouched.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Grade {
    pub black: [f32; 3],
    pub white: [f32; 3],
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub teal_orange: f32,
    pub fade: f32,
    pub strength: f32,
}

impl Default for Grade {
    fn default() -> Self {
        Self {
            black: [0.0; 3],
            white: [1.0; 3],
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            temperature: 0.0,
            tint: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            teal_orange: 0.0,
            fade: 0.0,
            strength: 1.0,
        }
    }
}

/// A named creative look. It sets the creative sliders and adds to the
/// corrections found by auto-fix (levels and white balance).
pub struct Look {
    pub name: &'static str,
    pub note: &'static str,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    warmth: f32,
    saturation: f32,
    vibrance: f32,
    teal_orange: f32,
    fade: f32,
}

pub const LOOKS: [Look; 8] = [
    Look { name: "YouTube Pop", note: "Bright, punchy, clean skin", contrast: 0.22, highlights: -0.15, shadows: 0.12, warmth: 0.03, saturation: 0.08, vibrance: 0.32, teal_orange: 0.0, fade: 0.0 },
    Look { name: "Clean Natural", note: "Corrected, nothing flashy", contrast: 0.08, highlights: -0.05, shadows: 0.05, warmth: 0.0, saturation: 0.0, vibrance: 0.1, teal_orange: 0.0, fade: 0.0 },
    Look { name: "Warm Vlog", note: "Golden, friendly, soft", contrast: 0.12, highlights: -0.2, shadows: 0.16, warmth: 0.16, saturation: 0.0, vibrance: 0.2, teal_orange: 0.05, fade: 0.02 },
    Look { name: "Teal & Orange", note: "Blockbuster film colour", contrast: 0.28, highlights: -0.2, shadows: -0.04, warmth: 0.02, saturation: -0.04, vibrance: 0.18, teal_orange: 0.55, fade: 0.0 },
    Look { name: "Moody Film", note: "Dark, faded, cinematic", contrast: 0.2, highlights: -0.28, shadows: -0.12, warmth: -0.02, saturation: -0.2, vibrance: 0.05, teal_orange: 0.25, fade: 0.05 },
    Look { name: "Gaming Vivid", note: "Loud colour for screen footage", contrast: 0.26, highlights: -0.08, shadows: 0.08, warmth: 0.0, saturation: 0.18, vibrance: 0.42, teal_orange: 0.0, fade: 0.0 },
    Look { name: "Bright Tutorial", note: "Airy and easy to read", contrast: 0.06, highlights: -0.1, shadows: 0.28, warmth: 0.02, saturation: 0.02, vibrance: 0.14, teal_orange: 0.0, fade: 0.0 },
    Look { name: "Food & Travel", note: "Rich reds and deep skies", contrast: 0.18, highlights: -0.18, shadows: 0.1, warmth: 0.08, saturation: 0.12, vibrance: 0.36, teal_orange: 0.1, fade: 0.0 },
];

impl Look {
    pub fn apply(&self, grade: &mut Grade, base_temperature: f32) {
        grade.contrast = self.contrast;
        grade.highlights = self.highlights;
        grade.shadows = self.shadows;
        grade.temperature = (base_temperature + self.warmth).clamp(-1.0, 1.0);
        grade.saturation = self.saturation;
        grade.vibrance = self.vibrance;
        grade.teal_orange = self.teal_orange;
        grade.fade = self.fade;
    }
}

/// A look learned by comparing a clip with Gemini Omni's graded version of it.
#[derive(Clone, PartialEq)]
pub struct LearnedLook {
    pub curves: [Vec<f32>; 3],
    pub saturation: f32,
    pub amount: f32,
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn sample_curve(curve: &[f32], v: f32) -> f32 {
    let p = v.clamp(0.0, 1.0) * (curve.len() - 1) as f32;
    let i = (p as usize).min(curve.len() - 2);
    let f = p - i as f32;
    curve[i] * (1.0 - f) + curve[i + 1] * f
}

fn smooth(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The colour transform for one pixel value.
pub fn transform(grade: &Grade, learned: Option<&LearnedLook>, input: [f32; 3]) -> [f32; 3] {
    let mut c = input;

    if let Some(look) = learned {
        let mut l = [0.0; 3];
        for i in 0..3 {
            l[i] = sample_curve(&look.curves[i], c[i]);
        }
        let y = luma(l);
        for v in &mut l {
            *v = y + (*v - y) * look.saturation;
        }
        for i in 0..3 {
            c[i] += (l[i] - c[i]) * look.amount;
        }
    }

    for i in 0..3 {
        c[i] = (c[i] - grade.black[i]) / (grade.white[i] - grade.black[i]).max(0.05);
    }

    // White balance as gentle channel gains.
    c[0] *= 1.0 + 0.22 * grade.temperature;
    c[2] *= 1.0 - 0.22 * grade.temperature;
    c[1] *= 1.0 - 0.16 * grade.tint;

    if grade.exposure != 0.0 {
        let gain = 2f32.powf(grade.exposure);
        for v in &mut c {
            *v = (v.max(0.0).powf(2.2) * gain).powf(1.0 / 2.2);
        }
    }

    let y = luma(c).clamp(0.0, 1.0);
    let lift = grade.shadows * 0.28 * (1.0 - smooth(0.0, 0.55, y)) * smooth(0.0, 0.12, y)
        + grade.highlights * 0.28 * smooth(0.5, 1.0, y);
    for v in &mut c {
        *v += lift;
    }

    if grade.contrast != 0.0 {
        // S-curve on brightness only, so contrast does not skew the colours.
        let y = luma(c).clamp(0.0, 1.0);
        let curved = y + (y - 0.5) * (1.0 - (2.0 * y - 1.0).abs().powi(2)) * grade.contrast * 1.1;
        let shift = curved - y;
        for v in &mut c {
            *v += shift;
        }
    }

    if grade.teal_orange != 0.0 {
        let y = luma(c).clamp(0.0, 1.0);
        let warm = smooth(0.25, 0.8, y) * 2.0 - 1.0;
        let a = grade.teal_orange * 0.09;
        c[0] += warm * a;
        c[1] += warm * a * 0.12;
        c[2] -= warm * a;
    }

    let y = luma(c);
    let sat = c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
    let chroma = 1.0 + grade.saturation + grade.vibrance * (1.0 - sat.clamp(0.0, 1.0)).powi(2);
    for v in &mut c {
        *v = y + (*v - y) * chroma;
    }

    if grade.fade != 0.0 {
        for v in &mut c {
            *v = grade.fade + *v * (1.0 - grade.fade);
        }
    }

    for i in 0..3 {
        c[i] = (input[i] + (c[i] - input[i]) * grade.strength).clamp(0.0, 1.0);
    }
    c
}

/// A cube of colours, ready for the preview and for the video exporter.
#[derive(Clone)]
pub struct Lut {
    size: usize,
    data: Vec<[f32; 3]>,
}

pub const LUT_SIZE: usize = 33;

impl Lut {
    pub fn build(size: usize, grade: &Grade, learned: Option<&LearnedLook>) -> Self {
        let mut data = Vec::with_capacity(size * size * size);
        let n = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push(transform(grade, learned, [r as f32 / n, g as f32 / n, b as f32 / n]));
                }
            }
        }
        Self { size, data }
    }

    /// Writes the table in the standard .cube format, red changing fastest.
    pub fn write_cube(&self, path: &Path) -> std::io::Result<()> {
        let mut text = String::with_capacity(self.data.len() * 28);
        let _ = writeln!(text, "LUT_3D_SIZE {}", self.size);
        for c in &self.data {
            let _ = writeln!(text, "{:.6} {:.6} {:.6}", c[0], c[1], c[2]);
        }
        std::fs::write(path, text)
    }

    /// Applies the table to tightly packed RGB bytes, using all CPU cores.
    pub fn apply(&self, rgb: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; rgb.len()];
        let n = self.size - 1;
        let mut low = [0usize; 256];
        let mut frac = [0f32; 256];
        for v in 0..256 {
            let p = v as f32 / 255.0 * n as f32;
            low[v] = (p as usize).min(n - 1);
            frac[v] = p - low[v] as f32;
        }
        let (size, data) = (self.size, &self.data);
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
        let chunk = (rgb.len() / 3 / workers + 1) * 3;
        std::thread::scope(|scope| {
            for (src, dst) in rgb.chunks(chunk).zip(out.chunks_mut(chunk)) {
                let (low, frac) = (&low, &frac);
                scope.spawn(move || {
                    let (dg, db) = (size, size * size);
                    for (s, d) in src.chunks_exact(3).zip(dst.chunks_exact_mut(3)) {
                        let (ri, gi, bi) = (low[s[0] as usize], low[s[1] as usize], low[s[2] as usize]);
                        let (rf, gf, bf) = (frac[s[0] as usize], frac[s[1] as usize], frac[s[2] as usize]);
                        let base = bi * db + gi * dg + ri;
                        for ch in 0..3 {
                            let at = |i: usize| data[i][ch];
                            let x00 = at(base) + (at(base + 1) - at(base)) * rf;
                            let x10 = at(base + dg) + (at(base + dg + 1) - at(base + dg)) * rf;
                            let x01 = at(base + db) + (at(base + db + 1) - at(base + db)) * rf;
                            let x11 = at(base + db + dg) + (at(base + db + dg + 1) - at(base + db + dg)) * rf;
                            let y0 = x00 + (x10 - x00) * gf;
                            let y1 = x01 + (x11 - x01) * gf;
                            d[ch] = ((y0 + (y1 - y0) * bf) * 255.0 + 0.5) as u8;
                        }
                    }
                });
            }
        });
        out
    }
}

fn histograms(rgb: &[u8]) -> [[u64; 256]; 3] {
    let mut h = [[0u64; 256]; 3];
    for px in rgb.chunks_exact(3) {
        for i in 0..3 {
            h[i][px[i] as usize] += 1;
        }
    }
    h
}

fn percentile(hist: &[u64; 256], total: u64, q: f64) -> f32 {
    let target = (total as f64 * q) as u64;
    let mut seen = 0;
    for (v, count) in hist.iter().enumerate() {
        seen += count;
        if seen > target {
            return v as f32 / 255.0;
        }
    }
    1.0
}

/// One-click correction: fixes the black and white points, colour cast and
/// overall brightness. Damped so it never overreacts to a single odd scene.
pub fn auto_correct(rgb: &[u8]) -> Grade {
    let mut g = Grade::default();
    let total = (rgb.len() / 3) as u64;
    if total == 0 {
        return g;
    }
    let h = histograms(rgb);
    let mut lo = [0f32; 3];
    let mut hi = [1f32; 3];
    for i in 0..3 {
        lo[i] = percentile(&h[i], total, 0.004);
        hi[i] = percentile(&h[i], total, 0.996);
    }
    // Mostly shared levels keep the colours true; the per-channel half removes casts.
    let (shared_lo, shared_hi) = (lo[0].min(lo[1]).min(lo[2]), hi[0].max(hi[1]).max(hi[2]));
    for i in 0..3 {
        g.black[i] = (shared_lo * 0.5 + lo[i] * 0.5).min(0.2);
        g.white[i] = (shared_hi * 0.5 + hi[i] * 0.5).max(0.7);
    }

    // Measure what is left after levels, on the mid-tones only, to balance white.
    let mut sum = [0f64; 3];
    let mut count = 0f64;
    let mut lumas = [0u64; 256];
    for px in rgb.chunks_exact(3).step_by(3) {
        let mut c = [0f32; 3];
        for i in 0..3 {
            c[i] = ((px[i] as f32 / 255.0 - g.black[i]) / (g.white[i] - g.black[i])).clamp(0.0, 1.0);
        }
        let y = luma(c);
        lumas[(y * 255.0) as usize] += 1;
        if (0.12..0.88).contains(&y) {
            for i in 0..3 {
                sum[i] += c[i] as f64;
            }
            count += 1.0;
        }
    }
    if count > 0.0 {
        let (r, gr, b) = ((sum[0] / count) as f32, (sum[1] / count) as f32, (sum[2] / count) as f32);
        let mid = (r + gr + b) / 3.0;
        // Solve the red/blue gains in `transform` for most of the measured imbalance,
        // leaving a little of the scene's own mood (golden hour stays warm).
        let k = (b.max(0.02) / r.max(0.02)).clamp(0.5, 2.0).powf(0.85);
        g.temperature = ((k - 1.0) / ((k + 1.0) * 0.22)).clamp(-0.9, 0.9);
        g.tint = (((gr - (r + b) * 0.5) / mid.max(0.05)) * 1.2).clamp(-0.4, 0.4);
    }
    let median = percentile(&lumas, lumas.iter().sum(), 0.5).max(0.04);
    g.exposure = ((0.46 / median).log2() * 1.1).clamp(-0.5, 0.8);
    g
}

/// Learns the colour change between a clip and Gemini Omni's version of it.
/// It compares overall colour spread rather than exact pixels, because Omni
/// redraws the frames and they will not line up perfectly.
pub fn learn_look(before: &[u8], after: &[u8]) -> LearnedLook {
    let (a, b) = (histograms(before), histograms(after));
    let curve = |ch: usize| -> Vec<f32> {
        let total_a: u64 = a[ch].iter().sum::<u64>().max(1);
        let total_b: u64 = b[ch].iter().sum::<u64>().max(1);
        let mut out = vec![0f32; 256];
        let (mut seen_a, mut seen_b, mut j) = (0u64, 0u64, 0usize);
        for (v, slot) in out.iter_mut().enumerate() {
            seen_a += a[ch][v];
            let target = seen_a as f64 / total_a as f64;
            while j < 255 && (seen_b + b[ch][j]) as f64 / (total_b as f64) < target {
                seen_b += b[ch][j];
                j += 1;
            }
            *slot = j as f32 / 255.0;
        }
        // Smooth, then make sure the curve never bends backwards.
        let copy = out.clone();
        for (i, slot) in out.iter_mut().enumerate() {
            let (s, e) = (i.saturating_sub(6), (i + 7).min(256));
            *slot = copy[s..e].iter().sum::<f32>() / (e - s) as f32;
        }
        for i in 1..256 {
            out[i] = out[i].max(out[i - 1]);
        }
        out
    };
    let curves = [curve(0), curve(1), curve(2)];
    let chroma = |rgb: &[u8], apply: bool| -> f64 {
        let (mut sum, mut n) = (0f64, 0f64);
        for px in rgb.chunks_exact(3).step_by(2) {
            let mut c = [px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0];
            if apply {
                for i in 0..3 {
                    c[i] = sample_curve(&curves[i], c[i]);
                }
            }
            sum += (c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])) as f64;
            n += 1.0;
        }
        sum / n.max(1.0)
    };
    let ratio = chroma(after, false) / chroma(before, true).max(0.005);
    LearnedLook { curves, saturation: (ratio as f32).clamp(0.75, 1.4), amount: 1.0 }
}

/// Per-channel brightness counts, scaled to 0..=1, for the on-screen scope.
pub fn scope(rgb: &[u8]) -> [[f32; 64]; 3] {
    let mut out = [[0f32; 64]; 3];
    for px in rgb.chunks_exact(3).step_by(2) {
        for i in 0..3 {
            out[i][(px[i] >> 2) as usize] += 1.0;
        }
    }
    let mut sorted: Vec<f32> = out.iter().flatten().copied().collect();
    sorted.sort_by(f32::total_cmp);
    let top = sorted[(sorted.len() as f32 * 0.98) as usize].max(1.0);
    for ch in &mut out {
        for v in ch.iter_mut() {
            *v = (*v / top).min(1.0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mean(rgb: &[u8]) -> [f32; 3] {
        let mut s = [0f32; 3];
        for p in rgb.chunks_exact(3) {
            for i in 0..3 {
                s[i] += p[i] as f32;
            }
        }
        let n = (rgb.len() / 3) as f32;
        [s[0] / n, s[1] / n, s[2] / n]
    }

    fn ramp() -> Vec<u8> {
        let mut v = Vec::new();
        for r in (0..=255u32).step_by(15) {
            for g in (0..=255u32).step_by(15) {
                for b in (0..=255u32).step_by(15) {
                    v.extend([r as u8, g as u8, b as u8]);
                }
            }
        }
        v
    }

    #[test]
    fn neutral_grade_changes_nothing() {
        let lut = Lut::build(LUT_SIZE, &Grade::default(), None);
        let input = ramp();
        for (a, b) in input.iter().zip(lut.apply(&input)) {
            assert!((*a as i32 - b as i32).abs() <= 1, "{a} vs {b}");
        }
    }

    #[test]
    fn strength_zero_cancels_any_look() {
        let mut g = Grade::default();
        LOOKS[3].apply(&mut g, 0.3);
        g.strength = 0.0;
        let c = transform(&g, None, [0.2, 0.5, 0.7]);
        assert!((c[0] - 0.2).abs() < 1e-5 && (c[1] - 0.5).abs() < 1e-5 && (c[2] - 0.7).abs() < 1e-5);
    }

    #[test]
    fn every_look_keeps_the_tonal_range() {
        for look in &LOOKS {
            let mut g = Grade::default();
            look.apply(&mut g, 0.0);
            let dark = transform(&g, None, [0.1, 0.1, 0.1]);
            let bright = transform(&g, None, [0.9, 0.9, 0.9]);
            assert!(luma(bright) > luma(dark) + 0.5, "{} crushed the range", look.name);
        }
    }

    #[test]
    fn auto_correct_removes_a_colour_cast_and_flatness() {
        // A flat, blue-tinted picture.
        let mut flat = Vec::new();
        for v in 0..=255u32 {
            for _ in 0..30 {
                let l = 60.0 + v as f32 * 0.4;
                flat.extend([(l * 0.82) as u8, (l * 0.95) as u8, (l * 1.15) as u8]);
            }
        }
        let g = auto_correct(&flat);
        let out = Lut::build(LUT_SIZE, &g, None).apply(&flat);
        let (before, after) = (mean(&flat), mean(&out));
        let cast = |m: [f32; 3]| (m[2] - m[0]).abs() / ((m[0] + m[1] + m[2]) / 3.0);
        assert!(cast(after) < cast(before) * 0.6, "cast {} -> {}", cast(before), cast(after));
        let range = |rgb: &[u8]| {
            let ys: Vec<u8> = rgb.chunks_exact(3).map(|p| p[1]).collect();
            *ys.iter().max().unwrap() as i32 - *ys.iter().min().unwrap() as i32
        };
        assert!(range(&out) > range(&flat) + 60, "range {} -> {}", range(&flat), range(&out));
    }

    #[test]
    fn learned_look_reproduces_a_known_grade() {
        let before = ramp();
        let target = Grade { contrast: 0.3, temperature: 0.25, ..Grade::default() };
        let after = Lut::build(LUT_SIZE, &target, None).apply(&before);
        let look = learn_look(&before, &after);
        let result = Lut::build(LUT_SIZE, &Grade::default(), Some(&look)).apply(&before);
        let (want, got) = (mean(&after), mean(&result));
        for i in 0..3 {
            assert!((want[i] - got[i]).abs() < 8.0, "channel {i}: want {want:?}, got {got:?}");
        }
    }

    #[test]
    fn cube_file_lists_red_fastest() {
        let path = std::env::temp_dir().join(format!("ve-cube-{}.cube", std::process::id()));
        Lut::build(2, &Grade::default(), None).write_cube(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "LUT_3D_SIZE 2");
        assert_eq!(lines[2], "1.000000 0.000000 0.000000");
        assert_eq!(lines[3], "0.000000 1.000000 0.000000");
        std::fs::remove_file(&path).ok();
    }
}
