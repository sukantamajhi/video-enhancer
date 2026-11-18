//! Everything that touches video files. Decoding and encoding are done by the
//! FFmpeg tools, which the app finds on the computer or downloads once.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

pub const TOOLS_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
const EXE: &str = std::env::consts::EXE_SUFFIX;

#[derive(Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// Where the app keeps its downloaded tools.
pub fn home() -> PathBuf {
    if let Some(dir) = std::env::var_os("VIDEO_ENHANCER_HOME") {
        return dir.into();
    }
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        return Path::new(&dir).join("VideoEnhancer");
    }
    std::env::var_os("HOME").map_or_else(std::env::temp_dir, |d| Path::new(&d).join(".video-enhancer"))
}

pub fn hidden(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes while video work runs.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

#[derive(Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Tools {
    pub fn find() -> Option<Self> {
        let in_dir = |dir: PathBuf| {
            let tools = Self {
                ffmpeg: dir.join(format!("ffmpeg{EXE}")),
                ffprobe: dir.join(format!("ffprobe{EXE}")),
            };
            (tools.ffmpeg.is_file() && tools.ffprobe.is_file()).then_some(tools)
        };
        if let Some(found) = in_dir(home().join("tools")) {
            return Some(found);
        }
        if let Some(found) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).and_then(in_dir) {
            return Some(found);
        }
        let on_path = Self { ffmpeg: "ffmpeg".into(), ffprobe: "ffprobe".into() };
        let works = |p: &Path| hidden(p).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        (works(&on_path.ffmpeg) && works(&on_path.ffprobe)).then_some(on_path)
    }

    /// Downloads and checks the video tools, then keeps only the two programs needed.
    pub fn download(mut progress: impl FnMut(f32)) -> Result<Self, String> {
        if !cfg!(windows) {
            return Err("Please install FFmpeg with your system's package manager, then restart the app.".into());
        }
        let dir = home().join("tools");
        std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
        let expected = ureq::get(format!("{TOOLS_URL}.sha256"))
            .call()
            .map_err(|e| format!("Couldn't reach the download site: {e}"))?
            .into_body()
            .read_to_string()
            .map_err(|e| format!("Couldn't read the checksum: {e}"))?;
        let expected = expected.split_whitespace().next().unwrap_or("").to_lowercase();

        let response = ureq::get(TOOLS_URL).call().map_err(|e| format!("Couldn't download the video tools: {e}"))?;
        let total = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let zip_path = dir.join("download.part");
        let mut file = std::fs::File::create(&zip_path).map_err(|e| e.to_string())?;
        let mut body = response.into_body().into_reader();
        let mut buffer = vec![0u8; 256 * 1024];
        let mut hash = Sha256::new();
        let mut seen = 0u64;
        loop {
            let n = body.read(&mut buffer).map_err(|e| format!("The download stopped: {e}"))?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            seen += n as u64;
            if total > 0 {
                progress(seen as f32 / total as f32);
            }
        }
        drop(file);
        let actual: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if expected.is_empty() || actual != expected {
            std::fs::remove_file(&zip_path).ok();
            return Err("The downloaded video tools failed their safety check, so they were deleted. Please try again.".into());
        }

        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().map_err(|e| e.to_string())?.replace('\\', "/");
            let wanted = ["ffmpeg.exe", "ffprobe.exe"].into_iter().find(|w| name.ends_with(&format!("/bin/{w}")));
            if let Some(wanted) = wanted {
                let mut out = std::fs::File::create(dir.join(wanted)).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
            }
        }
        std::fs::remove_file(&zip_path).ok();
        Self::find().ok_or_else(|| "The video tools were downloaded but couldn't be started.".into())
    }
}

#[derive(Clone, Debug)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub fps: f64,
    pub has_audio: bool,
    pub hdr: bool,
    pub untagged: bool,
}

impl Probe {
    pub fn aspect(&self) -> f64 {
        self.width as f64 / self.height.max(1) as f64
    }

    pub fn is_wide(&self) -> bool {
        (self.aspect() - 16.0 / 9.0).abs() < 0.02
    }

    /// The share of the picture's width that a 9:16 crop keeps (1.0 if the crop must cut height instead).
    pub fn vertical_share(&self) -> f64 {
        ((self.height as f64 * 9.0 / 16.0) / self.width as f64).min(1.0)
    }
}

