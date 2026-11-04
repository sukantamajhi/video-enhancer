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

fn hidden(program: &Path) -> Command {
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
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Finish {
    pub sharpen: f32,
    pub denoise: f32,
    pub loudness: bool,
}

impl Default for Finish {
    fn default() -> Self {
        Self { sharpen: 0.2, denoise: 0.0, loudness: true }
    }
}

fn even(v: f64) -> u32 {
    ((v / 2.0).round() as u32).max(1) * 2
}

/// Turns any source into the plain video colour the colour table expects.
fn base_filters(p: &Probe) -> Vec<String> {
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

fn sharpen_filter(f: Finish) -> Option<String> {
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

fn action_expression(points: &[(f64, f32)], start: f64) -> String {
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

#[derive(Clone)]
pub enum Framing {
    Position(f32),
    Follow(Vec<(f64, f32)>),
    Blur,
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
}

const TAGS: &str = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv";

fn export_graph(job: &ExportJob) -> String {
    let p = &job.probe;
    let mut chain = base_filters(p);
    chain.extend(denoise_filter(job.finish));
    chain.push("format=gbrp".into());
    chain.push(format!("lut3d=file={}:interp=trilinear", job.lut_file));
    chain.extend(sharpen_filter(job.finish));
    let graded = chain.join(",");
    let finish_to = |w: u32, h: u32| format!("scale={w}:{h}:flags=lanczos:out_color_matrix=bt709:out_range=tv,format=yuv420p,{TAGS}");
    let (w, h) = (p.width as f64, p.height as f64);
    match &job.shape {
        Shape::Wide { height } => {
            let th = if *height == 0 { even(if p.is_wide() { h } else { h.min(w * 9.0 / 16.0) }) } else { *height };
            let tw = even(th as f64 * 16.0 / 9.0);
            let crop = wide_crop(p).map_or(String::new(), |c| format!("{c},"));
            format!("[0:v]{graded},{crop}{}[v]", finish_to(tw, th))
        }
        Shape::Vertical(Framing::Blur) => format!(
            "[0:v]{graded},split[a][b];[a]scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920,boxblur=40:2[bg];\
             [b]scale=1080:-2:flags=lanczos[fg];[bg][fg]overlay=(W-w)/2:(H-h)/2:format=gbrp,{}[v]",
            finish_to(1080, 1920)
        ),
        Shape::Vertical(framing) => {
            let crop = if p.vertical_share() >= 1.0 {
                // Narrower than 9:16 already, so trim the height instead.
                format!("crop={}:{}", even(w), even(w * 16.0 / 9.0))
            } else {
                let cw = even(h * 9.0 / 16.0);
                let x = match framing {
                    Framing::Position(f) => format!("(in_w-out_w)*{f:.4}"),
                    Framing::Follow(points) => action_expression(points, job.start),
                    Framing::Blur => unreachable!("handled above"),
                };
                format!("crop={cw}:{}:'{x}':0", even(h))
            };
            format!("[0:v]{graded},{crop},{}[v]", finish_to(1080, 1920))
        }
    }
}

/// Stops the running program when the cancel flag is raised; returns whether it finished well.
fn watch(mut child: Child, cancel: &AtomicBool, total: f64, progress: &mut impl FnMut(f32)) -> Result<(), String> {
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
pub fn export(tools: &Tools, job: &ExportJob, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<(), String> {
    let part = job.output.with_extension("part.mp4");
    let length = (job.end - job.start).max(0.05);
    let mut cmd = hidden(&tools.ffmpeg);
    cmd.current_dir(&job.workdir)
        .args(["-y", "-v", "error", "-nostats", "-progress", "pipe:1"])
        .args(["-ss", &format!("{:.3}", job.start), "-t", &format!("{length:.3}"), "-i"])
        .arg(&job.input)
        .args(["-filter_complex", &export_graph(job), "-map", "[v]", "-map", "0:a:0?"])
        .args(["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-profile:v", "high"])
        .args(["-bf", "2", "-flags", "+cgop", "-g", &format!("{}", ((job.probe.fps / 2.0).round() as u32).max(1))])
        .args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]);
    if job.probe.has_audio {
        if job.finish.loudness {
            cmd.args(["-af", "loudnorm=I=-14:TP=-1.5:LRA=11"]);
        }
        cmd.args(["-c:a", "aac", "-b:a", "256k", "-ar", "48000"]);
    }
    cmd.args(["-movflags", "+faststart"]).arg(&part).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    let result = watch(child, cancel, length, &mut progress);
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
            (Shape::Vertical(Framing::Blur), "blur.mp4", (1080, 1920), 19.0),
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
