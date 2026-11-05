//! The Montage screen: add clips, let the AI plan, review the cut list, pick
//! music, then render 16:9 and 9:16 versions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use eframe::egui::{self, Align, Color32, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, pos2, vec2};
use serde::{Deserialize, Serialize};

use crate::grade::{Grade, LOOKS, LUT_SIZE, Lut};
use crate::media::{self, BlurLayout, Probe};
use crate::montage::{self, Clip, Cut, Job, Music, Out, Sound, Style, Vertical};
use crate::music::{self, Track};
use crate::pack_ui::{MontageFacts, PackState};
use crate::theme::{self, Icon, Kind};
use crate::{App, Msg, Step, ai, store};

const MAX_CLIPS: usize = 12;

pub enum Event {
    Probed(PathBuf, Result<Probe, String>),
    /// A small picture of a clip, for its card.
    Thumb(PathBuf, media::Frame),
    /// A still picture of one moment, for the preview.
    PreviewFrame(usize, Result<media::Frame, String>),
    /// The clips of the last session, probed again. The number is how many were saved.
    Restored(Vec<Clip>, usize),
    Note(String),
    Plan(Result<PlanData, String>),
    Tracks(Result<Vec<Track>, String>),
    Progress(f32, String),
    Done(Result<Vec<PathBuf>, String>),
    /// The moments exactly as they were rendered, and the music credit, for chapters and the description.
    Rendered(Vec<Cut>, String),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PlanData {
    pub cuts: Vec<Cut>,
    pub grades: Vec<Grade>,
    pub title: String,
    pub music_query: String,
    pub tempo: String,
    /// True when a saved AI answer was reused instead of asking again. Not saved.
    #[serde(skip)]
    pub reused: bool,
}

/// The montage in progress, remembered between runs (AI plan and music included).
#[derive(Serialize, Deserialize)]
struct Saved {
    clips: Vec<PathBuf>,
    style: Style,
    sound: Sound,
    vertical: Vertical,
    target: f64,
    look: Option<usize>,
    source: MusicSource,
    file: Option<PathBuf>,
    tracks: Vec<Track>,
    track: usize,
    music_query: String,
    plan: Option<PlanData>,
    keep: Vec<bool>,
    title: String,
    make_wide: bool,
    make_vertical: bool,
    out_dir: Option<PathBuf>,
    #[serde(default)]
    blur: BlurLayout,
    #[serde(default)]
    pack: Option<crate::pack::Pack>,
    #[serde(default)]
    rendered: Vec<Cut>,
    #[serde(default)]
    credit: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum MusicSource {
    Online,
    File,
}

pub struct State {
    clips: Vec<Clip>,
    probing: usize,
    style: Style,
    sound: Sound,
    vertical: Vertical,
    blur: BlurLayout,
    target: f64,
    look: Option<usize>,
    source: MusicSource,
    file: Option<PathBuf>,
    tracks: Vec<Track>,
    track: usize,
    searching: bool,
    music_query: String,
    plan: Option<PlanData>,
    keep: Vec<bool>,
    title: String,
    busy: Option<String>,
    make_wide: bool,
    make_vertical: bool,
    out_dir: Option<PathBuf>,
    render: Option<(f32, String, Arc<AtomicBool>)>,
    made: Vec<PathBuf>,
    pack: PackState,
    rendered: Vec<Cut>,
    credit: String,
    thumbs: HashMap<PathBuf, egui::TextureHandle>,
    preview: Preview,
}

/// The montage preview: which moment is showing, and the player that runs through them.
#[derive(Default)]
struct Preview {
    cur: usize,
    player: Option<media::Player>,
    tex: Option<egui::TextureHandle>,
    t: f64,
    busy: bool,
    phone: bool,
    started: bool,
    lut: Option<(usize, Lut)>,
}

impl State {
    pub fn new() -> Self {
        Self {
            clips: Vec::new(),
            probing: 0,
            style: Style::Highlights,
            sound: Sound::MusicAndOriginal,
            vertical: Vertical::Follow,
            blur: BlurLayout::default(),
            target: 60.0,
            look: Some(0),
            source: MusicSource::Online,
            file: None,
            tracks: Vec::new(),
            track: 0,
            searching: false,
            music_query: String::new(),
            plan: None,
            keep: Vec::new(),
            title: String::new(),
            busy: None,
            make_wide: true,
            make_vertical: true,
            out_dir: None,
            render: None,
            made: Vec::new(),
            pack: PackState::default(),
            rendered: Vec::new(),
            credit: String::new(),
            thumbs: HashMap::new(),
            preview: Preview::default(),
        }
    }

    fn session(&self) -> Saved {
        Saved {
            clips: self.clips.iter().map(|c| c.path.clone()).collect(),
            pack: self.pack.pack.clone(),
            rendered: self.rendered.clone(),
            credit: self.credit.clone(),
            style: self.style,
            sound: self.sound,
            vertical: self.vertical,
            blur: self.blur,
            target: self.target,
            look: self.look,
            source: self.source,
            file: self.file.clone(),
            tracks: self.tracks.clone(),
            track: self.track,
            music_query: self.music_query.clone(),
            plan: self.plan.clone(),
            keep: self.keep.clone(),
            title: self.title.clone(),
            make_wide: self.make_wide,
            make_vertical: self.make_vertical,
            out_dir: self.out_dir.clone(),
        }
    }

    /// A snapshot of what would be saved, or nothing while a restore is still loading the clips.
    pub fn autosave_text(&self) -> Option<String> {
        (self.probing == 0).then(|| serde_json::to_string(&self.session()).unwrap_or_default())
    }

    pub fn save(&self) {
        store::save("montage", &self.session());
    }

    /// True while something is running that needs the window to keep refreshing.
    pub fn working(&self) -> bool {
        self.busy.is_some() || self.render.is_some() || self.searching || self.probing > 0 || self.pack.working() || self.preview.player.is_some()
    }

    pub fn pack_state(&mut self) -> &mut PackState {
        &mut self.pack
    }

    /// What the upload pack needs: the moments as rendered (or as planned, until then), and the clips and colour.
    pub fn pack_facts(&self) -> Option<MontageFacts> {
        let plan = self.plan.as_ref()?;
        let rendered = !self.rendered.is_empty();
        let cuts: Vec<Cut> = if rendered { self.rendered.clone() } else { plan.cuts.iter().zip(&self.keep).filter(|(_, k)| **k).map(|(c, _)| c.clone()).collect() };
        (!cuts.is_empty() && !self.clips.is_empty()).then(|| MontageFacts { clips: self.clips.clone(), cuts, grades: plan.grades.clone(), style: self.style, title: self.title.clone(), credit: self.credit.clone(), out_dir: self.out_dir.clone(), rendered })
    }

    pub fn stop_preview(&mut self) {
        self.preview.player = None;
    }

    pub fn summary_line(&self) -> String {
        if self.clips.is_empty() { "no clips yet".into() } else { format!("{} clips · {}", self.clips.len(), clock(self.total())) }
    }

    pub fn has_clips(&self) -> bool {
        !self.clips.is_empty()
    }

    pub fn has_plan(&self) -> bool {
        self.plan.is_some()
    }

    pub fn has_made(&self) -> bool {
        !self.made.is_empty()
    }

    pub fn has_pack(&self) -> bool {
        self.pack.pack.is_some()
    }

    /// Progress, what is happening, and how to stop it, while a montage is rendering.
    pub fn render_info(&self) -> Option<(f32, String, Arc<AtomicBool>)> {
        self.render.as_ref().map(|(p, label, cancel)| (*p, label.clone(), cancel.clone()))
    }

    pub fn busy_text(&self) -> Option<String> {
        self.busy.clone().or_else(|| self.searching.then(|| "Searching NCS for music…".to_string()))
    }

    pub fn pack_busy(&self) -> Option<String> {
        self.pack.status()
    }