pub fn probe(tools: &Tools, path: &Path) -> Result<Probe, String> {
    let output = hidden(&tools.ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    if !output.status.success() {
        return Err("This file couldn't be read as a video.".into());
    }
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let streams = info["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64().unwrap_or(0) == 0)
        .ok_or("This file has no video picture in it.")?;

    let (mut width, mut height) = (video["width"].as_u64().unwrap_or(0) as u32, video["height"].as_u64().unwrap_or(0) as u32);
    // Phone videos are often stored sideways with a rotation note.
    let rotation = video["side_data_list"]
        .as_array()
        .and_then(|list| list.iter().find_map(|d| d["rotation"].as_f64()))
        .or_else(|| video["tags"]["rotate"].as_str().and_then(|r| r.parse().ok()))
        .unwrap_or(0.0);
    if (rotation.abs() - 90.0).abs() < 1.0 {
        std::mem::swap(&mut width, &mut height);
    }
    if width == 0 || height == 0 {
        return Err("This video's picture size couldn't be read.".into());
    }

    let rate = |s: &str| {
        let mut parts = s.split('/');
        let n: f64 = parts.next()?.parse().ok()?;
        let d: f64 = parts.next().unwrap_or("1").parse().ok()?;
        (d > 0.0 && n > 0.0).then_some(n / d)
    };
    let fps = video["avg_frame_rate"].as_str().and_then(rate).or_else(|| video["r_frame_rate"].as_str().and_then(rate)).unwrap_or(30.0);
    let duration = info["format"]["duration"].as_str().and_then(|d| d.parse().ok()).or_else(|| video["duration"].as_str().and_then(|d| d.parse().ok())).unwrap_or(0.0);
    let transfer = video["color_transfer"].as_str().unwrap_or("");
    let space = video["color_space"].as_str().unwrap_or("");
    Ok(Probe {
        width,
        height,
        duration,
        fps,
        has_audio: streams.iter().any(|s| s["codec_type"] == "audio"),
        hdr: transfer == "smpte2084" || transfer == "arib-std-b67",
        untagged: space.is_empty() || space == "unknown",
    })
}

/// Sharpening, noise cleanup and sound levelling, shared by preview and export.
#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Finish {
    pub sharpen: f32,
    pub denoise: f32,
    pub loudness: bool,
}

impl Default for Finish {
    fn default() -> Self {
        Self { sharpen: 0.0, denoise: 0.0, loudness: true }
    }
}

pub fn even(v: f64) -> u32 {
    ((v / 2.0).round() as u32).max(1) * 2
}

/// Turns any source into the plain video colour the colour table expects.
pub fn base_filters(p: &Probe) -> Vec<String> {
    if p.hdr {
        vec!["zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p".into()]
    } else if p.untagged {
        let (space, prim) = if p.height >= 720 { ("bt709", "bt709") } else { ("smpte170m", "smpte170m") };
        vec![format!("setparams=colorspace={space}:color_primaries={prim}")]
    } else {
        Vec::new()
    }
}

fn denoise_filter(f: Finish) -> Option<String> {
    (f.denoise > 0.01).then(|| {
        let d = f.denoise as f64;
        format!("hqdn3d={:.2}:{:.2}:{:.2}:{:.2}", d * 6.0, d * 4.0, d * 8.0, d * 6.0)
    })
}

pub fn sharpen_filter(f: Finish) -> Option<String> {
    (f.sharpen > 0.01).then(|| format!("unsharp=5:5:{:.2}:5:5:0", f.sharpen as f64 * 1.2))
}

fn wide_crop(p: &Probe) -> Option<String> {
    if p.is_wide() {
        return None;
    }
    let (w, h) = (p.width as f64, p.height as f64);
    Some(if p.aspect() > 16.0 / 9.0 { format!("crop={}:{}", even(h * 16.0 / 9.0), even(h)) } else { format!("crop={}:{}", even(w), even(w * 9.0 / 16.0)) })
}

fn picture_chain(p: &Probe, f: Finish) -> Vec<String> {
    let mut chain = base_filters(p);
    chain.extend(denoise_filter(f));
    chain.extend(sharpen_filter(f));
    chain
}

fn read_frames(tools: &Tools, path: &Path, t: f64, chain: &str) -> Result<Vec<u8>, String> {
    let output = hidden(&tools.ffmpeg)
        .args(["-v", "error", "-ss", &format!("{:.3}", t.max(0.0)), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", chain, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub struct GrabOptions {
    pub width: u32,
    pub finish: Finish,
    pub crop_wide: bool,
}

/// One decoded picture at the given time.
pub fn grab_frame(tools: &Tools, p: &Probe, path: &Path, t: f64, o: &GrabOptions) -> Result<Frame, String> {
    let mut chain = picture_chain(p, o.finish);
    let (mut sw, mut sh) = (p.width as f64, p.height as f64);
    if o.crop_wide
        && let Some(crop) = wide_crop(p)
    {
        chain.push(crop);
        if p.aspect() > 16.0 / 9.0 {
            sw = sh * 16.0 / 9.0;
        } else {
            sh = sw * 9.0 / 16.0;
        }
    }
    let (w, h) = (even(o.width as f64), even(o.width as f64 * sh / sw));
    chain.push(format!("scale={w}:{h}:flags=bicubic"));
    let chain = chain.join(",");

    let need = w as usize * h as usize * 3;
    // Seeking to the very end of a file can land past the last picture, so step back and retry.
    for back in [0.0, 0.5, 2.0] {
        let bytes = read_frames(tools, path, (t - back).min(p.duration - 0.05), &chain)?;
        if bytes.len() >= need {
            return Ok(Frame { width: w as usize, height: h as usize, rgb: bytes[..need].to_vec() });
        }
    }
    Err("No picture could be read at this point in the video.".into())
}

/// Evenly spread moments across a part of the video, so analysis sees all of it.
pub fn spread_times(start: f64, end: f64, count: usize) -> Vec<f64> {
    let length = (end - start).max(0.0);
    (0..count).map(|i| start + length * (i as f64 + 0.5) / count as f64).collect()
}

/// Silent preview playback: decodes small frames in real time on another thread.
pub struct Player {
    pub rx: Receiver<(f64, Frame)>,
    stop: Arc<AtomicBool>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

const PLAY_FPS: f64 = 24.0;

pub fn play(tools: &Tools, p: &Probe, path: &Path, start: f64, wake: impl Fn() + Send + 'static) -> Player {
    let (tx, rx): (SyncSender<(f64, Frame)>, _) = mpsc::sync_channel(2);
    let stop = Arc::new(AtomicBool::new(false));
    let (tools, path, stop_flag) = (tools.clone(), path.to_path_buf(), stop.clone());
    let (w, h) = (even(640.0), even(640.0 * p.height as f64 / p.width as f64));
    let mut chain = base_filters(p);
    chain.push(format!("fps={PLAY_FPS}"));
    chain.push(format!("scale={w}:{h}:flags=bilinear"));
    let chain = chain.join(",");
    std::thread::spawn(move || {
        let child = hidden(&tools.ffmpeg)
            .args(["-v", "error", "-ss", &format!("{start:.3}"), "-i"])
            .arg(&path)
            .args(["-vf", &chain, "-an", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            wake();
            return;
        };
        let mut out = child.stdout.take().expect("stdout is piped");
        let size = w as usize * h as usize * 3;
        let began = Instant::now();
        let mut index = 0u64;
        loop {
            let mut rgb = vec![0u8; size];
            if stop_flag.load(Ordering::Relaxed) || out.read_exact(&mut rgb).is_err() {
                break;
            }
            let at = index as f64 / PLAY_FPS;
            if let Some(wait) = Duration::from_secs_f64(at).checked_sub(began.elapsed()) {
                std::thread::sleep(wait);
            }
            index += 1;
            // Dropping a frame is fine when the screen can't keep up.
            let frame = Frame { width: w as usize, height: h as usize, rgb };
            if let Err(mpsc::TrySendError::Disconnected(_)) = tx.try_send((start + at, frame)) {
                break;
            }
            wake();
        }
        let _ = child.kill();
        let _ = child.wait();
        drop(tx);
        wake();
    });
    Player { rx, stop }
}

/// Finds where the action is over time, as (seconds, 0..=1 of the free crop travel).
pub fn analyse_action(tools: &Tools, p: &Probe, path: &Path) -> Result<Vec<(f64, f32)>, String> {
    let (w, h) = (160usize, 90usize);
    let fps = (240.0 / p.duration.max(1.0)).clamp(0.05, 2.0);
    let mut chain = base_filters(p);
    chain.push(format!("fps={fps:.4}"));
    chain.push(format!("scale={w}:{h}:flags=bilinear"));
    let mut child = hidden(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", &chain.join(","), "-an", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    let mut out = child.stdout.take().expect("stdout is piped");
    let share = p.vertical_share();
    let mut raw: Vec<f32> = Vec::new();
    let mut previous: Option<Vec<u8>> = None;
    let mut frame = vec![0u8; w * h];
    while out.read_exact(&mut frame).is_ok() {
        let mut column = vec![0f32; w];
        for y in 1..h {
            for x in 1..w {
                let i = y * w + x;
                let edges = (frame[i] as f32 - frame[i - 1] as f32).abs() + (frame[i] as f32 - frame[i - w] as f32).abs();
                let motion = previous.as_ref().map_or(0.0, |prev| (frame[i] as f32 - prev[i] as f32).abs());
                column[x] += edges * 0.35 + motion * 1.5;
            }
        }
        // Slide the crop window across and keep where it catches the most energy,
        // with a slight pull to the middle so quiet shots stay centred.
        let window = ((share * w as f64) as usize).clamp(1, w);
        let mut sum: f32 = column[..window].iter().sum();
        let (mut best, mut best_x) = (f32::MIN, 0usize);
        for x in 0..=(w - window) {
            if x > 0 {
                sum += column[x + window - 1] - column[x - 1];
            }
            let middle = (w - window) as f32 / 2.0;
            let score = sum * (1.0 - 0.15 * ((x as f32 - middle).abs() / middle.max(1.0)));
            if score > best {
                best = score;
                best_x = x;
            }
        }
        raw.push(if w > window { best_x as f32 / (w - window) as f32 } else { 0.5 });
        previous = Some(frame.clone());
    }
    let _ = child.wait();
    if raw.is_empty() {
        return Err("The video couldn't be read to follow the action.".into());
    }

    // Heavy smoothing: a calm virtual camera instead of a jittery one.
    let span = (fps * 2.5).ceil() as usize;
    let smooth: Vec<f32> = (0..raw.len())
        .map(|i| {
            let (s, e) = (i.saturating_sub(span), (i + span + 1).min(raw.len()));
            raw[s..e].iter().sum::<f32>() / (e - s) as f32
        })
        .collect();
    let stride = (smooth.len() / 60).max(1);
    let mut points: Vec<(f64, f32)> = smooth.iter().enumerate().step_by(stride).map(|(i, v)| (i as f64 / fps, *v)).collect();
    if let Some(last) = smooth.last() {
        points.push((p.duration, *last));
    }
    Ok(points)
}

/// Reads the crop position at a time from the path found by [`analyse_action`].
pub fn action_at(points: &[(f64, f32)], t: f64) -> f32 {
    let Some(first) = points.first() else { return 0.5 };
    if t <= first.0 {
        return first.1;
    }
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t < b.0 {
            let f = ((t - a.0) / (b.0 - a.0).max(1e-6)) as f32;
            return a.1 + (b.1 - a.1) * f;
        }
    }
    points.last().map_or(0.5, |p| p.1)
}

pub fn action_expression(points: &[(f64, f32)], start: f64) -> String {
    let t = format!("(t+{start:.3})");
    let mut terms = Vec::new();
    for pair in points.windows(2) {
        let ((a, x0), (b, x1)) = (pair[0], pair[1]);
        if b > a {
            terms.push(format!("gte({t},{a:.3})*lt({t},{b:.3})*({x0:.4}+{:.5}*({t}-{a:.3}))", (x1 - x0) as f64 / (b - a)));
        }
    }
    let last = points.last().map_or(0.5, |p| p.1);
    let end = points.last().map_or(0.0, |p| p.0);
    terms.push(format!("gte({t},{end:.3})*{last:.4}"));
    format!("(in_w-out_w)*({})", terms.join("+"))
}

/// How the main picture sits on the blurred background of a 9:16 video.
/// Everything is centred by default, with the picture zoomed in a little.
#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct BlurLayout {
    /// 1.0 shows the whole width; larger zooms in (and trims the sides).
    pub zoom: f32,
    /// Which part of a zoomed picture is shown, 0 = left edge, 1 = right edge.
    pub x: f32,
    /// Where the picture sits between the top and bottom of the frame.
    pub y: f32,
}

impl Default for BlurLayout {
    fn default() -> Self {
        Self { zoom: 1.2, x: 0.5, y: 0.5 }
    }
}

impl BlurLayout {
    pub fn clamped(self) -> Self {
        Self { zoom: self.zoom.clamp(1.0, 2.5), x: self.x.clamp(0.0, 1.0), y: self.y.clamp(0.0, 1.0) }
    }
}

/// The filters that put the main picture over its own blurred copy. `k` keeps
/// the labels unique when several of these sit in one graph.
pub fn blur_stack(layout: BlurLayout, k: &str) -> String {
    let l = layout.clamped();
    let width = even(1080.0 * l.zoom as f64);
    format!(
        "split[a{k}][b{k}];[a{k}]scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920,boxblur=40:2[bg{k}];\
         [b{k}]scale={width}:-2:flags=lanczos,crop='min(iw,1080)':'min(ih,1920)':'(iw-out_w)*{:.4}':'(ih-out_h)*0.5'[fg{k}];\
         [bg{k}][fg{k}]overlay=0:'(H-h)*{:.4}':format=gbrp",
        l.x, l.y
    )
}

#[derive(Clone)]
pub enum Framing {
    Position(f32),
    Follow(Vec<(f64, f32)>),
    Blur(BlurLayout),
}

#[derive(Clone)]
pub enum Shape {
    /// `height` 0 keeps the source's height.
    Wide { height: u32 },
    Vertical(Framing),
}

pub struct ExportJob {
    pub input: PathBuf,
    pub probe: Probe,
    pub start: f64,
    pub end: f64,
    pub shape: Shape,
    pub finish: Finish,
    /// The colour table's file name inside `workdir`.
    pub lut_file: String,
    pub workdir: PathBuf,
    pub output: PathBuf,
    pub encoder: Encoder,
    /// When set, the colour grade is done on the graphics card with this table instead of in FFmpeg.
    pub gpu_lut: Option<Arc<crate::grade::Lut>>,
}

/// What really made a finished video.
#[derive(Clone, Copy, Debug)]
pub struct Made {
    pub encoder: Encoder,
    pub gpu_grade: bool,
}

pub const TAGS: &str = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv";

/// What writes the finished video: the processor, or the graphics card's own video chip.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Encoder {
    Cpu,
    Nvidia,
    Amd,
    Intel,
}

impl Encoder {
    pub const HARDWARE: [Encoder; 3] = [Encoder::Nvidia, Encoder::Amd, Encoder::Intel];

    pub fn name(self) -> &'static str {
        match self {
            Encoder::Cpu => "CPU (best quality for the size, slowest)",
            Encoder::Nvidia => "NVIDIA GPU (NVENC)",
            Encoder::Amd => "AMD GPU (AMF)",
            Encoder::Intel => "Intel GPU (Quick Sync)",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Encoder::Cpu => "CPU",
            Encoder::Nvidia => "NVIDIA GPU",
            Encoder::Amd => "AMD GPU",
            Encoder::Intel => "Intel GPU",
        }
    }

    fn codec(self) -> &'static str {
        match self {
            Encoder::Cpu => "libx264",
            Encoder::Nvidia => "h264_nvenc",
            Encoder::Amd => "h264_amf",
            Encoder::Intel => "h264_qsv",
        }
    }
}

/// What the person picked. Automatic uses the first graphics card that works.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum EncoderChoice {
    #[default]
    Auto,
    Cpu,
    Nvidia,
    Amd,
    Intel,
}

