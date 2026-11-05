//! Turns several clips into one montage. The AI only chooses which moments to
//! keep (see `ai::openai_plan`). Everything else happens here: measuring how
//! lively each moment is, tidying the AI's cut list into the length you asked
//! for, and rendering it with FFmpeg using your real footage.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;

use crate::grade::{self, Grade};
use crate::media::{self, Frame, GrabOptions, Probe, Tools};

pub const MAX_CUTS: usize = 36;
const FADE: f64 = 0.25;
const FPS: u32 = 30;

#[derive(Clone)]
pub struct Clip {
    pub path: PathBuf,
    pub probe: Probe,
}

impl Clip {
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Style {
    Highlights,
    BeatSync,
    Story,
}

impl Style {
    pub const ALL: [Style; 3] = [Style::Highlights, Style::BeatSync, Style::Story];

    pub fn name(self) -> &'static str {
        match self {
            Style::Highlights => "Highlight reel",
            Style::BeatSync => "Beat-synced to music",
            Style::Story => "Story / vlog edit",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Style::Highlights => "The best 2-6 second moments from every clip, mixed so it never sits on one clip too long, with soft crossfades.",
            Style::BeatSync => "Fast cuts that land on the beat of your music. Needs music.",
            Style::Story => "Keeps your clips in order with longer moments, fades through black, and a title at the start.",
        }
    }

    /// Shortest and longest a single moment may be, in seconds.
    pub fn cut_range(self) -> (f64, f64) {
        match self {
            Style::Highlights => (1.5, 6.0),
            Style::BeatSync => (1.0, 4.0),
            Style::Story => (3.0, 12.0),
        }
    }

    fn crossfade(self) -> bool {
        self != Style::BeatSync
    }