    fn total(&self) -> f64 {
        self.clips.iter().map(|c| c.probe.duration).sum()
    }
}

fn clock(t: f64) -> String {
    format!("{}:{:02.0}", (t / 60.0) as u32, t % 60.0)
}

pub(crate) fn safe_name(title: &str) -> String {
    let name: String = title.trim().chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').collect::<String>().split_whitespace().collect::<Vec<_>>().join("_");
    if name.is_empty() { "montage".into() } else { name.chars().take(40).collect() }
}

/// A font for the story title, copied next to the render so FFmpeg can find it.
fn title_font() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("WINDIR").map(|w| Path::new(&w).join("Fonts").join("arial.ttf")),
        Some("/System/Library/Fonts/Supplemental/Arial.ttf".into()),
        Some("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into()),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

impl App {
    pub fn add_clips(&mut self, paths: Vec<PathBuf>) {
        let Some(tools) = self.tools.clone() else {
            self.fail("Install the video tools first (Single video screen, button on the left).");
            return;
        };
        for path in paths {
            if self.mon.clips.len() + self.mon.probing >= MAX_CLIPS {
                self.fail(format!("A montage can use up to {MAX_CLIPS} clips."));
                break;
            }
            if self.mon.clips.iter().any(|c| c.path == path) {
                continue;
            }
            self.mon.probing += 1;
            let (tools, mail) = (tools.clone(), self.mail.clone());
            std::thread::spawn(move || {
                let probe = media::probe(&tools, &path);
                let picture = probe.as_ref().ok().and_then(|p| grab_thumb(&tools, p, &path));
                mail.send(Msg::Montage(Event::Probed(path.clone(), probe)));
                if let Some(frame) = picture {
                    mail.send(Msg::Montage(Event::Thumb(path, frame)));
                }
            });
        }
    }

    pub fn montage_event(&mut self, event: Event) {
        match event {
            Event::Rendered(cuts, credit) => {
                self.mon.rendered = cuts;
                self.mon.credit = credit;
            }
            Event::Restored(clips, saved) => {
                self.mon.probing = self.mon.probing.saturating_sub(1);
                if clips.len() == saved {
                    self.mon.clips = clips;
                    self.note(if self.mon.plan.is_some() { "Restored your last montage, including the AI's plan. Nothing needs to be asked again." } else { "Restored your clips from last time." });
                } else {
                    // Some files moved or were deleted, so the saved cut list no longer lines up.
                    self.mon.clips = clips;
                    self.mon.plan = None;
                    self.mon.keep.clear();
                    self.mon.rendered.clear();
                    self.note("Some clips from your last session are gone, so the AI plan was cleared. Plan again when you're ready.");
                }
            }
            Event::Probed(path, result) => {
                self.mon.probing = self.mon.probing.saturating_sub(1);
                match result {
                    Ok(probe) if probe.duration >= 2.0 => {
                        self.mon.clips.push(Clip { path, probe });
                        self.mon.plan = None;
                        self.mon.tracks.clear();
                    }
                    Ok(_) => self.fail(format!("{} is shorter than 2 seconds.", path.display())),
                    Err(e) => self.fail(format!("{}: {e}", path.display())),
                }
            }
            Event::Thumb(path, frame) => {
                let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb);
                let tex = self.mail.ctx.load_texture(format!("clip-{}", path.display()), image, egui::TextureOptions::LINEAR);
                self.mon.thumbs.insert(path, tex);
            }
            Event::PreviewFrame(i, result) => {
                self.mon.preview.busy = false;
                match result {
                    Ok(frame) if i == self.mon.preview.cur && self.mon.preview.player.is_none() => {
                        if let Some(clip) = self.mon.plan.as_ref().and_then(|p| p.cuts.get(i)).map(|c| c.clip) {
                            self.preview_show(clip, &frame);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => self.fail(e),
                }
            }
            Event::Note(text) => self.note(text),
            Event::Plan(Ok(plan)) => {
                self.mon.busy = None;
                self.mon.preview = Preview::default();
                self.mon.keep = vec![true; plan.cuts.len()];
                self.mon.rendered.clear();
                self.mon.credit.clear();
                self.mon.title = plan.title.clone();
                self.mon.music_query = plan.music_query.clone();
                let total: f64 = plan.cuts.iter().map(Cut::len).sum();
                let reused = if plan.reused { " Reused your saved AI answer, so no tokens were used." } else { "" };
                self.note(format!("The AI chose {} moments ({} before fades). Untick any you don't want, then make the montage.{reused}", plan.cuts.len(), clock(total)));
                self.mon.plan = Some(plan);
            }
            Event::Plan(Err(e)) => {
                self.mon.busy = None;
                self.mon.searching = false;
                self.fail(e);
            }
            Event::Tracks(result) => {
                self.mon.searching = false;
                match result {
                    Ok(tracks) => {
                        self.mon.tracks = tracks;
                        self.mon.track = 0;
                        self.note("Found NCS music that fits. The first song is selected; pick another if you like.");
                    }
                    Err(e) => self.fail(e),
                }
            }
            Event::Progress(p, label) => {
                if let Some(r) = &mut self.mon.render {
                    r.0 = p;
                    r.1 = label;
                }
            }
            Event::Done(result) => {
                self.mon.render = None;
                match result {
                    Ok(files) => {
                        let how = std::mem::take(&mut self.encode_info);
                        self.note(format!("Finished. {} file(s) saved.{how}", files.len()));
                        self.mon.made = files;
                    }
                    Err(e) if e == "Cancelled" => self.note("Cancelled. Nothing half-finished was left behind."),
                    Err(e) => self.fail(format!("Couldn't make the montage: {e}")),
                }
            }
        }
    }

    /// Brings back the montage from the last run: choices, AI plan, songs and clips.
    pub fn restore_montage(&mut self) {
        let (Some(saved), Some(tools)) = (store::load::<Saved>("montage"), self.tools.clone()) else { return };
        let m = &mut self.mon;
        (m.style, m.sound, m.vertical, m.target, m.look, m.source) = (saved.style, saved.sound, saved.vertical, saved.target.clamp(10.0, 900.0), saved.look.filter(|i| *i < LOOKS.len()), saved.source);
        // Songs saved by an older version came from other catalogues; only NCS tracks are kept.
        (m.file, m.tracks, m.music_query, m.title) = (saved.file, saved.tracks.into_iter().filter(|t| t.license == "ncs").collect(), saved.music_query, saved.title);
        m.track = saved.track.min(m.tracks.len().saturating_sub(1));
        (m.make_wide, m.make_vertical, m.out_dir) = (saved.make_wide, saved.make_vertical, saved.out_dir.filter(|d| d.is_dir()));
        m.blur = saved.blur.clamped();
        m.keep = saved.keep;
        m.plan = saved.plan.filter(|p| p.cuts.len() == m.keep.len() && p.grades.len() == saved.clips.len());
        if m.plan.is_none() {
            m.keep.clear();
            m.rendered.clear();
        } else {
            m.rendered = saved.rendered;
        }
        m.credit = saved.credit;
        m.pack = PackState::with(saved.pack);
        if saved.clips.is_empty() {
            return;
        }
        m.probing += 1;
        let (mail, expected) = (self.mail.clone(), saved.clips.len());
        std::thread::spawn(move || {
            // Keep the original order, and drop any file that is no longer there.
            let clips: Vec<Clip> = saved.clips.into_iter().filter_map(|path| media::probe(&tools, &path).ok().map(|probe| Clip { path, probe })).collect();
            let pictures: Vec<(PathBuf, media::Frame)> = clips.iter().filter_map(|c| grab_thumb(&tools, &c.probe, &c.path).map(|f| (c.path.clone(), f))).collect();
            mail.send(Msg::Montage(Event::Restored(clips, expected)));
            for (path, frame) in pictures {
                mail.send(Msg::Montage(Event::Thumb(path, frame)));
            }
        });
    }

    fn search_music(&mut self) {
        if self.mon.searching {
            return;
        }
        self.mon.searching = true;
        let (query, target, mail) = (self.mon.music_query.clone(), self.mon.target, self.mail.clone());
        std::thread::spawn(move || mail.send(Msg::Montage(Event::Tracks(music::search(&query, target)))));
    }

    fn plan_montage(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else {
            self.fail("Add your OpenAI key first (the box under 'AI helper' on this screen).");
            return;
        };
        if self.mon.clips.is_empty() {
            self.fail("Add at least one clip first.");
            return;
        }
        if self.mon.style.needs_music() && !self.mon.sound.uses_music() {
            self.fail("A beat-synced montage needs music. Choose a sound option that includes music.");
            return;
        }
        let m = &self.mon;
        let (clips, style, target, look, notes) = (m.clips.clone(), m.style, m.target, m.look, self.style_notes.clone());
        let wants_music = m.sound.uses_music() && m.source == MusicSource::Online;
        let model = ai::OPENAI_MODELS[self.openai_model].0;
        let reuse = self.reuse_ai;
        self.mon.busy = Some("Planning the montage…".into());
        self.mon.plan = None;
        self.mon.tracks.clear();
        self.mon.searching = wants_music;
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let note = |text: String| mail.send(Msg::Montage(Event::Note(text)));
            let result = (|| -> Result<PlanData, String> {
                let (mut briefs, mut energies, mut grades) = (Vec::new(), Vec::new(), Vec::new());
                for (i, clip) in clips.iter().enumerate() {
                    note(format!("Looking at clip {} of {}: {}…", i + 1, clips.len(), clip.name()));
                    let count = (clip.probe.duration / 12.0).clamp(6.0, 12.0) as usize;
                    let frames = montage::sample_frames(&tools, clip, count, 384)?;
                    let energy = montage::analyse_energy(&tools, clip)?;
                    grades.push(montage::clip_grade(&frames, look));
                    briefs.push(ai::ClipBrief { name: clip.name(), duration: clip.probe.duration, peaks: energy.peaks(5), frames });
                    energies.push(energy);
                }
                // The same clips, style, length, model and wording always make the same question.
                let mut facts = vec!["plan".to_string(), model.to_string(), style.name().to_string(), format!("{target:.0}"), notes.trim().to_string(), format!("{look:?}")];
                facts.extend(clips.iter().map(|c| format!("{}|{:.2}", store::fingerprint(&c.path), c.probe.duration)));
                let cache = format!("ai-plan-{}", store::hash(&facts));
                let (plan, reused) = match reuse.then(|| store::load::<ai::AiPlan>(&cache)).flatten() {
                    Some(saved) => (saved, true),
                    None => {
                        note(format!("Asking {model} to choose the best moments…"));
                        let fresh = ai::openai_plan(&key, model, &briefs, style, target, &notes)?;
                        store::save(&cache, &fresh);
                        (fresh, false)
                    }
                };
                let durations: Vec<f64> = clips.iter().map(|c| c.probe.duration).collect();
                let cuts = montage::plan_cuts(plan.cuts, &durations, &energies, style, target);
                if cuts.is_empty() {
                    return Err("The AI's suggestions didn't fit your clips. Try again.".into());
                }
                Ok(PlanData { cuts, grades, title: plan.title, music_query: plan.music_query, tempo: plan.tempo, reused })
            })();
            let query = result.as_ref().ok().map(|p| p.music_query.clone());
            mail.send(Msg::Montage(Event::Plan(result)));
            if let (true, Some(query)) = (wants_music, query) {
                note("Searching NCS for music…".into());
                mail.send(Msg::Montage(Event::Tracks(music::search(&query, target))));
            }
        });
    }

    fn render_montage(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let m = &self.mon;
        let Some(plan) = &m.plan else { return };
        let cuts: Vec<Cut> = plan.cuts.iter().zip(&m.keep).filter(|(_, k)| **k).map(|(c, _)| c.clone()).collect();
        if cuts.is_empty() {
            self.fail("Every moment is unticked. Keep at least one.");
            return;
        }
        if !m.make_wide && !m.make_vertical {
            self.fail("Tick 16:9, 9:16, or both.");
            return;
        }
        // The chosen song first, then the next few, in case a server refuses one.
        let candidates: Vec<Track> = if m.source == MusicSource::Online { m.tracks.iter().cycle().skip(m.track).take(m.tracks.len().min(4)).cloned().collect() } else { Vec::new() };
        let file = (m.source == MusicSource::File).then(|| m.file.clone()).flatten();
        if m.sound.uses_music() && candidates.is_empty() && file.is_none() {
            self.fail("Choose music first: find NCS music, pick your own file, or switch the sound to original only.");
            return;
        }
        let (clips, grades, style, sound, vertical, target) = (m.clips.clone(), plan.grades.clone(), m.style, m.sound, m.vertical, m.target);
        let (wide, tall, title, blur) = (m.make_wide, m.make_vertical, m.title.trim().to_string(), m.blur);
        let encoder = self.encoder_choice.resolve(&self.encoders);
        let out_dir = m.out_dir.clone().or_else(|| clips.first().and_then(|c| c.path.parent().map(PathBuf::from))).unwrap_or_else(std::env::temp_dir);
        let cancel = Arc::new(AtomicBool::new(false));
        self.mon.render = Some((0.0, "Getting ready…".into(), cancel.clone()));
        self.mon.made.clear();
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let began = std::time::Instant::now();
            let work = std::env::temp_dir().join(format!("video-enhancer-montage-{}", std::process::id()));
            let result = (|| -> Result<Vec<PathBuf>, String> {
                let say = |p: f32, text: &str| mail.send(Msg::Montage(Event::Progress(p, text.to_string())));
                std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
                for (i, g) in grades.iter().enumerate() {
                    Lut::build(LUT_SIZE, g, None).write_cube(&work.join(format!("lut{i}.cube"))).map_err(|e| e.to_string())?;
                }
                let mut cuts = cuts;
                let durations: Vec<f64> = clips.iter().map(|c| c.probe.duration).collect();
                let mut used: Option<Track> = None;

                // Music, and for a beat-synced edit the beat grid.
                let mut song = None;
                if sound.uses_music() {
                    let path = match &file {
                        Some(f) => f.clone(),
                        None => {
                            let mut found = None;
                            let mut why = String::new();
                            for t in &candidates {
                                say(0.0, &format!("Downloading \"{}\"…", t.title));
                                match music::download(t, &work) {
                                    Ok((p, track)) => {
                                        used = Some(track);
                                        found = Some(p);
                                        break;
                                    }
                                    Err(e) => why = e,
                                }
                            }
                            found.ok_or_else(|| format!("None of the songs could be downloaded. Last problem: {why} Choose your own music file instead, or switch the sound to original only."))?
                        }
                    };
                    let mut start = 0.0;
                    if style == Style::BeatSync {
                        say(0.0, "Finding the beat…");
                        let beats = music::detect_beats(&tools, &path)?;
                        montage::quantise(&mut cuts, &durations, beats.interval, target);
                        start = beats.first;
                    }
                    song = Some(Music { path, start });
                }

                // Where the action is, for vertical videos that follow it.
                let mut actions: Vec<Option<Vec<(f64, f32)>>> = vec![None; clips.len()];
                if tall && vertical == Vertical::Follow {
                    for i in cuts.iter().map(|c| c.clip).collect::<std::collections::BTreeSet<_>>() {
                        say(0.0, &format!("Following the action in {}…", clips[i].name()));
                        actions[i] = media::analyse_action(&tools, &clips[i].probe, &clips[i].path).ok();
                    }
                }

                if style == Style::Story && !title.is_empty() {
                    if let Some(font) = title_font() {
                        std::fs::copy(font, work.join("font.ttf")).map_err(|e| e.to_string())?;
                        std::fs::write(work.join("title.txt"), &title).map_err(|e| e.to_string())?;
                    }
                }

                let stem = safe_name(&title);
                let shapes: Vec<(&str, Out, &str)> = [(wide, "16:9", Out::Wide, "_16x9"), (tall, "9:16", Out::Vertical(vertical), "_9x16")].into_iter().filter(|s| s.0).map(|s| (s.1, s.2, s.3)).collect();
                let mut made = Vec::new();
                let mut fell_back = false;
                for (i, (label, out, suffix)) in shapes.iter().enumerate() {
                    let output = media::free_name(&out_dir, &stem, suffix, "mp4");
                    let job = Job { clips: &clips, actions: &actions, cuts: &cuts, style, sound, music: song.as_ref(), out: *out, encoder, blur, title: Some(&title), workdir: &work, output: &output };
                    let steps = shapes.len() as f32;
                    media_progress(&mail, i as f32, steps, label);
                    fell_back |= montage::render(&tools, &job, &cancel, |f| media_progress(&mail, i as f32 + f, steps, label))? != encoder;
                    made.push(output);
                }
                mail.send(Msg::EncodeInfo(crate::encode_summary(began.elapsed().as_secs(), encoder, fell_back, false)));
                mail.send(Msg::Montage(Event::Rendered(cuts.clone(), used.as_ref().map(Track::credit).unwrap_or_default())));
                if let Some(t) = &used {
                    let credits = media::free_name(&out_dir, &stem, "_music_credits", "txt");
                    std::fs::write(&credits, music::credits_text(t)).map_err(|e| e.to_string())?;
                    made.push(credits);
                }
                Ok(made)
            })();
            std::fs::remove_dir_all(&work).ok();
            mail.send(Msg::Montage(Event::Done(result)));
        });
    }