impl EncoderChoice {
    /// The encoder to use, given which ones really work on this computer.
    pub fn resolve(self, available: &[Encoder]) -> Encoder {
        let wanted = match self {
            EncoderChoice::Auto => Encoder::HARDWARE.into_iter().find(|e| available.contains(e)),
            EncoderChoice::Cpu => None,
            EncoderChoice::Nvidia => Some(Encoder::Nvidia),
            EncoderChoice::Amd => Some(Encoder::Amd),
            EncoderChoice::Intel => Some(Encoder::Intel),
        };
        wanted.filter(|e| available.contains(e)).unwrap_or(Encoder::Cpu)
    }
}

/// Which graphics-card encoders really work here. Each is tried on a tiny test
/// picture, because FFmpeg lists an encoder even on a computer without that card.
pub fn detect_encoders(tools: &Tools) -> Vec<Encoder> {
    Encoder::HARDWARE
        .into_iter()
        .filter(|e| {
            hidden(&tools.ffmpeg)
                .args(["-v", "error", "-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=0.4", "-pix_fmt", "yuv420p", "-c:v", e.codec(), "-f", "null", "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .collect()
}

/// The video settings for each encoder, tuned for near-identical quality.
/// Graphics-card encoders are much faster but make slightly larger files.
pub fn video_args(encoder: Encoder, gop: u32) -> Vec<String> {
    let gop = gop.max(1).to_string();
    let list: &[&str] = match encoder {
        Encoder::Cpu => &["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-profile:v", "high", "-bf", "2", "-flags", "+cgop", "-g"],
        Encoder::Nvidia => &["-c:v", "h264_nvenc", "-preset", "p2", "-rc", "vbr", "-cq", "19", "-b:v", "0", "-spatial-aq", "1", "-profile:v", "high", "-bf", "2", "-g"],
        Encoder::Amd => &["-c:v", "h264_amf", "-usage", "high_quality", "-quality", "quality", "-rc", "cqp", "-qp_i", "18", "-qp_p", "20", "-qp_b", "22", "-profile:v", "high", "-g"],
        Encoder::Intel => &["-c:v", "h264_qsv", "-preset", "slower", "-global_quality", "19", "-profile:v", "high", "-g"],
    };
    list.iter().map(|a| a.to_string()).chain([gop]).collect()
}

fn export_graph(job: &ExportJob) -> String {
    let p = &job.probe;
    // The colour grade is the most expensive step, so it runs last, on the final picture.
    // It must run last: scaling or blurring an already graded (RGB) picture shifts its colours.
    let mut pre = base_filters(p);
    pre.extend(denoise_filter(job.finish));
    let lead = if pre.is_empty() { "[0:v]".to_string() } else { format!("[0:v]{},", pre.join(",")) };
    let grade = format!("format=gbrp,lut3d=file={}:interp=tetrahedral", job.lut_file);
    // Back to video colour. Sharpening works on the brightness picture only, after the conversion.
    let finish = format!("scale=out_color_matrix=bt709:out_range=tv:flags=lanczos,format=yuv420p,{TAGS}{}", sharpen_filter(job.finish).map_or(String::new(), |s| format!(",{s}")));
    let (w, h) = (p.width as f64, p.height as f64);
    match &job.shape {
        Shape::Wide { height } => {
            let th = if *height == 0 { even(if p.is_wide() { h } else { h.min(w * 9.0 / 16.0) }) } else { *height };
            let tw = even(th as f64 * 16.0 / 9.0);
            let crop = wide_crop(p).map_or(String::new(), |c| format!("{c},"));
            // Scaling must come before the grade: scaling a graded (RGB) picture shifts its colours.
            format!("{lead}{crop}scale={tw}:{th}:flags=lanczos,{grade},{finish}[v]")
        }
        Shape::Vertical(Framing::Blur(layout)) => format!("{lead}{},{grade},{finish}[v]", blur_stack(*layout, "")),
        Shape::Vertical(framing) => {
            let crop = if p.vertical_share() >= 1.0 {
                // Narrower than 9:16 already, so trim the height instead.
                format!("crop={}:{}", even(w), even(w * 16.0 / 9.0))
            } else {
                let cw = even(h * 9.0 / 16.0);
                let x = match framing {
                    Framing::Position(f) => format!("(in_w-out_w)*{f:.4}"),
                    Framing::Follow(points) => action_expression(points, job.start),
                    Framing::Blur(_) => unreachable!("handled above"),
                };
                format!("crop={cw}:{}:'{x}':0", even(h))
            };
            format!("{lead}{crop},scale=1080:1920:flags=lanczos,{grade},{finish}[v]")
        }
    }
}

/// The picture the colour grade is applied to: decoded, cropped and scaled to the final size, in standard
/// HD video colour (BT.709, limited range) at a steady frame rate, as raw frames. Returns the filter graph
/// and the frame size.
pub fn geometry_graph(job: &ExportJob) -> (String, u32, u32) {
    let p = &job.probe;
    let mut pre = base_filters(p);
    pre.extend(denoise_filter(job.finish));
    let lead = if pre.is_empty() { "[0:v]".to_string() } else { format!("[0:v]{},", pre.join(",")) };
    // Games and phones often record at an uneven rate; the card path needs a steady one.
    let steady = format!("fps={:.5}", p.fps.clamp(1.0, 240.0));
    // Whatever the source is, the card is handed standard HD video colour.
    let standard = "scale=out_color_matrix=bt709:out_range=tv:flags=bicubic,format=yuv420p";
    let (w, h) = (p.width as f64, p.height as f64);
    match &job.shape {
        Shape::Wide { height } => {
            let th = if *height == 0 { even(if p.is_wide() { h } else { h.min(w * 9.0 / 16.0) }) } else { *height };
            let tw = even(th as f64 * 16.0 / 9.0);
            let crop = wide_crop(p).map_or(String::new(), |c| format!("{c},"));
            (format!("{lead}{crop}scale={tw}:{th}:flags=lanczos,{steady},{standard}[v]"), tw, th)
        }
        Shape::Vertical(Framing::Blur(layout)) => (format!("{lead}{},{steady},{standard}[v]", blur_stack(*layout, "")), 1080, 1920),
        Shape::Vertical(framing) => {
            let crop = if p.vertical_share() >= 1.0 {
                format!("crop={}:{}", even(w), even(w * 16.0 / 9.0))
            } else {
                let x = match framing {
                    Framing::Position(f) => format!("(in_w-out_w)*{f:.4}"),
                    Framing::Follow(points) => action_expression(points, job.start),
                    Framing::Blur(_) => unreachable!("handled above"),
                };
                format!("crop={}:{}:'{x}':0", even(h * 9.0 / 16.0), even(h))
            };
            (format!("{lead}{crop},scale=1080:1920:flags=lanczos,{steady},{standard}[v]"), 1080, 1920)
        }
    }
}

/// Stops the running program when the cancel flag is raised; returns whether it finished well.
pub fn watch(mut child: Child, cancel: &AtomicBool, total: f64, progress: &mut impl FnMut(f32)) -> Result<(), String> {
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        text
    });
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok())
                && tx.send(us / 1_000_000.0).is_err()
            {
                break;
            }
        }
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(seconds) => progress((seconds / total.max(0.01)).clamp(0.0, 1.0) as f32),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cancelled".into());
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let text = errors.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("The video tools stopped unexpectedly.");
        Err(last.to_string())
    }
}