    pub fn needs_music(self) -> bool {
        self == Style::BeatSync
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Sound {
    MusicAndOriginal,
    OriginalOnly,
    MusicOnly,
}

impl Sound {
    pub const ALL: [Sound; 3] = [Sound::MusicAndOriginal, Sound::OriginalOnly, Sound::MusicOnly];

    pub fn name(self) -> &'static str {
        match self {
            Sound::MusicAndOriginal => "Music, with the original sound underneath",
            Sound::OriginalOnly => "Original sound only",
            Sound::MusicOnly => "Music only",
        }
    }

    pub fn uses_music(self) -> bool {
        self != Sound::OriginalOnly
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Vertical {
    Follow,
    Centre,
    Blur,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Out {
    Wide,
    Vertical(Vertical),
}

/// One moment taken from one clip.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Cut {
    pub clip: usize,
    pub start: f64,
    pub end: f64,
    pub score: f32,
    pub why: String,
}

impl Cut {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }
}

// ------------------------------------------------------------------- measuring

/// How lively each half-second of a clip is, from 0 to 1 (movement and sound).
#[derive(Clone, Debug, Default)]
pub struct Energy {
    pub level: Vec<f32>,
}

const STEP: f64 = 0.5;

impl Energy {
    pub fn mean(&self, a: f64, b: f64) -> f32 {
        if self.level.is_empty() {
            return 0.5;
        }
        let last = self.level.len() - 1;
        let (i, j) = (((a / STEP) as usize).min(last), ((b / STEP).ceil() as usize).clamp(1, self.level.len()));
        let slice = &self.level[i..j.max(i + 1).min(self.level.len())];
        slice.iter().sum::<f32>() / slice.len() as f32
    }

    /// The most active moments, spread apart, as seconds.
    pub fn peaks(&self, count: usize) -> Vec<f64> {
        let mut order: Vec<usize> = (0..self.level.len()).collect();
        order.sort_by(|&a, &b| self.level[b].total_cmp(&self.level[a]));
        let mut chosen: Vec<usize> = Vec::new();
        for i in order {
            if chosen.iter().all(|&c| c.abs_diff(i) as f64 * STEP > 6.0) {
                chosen.push(i);
            }
            if chosen.len() == count {
                break;
            }
        }
        let mut seconds: Vec<f64> = chosen.into_iter().map(|i| i as f64 * STEP).collect();
        seconds.sort_by(f64::total_cmp);
        seconds
    }
}

fn normalise(values: &mut [f32]) {
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let top = sorted.get((sorted.len() as f32 * 0.95) as usize).copied().unwrap_or(1.0).max(1e-4);
    for v in values {
        *v = (*v / top).min(1.0);
    }
}

pub fn analyse_energy(tools: &Tools, clip: &Clip) -> Result<Energy, String> {
    let p = &clip.probe;
    let mut chain = media::base_filters(p);
    chain.push("fps=2".into());
    chain.push("scale=64:36:flags=bilinear".into());
    let mut child = media::hidden(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&clip.path)
        .args(["-vf", &chain.join(","), "-an", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    let mut out = child.stdout.take().expect("stdout is piped");
    let mut frame = vec![0u8; 64 * 36];
    let mut previous: Option<Vec<u8>> = None;
    let mut motion: Vec<f32> = Vec::new();
    while out.read_exact(&mut frame).is_ok() {
        motion.push(previous.as_ref().map_or(0.0, |prev| frame.iter().zip(prev).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as f32).sum::<f32>() / frame.len() as f32));
        previous = Some(frame.clone());
    }
    let _ = child.wait();
    if motion.is_empty() {
        return Err(format!("{} couldn't be read.", clip.name()));
    }
    if let Some(first) = motion.get(1).copied() {
        motion[0] = first;
    }
    normalise(&mut motion);

    let mut level = motion.clone();
    if p.has_audio {
        let output = media::hidden(&tools.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&clip.path)
            .args(["-vn", "-ac", "1", "-ar", "8000", "-f", "s16le", "-"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        let samples: Vec<f32> = output.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        let mut loud: Vec<f32> = samples.chunks(4000).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt()).collect();
        if loud.len() > 1 {
            normalise(&mut loud);
            for (i, l) in level.iter_mut().enumerate() {
                *l = *l * 0.6 + loud.get(i).copied().unwrap_or(0.0) * 0.4;
            }
        }
    }
    // A light smoothing, so one flicker doesn't look like action.
    let copy = level.clone();
    for (i, l) in level.iter_mut().enumerate() {
        let (s, e) = (i.saturating_sub(1), (i + 2).min(copy.len()));
        *l = copy[s..e].iter().sum::<f32>() / (e - s) as f32;
    }
    Ok(Energy { level })
}

/// Small pictures from across a clip, for the AI to look at.
pub fn sample_frames(tools: &Tools, clip: &Clip, count: usize, width: u32) -> Result<Vec<(f64, Frame)>, String> {
    let options = GrabOptions { width, finish: media::Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
    media::spread_times(0.0, clip.probe.duration, count).into_iter().map(|t| media::grab_frame(tools, &clip.probe, &clip.path, t, &options).map(|f| (t, f))).collect()
}

/// A colour correction for each clip, so footage from different cameras and
/// different light still sits together in one video.
pub fn clip_grade(frames: &[(f64, Frame)], look: Option<usize>) -> Grade {
    let all: Vec<u8> = frames.iter().flat_map(|(_, f)| f.rgb.iter().copied()).collect();
    let mut g = grade::auto_correct(&all);
    if let Some(i) = look {
        let base = g.temperature;
        grade::LOOKS[i].apply(&mut g, base);
    }
    g
}

// -------------------------------------------------------------------- planning

fn overlap(a: &Cut, b: &Cut) -> f64 {
    if a.clip != b.clip {
        return 0.0;
    }
    (a.end.min(b.end) - a.start.max(b.start)).max(0.0)
}

/// Tidies the AI's suggestions into a cut list that fits the clips and the
/// wanted length. `durations` holds each clip's length in seconds.
pub fn plan_cuts(raw: Vec<Cut>, durations: &[f64], energy: &[Energy], style: Style, target: f64) -> Vec<Cut> {
    let (min_len, max_len) = style.cut_range();
    let fade = if style.crossfade() { FADE } else { 0.0 };

    // 1. Make every cut real: inside its clip and a sensible length.
    let mut cuts: Vec<Cut> = raw
        .into_iter()
        .filter(|c| c.clip < durations.len() && durations[c.clip] > 0.5)
        .filter_map(|mut c| {
            let d = durations[c.clip];
            let want = (c.end - c.start).clamp(min_len, max_len).min(d);
            c.start = c.start.clamp(0.0, (d - want).max(0.0));
            c.end = c.start + want;
            c.score = c.score.clamp(0.0, 1.0);
            // Let the measured liveliness break ties between equally good ideas.
            if let Some(e) = energy.get(c.clip) {
                c.score = c.score * 0.7 + e.mean(c.start, c.end) * 0.3;
            }
            (c.len() >= min_len.min(d) * 0.99).then_some(c)
        })
        .collect();

    // 2. Drop repeats of the same moment, keeping the better one.
    cuts.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Cut> = Vec::new();
    for c in cuts {
        if kept.iter().all(|k| overlap(k, &c) < 0.3 * k.len().min(c.len())) {
            kept.push(c);
        }
    }
    kept.truncate(MAX_CUTS);

    // 3. Too much: drop the weakest (kept is sorted best first).
    let need = |n: usize| target + fade * n.saturating_sub(1) as f64;
    while kept.len() > 1 && kept.iter().map(Cut::len).sum::<f64>() > need(kept.len()) * 1.02 {
        kept.pop();
    }

    // 4. Too little: grow moments around their centre, a half-second at a time.
    let mut grew = true;
    while grew && kept.iter().map(Cut::len).sum::<f64>() < need(kept.len()) * 0.97 {
        grew = false;
        for i in 0..kept.len() {
            if kept.iter().map(Cut::len).sum::<f64>() >= need(kept.len()) {
                break;
            }
            let (clip, d) = (kept[i].clip, durations[kept[i].clip]);
            if kept[i].len() + STEP > max_len {
                continue;
            }
            let mut trial = kept[i].clone();
            if trial.end + STEP <= d {
                trial.end += STEP;
            } else if trial.start - STEP >= 0.0 {
                trial.start -= STEP;
            } else {
                continue;
            }
            if kept.iter().enumerate().all(|(j, o)| j == i || o.clip != clip || overlap(o, &trial) < 0.3 * o.len().min(trial.len())) {
                kept[i] = trial;
                grew = true;
            }
        }
    }

    // 5. Put them in a good order.
    let mut ordered = order(kept, style);

    // 6. Trim the longest moments until the total is exact.
    let want = need(ordered.len());
    loop {
        let total: f64 = ordered.iter().map(Cut::len).sum();
        if total <= want + 0.05 {
            break;
        }
        let Some(i) = (0..ordered.len()).filter(|&i| ordered[i].len() - 0.5 >= min_len.min(1.0).max(0.6)).max_by(|&a, &b| ordered[a].len().total_cmp(&ordered[b].len())) else { break };
        let cut = (total - want).min(0.5).min(ordered[i].len() - 0.6);
        ordered[i].end -= cut.max(0.05);
    }
    ordered
}

fn order(mut cuts: Vec<Cut>, style: Style) -> Vec<Cut> {
    match style {
        Style::Story => {
            cuts.sort_by(|a, b| (a.clip, a.start).partial_cmp(&(b.clip, b.start)).unwrap_or(std::cmp::Ordering::Equal));
            cuts
        }
        Style::Highlights | Style::BeatSync => {
            // Highlights open with the strongest moment. Beat-synced edits build up to it.
            cuts.sort_by(|a, b| if style == Style::Highlights { b.score.total_cmp(&a.score) } else { a.score.total_cmp(&b.score) });
            let mut out: Vec<Cut> = Vec::with_capacity(cuts.len());
            while !cuts.is_empty() {
                let previous = out.last().map(|c| c.clip);
                let pick = cuts.iter().position(|c| Some(c.clip) != previous).unwrap_or(0);
                out.push(cuts.remove(pick));
            }
            out
        }
    }
}

/// Snaps moments to whole beats so each cut lands on the music.
pub fn quantise(cuts: &mut [Cut], durations: &[f64], interval: f64, target: f64) {
    if interval <= 0.05 || cuts.is_empty() {
        return;
    }
    let min_beats = (1.0 / interval).ceil().max(1.0) as i64;
    let mut beats: Vec<i64> = cuts.iter().map(|c| ((c.len() / interval).round() as i64).max(min_beats)).collect();
    let want = (target / interval).round() as i64;
    let mut guard = 0;
    while beats.iter().sum::<i64>() != want && guard < 500 {
        guard += 1;
        let total: i64 = beats.iter().sum();
        if total > want {
            if let Some(i) = (0..beats.len()).filter(|&i| beats[i] > min_beats).max_by_key(|&i| beats[i]) {
                beats[i] -= 1;
            } else {
                break;
            }
        } else if let Some(i) = (0..beats.len()).filter(|&i| (beats[i] + 1) as f64 * interval <= durations[cuts[i].clip]).min_by_key(|&i| beats[i]) {
            beats[i] += 1;
        } else {
            break;
        }
    }
    for (c, b) in cuts.iter_mut().zip(beats) {
        let len = (b as f64 * interval).min(durations[c.clip]);
        c.start = c.start.min((durations[c.clip] - len).max(0.0));
        c.end = c.start + len;
    }
}

// ------------------------------------------------------------------- rendering

pub struct Music {
    pub path: PathBuf,
    /// Where in the song to begin (the first beat, for beat-synced edits).
    pub start: f64,
}

pub struct Job<'a> {
    pub clips: &'a [Clip],
    pub actions: &'a [Option<Vec<(f64, f32)>>],
    pub cuts: &'a [Cut],
    pub style: Style,
    pub sound: Sound,
    pub music: Option<&'a Music>,
    pub out: Out,
    pub encoder: media::Encoder,
    /// Where the main picture sits when the 9:16 video uses a blurred background.
    pub blur: media::BlurLayout,
    pub title: Option<&'a str>,
    pub workdir: &'a Path,
    pub output: &'a Path,
}

impl Job<'_> {
    pub fn total(&self) -> f64 {
        let sum: f64 = self.cuts.iter().map(Cut::len).sum();
        if self.style.crossfade() { sum - FADE * self.cuts.len().saturating_sub(1) as f64 } else { sum }
    }

    fn uses_original(&self) -> bool {
        self.sound != Sound::MusicOnly
    }

    fn has_music(&self) -> bool {
        self.sound.uses_music() && self.music.is_some()
    }
}

fn shape_filter(job: &Job, k: usize, cut: &Cut) -> String {
    let p = &job.clips[cut.clip].probe;
    match job.out {
        Out::Wide => "scale=1920:1080:force_original_aspect_ratio=increase,crop=1920:1080".into(),
        Out::Vertical(Vertical::Blur) => media::blur_stack(job.blur, &k.to_string()),
        Out::Vertical(mode) => {
            if p.vertical_share() >= 1.0 {
                "scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920".into()
            } else {
                let x = match (mode, job.actions.get(cut.clip).and_then(Option::as_ref)) {
                    (Vertical::Follow, Some(points)) => media::action_expression(points, cut.start),
                    _ => "(in_w-out_w)*0.5".into(),
                };
                format!("crop={}:{}:'{x}':0,scale=1080:1920:flags=lanczos", media::even(p.height as f64 * 9.0 / 16.0), media::even(p.height as f64))
            }
        }
    }
}

/// The FFmpeg filter graph and the input arguments that go with it.
pub fn build(job: &Job) -> Result<(Vec<String>, String), String> {
    let n = job.cuts.len();
    if n == 0 {
        return Err("There are no moments to put together.".into());
    }
    if job.sound.uses_music() && job.music.is_none() {
        return Err("This sound choice needs a music track.".into());
    }
    let mut args: Vec<String> = Vec::new();
    for cut in job.cuts {
        args.extend(["-ss".into(), format!("{:.3}", cut.start), "-t".into(), format!("{:.3}", cut.len()), "-i".into(), job.clips[cut.clip].path.to_string_lossy().into_owned()]);
    }
    let total = job.total();
    if let (true, Some(music)) = (job.has_music(), job.music) {
        args.extend(["-stream_loop".into(), "-1".into(), "-ss".into(), format!("{:.3}", music.start), "-i".into(), music.path.to_string_lossy().into_owned()]);
    }

    let mut g: Vec<String> = Vec::new();
    for (k, cut) in job.cuts.iter().enumerate() {
        let probe = &job.clips[cut.clip].probe;
        // The grade runs last, after dropping frames to the output rate and after all cropping, scaling
        // and blurring (scaling an already graded RGB picture shifts its colours).
        let mut pre = vec!["setpts=PTS-STARTPTS".to_string()];
        pre.extend(media::base_filters(probe));
        pre.push(format!("fps={FPS}"));
        let grade = format!("format=gbrp,lut3d=file=lut{}.cube:interp=tetrahedral", cut.clip);
        let picture = format!("{},{grade}", shape_filter(job, k, cut));
        g.push(format!("[{k}:v]{},{picture},setsar=1,scale=out_color_matrix=bt709:out_range=tv:flags=lanczos,format=yuv420p,{}[v{k}]", pre.join(","), media::TAGS));
        if job.uses_original() {
            let fades = if job.style.crossfade() { String::new() } else { format!(",afade=t=in:d=0.02,afade=t=out:st={:.3}:d=0.02", (cut.len() - 0.02).max(0.0)) };
            if probe.has_audio {
                g.push(format!("[{k}:a]asetpts=PTS-STARTPTS,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo{fades}[a{k}]"));
            } else {
                g.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={:.3},asetpts=PTS-STARTPTS{fades}[a{k}]", cut.len()));
            }
        }
    }

    // Join the moments.
    let (mut video, mut audio) = ("v0".to_string(), "a0".to_string());
    if job.style.crossfade() {
        let kind = if job.style == Style::Story { "fadeblack" } else { "fade" };
        let mut so_far = job.cuts[0].len();
        for i in 1..n {
            g.push(format!("[{video}][v{i}]xfade=transition={kind}:duration={FADE}:offset={:.3}[xv{i}]", so_far - FADE));
            video = format!("xv{i}");
            if job.uses_original() {
                g.push(format!("[{audio}][a{i}]acrossfade=d={FADE}[xa{i}]"));
                audio = format!("xa{i}");
            }
            so_far += job.cuts[i].len() - FADE;
        }
    } else {
        let labels: String = (0..n).map(|i| if job.uses_original() { format!("[v{i}][a{i}]") } else { format!("[v{i}]") }).collect();
        g.push(format!("{labels}concat=n={n}:v=1:a={}[vj]{}", u8::from(job.uses_original()), if job.uses_original() { "[aj]" } else { "" }));
        video = "vj".into();
        audio = "aj".into();
    }

    // A title over the first moments of a story edit, if the font is available.
    match job.title.filter(|_| job.style == Style::Story && job.workdir.join("font.ttf").is_file()) {
        Some(_) => g.push(format!(
            "[{video}]drawtext=fontfile=font.ttf:textfile=title.txt:fontsize=h/13:fontcolor=white:borderw=3:bordercolor=black@0.55:x=(w-text_w)/2:y=h*0.42:enable='lt(t,2.8)':alpha='if(lt(t,0.4),t/0.4,if(gt(t,2.3),(2.8-t)/0.5,1))',format=yuv420p[vout]"
        )),
        // xfade hands back 4:4:4 picture; the encoder's usual profile needs 4:2:0.
        None => g.push(format!("[{video}]format=yuv420p[vout]")),
    }

    // The sound.
    let loud = "loudnorm=I=-14:TP=-1.5:LRA=11";
    let music_chain = |volume: f64| {
        let index = n;
        format!("[{index}:a]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,atrim=duration={total:.3},asetpts=PTS-STARTPTS,volume={volume},afade=t=in:d=0.6,afade=t=out:st={:.3}:d=2.2", (total - 2.2).max(0.0))
    };
    let any_audio = job.uses_original() && job.cuts.iter().any(|c| job.clips[c.clip].probe.has_audio);
    let sound_label = match job.sound {
        Sound::OriginalOnly if any_audio => {
            g.push(format!("[{audio}]{loud}[aout]"));
            Some("[aout]")
        }
        Sound::OriginalOnly => None,
        Sound::MusicOnly => {
            g.push(format!("{}[mu];[mu]{loud}[aout]", music_chain(1.0)));
            Some("[aout]")
        }
        Sound::MusicAndOriginal => {
            g.push(format!("{}[mu]", music_chain(0.6)));
            g.push(format!("[{audio}]asplit[o1][o2]"));
            g.push("[mu][o1]sidechaincompress=threshold=0.03:ratio=8:attack=20:release=500[md]".into());
            g.push(format!("[md][o2]amix=inputs=2:duration=first:normalize=0,{loud}[aout]"));
            Some("[aout]")
        }
    };
    args.extend(["-map".into(), "[vout]".into()]);
    if let Some(label) = sound_label {
        args.extend(["-map".into(), label.into()]);
    }
    Ok((args, g.join(";")))
}

/// Writes one finished montage. It only appears under its real name once complete.
/// Returns the encoder that really did the work: if a graphics-card encoder fails,
/// the montage is made again on the processor.
pub fn render(tools: &Tools, job: &Job, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<media::Encoder, String> {
    match render_once(tools, job, job.encoder, cancel, &mut progress) {
        Err(e) if e != "Cancelled" && job.encoder != media::Encoder::Cpu => render_once(tools, job, media::Encoder::Cpu, cancel, &mut progress).map(|()| media::Encoder::Cpu),
        other => other.map(|()| job.encoder),
    }
}

fn render_once(tools: &Tools, job: &Job, encoder: media::Encoder, cancel: &AtomicBool, progress: &mut impl FnMut(f32)) -> Result<(), String> {
    let (maps, graph) = build(job)?;
    let script = job.workdir.join("graph.txt");
    std::fs::write(&script, graph).map_err(|e| e.to_string())?;
    let part = job.output.with_extension("part.mp4");
    let mut cmd = media::hidden(&tools.ffmpeg);
    cmd.current_dir(job.workdir).args(["-y", "-v", "error", "-nostats", "-progress", "pipe:1"]);
    // Inputs and maps come before the output options.
    let split = maps.iter().position(|a| a == "-map").unwrap_or(maps.len());
    cmd.args(&maps[..split]).args(["-/filter_complex", "graph.txt"]).args(&maps[split..]);
    cmd.args(["-t", &format!("{:.3}", job.total())])
        .args(media::video_args(encoder, FPS))
        .args(["-r", &FPS.to_string()])
        .args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"])
        .args(["-c:a", "aac", "-b:a", "256k", "-ar", "48000", "-movflags", "+faststart"])
        .arg(&part)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    let result = media::watch(child, cancel, job.total(), progress);
    if result.is_ok() {
        std::fs::rename(&part, job.output).map_err(|e| format!("Couldn't save the finished file: {e}"))?;
    } else {
        std::fs::remove_file(&part).ok();
    }
    result
}

/// When each moment begins in the finished video, in seconds.
pub fn segment_starts(cuts: &[Cut], style: Style) -> Vec<f64> {
    let fade = if style.crossfade() { FADE } else { 0.0 };
    let mut at = 0.0;
    cuts.iter()
        .map(|c| {
            let start = at;
            at += c.len() - fade;
            start
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::LUT_SIZE;
    use crate::media::tests::{make_video, scratch};

    fn cut(clip: usize, start: f64, end: f64, score: f32) -> Cut {
        Cut { clip, start, end, score, why: String::new() }
    }

    fn lively(seconds: usize) -> Energy {
        Energy { level: (0..seconds * 2).map(|i| if i % 8 < 4 { 0.9 } else { 0.2 }).collect() }
    }

    #[test]
    fn plan_fits_the_length_and_stays_inside_the_clips() {
        let durations = [120.0, 90.0, 40.0];
        let energy = [lively(120), lively(90), lively(40)];
        let raw: Vec<Cut> = (0..30).map(|i| cut(i % 3, (i * 3) as f64 % 80.0, (i * 3) as f64 % 80.0 + 4.0 + (i % 3) as f64, 0.2 + (i % 7) as f32 * 0.1)).chain([cut(9, 0.0, 5.0, 1.0), cut(0, 500.0, 509.0, 1.0)]).collect();
        for style in Style::ALL {
            let plan = plan_cuts(raw.clone(), &durations, &energy, style, 60.0);
            let fade = if style.crossfade() { FADE } else { 0.0 };
            let total: f64 = plan.iter().map(Cut::len).sum::<f64>() - fade * (plan.len() - 1) as f64;
            assert!((total - 60.0).abs() < 2.0, "{style:?}: {total}");
            let (min, max) = style.cut_range();
            for c in &plan {
                assert!(c.start >= 0.0 && c.end <= durations[c.clip] + 1e-6, "{style:?} {c:?}");
                assert!(c.len() >= min.min(1.0) * 0.9 && c.len() <= max + 1e-6, "{style:?} {c:?}");
            }
            for (i, a) in plan.iter().enumerate() {
                for b in &plan[i + 1..] {
                    assert!(overlap(a, b) < 0.3 * a.len().min(b.len()), "{style:?} overlap {a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn story_keeps_order_and_highlights_mix_the_clips() {
        let durations = [100.0; 3];
        let energy = [lively(100), lively(100), lively(100)];
        let raw: Vec<Cut> = (0..12).map(|i| cut(i / 4, (i % 4) as f64 * 20.0, (i % 4) as f64 * 20.0 + 8.0, 0.5 + (i % 5) as f32 * 0.1)).collect();
        let story = plan_cuts(raw.clone(), &durations, &energy, Style::Story, 40.0);
        assert!(story.windows(2).all(|w| (w[0].clip, w[0].start) <= (w[1].clip, w[1].start)));
        let reel = plan_cuts(raw, &durations, &energy, Style::Highlights, 40.0);
        assert!(reel.windows(2).all(|w| w[0].clip != w[1].clip), "same clip back to back: {reel:?}");
    }

    #[test]
    fn quantised_cuts_land_on_whole_beats_and_keep_the_length() {
        let durations = [100.0; 2];
        let mut cuts: Vec<Cut> = (0..8).map(|i| cut(i % 2, i as f64 * 10.0, i as f64 * 10.0 + 3.3, 0.5)).collect();
        quantise(&mut cuts, &durations, 0.5, 24.0);
        for c in &cuts {
            assert!(((c.len() / 0.5) - (c.len() / 0.5).round()).abs() < 1e-6, "{c:?}");
        }
        assert!((cuts.iter().map(Cut::len).sum::<f64>() - 24.0).abs() < 0.5 + 1e-6);
    }

    #[test]
    fn chapter_times_follow_the_crossfades() {
        let cuts = vec![cut(0, 0.0, 4.0, 0.5), cut(0, 10.0, 15.0, 0.5), cut(0, 20.0, 23.0, 0.5)];
        assert_eq!(segment_starts(&cuts, Style::BeatSync), vec![0.0, 4.0, 9.0]);
        let faded = segment_starts(&cuts, Style::Highlights);
        assert_eq!(faded, vec![0.0, 4.0 - FADE, 4.0 - FADE + 5.0 - FADE]);
        let total = faded[2] + 3.0;
        assert!((total - (12.0 - 2.0 * FADE)).abs() < 1e-9, "the last moment ends exactly where the video does");
    }

    #[test]
    fn energy_finds_the_lively_part() {
        let mut level = vec![0.1f32; 100];
        for l in &mut level[60..80] {
            *l = 1.0;
        }
        let e = Energy { level };
        assert!(e.mean(30.0, 39.0) > e.mean(0.0, 10.0));
        let peaks = e.peaks(1);
        assert!((30.0..40.0).contains(&peaks[0]), "{peaks:?}");
    }

    /// A beat-synced edit end to end: find the beat in a real song file, snap
    /// the cuts to it, start the song on the first beat and render.
    #[test]
    fn beat_synced_montage_follows_the_detected_beat() {
        let Some(tools) = Tools::find() else {
            eprintln!("skipped: video tools not installed");
            return;
        };
        let dir = scratch("beatsync");
        let clip = dir.join("a.mp4");
        make_video(&tools, &clip, "640x360", 30);

        // A 24-second click track at 120 beats a minute, first click at 0.35 s.
        let rate = 22050usize;
        let mut pcm = vec![0i16; rate * 24];
        let mut t = 0.35f64;
        while t < 23.5 {
            let start = (t * rate as f64) as usize;
            for i in 0..600 {
                pcm[start + i] = (28000.0 * (1.0 - i as f32 / 600.0) * ((i as f32) * 0.4).sin()) as i16;
            }
            t += 0.5;
        }
        let mut wav = Vec::new();
        let data = (pcm.len() * 2) as u32;
        wav.extend(b"RIFF");
        wav.extend((36 + data).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend([1u8, 0, 1, 0]);
        wav.extend((rate as u32).to_le_bytes());
        wav.extend((rate as u32 * 2).to_le_bytes());
        wav.extend([2u8, 0, 16, 0]);
        wav.extend(b"data");
        wav.extend(data.to_le_bytes());
        for s in &pcm {
            wav.extend(s.to_le_bytes());
        }
        let song_path = dir.join("song.wav");
        std::fs::write(&song_path, wav).unwrap();

        let beats = crate::music::detect_beats(&tools, &song_path).unwrap();
        assert!((beats.bpm - 120.0).abs() < 1.5, "{beats:?}");
        assert!((beats.first - 0.35).abs() < 0.03 || (beats.first - 0.85).abs() < 0.03, "{beats:?}");

        let clips = vec![Clip { path: clip, probe: media::probe(&tools, &dir.join("a.mp4")).unwrap() }];
        grade::Lut::build(LUT_SIZE, &Grade::default(), None).write_cube(&dir.join("lut0.cube")).unwrap();
        let energy = [analyse_energy(&tools, &clips[0]).unwrap()];
        let raw: Vec<Cut> = (0..8).map(|i| cut(0, i as f64 * 3.5, i as f64 * 3.5 + 3.0, 0.3 + i as f32 * 0.05)).collect();
        let mut cuts = plan_cuts(raw, &[30.0], &energy, Style::BeatSync, 12.0);
        quantise(&mut cuts, &[30.0], beats.interval, 12.0);
        for c in &cuts {
            let beats_long = c.len() / beats.interval;
            assert!((beats_long - beats_long.round()).abs() < 1e-6, "{c:?}");
        }

        let song = Music { path: song_path, start: beats.first };
        let output = dir.join("beat.mp4");
        let job = Job { clips: &clips, actions: &[None], cuts: &cuts, style: Style::BeatSync, sound: Sound::MusicAndOriginal, music: Some(&song), out: Out::Vertical(Vertical::Centre), encoder: media::Encoder::Cpu, title: None, workdir: &dir, output: &output, blur: media::BlurLayout { zoom: 1.4, x: 0.3, y: 0.5 } };
        render(&tools, &job, &AtomicBool::new(false), |_| {}).unwrap();
        let made = media::probe(&tools, &output).unwrap();
        assert_eq!((made.width, made.height), (1080, 1920));
        assert!((made.duration - 12.0).abs() < 0.6, "{}", made.duration);
        assert!(made.has_audio);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The promise of the feature: clips of different sizes, one without sound,
    /// become one montage in both shapes, for every style and sound choice.
    #[test]
    fn mixed_clips_become_a_montage_in_every_style_and_shape() {
        let Some(tools) = Tools::find() else {
            eprintln!("skipped: video tools not installed");
            return;
        };
        let dir = scratch("montage");
        let (a, b, c) = (dir.join("a.mp4"), dir.join("b.mp4"), dir.join("c.mp4"));
        make_video(&tools, &a, "1280x720", 12);
        make_video(&tools, &b, "640x640", 12);
        // A portrait clip without any sound.
        let status = media::hidden(&tools.ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=s=360x640:r=24:d=12", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
            .arg(&c)
            .status()
            .unwrap();
        assert!(status.success());
        let music = dir.join("song.mp3");
        let status = media::hidden(&tools.ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=f=220:d=6", "-c:a", "libmp3lame"])
            .arg(&music)
            .status()
            .unwrap();
        assert!(status.success());

        let clips: Vec<Clip> = [&a, &b, &c].iter().map(|p| Clip { path: (*p).clone(), probe: media::probe(&tools, p).unwrap() }).collect();
        assert!(!clips[2].probe.has_audio);
        let grades = vec![Grade::default(); 3];
        for i in 0..3 {
            grade::Lut::build(LUT_SIZE, &grades[i], None).write_cube(&dir.join(format!("lut{i}.cube"))).unwrap();
        }
        let actions = vec![media::analyse_action(&tools, &clips[0].probe, &a).ok(), None, None];
        std::fs::write(dir.join("title.txt"), "My Trip").unwrap();
        if let Some(font) = std::env::var_os("WINDIR").map(|w| Path::new(&w).join("Fonts").join("arial.ttf")).filter(|f| f.is_file()) {
            std::fs::copy(font, dir.join("font.ttf")).unwrap();
        }
        let cuts = vec![cut(0, 1.0, 4.0, 0.9), cut(1, 2.0, 5.5, 0.8), cut(2, 0.0, 3.0, 0.7), cut(0, 6.0, 9.0, 0.6), cut(1, 7.0, 10.0, 0.5)];
        let song = Music { path: music, start: 0.0 };

        let cases = [
            (Style::Highlights, Sound::MusicAndOriginal, Out::Wide),
            (Style::Highlights, Sound::OriginalOnly, Out::Vertical(Vertical::Follow)),
            (Style::BeatSync, Sound::MusicOnly, Out::Vertical(Vertical::Blur)),
            (Style::Story, Sound::MusicAndOriginal, Out::Vertical(Vertical::Centre)),
            (Style::Story, Sound::OriginalOnly, Out::Wide),
        ];
        for (n, (style, sound, out)) in cases.into_iter().enumerate() {
            let output = dir.join(format!("out{n}.mp4"));
            let job = Job { clips: &clips, actions: &actions, cuts: &cuts, style, sound, music: Some(&song), out, encoder: media::Encoder::Cpu, title: Some("My Trip"), workdir: &dir, output: &output, blur: media::BlurLayout { zoom: 1.4, x: 0.3, y: 0.5 } };
            let mut last = 0.0;
            render(&tools, &job, &AtomicBool::new(false), |f| last = f).unwrap_or_else(|e| panic!("{style:?}/{sound:?}/{out:?}: {e}"));
            let made = media::probe(&tools, &output).unwrap();
            let size = if matches!(out, Out::Wide) { (1920, 1080) } else { (1080, 1920) };
            assert_eq!((made.width, made.height), size, "{style:?}/{sound:?}/{out:?}");
            assert!((made.duration - job.total()).abs() < 0.6, "{style:?}/{sound:?}/{out:?}: {} vs {}", made.duration, job.total());
            assert!(made.has_audio, "{style:?}/{sound:?}/{out:?} has no sound");
            assert!(last > 0.8, "progress only reached {last}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