    pub fn montage_screen(&mut self, ui: &mut egui::Ui, step: Step) {
        self.preview_tick();
        self.montage_summary(ui, step);
        egui::CentralPanel::default().frame(Frame::new().fill(theme::WINDOW)).show(ui, |ui| {
            // VE_SCROLL=900 starts the page scrolled down, for screenshots of the lower sections.
            let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
            if let Some(y) = std::env::var("VE_SCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
                area = area.vertical_scroll_offset(y);
            }
            area.show(ui, |ui| {
                Frame::new().inner_margin(Margin { left: 28, right: 28, top: 16, bottom: 28 }).show(ui, |ui| match step {
                    Step::Source => self.clips_step(ui),
                    Step::Look => self.plan_step(ui),
                    _ => self.make_step(ui),
                });
            });
        });
    }

    // ----------------------------------------------------------------- preview

    fn preview_kept(&self) -> Vec<usize> {
        self.mon.keep.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| i).collect()
    }

    /// Shows one moment as a still picture, and moves the preview to it.
    fn preview_select(&mut self, i: usize) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(cut) = self.mon.plan.as_ref().and_then(|p| p.cuts.get(i).cloned()) else { return };
        let Some(clip) = self.mon.clips.get(cut.clip).cloned() else { return };
        let p = &mut self.mon.preview;
        (p.player, p.cur, p.t, p.busy, p.started) = (None, i, cut.start, true, true);
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let options = media::GrabOptions { width: 960, finish: media::Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
            let at = cut.start + cut.len() * 0.3;
            mail.send(Msg::Montage(Event::PreviewFrame(i, media::grab_frame(&tools, &clip.probe, &clip.path, at, &options))));
        });
    }

    fn preview_play_from(&mut self, i: usize) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(cut) = self.mon.plan.as_ref().and_then(|p| p.cuts.get(i).cloned()) else { return };
        let Some(clip) = self.mon.clips.get(cut.clip).cloned() else { return };
        let ctx = self.mail.ctx.clone();
        let p = &mut self.mon.preview;
        (p.cur, p.t, p.started) = (i, cut.start, true);
        p.player = Some(media::play(&tools, &clip.probe, &clip.path, cut.start, move || ctx.request_repaint()));
    }

    fn preview_toggle(&mut self) {
        if self.mon.preview.player.is_some() {
            self.mon.preview.player = None;
            return;
        }
        let kept = self.preview_kept();
        let cur = self.mon.preview.cur;
        let Some(first) = kept.first().copied() else { return };
        let start = if kept.contains(&cur) { cur } else { kept.iter().copied().find(|k| *k > cur).unwrap_or(first) };
        self.preview_play_from(start);
    }

    /// Draws a decoded frame with the colour of its clip.
    fn preview_show(&mut self, clip: usize, frame: &media::Frame) {
        let Some(grade) = self.mon.plan.as_ref().and_then(|p| p.grades.get(clip).copied()) else { return };
        if self.mon.preview.lut.as_ref().is_none_or(|(c, _)| *c != clip) {
            self.mon.preview.lut = Some((clip, Lut::build(17, &grade, None)));
        }
        let graded = self.mon.preview.lut.as_ref().map(|(_, l)| l.apply(&frame.rgb)).unwrap_or_default();
        let size = [frame.width, frame.height];
        let image = egui::ColorImage::from_rgb(size, &graded);
        let options = egui::TextureOptions::LINEAR;
        match &mut self.mon.preview.tex {
            Some(tex) if tex.size() == size => tex.set(image, options),
            slot => *slot = Some(self.mail.ctx.load_texture("montage-preview", image, options)),
        }
    }

    /// Moves playback along: new frames to the screen, and on to the next kept moment at the end of one.
    fn preview_tick(&mut self) {
        let Some(cut) = self.mon.plan.as_ref().and_then(|p| p.cuts.get(self.mon.preview.cur).cloned()) else { return };
        let (mut latest, mut ended) = (None, false);
        if let Some(player) = &self.mon.preview.player {
            loop {
                match player.rx.try_recv() {
                    Ok(item) => latest = Some(item),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        ended = true;
                        break;
                    }
                }
            }
        }
        if let Some((t, frame)) = latest {
            self.preview_show(cut.clip, &frame);
            self.mon.preview.t = t;
            ended |= t >= cut.end;
        }
        if ended && self.mon.preview.player.is_some() {
            let cur = self.mon.preview.cur;
            match self.preview_kept().into_iter().find(|k| *k > cur) {
                Some(next) => self.preview_play_from(next),
                None => self.mon.preview.player = None,
            }
        }
    }

    /// The picture, the play controls and the moments as one clickable bar.
    fn preview_card(&mut self, ui: &mut egui::Ui) {
        let Some(plan) = &self.mon.plan else { return };
        let cuts = plan.cuts.clone();
        let (cur, playing, phone) = (self.mon.preview.cur.min(cuts.len().saturating_sub(1)), self.mon.preview.player.is_some(), self.mon.preview.phone);
        let tex = self.mon.preview.tex.clone();
        let kept: Vec<usize> = self.preview_kept();
        let (mut select, mut toggle, mut step) = (None, false, 0i32);
        let mut phone_now = phone;
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::bold("Preview", 14.0));
                ui.label(theme::muted("A quick silent look. Music, fades and exact colour come when you make it."));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    theme::segmented(ui, &mut phone_now, &[(true, "9:16"), (false, "16:9")], false);
                });
            });
            let (region, _) = ui.allocate_exact_size(vec2(ui.available_width(), 330.0), Sense::hover());
            ui.painter().rect_filled(region, 12, Color32::from_rgb(0x0B, 0x0B, 0x0D));
            if let Some(tex) = &tex {
                let aspect = tex.size()[0] as f32 / tex.size()[1].max(1) as f32;
                if phone_now {
                    let h = region.height() - 20.0;
                    paint_phone(ui, Rect::from_center_size(region.center(), vec2(h * 9.0 / 16.0, h)), tex, self.mon.vertical, self.mon.blur);
                } else {
                    let w = (region.width() - 20.0).min((region.height() - 20.0) * aspect);
                    let r = Rect::from_center_size(region.center(), vec2(w, w / aspect));
                    egui::Image::new(egui::load::SizedTexture::new(tex.id(), r.size())).corner_radius(10).paint_at(ui, r);
                }
            } else {
                ui.painter().text(region.center(), egui::Align2::CENTER_CENTER, if self.mon.preview.busy { "Getting the picture…" } else { "Pick a moment" }, egui::FontId::proportional(14.0), theme::TEXT3);
            }
            let kept_total: f64 = kept.iter().map(|&i| cuts[i].len()).sum();
            let done: f64 = kept.iter().filter(|&&i| i < cur).map(|&i| cuts[i].len()).sum::<f64>() + if kept.contains(&cur) { (self.mon.preview.t - cuts[cur].start).clamp(0.0, cuts[cur].len()) } else { 0.0 };
            ui.horizontal(|ui| {
                if theme::round_button(ui, if playing { Icon::Pause } else { Icon::Play }, if playing { "Pause" } else { "Play" }).clicked() {
                    toggle = true;
                }
                if theme::small_button(ui, "Previous").clicked() {
                    step = -1;
                }
                if theme::small_button(ui, "Next").clicked() {
                    step = 1;
                }
                ui.label(theme::mono(format!("{} / {}", clock(done), clock(kept_total)), 13.0).color(theme::TEXT));
                if let Some(cut) = cuts.get(cur) {
                    let name = self.mon.clips.get(cut.clip).map_or_else(String::new, Clip::name);
                    ui.add(egui::Label::new(theme::muted(format!("Moment {} of {} · {} · {}", cur + 1, cuts.len(), name, cut.why))).truncate());
                }
            });
            // The moments as one bar: width is length, colour is the clip. Click one to look at it.
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
            let total: f64 = cuts.iter().map(Cut::len).sum::<f64>().max(0.1);
            let mut x = rect.left();
            let mut head = None;
            for (i, cut) in cuts.iter().enumerate() {
                let w = (rect.width() as f64 * cut.len() / total) as f32;
                let colour = clip_colour(cut.clip);
                let block = Rect::from_min_size(pos2(x, rect.top()), vec2((w - 2.0).max(2.0), rect.height()));
                let on = self.mon.keep.get(i).copied().unwrap_or(true);
                ui.painter().rect_filled(block, 5, if on { colour } else { theme::tint(colour, 40) });
                if i == cur {
                    ui.painter().rect_stroke(block.expand(1.0), 6, Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Outside);
                    head = Some(block.left() + block.width() * ((self.mon.preview.t - cut.start) / cut.len().max(0.1)).clamp(0.0, 1.0) as f32);
                }
                if response.clicked() && response.interact_pointer_pos().is_some_and(|p| p.x >= x && p.x < x + w) {
                    select = Some(i);
                }
                x += w;
            }
            if let Some(h) = head {
                ui.painter().rect_filled(Rect::from_min_max(pos2(h - 1.0, rect.top() - 4.0), pos2(h + 1.0, rect.bottom() + 4.0)), 1, Color32::WHITE);
            }
            ui.horizontal_wrapped(|ui| {
                for (i, clip) in self.mon.clips.iter().enumerate() {
                    let (dot, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, clip_colour(i));
                    ui.label(theme::muted(format!("{}. {}", i + 1, clip.name())));
                }
            });
        });
        self.mon.preview.phone = phone_now;
        if step != 0 {
            let at = kept.iter().position(|&k| k >= cur);
            let target = match (step, at) {
                (1, Some(p)) if kept.get(p) == Some(&cur) => kept.get(p + 1),
                (1, Some(p)) => kept.get(p),
                (_, Some(p)) => p.checked_sub(1).and_then(|q| kept.get(q)),
                (1, None) => None,
                (_, None) => kept.last(),
            };
            select = target.copied().or(select);
        }
        if let Some(i) = select {
            let was_playing = self.mon.preview.player.is_some();
            if was_playing && kept.contains(&i) {
                self.preview_play_from(i);
            } else {
                self.preview_select(i);
            }
        } else if toggle {
            self.preview_toggle();
        } else if tex.is_none() && !self.mon.preview.busy && !self.mon.preview.started && let Some(first) = kept.first() {
            self.preview_select(*first);
            // Developer aid: VE_PLAY=1 starts playing at once, for screenshots.
            if std::env::var_os("VE_PLAY").is_some() {
                self.preview_toggle();
            }
        }
    }

    // ------------------------------------------------------------------ summary

    fn montage_summary(&mut self, ui: &mut egui::Ui, step: Step) {
        let mut plan = false;
        let mut make = false;
        let mut publish = false;
        egui::Panel::right("montage-summary").exact_size(372.0).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(20, 0))).show(ui, |ui| {
            egui::Panel::bottom("montage-footer").exact_size(if self.mon.made.is_empty() { 84.0 } else { 140.0 }).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(0, 16))).show_separator_line(true).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                let m = &self.mon;
                let working = m.busy.is_some() || m.render.is_some();
                let width = ui.available_width();
                if m.plan.is_none() {
                    ui.add_enabled_ui(!m.clips.is_empty() && self.has_openai && !working, |ui| {
                        plan = theme::button(ui, if m.busy.is_some() { "Planning…" } else { "Plan with AI" }, Kind::Primary, width).clicked();
                    });
                } else {
                    ui.add_enabled_ui(!working, |ui| {
                        make = theme::button(ui, if m.render.is_some() { "Making the montage…" } else if m.made.is_empty() { "Make the montage" } else { "Make it again" }, if m.made.is_empty() { Kind::Primary } else { Kind::Secondary }, width).clicked();
                    });
                    if !m.made.is_empty() {
                        publish = theme::button_with_icon(ui, "Continue to Publish", Some(Icon::ArrowRight), Kind::Primary, width).clicked();
                    }
                }
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(16.0);
                ui.label(theme::bold("Your montage", 15.0));
                let m = &self.mon;
                theme::card(ui, |ui| {
                    summary_row(ui, "Clips", &if m.clips.is_empty() { "none yet".to_string() } else { format!("{} · {}", m.clips.len(), clock(m.total())) });
                    summary_row(ui, "Style", m.style.name());
                    summary_row(ui, "Length", &format!("about {}", clock(m.target)));
                    summary_row(ui, "Sound", m.sound.name());
                    if m.sound.uses_music() {
                        let music = match (m.source, m.tracks.get(m.track), &m.file) {
                            (MusicSource::File, _, Some(f)) => f.file_name().map_or_else(|| "your file".to_string(), |n| n.to_string_lossy().into_owned()),
                            (MusicSource::File, _, None) => "no file chosen".to_string(),
                            (MusicSource::Online, Some(t), _) => t.title.clone(),
                            (MusicSource::Online, None, _) => "found after planning".to_string(),
                        };
                        summary_row(ui, "Music", &music);
                    }
                    summary_row(ui, "Look", m.look.map_or("Natural (fix only)", |i| LOOKS[i].name));
                    let shapes = match (m.make_wide, m.make_vertical) {
                        (true, true) => "16:9 and 9:16",
                        (true, false) => "16:9",
                        (false, true) => "9:16",
                        (false, false) => "nothing ticked",
                    };
                    summary_row(ui, "Makes", shapes);
                    if let Some(p) = &m.plan {
                        let kept = m.keep.iter().filter(|k| **k).count();
                        summary_row(ui, "Moments", &format!("{kept} of {} kept", p.cuts.len()));
                    }
                });
                match (step, self.mon.plan.is_some()) {
                    (Step::Source, false) => {
                        ui.label(theme::muted("Next, the AI looks at small pictures from each clip and picks the best moments. Your video files are not uploaded."));
                    }
                    (_, true) if self.mon.made.is_empty() => {
                        ui.label(theme::muted("Untick any moment you don't want, then make the montage."));
                    }
                    _ => {}
                }
                if !self.has_openai && self.mon.plan.is_none() {
                    theme::card_with(ui, theme::tint(theme::WARN, 18), theme::tint(theme::WARN, 70), |ui| {
                        ui.label(theme::bold("OpenAI key needed", 13.5));
                        ui.label(theme::soft("Planning uses your own OpenAI key."));
                        if theme::small_button(ui, "Add it in Settings").clicked() {
                            self.show_settings = true;
                        }
                    });
                }
                if !self.mon.made.is_empty() {
                    theme::card_with(ui, theme::tint(theme::OK, 18), theme::tint(theme::OK, 70), |ui| {
                        ui.label(theme::bold("Saved", 14.0));
                        for file in &self.mon.made {
                            ui.label(RichText::new(file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())).size(12.5).color(theme::TEXT2));
                        }
                        if theme::small_button(ui, "Show in folder").clicked()
                            && let Some(parent) = self.mon.made[0].parent()
                        {
                            crate::screens::show_in_folder(parent);
                        }
                    });
                }
                ui.add_space(16.0);
            });
        });
        if plan {
            self.step = Step::Look;
            self.plan_montage();
        }
        if make {
            self.step = Step::Export;
            self.render_montage();
        }
        if publish {
            self.step = Step::Publish;
        }
    }

    // ------------------------------------------------------------------- clips

    fn clips_step(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::bold("Clips", 18.0));
            ui.label(theme::soft("Drop several clips anywhere in this window. The order is the order they play in."));
        });
        ui.add_space(6.0);
        let mut action: Option<(usize, i32)> = None;
        let mut add = false;
        let gap = 14.0;
        let width = ((ui.available_width() - gap * 2.0) / 3.0).floor();
        let count = self.mon.clips.len();
        let tiles = count + 1;
        for row in 0..tiles.div_ceil(3) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for col in 0..3 {
                    let i = row * 3 + col;
                    if i < count {
                        let (name, probe_text, duration) = {
                            let c = &self.mon.clips[i];
                            (c.name(), format!("{}×{}", c.probe.width, c.probe.height), c.probe.duration)
                        };
                        let thumb = self.mon.thumbs.get(&self.mon.clips[i].path).cloned();
                        ui.allocate_ui_with_layout(vec2(width, width * 9.0 / 16.0 + 74.0), Layout::top_down(Align::LEFT), |ui| {
                            let (rect, _) = ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::hover());
                            ui.painter().rect_filled(rect, 12, theme::INPUT);
                            if let Some(tex) = thumb {
                                egui::Image::new(egui::load::SizedTexture::new(tex.id(), rect.size())).corner_radius(12).paint_at(ui, rect);
                            } else {
                                theme::paint_icon(ui.painter(), rect.center(), 28.0, Icon::Film, theme::TEXT3, 1.5);
                            }
                            let badge = ui.painter().layout_no_wrap(clock(duration), egui::FontId::new(12.0, egui::FontFamily::Monospace), theme::TEXT);
                            let br = Rect::from_min_size(pos2(rect.right() - badge.size().x - 22.0, rect.bottom() - 30.0), badge.size() + vec2(16.0, 8.0));
                            ui.painter().rect_filled(br, 6, Color32::from_black_alpha(190));
                            ui.painter().galley(br.center() - badge.size() / 2.0, badge, theme::TEXT);
                            let number = Rect::from_min_size(rect.min + vec2(10.0, 10.0), vec2(26.0, 26.0));
                            ui.painter().circle_filled(number.center(), 13.0, theme::ACCENT);
                            ui.painter().text(number.center(), egui::Align2::CENTER_CENTER, format!("{}", i + 1), egui::FontId::new(13.0, theme::bold_family()), theme::ON_ACCENT);
                            ui.add(egui::Label::new(theme::bold(&name, 13.5)).truncate());
                            ui.label(theme::mono(probe_text, 12.0).color(theme::TEXT3));
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if i > 0 && theme::small_button(ui, "Earlier").clicked() {
                                    action = Some((i, -1));
                                }
                                if i + 1 < count && theme::small_button(ui, "Later").clicked() {
                                    action = Some((i, 1));
                                }
                                if theme::small_button(ui, "Remove").clicked() {
                                    action = Some((i, 0));
                                }
                            });
                        });
                    } else if i == count {
                        let (rect, response) = ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::click());
                        let painter = ui.painter();
                        painter.rect(rect, 12, if response.hovered() { theme::CARD } else { Color32::from_rgb(0x14, 0x15, 0x18) }, Stroke::new(2.0, theme::LINE_STRONG), egui::StrokeKind::Inside);
                        theme::paint_icon(painter, rect.center() - vec2(0.0, 12.0), 26.0, Icon::Plus, theme::ACCENT_TEXT, 2.0);
                        painter.text(rect.center() + vec2(0.0, 22.0), egui::Align2::CENTER_CENTER, if self.mon.probing > 0 { "Reading clips…" } else { "Add clips" }, egui::FontId::new(14.0, theme::bold_family()), theme::TEXT);
                        if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            add = true;
                        }
                    }
                }
            });
            ui.add_space(8.0);
        }
        if add && let Some(paths) = rfd::FileDialog::new().add_filter("Video", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "wmv"]).pick_files() {
            self.add_clips(paths);
        }
        if let Some((i, step)) = action {
            match step {
                0 => {
                    let gone = self.mon.clips.remove(i);
                    self.mon.thumbs.remove(&gone.path);
                }
                s => self.mon.clips.swap(i, (i as i32 + s) as usize),
            }
            self.mon.plan = None;
            self.mon.tracks.clear();
        }

        ui.add_space(8.0);
        ui.label(theme::bold("Style", 15.0));
        let gap = 14.0;
        let width = ((ui.available_width() - gap * 2.0) / 3.0).floor();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for style in Style::ALL {
                if choice_card(ui, width, style.name(), style.blurb(), self.mon.style == style).clicked() {
                    self.mon.style = style;
                }
            }
        });
        ui.add_space(10.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let half = ((ui.available_width() - gap) / 2.0).floor();
            ui.allocate_ui_with_layout(vec2(half, 0.0), Layout::top_down(Align::LEFT), |ui| {
                theme::card(ui, |ui| {
                    ui.label(theme::bold("Length", 14.0));
                    ui.label(theme::muted("About how long the finished montage should be"));
                    ui.horizontal_wrapped(|ui| {
                        for (label, secs) in [("30s", 30.0), ("1 min", 60.0), ("2 min", 120.0), ("3 min", 180.0), ("5 min", 300.0)] {
                            let on = (self.mon.target - secs).abs() < 0.5;
                            if chip(ui, label, on).clicked() {
                                self.mon.target = secs;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(theme::soft("Exactly"));
                        ui.add(egui::DragValue::new(&mut self.mon.target).range(10.0..=900.0).speed(1.0).suffix(" seconds"));
                    });
                });
            });
            ui.allocate_ui_with_layout(vec2(half, 0.0), Layout::top_down(Align::LEFT), |ui| {
                theme::card(ui, |ui| {
                    ui.label(theme::bold("Sound", 14.0));
                    ui.label(theme::muted(self.mon.sound.name()));
                    theme::segmented(ui, &mut self.mon.sound, &[(Sound::MusicAndOriginal, "Music + game"), (Sound::OriginalOnly, "Game only"), (Sound::MusicOnly, "Music only")], true);
                });
            });
        });
        ui.add_space(4.0);
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::bold("Colour look", 14.0));
                ui.label(theme::muted("Each clip is corrected on its own, then given this look"));
            });
            egui::ComboBox::from_id_salt("m_look").selected_text(self.mon.look.map_or("Natural (fix only)", |i| LOOKS[i].name)).width(260.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.mon.look, None, "Natural (fix only)");
                for (i, look) in LOOKS.iter().enumerate() {
                    ui.selectable_value(&mut self.mon.look, Some(i), look.name);
                }
            });
            ui.add_space(4.0);
            ui.label(theme::bold("Anything special?", 14.0));
            ui.add(egui::TextEdit::multiline(&mut self.style_notes).desired_rows(2).desired_width(f32::INFINITY).margin(Margin::symmetric(12, 9)));
        });
        if self.mon.sound.uses_music() {
            self.music_card(ui);
        }
    }

    fn music_card(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::icon(ui, Icon::Music, 16.0, theme::ACCENT_TEXT);
                ui.label(theme::bold("Music", 14.0));
            });
            theme::segmented(ui, &mut self.mon.source, &[(MusicSource::Online, "NCS music (no copyright claims)"), (MusicSource::File, "Use my own file")], false);
            match self.mon.source {
                MusicSource::Online => {
                    ui.label(theme::muted("Music from NCS (NoCopyrightSounds), made for creators: free in monetised YouTube and Twitch videos with no copyright strikes. The AI chooses the mood. NCS asks you to credit the song in your description, so the exact credit text is saved next to the montage and added to your upload pack."));
                    if self.mon.plan.is_some() {
                        let mut again = false;
                        ui.horizontal(|ui| {
                            ui.label(theme::soft("Mood"));
                            ui.add(egui::TextEdit::singleline(&mut self.mon.music_query).desired_width(200.0).margin(Margin::symmetric(10, 7)));
                            ui.add_enabled_ui(!self.mon.searching, |ui| again = theme::small_button(ui, "Search again").clicked());
                            if self.mon.searching {
                                ui.spinner();
                            }
                        });
                        if again {
                            self.search_music();
                        }
                        let mut picked = None;
                        for (i, t) in self.mon.tracks.iter().enumerate().take(8) {
                            let on = self.mon.track == i;
                            let response = Frame::new().fill(if on { theme::accent_soft(24) } else { theme::INPUT }).stroke(Stroke::new(if on { 1.5 } else { 1.0 }, if on { theme::ACCENT } else { theme::LINE_STRONG })).corner_radius(10).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.add(egui::Label::new(theme::bold(&t.title, 13.0)).truncate());
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        theme::pill(ui, if t.instrumental { "Instrumental" } else { "With vocals" }, theme::TEXT2, theme::CARD, None);
                                    });
                                });
                                ui.label(theme::muted(format!("{} · {}", t.creator, t.source)));
                            }).response.interact(Sense::click());
                            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                picked = Some(i);
                            }
                        }
                        if let Some(i) = picked {
                            self.mon.track = i;
                        }
                    } else {
                        ui.label(theme::soft("After planning, songs that fit your montage appear here."));
                    }
                }
                MusicSource::File => {
                    ui.horizontal(|ui| {
                        if theme::button(ui, "Choose music file", Kind::Secondary, 0.0).clicked()
                            && let Some(f) = rfd::FileDialog::new().add_filter("Audio", &["mp3", "wav", "m4a", "aac", "ogg", "flac"]).pick_file()
                        {
                            self.mon.file = Some(f);
                        }
                        ui.add(egui::Label::new(theme::mono(self.mon.file.as_ref().map_or_else(|| "No file chosen".to_string(), |f| f.display().to_string()), 12.0).color(theme::TEXT2)).truncate());
                    });
                    ui.label(theme::muted("Make sure you have the right to use this song in your video."));
                }
            }
        });
    }

    // -------------------------------------------------------------------- plan

    fn plan_step(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::bold("Plan", 18.0));
            ui.label(theme::soft("The moments the AI chose, in play order. Untick any you don't want."));
        });
        ui.add_space(6.0);
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::icon(ui, Icon::Sparkle, 16.0, theme::ACCENT_TEXT);
                ui.label(theme::bold("AI plan", 14.0));
                ui.label(theme::muted("uses your own OpenAI key"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("m_model").selected_text(ai::OPENAI_MODELS[self.openai_model].1).width(150.0).show_ui(ui, |ui| {
                        for (i, (_, name)) in ai::OPENAI_MODELS.iter().enumerate() {
                            ui.selectable_value(&mut self.openai_model, i, *name);
                        }
                    });
                });
            });
            ui.horizontal(|ui| {
                let ready = !self.mon.clips.is_empty() && self.mon.busy.is_none() && self.mon.render.is_none();
                ui.add_enabled_ui(ready && self.has_openai, |ui| {
                    if theme::button(ui, if self.mon.plan.is_some() { "Plan again" } else { "Plan with AI" }, Kind::Secondary, 0.0).on_hover_text("Sends small pictures from each clip to OpenAI (about 10 per clip). Your video files are not uploaded.").clicked() {
                        self.plan_montage();
                    }
                });
                if let Some(text) = &self.mon.busy {
                    ui.spinner();
                    ui.label(theme::soft(text));
                }
            });
            theme::checkbox(ui, &mut self.reuse_ai, "Reuse saved AI answers when nothing changed (no tokens spent)");
        });
        let Some(plan) = &self.mon.plan else {
            if self.mon.clips.is_empty() {
                ui.label(theme::muted("Add clips on the first step, then plan."));
            }
            return;
        };
        let _ = plan;
        self.preview_card(ui);
        let Some(plan) = &self.mon.plan else { return };
        ui.add_space(4.0);
        ui.label(theme::muted(format!("Tempo the AI suggests: {}", plan.tempo)));
        if self.mon.style == Style::Story {
            theme::card(ui, |ui| {
                ui.label(theme::bold("Title shown at the start", 14.0));
                ui.add(egui::TextEdit::singleline(&mut self.mon.title).desired_width(f32::INFINITY).margin(Margin::symmetric(12, 9)));
            });
        }
        let mut view = None;
        theme::card(ui, |ui| {
            ui.label(theme::bold("Moments", 14.0));
            for (i, cut) in plan.cuts.iter().enumerate() {
                ui.horizontal(|ui| {
                    theme::checkbox(ui, &mut self.mon.keep[i], "");
                    let (dot, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, clip_colour(cut.clip));
                    ui.label(theme::mono(format!("{} to {}", clock(cut.start), clock(cut.end)), 12.5).color(theme::TEXT2));
                    ui.label(theme::mono(format!("{:.0}%", cut.score * 100.0), 12.5).color(if cut.score > 0.7 { theme::OK } else { theme::TEXT3 }));
                    if theme::small_button(ui, "View").clicked() {
                        view = Some(i);
                    }
                    ui.add(egui::Label::new(theme::soft(&cut.why)).truncate());
                });
            }
        });
        if let Some(i) = view {
            self.preview_select(i);
        }
    }

    // -------------------------------------------------------------------- make

    fn make_step(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::bold("Make", 18.0));
            ui.label(theme::soft("Choose the shapes and where they are saved."));
        });
        ui.add_space(6.0);
        self.preview_card(ui);
        let working = self.mon.render.is_some();
        ui.add_enabled_ui(!working, |ui| {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::checkbox(ui, &mut self.mon.make_wide, "");
                    ui.label(theme::bold("16:9 video", 14.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::muted("For YouTube")));
                });
            });
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::checkbox(ui, &mut self.mon.make_vertical, "");
                    ui.label(theme::bold("9:16 video", 14.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::muted("Shorts, Reels, TikTok")));
                });
                if self.mon.make_vertical {
                    theme::segmented(ui, &mut self.mon.vertical, &[(Vertical::Follow, "Follow"), (Vertical::Centre, "Centre"), (Vertical::Blur, "Blurred")], true);
                    ui.label(theme::muted(match self.mon.vertical {
                        Vertical::Follow => "A calm virtual camera that pans toward movement and detail.",
                        Vertical::Centre => "Always the middle of the picture.",
                        Vertical::Blur => "The whole picture over a blurred copy of itself. Zoom trims the sides so it fills more of the tall frame.",
                    }));
                    if self.mon.vertical == Vertical::Blur {
                        theme::slider_row(ui, "Zoom", &mut self.mon.blur.zoom, 1.0..=2.0, 1.0, false);
                        theme::slider_row(ui, "Left / right", &mut self.mon.blur.x, 0.0..=1.0, 0.0, false);
                        theme::slider_row(ui, "Up / down", &mut self.mon.blur.y, 0.0..=1.0, 0.0, false);
                        if theme::small_button(ui, "Centre and reset").clicked() {
                            self.mon.blur = BlurLayout::default();
                        }
                    }
                }
            });
            theme::card(ui, |ui| {
                ui.label(theme::bold("Speed", 14.0));
                self.encoder_picker(ui, false);
            });
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::icon(ui, Icon::Folder, 18.0, theme::TEXT3);
                    let shown = self.mon.out_dir.clone().or_else(|| self.mon.clips.first().and_then(|c| c.path.parent().map(PathBuf::from)));
                    let text = shown.map_or_else(|| "next to the first clip".to_string(), |d| d.display().to_string());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::small_button(ui, "Change").clicked()
                            && let Some(dir) = rfd::FileDialog::new().pick_folder()
                        {
                            self.mon.out_dir = Some(dir);
                        }
                        ui.add(egui::Label::new(theme::mono(text, 12.0).color(theme::TEXT2)).truncate());
                    });
                });
            });
        });
        if self.mon.plan.is_none() {
            ui.label(theme::muted("Plan with AI first, on the Plan step."));
        }
    }
}

