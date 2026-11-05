//! Colour grading on the graphics card.
//!
//! FFmpeg's own colour-table filter is the slowest part of an export (about
//! 6 ms a frame even on 16 threads). The graphics card does the same job in a
//! fraction of that, so an export is split in three, running side by side:
//!
//!   FFmpeg #1  decodes, crops and scales to the final size, and streams raw frames
//!   this file  grades every frame on the graphics card
//!   FFmpeg #2  adds the original sound, encodes (graphics-card or CPU encoder) and saves
//!
//! Frames travel as plain video (YUV 4:2:0, half the bytes of RGB), so FFmpeg
//! never has to convert them. The card does the conversion to RGB, applies the
//! same colour table, with the same arithmetic, as the preview, and converts
//! back. If the card, its driver or the start-up self-test is not up to it,
//! nothing here is used and the ordinary all-FFmpeg export runs instead.
//!
//! The pictures are always standard HD video colour (BT.709, limited range):
//! the first FFmpeg converts whatever the source is into that.

use std::io::{Read, Write};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use eframe::wgpu;

use crate::grade::{Grade, Lut};
use crate::media::{self, Encoder, ExportJob, Tools};

/// One thread handles a block of 8x2 pixels, so every plane is read and written in whole 32-bit words
/// and no two threads ever write the same word.
const SHADER: &str = r#"
struct Params { w: u32, h: u32, size: u32, blocks_x: u32 }

@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<storage, read> lut: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> p: Params;

fn byte_at(i: u32) -> f32 {
    return f32((src[i >> 2u] >> ((i & 3u) * 8u)) & 255u);
}

// One chroma sample, with the edges repeated.
fn chroma_sample(plane: u32, cx: i32, cy: i32) -> f32 {
    let cw = i32(p.w / 2u);
    let ch = i32(p.h / 2u);
    let x = u32(clamp(cx, 0, cw - 1));
    let y = u32(clamp(cy, 0, ch - 1));
    let base = p.w * p.h + plane * (p.w / 2u) * (p.h / 2u);
    return byte_at(base + y * u32(cw) + x);
}

// Chroma at a full-size pixel. Video chroma sits level with every second column and half a row
// between rows, so the in-between positions are blended from their neighbours.
fn chroma_at(plane: u32, x: u32, y: u32) -> f32 {
    let fx = f32(x) * 0.5;
    let fy = f32(y) * 0.5 - 0.25;
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let wx = fx - f32(x0);
    let wy = fy - f32(y0);
    let top = mix(chroma_sample(plane, x0, y0), chroma_sample(plane, x0 + 1, y0), wx);
    let bottom = mix(chroma_sample(plane, x0, y0 + 1), chroma_sample(plane, x0 + 1, y0 + 1), wx);
    return mix(top, bottom, wy);
}

fn lookup(rgb: vec3<f32>) -> vec3<f32> {
    let s = p.size;
    let pos = rgb * (f32(s - 1u) / 255.0);
    let lo = min(vec3<u32>(pos), vec3<u32>(s - 2u));
    let f = pos - vec3<f32>(lo);
    let base = (lo.z * s + lo.y) * s + lo.x;
    let ss = s * s;
    let x00 = mix(lut[base].xyz,           lut[base + 1u].xyz,           f.x);
    let x10 = mix(lut[base + s].xyz,       lut[base + s + 1u].xyz,       f.x);
    let x01 = mix(lut[base + ss].xyz,      lut[base + ss + 1u].xyz,      f.x);
    let x11 = mix(lut[base + ss + s].xyz,  lut[base + ss + s + 1u].xyz,  f.x);
    return mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z);
}