/// Writes one finished video. The file only appears under its real name once complete.
/// Returns the encoder that really did the work: if a graphics-card encoder fails
/// (an old driver, a card that is busy), the same video is made on the processor instead.
pub fn export(tools: &Tools, job: &ExportJob, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<Made, String> {
    // The graphics card does the colour grade much faster. Whatever goes wrong with it
    // (no card, an old driver, a failed check), the ordinary all-FFmpeg export takes over.
    if let (Some(lut), Ok(grader)) = (&job.gpu_lut, crate::gpu::shared()) {
        match crate::gpu::export(tools, job, lut, &grader, job.encoder, cancel, &mut progress) {
            Ok(()) => return Ok(Made { encoder: job.encoder, gpu_grade: true }),
            Err(e) if e == "Cancelled" => return Err(e),
            Err(_) => {}
        }
    }
    match export_once(tools, job, job.encoder, cancel, &mut progress) {
        Err(e) if e != "Cancelled" && job.encoder != Encoder::Cpu => export_once(tools, job, Encoder::Cpu, cancel, &mut progress).map(|()| Made { encoder: Encoder::Cpu, gpu_grade: false }),
        other => other.map(|()| Made { encoder: job.encoder, gpu_grade: false }),
    }
}

fn export_once(tools: &Tools, job: &ExportJob, encoder: Encoder, cancel: &AtomicBool, progress: &mut impl FnMut(f32)) -> Result<(), String> {
    let part = job.output.with_extension("part.mp4");
    let length = (job.end - job.start).max(0.05);
    let mut cmd = hidden(&tools.ffmpeg);
    cmd.current_dir(&job.workdir).args(["-y", "-v", "error", "-nostats", "-progress", "pipe:1"]);
    cmd.args(["-ss", &format!("{:.3}", job.start), "-t", &format!("{length:.3}"), "-i"])
        .arg(&job.input)
        .args(["-filter_complex", &export_graph(job), "-map", "[v]", "-map", "0:a:0?"])
        .args(video_args(encoder, ((job.probe.fps / 2.0).round() as u32).max(1)))
        .args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]);
    if job.probe.has_audio {
        if job.finish.loudness {
            cmd.args(["-af", "loudnorm=I=-14:TP=-1.5:LRA=11"]);
        }
        cmd.args(["-c:a", "aac", "-b:a", "256k", "-ar", "48000"]);
    }
    cmd.args(["-movflags", "+faststart"]).arg(&part).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    let result = watch(child, cancel, length, progress);
    if result.is_ok() {
        std::fs::rename(&part, &job.output).map_err(|e| format!("Couldn't save the finished file: {e}"))?;
    } else {
        std::fs::remove_file(&part).ok();
    }
    result
}