/// The 9:16 picture as the montage will frame it. Follow is shown centred, because it is only worked out while rendering.
fn paint_phone(ui: &egui::Ui, rect: Rect, tex: &egui::TextureHandle, vertical: Vertical, blur: BlurLayout) {
    let aspect = tex.size()[0] as f32 / tex.size()[1].max(1) as f32;
    let radius = egui::CornerRadius::same(16);
    let draw = |at: Rect, uv: Rect, tint: Color32, r: egui::CornerRadius| {
        egui::Image::new(egui::load::SizedTexture::new(tex.id(), at.size())).uv(uv).tint(tint).corner_radius(r).paint_at(ui, at);
    };
    ui.painter().rect_stroke(rect.expand(3.0), 19, Stroke::new(2.0, theme::LINE_STRONG), egui::StrokeKind::Outside);
    if vertical == Vertical::Blur {
        let b = blur.clamped();
        let cover = ((9.0 / 16.0) / aspect).min(1.0);
        draw(rect, Rect::from_min_max(pos2(0.5 - cover / 2.0, 0.0), pos2(0.5 + cover / 2.0, 1.0)), Color32::from_gray(70), radius);
        let (fw, fh) = (rect.width() * b.zoom, rect.width() * b.zoom / aspect);
        let shown_h = fh.min(rect.height());
        let top = rect.top() + (rect.height() - shown_h) * b.y;
        let u_span = (1.0 / b.zoom).min(1.0);
        let u0 = (1.0 - u_span) * b.x;
        let _ = fw;
        draw(Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), shown_h)), Rect::from_min_max(pos2(u0, 0.0), pos2(u0 + u_span, 1.0)), Color32::WHITE, egui::CornerRadius::ZERO);
    } else {
        let share = ((9.0 / 16.0) / aspect).min(1.0);
        let u0 = (1.0 - share) / 2.0;
        draw(rect, Rect::from_min_max(pos2(u0, 0.0), pos2(u0 + share, 1.0)), Color32::WHITE, radius);
    }
}