// BT.709 limited-range video to 8-bit RGB, and back.
fn to_rgb(y: f32, cb: f32, cr: f32) -> vec3<f32> {
    let yy = (y - 16.0) / 219.0;
    let b = (cb - 128.0) / 224.0;
    let r = (cr - 128.0) / 224.0;
    return round(clamp(vec3<f32>(yy + 1.5748 * r, yy - 0.187324 * b - 0.468124 * r, yy + 1.8556 * b), vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0);
}

// Returns brightness (16..235) and the two colour differences (16..240), before rounding.
fn to_video(rgb: vec3<f32>) -> vec3<f32> {
    let y = 0.2126 * rgb.x + 0.7152 * rgb.y + 0.0722 * rgb.z;
    return vec3<f32>(16.0 + 219.0 * y, 128.0 + 224.0 * (rgb.z - y) / 1.8556, 128.0 + 224.0 * (rgb.x - y) / 1.5748);
}

fn q(v: f32) -> u32 {
    return u32(clamp(round(v), 0.0, 255.0));
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let t = id.x;
    let blocks = p.blocks_x * (p.h / 2u);
    if (t >= blocks) { return; }
    let bx = t % p.blocks_x;
    let by = t / p.blocks_x;
    let x0 = bx * 8u;
    let y0 = by * 2u;

    var y_words = array<u32, 4>(0u, 0u, 0u, 0u);   // two words for each of the two rows
    var u_word = 0u;
    var v_word = 0u;
    var cb_sum = array<f32, 4>(0.0, 0.0, 0.0, 0.0);
    var cr_sum = array<f32, 4>(0.0, 0.0, 0.0, 0.0);

    for (var row = 0u; row < 2u; row = row + 1u) {
        for (var k = 0u; k < 8u; k = k + 1u) {
            let x = x0 + k;
            let y = y0 + row;
            let luma = byte_at(y * p.w + x);
            let rgb = to_rgb(luma, chroma_at(0u, x, y), chroma_at(1u, x, y));
            let graded = round(clamp(lookup(rgb), vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0) / 255.0;
            let v = to_video(graded);
            y_words[row * 2u + k / 4u] = y_words[row * 2u + k / 4u] | (q(v.x) << ((k % 4u) * 8u));
            cb_sum[k / 2u] = cb_sum[k / 2u] + v.y;
            cr_sum[k / 2u] = cr_sum[k / 2u] + v.z;
        }
    }
    for (var c = 0u; c < 4u; c = c + 1u) {
        u_word = u_word | (q(cb_sum[c] * 0.25) << (c * 8u));
        v_word = v_word | (q(cr_sum[c] * 0.25) << (c * 8u));
    }

    let wy = p.w;
    dst[(y0 * wy + x0) / 4u] = y_words[0];
    dst[(y0 * wy + x0) / 4u + 1u] = y_words[1];
    dst[((y0 + 1u) * wy + x0) / 4u] = y_words[2];
    dst[((y0 + 1u) * wy + x0) / 4u + 1u] = y_words[3];
    let u_base = p.w * p.h;
    let v_base = u_base + (p.w / 2u) * (p.h / 2u);
    dst[(u_base + by * (p.w / 2u) + x0 / 2u) / 4u] = u_word;
    dst[(v_base + by * (p.w / 2u) + x0 / 2u) / 4u] = v_word;
}
"#;

pub struct GpuGrader {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    name: String,
}

impl GpuGrader {
    pub fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }))
            .map_err(|e| format!("no graphics card was found ({e})"))?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err("only a software renderer is available, which is slower than FFmpeg".into());
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).map_err(|e| format!("the graphics card refused to start ({e})"))?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("colour grade"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("colour grade"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(Self { device, queue, pipeline, name: info.name })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn buffer(&self, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false })
    }

    /// Everything needed to grade frames of one size with one colour table.
    pub fn session(self: &Arc<Self>, lut: &Lut, width: usize, height: usize) -> Result<Session, String> {
        if width == 0 || height == 0 || width % 8 != 0 || height % 2 != 0 {
            return Err("this picture size can't be graded on the graphics card".into());
        }
        let bytes = (width * height * 3 / 2) as u64;
        let blocks = ((width / 8) * (height / 2)) as u32;
        let entries = lut.entries();
        let table = self.buffer("colour table", (entries.len() * 16) as u64, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let mut table_bytes = Vec::with_capacity(entries.len() * 16);
        for e in entries {
            for v in [e[0], e[1], e[2], 0.0] {
                table_bytes.extend(v.to_le_bytes());
            }
        }
        self.queue.write_buffer(&table, 0, &table_bytes);
        let params = self.buffer("settings", 16, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        let mut settings = Vec::with_capacity(16);
        for v in [width as u32, height as u32, lut.size() as u32, (width / 8) as u32] {
            settings.extend(v.to_le_bytes());
        }
        self.queue.write_buffer(&params, 0, &settings);
        let layout = self.pipeline.get_bind_group_layout(0);
        // Several frames travel at once, each in its own set of buffers, so the card is never waiting for one.
        let slots = (0..Session::SLOTS)
            .map(|_| {
                let src = self.buffer("frame in", bytes, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
                let dst = self.buffer("frame out", bytes, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC);
                let staging = self.buffer("frame back", bytes, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("colour grade"),
                    layout: &layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: dst.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: table.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
                    ],
                });
                Slot { src, dst, staging, bind }
            })
            .collect();
        Ok(Session { grader: self.clone(), slots, groups: blocks, bytes, _table: table, _params: params })
    }
}