/// A small 16:9 clip made of short moments from across the video, for an AI model to grade.
pub fn cut_montage(tools: &Tools, p: &Probe, path: &Path, times: &[f64], each: f64, out: &Path) -> Result<(), String> {
    let mut cmd = hidden(&tools.ffmpeg);
    cmd.args(["-y", "-v", "error"]);
    let mut graph = String::new();
    let mut labels = String::new();
    let mut chain = base_filters(p);
    chain.extend(wide_crop(p));
    chain.push("scale=1280:720:flags=bicubic,setsar=1,fps=24".into());
    let chain = chain.join(",");
    for (i, t) in times.iter().enumerate() {
        let start = t.clamp(0.0, (p.duration - each).max(0.0));
        cmd.args(["-ss", &format!("{start:.3}"), "-t", &format!("{each:.3}"), "-i"]).arg(path);
        graph += &format!("[{i}:v]{chain}[v{i}];");
        labels += &format!("[v{i}]");
    }
    graph += &format!("{labels}concat=n={}:v=1:a=0[v]", times.len());
    encode_clip(cmd.args(["-filter_complex", &graph, "-map", "[v]"]), out)
}

fn encode_clip(cmd: &mut Command, out: &Path) -> Result<(), String> {
    let output = cmd
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-maxrate", "8M", "-bufsize", "16M", "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(out)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Finds an unused name, so nothing the person made earlier is overwritten.
pub fn free_name(dir: &Path, stem: &str, suffix: &str, extension: &str) -> PathBuf {
    let first = dir.join(format!("{stem}{suffix}.{extension}"));
    if !first.exists() {
        return first;
    }
    (2..10_000)
        .map(|n| dir.join(format!("{stem}{suffix} ({n}).{extension}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::grade::{Grade, LUT_SIZE, Lut};

    pub fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ve-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn make_video(tools: &Tools, out: &Path, size: &str, seconds: u32) {
        let status = hidden(&tools.ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", &format!("testsrc2=s={size}:r=24:d={seconds}")])
            .args(["-f", "lavfi", "-i", &format!("sine=f=330:d={seconds}")])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(out)
            .status()
            .unwrap();
        assert!(status.success());
    }

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

    fn job(dir: &Path, input: &Path, p: &Probe, shape: Shape, name: &str, end: f64) -> ExportJob {
        ExportJob {
            input: input.to_path_buf(),
            probe: p.clone(),
            start: 1.0,
            end,
            shape,
            finish: Finish::default(),
            lut_file: "lut.cube".into(),
            workdir: dir.to_path_buf(),
            output: dir.join(name),
            encoder: Encoder::Cpu,
            gpu_lut: None,
        }
    }

    const PLAIN: GrabOptions = GrabOptions { width: 320, finish: Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };

    /// The promise of the app: a two-minute video comes out in both shapes, at
    /// the right sizes and full length, with the graded colour the preview shows.
    #[test]
    fn two_minute_video_exports_wide_and_vertical_with_the_previewed_colour() {
        let Some(tools) = Tools::find() else {
            eprintln!("skipped: video tools not installed");
            return;
        };
        let dir = scratch("two-minutes");
        let input = dir.join("input video.mp4");
        make_video(&tools, &input, "1280x720", 120);
        let p = probe(&tools, &input).unwrap();
        assert_eq!((p.width, p.height), (1280, 720));
        assert!(p.is_wide() && p.has_audio && (p.duration - 120.0).abs() < 0.5);

        let grade = Grade { contrast: 0.3, temperature: 0.35, saturation: -0.2, ..Grade::default() };
        let lut = Lut::build(LUT_SIZE, &grade, None);
        lut.write_cube(&dir.join("lut.cube")).unwrap();
        let cancel = AtomicBool::new(false);

        let points = analyse_action(&tools, &p, &input).unwrap();
        assert!(points.len() >= 2 && points.iter().all(|(_, x)| (0.0..=1.0).contains(x)));

        let cases = [
            (Shape::Wide { height: 0 }, "wide.mp4", (1280, 720), 119.0),
            (Shape::Vertical(Framing::Position(0.5)), "vertical.mp4", (1080, 1920), 19.0),
            (Shape::Vertical(Framing::Follow(points)), "follow.mp4", (1080, 1920), 19.0),
            (Shape::Vertical(Framing::Blur(BlurLayout { zoom: 1.4, x: 0.3, y: 0.4 })), "blur.mp4", (1080, 1920), 19.0),
        ];
        for (shape, name, size, length) in cases {
            let j = job(&dir, &input, &p, shape, name, 1.0 + length);
            let mut last = 0.0;
            export(&tools, &j, &cancel, |f| last = f).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(last > 0.8, "{name}: progress only reached {last}");
            let made = probe(&tools, &j.output).unwrap();
            assert_eq!((made.width, made.height), size, "{name}");
            assert!((made.duration - length).abs() < 0.5, "{name}: {}", made.duration);
            assert!(made.has_audio, "{name} lost its sound");
        }

        // Same moment, preview path against exported file.
        let original = grab_frame(&tools, &p, &input, 31.0, &PLAIN).unwrap();
        let previewed = lut.apply(&original.rgb);
        let exported = grab_frame(&tools, &probe(&tools, &dir.join("wide.mp4")).unwrap(), &dir.join("wide.mp4"), 30.0, &PLAIN).unwrap();
        let (a, b) = (mean(&previewed), mean(&exported.rgb));
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 6.0, "preview {a:?} vs export {b:?}");
        }
        assert!((mean(&original.rgb)[0] - a[0]).abs() + (mean(&original.rgb)[2] - a[2]).abs() > 3.0, "the grade should change the picture");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn blur_layout_is_clamped_and_written_into_the_graph() {
        let stack = blur_stack(BlurLayout { zoom: 9.0, x: -3.0, y: 0.25 }, "7");
        assert!(stack.contains("scale=2700:-2"), "zoom is capped at 2.5: {stack}");
        assert!(stack.contains("(iw-out_w)*0.0000") && stack.contains("(H-h)*0.2500"), "{stack}");
        assert!(stack.contains("[a7][b7]") || stack.contains("split[a7][b7]"));
        let default = blur_stack(BlurLayout::default(), "");
        assert!(default.contains("scale=1296:-2") && default.contains("*0.5000"), "a little zoomed and centred by default: {default}");
    }

    /// Zooming and moving the main picture really changes the exported frame.
    #[test]
    fn the_blurred_background_layout_changes_the_picture() {
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("blurlayout");
        let input = dir.join("in.mp4");
        make_video(&tools, &input, "1280x720", 6);
        let p = probe(&tools, &input).unwrap();
        Lut::build(5, &Grade::default(), None).write_cube(&dir.join("lut.cube")).unwrap();
        let frame_of = |name: &str, layout: BlurLayout| {
            let j = job(&dir, &input, &p, Shape::Vertical(Framing::Blur(layout)), name, 4.0);
            export(&tools, &j, &AtomicBool::new(false), |_| {}).unwrap();
            let made = probe(&tools, &j.output).unwrap();
            assert_eq!((made.width, made.height), (1080, 1920), "{name}");
            grab_frame(&tools, &made, &j.output, 1.5, &PLAIN).unwrap()
        };
        let whole = frame_of("whole.mp4", BlurLayout { zoom: 1.0, x: 0.5, y: 0.5 });
        let zoomed = frame_of("zoomed.mp4", BlurLayout { zoom: 1.8, x: 0.5, y: 0.5 });
        let moved = frame_of("moved.mp4", BlurLayout { zoom: 1.8, x: 0.0, y: 0.1 });
        let difference = |a: &Frame, b: &Frame| a.rgb.iter().zip(&b.rgb).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as f64).sum::<f64>() / a.rgb.len() as f64;
        assert!(difference(&whole, &zoomed) > 4.0, "zoom made no difference: {}", difference(&whole, &zoomed));
        assert!(difference(&zoomed, &moved) > 4.0, "moving made no difference: {}", difference(&zoomed, &moved));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_automatic_choice_uses_a_working_graphics_card_or_the_cpu() {
        let all = [Encoder::Intel, Encoder::Nvidia];
        assert_eq!(EncoderChoice::Auto.resolve(&all), Encoder::Nvidia, "NVIDIA is preferred when several work");
        assert_eq!(EncoderChoice::Auto.resolve(&[]), Encoder::Cpu);
        assert_eq!(EncoderChoice::Cpu.resolve(&all), Encoder::Cpu);
        assert_eq!(EncoderChoice::Intel.resolve(&all), Encoder::Intel);
        assert_eq!(EncoderChoice::Amd.resolve(&all), Encoder::Cpu, "an encoder that doesn't work here is never chosen");
        for e in [Encoder::Cpu, Encoder::Nvidia, Encoder::Amd, Encoder::Intel] {
            let args = video_args(e, 12);
            assert!(args.contains(&e.codec().to_string()) && args.last().map(String::as_str) == Some("12"), "{e:?}: {args:?}");
        }
    }

    /// Every graphics-card encoder this computer really has must produce a correct video.
    #[test]
    fn graphics_card_encoders_make_correct_videos() {
        let Some(tools) = Tools::find() else { return };
        let found = detect_encoders(&tools);
        eprintln!("graphics-card encoders that work here: {found:?}");
        let dir = scratch("gpu");
        let input = dir.join("in.mp4");
        make_video(&tools, &input, "1280x720", 8);
        let p = probe(&tools, &input).unwrap();
        Lut::build(5, &Grade::default(), None).write_cube(&dir.join("lut.cube")).unwrap();
        for encoder in found {
            for (shape, size) in [(Shape::Wide { height: 0 }, (1280, 720)), (Shape::Vertical(Framing::Position(0.5)), (1080, 1920))] {
                let mut j = job(&dir, &input, &p, shape, &format!("{encoder:?}-{}.mp4", size.0), 7.0);
                j.encoder = encoder;
                let used = export(&tools, &j, &AtomicBool::new(false), |_| {}).unwrap_or_else(|e| panic!("{encoder:?}: {e}"));
                assert_eq!(used.encoder, encoder, "{encoder:?} silently fell back to the CPU");
                let made = probe(&tools, &j.output).unwrap();
                assert_eq!((made.width, made.height), size, "{encoder:?}");
                assert!((made.duration - 6.0).abs() < 0.5 && made.has_audio, "{encoder:?}: {made:?}");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// If a graphics-card encoder can't be used, the video is still made, on the CPU.
    #[test]
    fn a_missing_graphics_encoder_falls_back_to_the_cpu() {
        let Some(tools) = Tools::find() else { return };
        let working = detect_encoders(&tools);
        let Some(missing) = Encoder::HARDWARE.into_iter().find(|e| !working.contains(e)) else {
            eprintln!("skipped: every graphics-card encoder works on this computer");
            return;
        };
        let dir = scratch("fallback");
        let input = dir.join("in.mp4");
        make_video(&tools, &input, "640x360", 4);
        let p = probe(&tools, &input).unwrap();
        Lut::build(5, &Grade::default(), None).write_cube(&dir.join("lut.cube")).unwrap();
        let mut j = job(&dir, &input, &p, Shape::Wide { height: 0 }, "out.mp4", 3.0);
        j.encoder = missing;
        let used = export(&tools, &j, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(used.encoder, Encoder::Cpu);
        assert_eq!(probe(&tools, &j.output).unwrap().width, 640);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// How much faster the graphics card is here: `cargo test speed_comparison -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn speed_comparison_of_every_encoder() {
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("speed");
        let input = dir.join("in.mp4");
        let status = hidden(&tools.ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=60:d=60", "-f", "lavfi", "-i", "sine=f=330:d=60", "-c:v", "libx264", "-preset", "veryfast", "-crf", "16", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&input)
            .status()
            .unwrap();
        assert!(status.success());
        let p = probe(&tools, &input).unwrap();
        let lut = Arc::new(Lut::build(LUT_SIZE, &Grade { contrast: 0.2, ..Grade::default() }, None));
        lut.write_cube(&dir.join("lut.cube")).unwrap();
        let mut results = Vec::new();
        let mut n = 0;
        for (name, shape) in [("16:9", Shape::Wide { height: 0 }), ("9:16", Shape::Vertical(Framing::Position(0.5)))] {
            for (encoder, on_card) in std::iter::once(Encoder::Cpu).chain(detect_encoders(&tools)).flat_map(|e| [(e, false), (e, true)]) {
                n += 1;
                let mut j = job(&dir, &input, &p, shape.clone(), &format!("speed{n}.mp4"), 61.0);
                j.start = 0.0;
                j.encoder = encoder;
                j.gpu_lut = on_card.then(|| lut.clone());
                let began = std::time::Instant::now();
                export(&tools, &j, &AtomicBool::new(false), |_| {}).unwrap();
                let seconds = began.elapsed().as_secs_f64();
                let size = std::fs::metadata(&j.output).unwrap().len() as f64 / 1e6;
                eprintln!("SPEED {name} {:<11} colour on {:<8} {seconds:6.1} s for 60 s of 1080p60  ({:.1}x real time)  {size:.1} MB", encoder.short(), if on_card { "the card" } else { "FFmpeg" }, 60.0 / seconds);
                results.push((encoder, seconds));
            }
        }
        std::fs::remove_dir_all(&dir).ok();
        assert!(!results.is_empty());
    }

    #[test]
    fn cancelling_leaves_no_partial_file() {
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("cancel");
        let input = dir.join("in.mp4");
        make_video(&tools, &input, "640x360", 20);
        let p = probe(&tools, &input).unwrap();
        Lut::build(5, &Grade::default(), None).write_cube(&dir.join("lut.cube")).unwrap();
        let j = job(&dir, &input, &p, Shape::Wide { height: 2160 }, "out.mp4", 19.0);
        let err = export(&tools, &j, &AtomicBool::new(true), |_| {}).unwrap_err();
        assert_eq!(err, "Cancelled");
        assert!(!j.output.exists() && !j.output.with_extension("part.mp4").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_wide_source_is_cropped_to_16_9() {
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("square");
        let input = dir.join("in.mp4");
        make_video(&tools, &input, "640x640", 3);
        let p = probe(&tools, &input).unwrap();
        assert!(!p.is_wide());
        Lut::build(5, &Grade::default(), None).write_cube(&dir.join("lut.cube")).unwrap();
        let j = job(&dir, &input, &p, Shape::Wide { height: 0 }, "out.mp4", 2.5);
        export(&tools, &j, &AtomicBool::new(false), |_| {}).unwrap();
        let made = probe(&tools, &j.output).unwrap();
        assert_eq!((made.width, made.height), (640, 360));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn free_name_never_reuses_a_file() {
        let dir = scratch("names");
        let first = free_name(&dir, "clip", "_graded", "mp4");
        std::fs::write(&first, b"x").unwrap();
        assert_ne!(first, free_name(&dir, "clip", "_graded", "mp4"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn action_path_interpolates() {
        let pts = [(0.0, 0.0), (10.0, 1.0)];
        assert!((action_at(&pts, 5.0) - 0.5).abs() < 1e-6);
        assert_eq!(action_at(&pts, 99.0), 1.0);
    }

    #[test]
    fn spread_times_cover_the_whole_range() {
        let t = spread_times(0.0, 120.0, 4);
        assert_eq!(t, vec![15.0, 45.0, 75.0, 105.0]);
    }
}