/// The three style cards.
fn choice_card(ui: &mut egui::Ui, width: f32, title: &str, blurb: &str, selected: bool) -> egui::Response {
    let galley = ui.painter().layout(blurb.to_string(), egui::FontId::proportional(13.0), theme::TEXT2, width - 32.0);
    let height = 52.0 + galley.size().y + 16.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height.max(120.0)), Sense::click());
    let painter = ui.painter();
    let fill = if selected { theme::accent_soft(26) } else if response.hovered() { theme::RAISED } else { theme::CARD };
    painter.rect(rect, 12, fill, Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { theme::ACCENT } else { theme::LINE_SOFT }), egui::StrokeKind::Inside);
    theme::radio_dot(painter, pos2(rect.left() + 26.0, rect.top() + 26.0), selected);
    painter.text(pos2(rect.left() + 46.0, rect.top() + 26.0), egui::Align2::LEFT_CENTER, title, egui::FontId::new(14.5, theme::bold_family()), theme::TEXT);
    painter.galley(pos2(rect.left() + 16.0, rect.top() + 52.0), galley, theme::TEXT2);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small choice chip, like "1 min".
fn chip(ui: &mut egui::Ui, label: &str, on: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::new(13.0, theme::bold_family()), if on { theme::ON_ACCENT } else { theme::TEXT2 });
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 28.0, 32.0), Sense::click());
    ui.painter().rect(rect, 16, if on { theme::ACCENT } else { theme::INPUT }, Stroke::new(1.0, if on { theme::ACCENT } else { theme::LINE_STRONG }), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, theme::TEXT);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn summary_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(theme::muted(label));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(value).size(13.0).color(theme::TEXT)).truncate());
        });
    });
}