struct Slot {
    src: wgpu::Buffer,
    dst: wgpu::Buffer,
    staging: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

pub struct Session {
    grader: Arc<GpuGrader>,
    slots: Vec<Slot>,
    groups: u32,
    bytes: u64,
    _table: wgpu::Buffer,
    _params: wgpu::Buffer,
}

/// A frame on its way through the card.
pub struct Pending {
    slot: usize,
    submission: wgpu::SubmissionIndex,
    mapped: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

impl Session {
    /// How many frames can be in flight at once.
    pub const SLOTS: usize = 3;

    pub fn frame_bytes(&self) -> usize {
        self.bytes as usize
    }

    /// Sends a frame to the card and returns at once. The frame is plain video: the brightness
    /// plane, then the two colour planes at half size. Take the result back with `collect`.
    pub fn submit(&self, slot: usize, frame: &[u8]) -> Result<Pending, String> {
        if frame.len() as u64 != self.bytes {
            return Err("a frame of the wrong size reached the graphics card".into());
        }
        let (g, s) = (&self.grader, &self.slots[slot % Self::SLOTS]);
        g.queue.write_buffer(&s.src, 0, frame);
        let mut encoder = g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&g.pipeline);
            pass.set_bind_group(0, &s.bind, &[]);
            pass.dispatch_workgroups(self.groups.div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&s.dst, 0, &s.staging, 0, self.bytes);
        let submission = g.queue.submit(std::iter::once(encoder.finish()));
        let (tx, mapped) = mpsc::channel();
        s.staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        Ok(Pending { slot: slot % Self::SLOTS, submission, mapped })
    }

    /// Waits for one frame (not the ones behind it) and writes the graded picture into `frame`.
    pub fn collect(&self, pending: Pending, frame: &mut [u8]) -> Result<(), String> {
        let (g, s) = (&self.grader, &self.slots[pending.slot]);
        g.device
            .poll(wgpu::PollType::Wait { submission_index: Some(pending.submission), timeout: None })
            .map_err(|e| format!("the graphics card stopped answering ({e})"))?;
        loop {
            match pending.mapped.try_recv() {
                Ok(result) => break result.map_err(|e| format!("couldn't read the graded frame back ({e})"))?,
                Err(mpsc::TryRecvError::Empty) => {
                    let _ = g.device.poll(wgpu::PollType::Poll);
                    std::thread::sleep(Duration::from_micros(50));
                }
                Err(mpsc::TryRecvError::Disconnected) => return Err("the graphics card's answer was lost".into()),
            }
        }
        {
            let view = s.staging.slice(..).get_mapped_range().map_err(|e| format!("couldn't read the graded frame back ({e})"))?;
            frame.copy_from_slice(&view);
        }
        s.staging.unmap();
        Ok(())
    }

    /// Grades one frame and waits for it. Export uses `submit` and `collect` to keep several frames moving.
    pub fn grade(&self, frame: &mut [u8]) -> Result<(), String> {
        let pending = self.submit(0, frame)?;
        self.collect(pending, frame)
    }
}

/// The shader's arithmetic done on the processor, one step at a time. The card is checked against it.
fn reference(lut: &Lut, w: usize, h: usize, video: &[u8]) -> Vec<u8> {
    let (cw, ch) = (w / 2, h / 2);
    let (luma, blue, red) = (&video[..w * h], &video[w * h..w * h + cw * ch], &video[w * h + cw * ch..]);
    let mix = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
    let chroma = |plane: &[u8], x: usize, y: usize| -> f32 {
        let (fx, fy) = (x as f32 * 0.5, y as f32 * 0.5 - 0.25);
        let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
        let (wx, wy) = (fx - x0 as f32, fy - y0 as f32);
        let at = |cx: i32, cy: i32| plane[cy.clamp(0, ch as i32 - 1) as usize * cw + cx.clamp(0, cw as i32 - 1) as usize] as f32;
        mix(mix(at(x0, y0), at(x0 + 1, y0), wx), mix(at(x0, y0 + 1), at(x0 + 1, y0 + 1), wx), wy)
    };
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let (yy, b, r) = ((luma[y * w + x] as f32 - 16.0) / 219.0, (chroma(blue, x, y) - 128.0) / 224.0, (chroma(red, x, y) - 128.0) / 224.0);
            let out = [yy + 1.5748 * r, yy - 0.187324 * b - 0.468124 * r, yy + 1.8556 * b];
            for c in 0..3 {
                rgb[(y * w + x) * 3 + c] = (out[c].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
    let graded = lut.apply(&rgb);
    let mut out = vec![0u8; video.len()];
    let (mut cb_sum, mut cr_sum) = (vec![0f32; cw * ch], vec![0f32; cw * ch]);
    for y in 0..h {
        for x in 0..w {
            let p = &graded[(y * w + x) * 3..(y * w + x) * 3 + 3];
            let (r, g, b) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
            let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            out[y * w + x] = (16.0 + 219.0 * yy).round().clamp(0.0, 255.0) as u8;
            cb_sum[(y / 2) * cw + x / 2] += 128.0 + 224.0 * (b - yy) / 1.8556;
            cr_sum[(y / 2) * cw + x / 2] += 128.0 + 224.0 * (r - yy) / 1.5748;
        }
    }
    for i in 0..cw * ch {
        out[w * h + i] = (cb_sum[i] * 0.25).round().clamp(0.0, 255.0) as u8;
        out[w * h + cw * ch + i] = (cr_sum[i] * 0.25).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Checks the card against the processor on a test picture. The two must agree to within one level.
pub fn self_test(grader: &Arc<GpuGrader>) -> Result<(), String> {
    let grade = Grade { contrast: 0.3, temperature: 0.25, saturation: 0.2, teal_orange: 0.4, shadows: 0.2, fade: 0.03, ..Grade::default() };
    let lut = Lut::build(33, &grade, None);
    let (w, h) = (128usize, 64usize);
    let mut seed = 12345u32;
    // A brightness ramp mixed with noise, so every corner of the colour range is reached.
    let video: Vec<u8> = (0..w * h * 3 / 2)
        .map(|i| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (seed >> 24) as u8;
            if i < w * h { ((16 + (i % w) * 219 / w).min(235) as u8) / 2 + noise / 2 } else { noise }
        })
        .collect();
    let expected = reference(&lut, w, h, &video);
    let mut got = video.clone();
    grader.session(&lut, w, h)?.grade(&mut got)?;
    let worst = got.iter().zip(&expected).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    if worst > 1 { Err(format!("its colours differ from the processor's by up to {worst} levels")) } else { Ok(()) }
}

static SHARED: OnceLock<Result<Arc<GpuGrader>, String>> = OnceLock::new();

/// The graphics card, started once and checked once. The error says why it can't be used.
pub fn shared() -> Result<Arc<GpuGrader>, String> {
    SHARED
        .get_or_init(|| {
            let grader = Arc::new(GpuGrader::new()?);
            self_test(&grader).map_err(|e| format!("{} failed its accuracy check: {e}", grader.name()))?;
            Ok(grader)
        })
        .clone()
}

/// Makes one finished video with the colour grade done on the graphics card.
pub fn export(tools: &Tools, job: &ExportJob, lut: &Lut, grader: &Arc<GpuGrader>, encoder: Encoder, cancel: &AtomicBool, progress: &mut impl FnMut(f32)) -> Result<(), String> {
    let (graph, width, height) = media::geometry_graph(job);
    let fps = job.probe.fps.clamp(1.0, 240.0);
    let length = (job.end - job.start).max(0.05);
    let session = grader.session(lut, width as usize, height as usize)?;
    let frame_bytes = session.frame_bytes();
    let expected = (length * fps).max(1.0);
    let part = job.output.with_extension("part.mp4");

    let mut decoder = media::hidden(&tools.ffmpeg)
        .args(["-v", "error", "-nostdin", "-ss", &format!("{:.3}", job.start), "-t", &format!("{length:.3}"), "-i"])
        .arg(&job.input)
        .args(["-filter_complex", &graph, "-map", "[v]", "-an", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start the video tools: {e}"))?;

    let mut cmd = media::hidden(&tools.ffmpeg);
    cmd.current_dir(&job.workdir)
        .args(["-y", "-v", "error", "-nostats", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-video_size", &format!("{width}x{height}"), "-framerate", &format!("{fps:.5}"), "-i", "pipe:0"]);
    if job.probe.has_audio {
        cmd.args(["-ss", &format!("{:.3}", job.start), "-t", &format!("{length:.3}"), "-i"]).arg(&job.input);
    }
    let sharpen = media::sharpen_filter(job.finish).map_or(String::new(), |s| format!(",{s}"));
    cmd.args(["-vf", &format!("{}{sharpen}", media::TAGS), "-map", "0:v"]);
    if job.probe.has_audio {
        cmd.args(["-map", "1:a:0?"]);
    }
    cmd.args(media::video_args(encoder, ((fps / 2.0).round() as u32).max(1))).args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]);
    if job.probe.has_audio {
        if job.finish.loudness {
            cmd.args(["-af", "loudnorm=I=-14:TP=-1.5:LRA=11"]);
        }
        cmd.args(["-c:a", "aac", "-b:a", "256k", "-ar", "48000"]);
    }
    cmd.args(["-movflags", "+faststart"]).arg(&part).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut encoder_child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = decoder.kill();
            return Err(format!("Couldn't start the video tools: {e}"));
        }
    };

    let mut decoder_out = decoder.stdout.take().expect("stdout is piped");
    let mut encoder_in = encoder_child.stdin.take().expect("stdin is piped");
    let decoder_errors = drain(decoder.stderr.take());
    let encoder_errors = drain(encoder_child.stderr.take());

    // A small pool of frame buffers is passed around, so nothing is allocated per frame.
    let (pool_tx, pool_rx) = mpsc::channel::<Vec<u8>>();
    for _ in 0..6 {
        let _ = pool_tx.send(vec![0u8; frame_bytes]);
    }
    let (raw_tx, raw_rx) = mpsc::sync_channel::<Vec<u8>>(3);
    let (done_tx, done_rx) = mpsc::sync_channel::<Vec<u8>>(3);
    let frames = Arc::new(AtomicU64::new(0));
    // Time each stage spends working (not waiting), for VE_PROFILE=1.
    let (read_ns, grade_ns, write_ns) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let began = std::time::Instant::now();

    let read_time = read_ns.clone();
    let reader = std::thread::spawn(move || {
        while let Ok(mut buffer) = pool_rx.recv() {
            let t = std::time::Instant::now();
            let ok = decoder_out.read_exact(&mut buffer).is_ok();
            read_time.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
            if !ok || raw_tx.send(buffer).is_err() {
                break;
            }
        }
    });
    let grade_time = grade_ns.clone();
    let grader_thread = std::thread::spawn(move || -> Result<(), String> {
        let mut in_flight: std::collections::VecDeque<(Vec<u8>, Pending)> = std::collections::VecDeque::new();
        let mut next = 0usize;
        let finish = |mut buffer: Vec<u8>, pending: Pending| -> Result<bool, String> {
            let t = std::time::Instant::now();
            session.collect(pending, &mut buffer)?;
            grade_time.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
            Ok(done_tx.send(buffer).is_ok())
        };
        for buffer in raw_rx {
            let pending = session.submit(next, &buffer)?;
            next += 1;
            in_flight.push_back((buffer, pending));
            if in_flight.len() == Session::SLOTS {
                let (buffer, pending) = in_flight.pop_front().expect("just checked");
                if !finish(buffer, pending)? {
                    return Ok(());
                }
            }
        }
        while let Some((buffer, pending)) = in_flight.pop_front() {
            if !finish(buffer, pending)? {
                break;
            }
        }
        Ok(())
    });
    let counted = frames.clone();
    let write_time = write_ns.clone();
    let writer = std::thread::spawn(move || -> Result<(), String> {
        for buffer in done_rx {
            let t = std::time::Instant::now();
            encoder_in.write_all(&buffer).map_err(|e| format!("the encoder stopped early ({e})"))?;
            write_time.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
            counted.fetch_add(1, Ordering::Relaxed);
            let _ = pool_tx.send(buffer);
        }
        // Dropping the pipe tells the encoder the video is complete.
        Ok(())
    });

    let mut cancelled = false;
    while !writer.is_finished() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            let _ = decoder.kill();
            let _ = encoder_child.kill();
            break;
        }
        progress((frames.load(Ordering::Relaxed) as f64 / expected).min(1.0) as f32);
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = reader.join();
    let graded = grader_thread.join().unwrap_or_else(|_| Err("the colour stage crashed".into()));
    let written = writer.join().unwrap_or_else(|_| Err("the writing stage crashed".into()));
    let decoder_status = decoder.wait();
    let encoder_status = encoder_child.wait();
    let (decoder_text, encoder_text) = (decoder_errors.join().unwrap_or_default(), encoder_errors.join().unwrap_or_default());

    if std::env::var_os("VE_PROFILE").is_some() {
        let (n, total) = (frames.load(Ordering::Relaxed).max(1), began.elapsed().as_secs_f64());
        let secs = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1e9;
        eprintln!("PROFILE {n} frames in {total:.1} s ({:.0} fps). Time spent: reading {:.1} s ({:.1} ms/frame), grading {:.1} s ({:.1} ms/frame), writing {:.1} s ({:.1} ms/frame)", n as f64 / total, secs(&read_ns), secs(&read_ns) * 1000.0 / n as f64, secs(&grade_ns), secs(&grade_ns) * 1000.0 / n as f64, secs(&write_ns), secs(&write_ns) * 1000.0 / n as f64);
    }
    let fail = |why: String| {
        std::fs::remove_file(&part).ok();
        Err(why)
    };
    if cancelled {
        return fail("Cancelled".into());
    }
    let last = |text: &str| text.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string);
    if let Err(e) = graded.and(written) {
        return fail(last(&encoder_text).or_else(|| last(&decoder_text)).unwrap_or(e));
    }
    if !encoder_status.is_ok_and(|s| s.success()) {
        return fail(last(&encoder_text).unwrap_or_else(|| "The video tools stopped unexpectedly.".into()));
    }
    if !decoder_status.is_ok_and(|s| s.success()) && frames.load(Ordering::Relaxed) == 0 {
        return fail(last(&decoder_text).unwrap_or_else(|| "The video couldn't be read.".into()));
    }
    if frames.load(Ordering::Relaxed) == 0 {
        return fail("No picture could be read from the video.".into());
    }
    progress(1.0);
    std::fs::rename(&part, &job.output).map_err(|e| format!("Couldn't save the finished file: {e}"))
}

/// Collects a program's error text on its own thread, so a full pipe can never stall it.
fn drain(pipe: Option<std::process::ChildStderr>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::tests::{make_video, scratch};
    use crate::media::{Finish, Framing, GrabOptions, Shape, grab_frame, probe};

    fn grader() -> Option<Arc<GpuGrader>> {
        match shared() {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("skipped: no usable graphics card ({e})");
                None
            }
        }
    }

    /// Which stage limits a card export: `cargo test card_profile -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn card_profile_of_a_one_minute_export() {
        let (Some(tools), Some(g)) = (Tools::find(), grader()) else { return };
        let dir = scratch("gpu-profile");
        let input = dir.join("in.mp4");
        let status = media::hidden(&tools.ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=60:d=60", "-f", "lavfi", "-i", "sine=f=330:d=60", "-c:v", "libx264", "-preset", "veryfast", "-crf", "16", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&input)
            .status()
            .unwrap();
        assert!(status.success());
        let lut = Arc::new(Lut::build(33, &Grade { contrast: 0.2, ..Grade::default() }, None));
        lut.write_cube(&dir.join("lut.cube")).unwrap();
        for encoder in [Encoder::Nvidia, Encoder::Cpu] {
            let mut j = job(&dir, &input, Shape::Wide { height: 0 }, "out.mp4", Some(lut.clone()));
            j.start = 0.0;
            j.end = 60.0;
            let began = std::time::Instant::now();
            eprintln!("--- {encoder:?}");
            unsafe { std::env::set_var("VE_PROFILE", "1") };
            export(&tools, &j, &lut, &g, encoder, &AtomicBool::new(false), &mut |_| {}).unwrap();
            eprintln!("    total {:.1} s", began.elapsed().as_secs_f64());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_card_grades_exactly_like_the_processor() {
        let Some(g) = grader() else { return };
        eprintln!("graphics card: {}", g.name());
        self_test(&g).unwrap();
    }

    #[test]
    fn odd_sizes_are_refused_not_corrupted() {
        let Some(g) = grader() else { return };
        let lut = Lut::build(5, &Grade::default(), None);
        assert!(g.session(&lut, 3, 3).is_err());
        assert!(g.session(&lut, 0, 10).is_err());
        assert!(g.session(&lut, 854, 480).is_err(), "a width that isn't a multiple of 8 falls back to FFmpeg");
        let s = g.session(&lut, 8, 8).unwrap();
        assert!(s.grade(&mut [0u8; 10]).is_err());
    }

    fn job(dir: &std::path::Path, input: &std::path::Path, shape: Shape, name: &str, lut: Option<Arc<Lut>>) -> ExportJob {
        let p = probe(&Tools::find().unwrap(), input).unwrap();
        ExportJob {
            input: input.to_path_buf(),
            probe: p,
            start: 1.0,
            end: 7.0,
            shape,
            finish: Finish { sharpen: 0.0, denoise: 0.0, loudness: false },
            lut_file: "lut.cube".into(),
            workdir: dir.to_path_buf(),
            output: dir.join(name),
            encoder: Encoder::Cpu,
            gpu_lut: lut,
        }
    }

    /// The same export, graded by FFmpeg and by the graphics card, must look the same and be the same shape.
    #[test]
    fn graphics_card_and_ffmpeg_exports_match() {
        let (Some(tools), Some(g)) = (Tools::find(), grader()) else { return };
        let dir = scratch("gpu-grade");
        let input = dir.join("in.mp4");
        make_video(&tools, &input, "1280x720", 8);
        let grade = Grade { contrast: 0.3, temperature: 0.3, saturation: -0.2, teal_orange: 0.3, ..Grade::default() };
        let lut = Arc::new(Lut::build(33, &grade, None));
        lut.write_cube(&dir.join("lut.cube")).unwrap();
        let cases = [
            ("wide", Shape::Wide { height: 0 }, (1280, 720)),
            ("tall", Shape::Vertical(Framing::Position(0.4)), (1080, 1920)),
            ("blur", Shape::Vertical(Framing::Blur(media::BlurLayout { zoom: 1.4, x: 0.3, y: 0.4 })), (1080, 1920)),
        ];
        let plain = GrabOptions { width: 320, finish: Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
        for (name, shape, size) in cases {
            let ffmpeg_job = job(&dir, &input, shape.clone(), &format!("{name}-ffmpeg.mp4"), None);
            let card_job = job(&dir, &input, shape, &format!("{name}-card.mp4"), Some(lut.clone()));
            media::export(&tools, &ffmpeg_job, &AtomicBool::new(false), |_| {}).unwrap();
            let mut last = 0.0;
            export(&tools, &card_job, &lut, &g, Encoder::Cpu, &AtomicBool::new(false), &mut |f| last = f).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(last > 0.9, "{name}: progress reached only {last}");
            let (a, b) = (probe(&tools, &ffmpeg_job.output).unwrap(), probe(&tools, &card_job.output).unwrap());
            assert_eq!((b.width, b.height), size, "{name}");
            assert!(b.has_audio, "{name}: the card export lost its sound");
            assert_eq!((a.width, a.height), (b.width, b.height), "{name}");
            assert!((a.duration - b.duration).abs() < 0.2, "{name}: {} vs {}", a.duration, b.duration);
            // Colour: the middle of the picture must agree closely.
            for t in [1.0, 3.0, 5.0] {
                let (fa, fb) = (grab_frame(&tools, &a, &ffmpeg_job.output, t, &plain).unwrap(), grab_frame(&tools, &b, &card_job.output, t, &plain).unwrap());
                let diff = fa.rgb.iter().zip(&fb.rgb).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as f64).sum::<f64>() / fa.rgb.len() as f64;
                assert!(diff < 2.0, "{name} at {t}s: the two exports differ by {diff:.2} levels on average");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_cancelled_card_export_leaves_nothing_behind() {
        let (Some(tools), Some(g)) = (Tools::find(), grader()) else { return };
        let dir = scratch("gpu-cancel");
        let input = dir.join("in.mp4");
        make_video(&tools, &input, "640x360", 20);
        let lut = Arc::new(Lut::build(5, &Grade::default(), None));
        lut.write_cube(&dir.join("lut.cube")).unwrap();
        let j = job(&dir, &input, Shape::Wide { height: 0 }, "out.mp4", Some(lut.clone()));
        let err = export(&tools, &j, &lut, &g, Encoder::Cpu, &AtomicBool::new(true), &mut |_| {}).unwrap_err();
        assert_eq!(err, "Cancelled");
        assert!(!j.output.exists() && !j.output.with_extension("part.mp4").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