/// One colour per clip, so the plan bar and the moment list match.
fn clip_colour(i: usize) -> Color32 {
    const PALETTE: [Color32; 6] = [Color32::from_rgb(0xFF, 0x6B, 0x3D), Color32::from_rgb(0x5B, 0xA8, 0xFF), Color32::from_rgb(0x62, 0xD0, 0x8F), Color32::from_rgb(0xF4, 0xBE, 0x4F), Color32::from_rgb(0xC0, 0x84, 0xFC), Color32::from_rgb(0xFF, 0x8F, 0xB8)];
    PALETTE[i % PALETTE.len()]
}

/// A small picture from a third of the way into a clip.
fn grab_thumb(tools: &media::Tools, probe: &Probe, path: &Path) -> Option<media::Frame> {
    let options = media::GrabOptions { width: 480, finish: media::Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
    media::grab_frame(tools, probe, path, probe.duration / 3.0, &options).ok()
}

fn media_progress(mail: &crate::Mailbox, done: f32, steps: f32, label: &str) {
    mail.send(Msg::Montage(Event::Progress((done / steps).clamp(0.0, 1.0), format!("Making the {label} montage…"))));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_montage_in_progress_survives_a_restart() {
        let mut state = State::new();
        state.style = Style::BeatSync;
        state.target = 90.0;
        state.title = "Road Trip".into();
        state.keep = vec![true, false];
        state.tracks = vec![Track {
            title: "Song".into(),
            creator: "Ann".into(),
            url: "https://example.org/s.mp3".into(),
            landing: "https://example.org/s".into(),
            license: "by".into(),
            license_url: "https://creativecommons.org/licenses/by/4.0/".into(),
            attribution: String::new(),
            seconds: 120.0,
            source: "x".into(),
            instrumental: true,
        }];
        let cut = |start: f64| Cut { clip: 0, start, end: start + 3.0, score: 0.8, why: "sunset".into() };
        state.plan = Some(PlanData { cuts: vec![cut(1.0), cut(9.0)], grades: vec![Grade::default()], title: "Road Trip".into(), music_query: "upbeat".into(), tempo: "fast".into(), reused: true });

        let text = state.autosave_text().unwrap();
        let back: Saved = serde_json::from_str(&text).unwrap();
        let plan = back.plan.expect("the AI plan must be saved");
        assert_eq!((back.style, back.target, back.keep.clone()), (Style::BeatSync, 90.0, vec![true, false]));
        assert_eq!((plan.cuts.len(), plan.cuts[1].start, plan.cuts[1].why.as_str(), plan.music_query.as_str()), (2, 9.0, "sunset", "upbeat"));
        assert!(!plan.reused, "the reused marker belongs to one run only");
        assert_eq!(back.tracks[0].title, "Song");
        // While clips are still loading after a restart nothing is saved, so a half-restored state can't overwrite a good one.
        state.probing = 1;
        assert!(state.autosave_text().is_none());
    }
}
