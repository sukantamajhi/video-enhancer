//! The montage: several clips, cut together by the AI into one video. This is the session for it, with no user
//! interface in it, taken from the egui app's montage screen. A front end holds the engine, calls these methods when
//! something is clicked, and draws `engine.montage`.
//!
//! Like the single-video session it is saved as it goes (clips, choices, the AI's plan, the songs, the upload pack),
//! so closing the app never loses an AI answer that was paid for.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::TryRecvError;

use serde::{Deserialize, Serialize};

use crate::engine::{Engine, Msg, encode_summary};
use crate::grade::{Grade, LOOKS, LUT_SIZE, Lut};
use crate::media::{self, BlurLayout, Frame, Probe};
use crate::montage::{self, Clip, Cut, Job, Music, Out, Sound, Style, Vertical};
use crate::music::{self, Track};
use crate::publish::{PublishState, Target};
use crate::{ai, store};

pub const MAX_CLIPS: usize = 12;

pub enum Event {
    Probed(PathBuf, Result<Probe, String>),
    /// A small picture of a clip, for its card.
    Thumb(PathBuf, Frame),
    /// A still picture of one moment, for the preview.
    PreviewFrame(usize, Result<Frame, String>),
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

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum MusicSource {
    Online,
    File,
}

/// What the Publish step needs to know about the montage, for chapters and thumbnails.
pub struct MontageFacts {
    pub clips: Vec<Clip>,
    pub cuts: Vec<Cut>,
    pub grades: Vec<Grade>,
    pub style: Style,
    pub title: String,
    pub credit: String,
    pub out_dir: Option<PathBuf>,
    /// False while the chapter times are estimates from the plan rather than the rendered video.
    pub rendered: bool,
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

/// The preview: which moment is showing, and the player that runs through the kept ones.
#[derive(Default)]
pub struct Preview {
    pub cur: usize,
    player: Option<media::Player>,
    /// The picture showing now, with its clip's colour, and a counter for when it changes.
    pub frame: Option<Frame>,
    pub shown: u64,
    pub t: f64,
    pub busy: bool,
    pub started: bool,
    lut: Option<(usize, Lut)>,
}

pub struct MontageState {
    pub clips: Vec<Clip>,
    pub probing: usize,
    pub style: Style,
    pub sound: Sound,
    pub vertical: Vertical,
    pub blur: BlurLayout,
    /// How long the montage should be, in seconds.
    pub target: f64,
    pub look: Option<usize>,
    pub source: MusicSource,
    pub file: Option<PathBuf>,
    pub tracks: Vec<Track>,
    pub track: usize,
    pub searching: bool,
    pub music_query: String,
    pub plan: Option<PlanData>,
    pub keep: Vec<bool>,
    pub title: String,
    pub busy: Option<String>,
    pub make_wide: bool,
    pub make_vertical: bool,
    pub out_dir: Option<PathBuf>,
    pub render: Option<(f32, String, Arc<AtomicBool>)>,
    pub made: Vec<PathBuf>,
    pub rendered: Vec<Cut>,
    pub credit: String,
    /// Small pictures of the clips, and a counter for when they change.
    pub thumbs: HashMap<PathBuf, Frame>,
    pub thumbs_gen: u64,
    pub preview: Preview,
}

impl MontageState {
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
            rendered: Vec::new(),
            credit: String::new(),
            thumbs: HashMap::new(),
            thumbs_gen: 0,
            preview: Preview::default(),
        }
    }

    fn session(&self, pack: Option<crate::pack::Pack>) -> Saved {
        Saved {
            clips: self.clips.iter().map(|c| c.path.clone()).collect(),
            pack,
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

    /// Where the clips add up to.
    pub fn total(&self) -> f64 {
        self.clips.iter().map(|c| c.probe.duration).sum()
    }

    /// True while something is running that needs the window to keep refreshing.
    pub fn working(&self) -> bool {
        self.busy.is_some() || self.render.is_some() || self.searching || self.probing > 0 || self.preview.player.is_some()
    }

    pub fn playing(&self) -> bool {
        self.preview.player.is_some()
    }

    pub fn stop_preview(&mut self) {
        self.preview.player = None;
    }

    pub fn busy_text(&self) -> Option<String> {
        self.busy.clone().or_else(|| self.searching.then(|| "Searching NCS for music…".to_string()))
    }

    /// The moments that are ticked, as indices into the plan.
    pub fn kept(&self) -> Vec<usize> {
        self.keep.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| i).collect()
    }
}

impl Default for MontageState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn safe_name(title: &str) -> String {
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

/// A small picture from a third of the way into a clip.
fn grab_thumb(tools: &media::Tools, probe: &Probe, path: &Path) -> Option<Frame> {
    let options = media::GrabOptions { width: 480, finish: media::Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
    media::grab_frame(tools, probe, path, probe.duration / 3.0, &options).ok()
}

fn media_progress(mail: &crate::engine::Mailbox, done: f32, steps: f32, label: &str) {
    mail.send(Msg::Montage(Event::Progress((done / steps).clamp(0.0, 1.0), format!("Making the {label} montage…"))));
}

fn clock(t: f64) -> String {
    format!("{}:{:02.0}", (t / 60.0) as u32, t % 60.0)
}

impl Engine {
    /// The upload pack of the montage, wherever it is held at the moment.
    pub fn montage_pack(&self) -> Option<crate::pack::Pack> {
        if self.target == Target::Montage { self.publish.pack.clone() } else { self.stash.pack.clone() }
    }

    /// What the Publish step needs: the moments as rendered (or as planned, until then), and the clips and colour.
    pub fn montage_facts(&self) -> Option<MontageFacts> {
        let m = &self.montage;
        let plan = m.plan.as_ref()?;
        let rendered = !m.rendered.is_empty();
        let cuts: Vec<Cut> = if rendered { m.rendered.clone() } else { plan.cuts.iter().zip(&m.keep).filter(|(_, k)| **k).map(|(c, _)| c.clone()).collect() };
        (!cuts.is_empty() && !m.clips.is_empty()).then(|| MontageFacts { clips: m.clips.clone(), cuts, grades: plan.grades.clone(), style: m.style, title: m.title.clone(), credit: m.credit.clone(), out_dir: m.out_dir.clone(), rendered })
    }

    /// Writes the montage if it changed. Cheap to call often. Nothing is written while a restore is still loading the clips.
    pub(crate) fn autosave_montage(&mut self) {
        // Nothing to remember until a clip was added or a session restored.
        if self.montage.probing > 0 || (self.montage.clips.is_empty() && self.saved_text[2].is_empty()) {
            return;
        }
        let saved = self.montage.session(self.montage_pack());
        let text = serde_json::to_string(&saved).unwrap_or_default();
        if text != self.saved_text[2] {
            store::save("montage", &saved);
            self.saved_text[2] = text;
        }
    }

    pub fn add_clips(&mut self, paths: Vec<PathBuf>) {
        let Some(tools) = self.tools.clone() else {
            self.fail("Install the video tools first.");
            return;
        };
        for path in paths {
            if self.montage.clips.len() + self.montage.probing >= MAX_CLIPS {
                self.fail(format!("A montage can use up to {MAX_CLIPS} clips."));
                break;
            }
            if self.montage.clips.iter().any(|c| c.path == path) {
                continue;
            }
            self.montage.probing += 1;
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

    /// Takes a clip out of the montage. The AI's plan no longer fits, so it is cleared.
    pub fn remove_clip(&mut self, i: usize) {
        if i < self.montage.clips.len() {
            let gone = self.montage.clips.remove(i);
            self.montage.thumbs.remove(&gone.path);
            self.montage.thumbs_gen += 1;
            self.clips_changed();
        }
    }

    /// Moves a clip one place earlier (`-1`) or later (`1`).
    pub fn move_clip(&mut self, i: usize, step: i32) {
        let to = i as i32 + step;
        if i < self.montage.clips.len() && to >= 0 && (to as usize) < self.montage.clips.len() {
            self.montage.clips.swap(i, to as usize);
            self.clips_changed();
        }
    }

    fn clips_changed(&mut self) {
        let m = &mut self.montage;
        m.plan = None;
        m.keep.clear();
        m.rendered.clear();
        m.tracks.clear();
        m.preview = Preview::default();
    }

    pub(crate) fn montage_event(&mut self, event: Event) {
        match event {
            Event::Rendered(cuts, credit) => {
                self.montage.rendered = cuts;
                self.montage.credit = credit;
            }
            Event::Restored(clips, saved) => {
                self.montage.probing = self.montage.probing.saturating_sub(1);
                if clips.len() == saved {
                    self.montage.clips = clips;
                    self.note(if self.montage.plan.is_some() { "Restored your last montage, including the AI's plan. Nothing needs to be asked again." } else { "Restored your clips from last time." });
                } else {
                    // Some files moved or were deleted, so the saved cut list no longer lines up.
                    self.montage.clips = clips;
                    self.montage.plan = None;
                    self.montage.keep.clear();
                    self.montage.rendered.clear();
                    self.note("Some clips from your last session are gone, so the AI plan was cleared. Plan again when you're ready.");
                }
            }
            Event::Probed(path, result) => {
                self.montage.probing = self.montage.probing.saturating_sub(1);
                match result {
                    Ok(probe) if probe.duration >= 2.0 => {
                        self.montage.clips.push(Clip { path, probe });
                        self.clips_changed();
                    }
                    Ok(_) => self.fail(format!("{} is shorter than 2 seconds.", path.display())),
                    Err(e) => self.fail(format!("{}: {e}", path.display())),
                }
            }
            Event::Thumb(path, frame) => {
                self.montage.thumbs.insert(path, frame);
                self.montage.thumbs_gen += 1;
            }
            Event::PreviewFrame(i, result) => {
                self.montage.preview.busy = false;
                match result {
                    Ok(frame) if i == self.montage.preview.cur && self.montage.preview.player.is_none() => {
                        if let Some(clip) = self.montage.plan.as_ref().and_then(|p| p.cuts.get(i)).map(|c| c.clip) {
                            self.preview_show(clip, &frame);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => self.fail(e),
                }
            }
            Event::Note(text) => self.note(text),
            Event::Plan(Ok(plan)) => {
                let m = &mut self.montage;
                m.busy = None;
                m.preview = Preview::default();
                m.keep = vec![true; plan.cuts.len()];
                m.rendered.clear();
                m.credit.clear();
                m.title = plan.title.clone();
                m.music_query = plan.music_query.clone();
                let total: f64 = plan.cuts.iter().map(Cut::len).sum();
                let reused = if plan.reused { " Reused your saved AI answer, so no tokens were used." } else { "" };
                let text = format!("The AI chose {} moments ({} before fades). Untick any you don't want, then make the montage.{reused}", plan.cuts.len(), clock(total));
                m.plan = Some(plan);
                self.note(text);
            }
            Event::Plan(Err(e)) => {
                self.montage.busy = None;
                self.montage.searching = false;
                self.fail(e);
            }
            Event::Tracks(result) => {
                self.montage.searching = false;
                match result {
                    Ok(tracks) => {
                        self.montage.tracks = tracks;
                        self.montage.track = 0;
                        self.note("Found NCS music that fits. The first song is selected; pick another if you like.");
                    }
                    Err(e) => self.fail(e),
                }
            }
            Event::Progress(p, label) => {
                if let Some(r) = &mut self.montage.render {
                    r.0 = p;
                    r.1 = label;
                }
            }
            Event::Done(result) => {
                self.montage.render = None;
                match result {
                    Ok(files) => {
                        let how = std::mem::take(&mut self.encode_info);
                        self.note(format!("Finished. {} file(s) saved.{how}", files.len()));
                        self.montage.made = files;
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
        let m = &mut self.montage;
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
        // The pack goes to whichever slot holds the montage's.
        if self.target == Target::Montage {
            self.publish = PublishState::with(saved.pack);
        } else {
            self.stash = PublishState::with(saved.pack);
        }
        if saved.clips.is_empty() {
            return;
        }
        self.montage.probing += 1;
        let (mail, expected) = (self.mail.clone(), saved.clips.len());
        std::thread::spawn(move || {
            // Keep the original order, and drop any file that is no longer there.
            let clips: Vec<Clip> = saved.clips.into_iter().filter_map(|path| media::probe(&tools, &path).ok().map(|probe| Clip { path, probe })).collect();
            let pictures: Vec<(PathBuf, Frame)> = clips.iter().filter_map(|c| grab_thumb(&tools, &c.probe, &c.path).map(|f| (c.path.clone(), f))).collect();
            mail.send(Msg::Montage(Event::Restored(clips, expected)));
            for (path, frame) in pictures {
                mail.send(Msg::Montage(Event::Thumb(path, frame)));
            }
        });
    }

    pub fn search_music(&mut self) {
        if self.montage.searching {
            return;
        }
        self.montage.searching = true;
        let (query, target, mail) = (self.montage.music_query.clone(), self.montage.target, self.mail.clone());
        std::thread::spawn(move || mail.send(Msg::Montage(Event::Tracks(music::search(&query, target)))));
    }

    /// Asks the AI to choose the best moments from every clip, and looks for music to match.
    pub fn plan_montage(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else {
            self.fail("Add your OpenAI key first.");
            return;
        };
        if self.montage.clips.is_empty() {
            self.fail("Add at least one clip first.");
            return;
        }
        if self.montage.style.needs_music() && !self.montage.sound.uses_music() {
            self.fail("A beat-synced montage needs music. Choose a sound option that includes music.");
            return;
        }
        let m = &self.montage;
        let (clips, style, target, look, notes) = (m.clips.clone(), m.style, m.target, m.look, self.settings.style_notes.clone());
        let wants_music = m.sound.uses_music() && m.source == MusicSource::Online;
        let model = ai::OPENAI_MODELS[self.settings.openai_model].0;
        let reuse = self.settings.reuse_ai;
        let m = &mut self.montage;
        m.busy = Some("Planning the montage…".into());
        m.plan = None;
        m.tracks.clear();
        m.searching = wants_music;
        m.preview = Preview::default();
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

    /// Cuts the kept moments together, with the music, in each shape that is ticked.
    pub fn render_montage(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let m = &self.montage;
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
        let encoder = self.encoder();
        let out_dir = m.out_dir.clone().or_else(|| clips.first().and_then(|c| c.path.parent().map(PathBuf::from))).unwrap_or_else(std::env::temp_dir);
        let cancel = Arc::new(AtomicBool::new(false));
        self.montage.preview.player = None;
        self.montage.render = Some((0.0, "Getting ready…".into(), cancel.clone()));
        self.montage.made.clear();
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
                mail.send(Msg::EncodeInfo(encode_summary(began.elapsed().as_secs(), encoder, fell_back, false)));
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

    // ------------------------------------------------------------------ the preview

    /// Shows one moment as a still picture, and moves the preview to it.
    pub fn preview_select(&mut self, i: usize) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(cut) = self.montage.plan.as_ref().and_then(|p| p.cuts.get(i).cloned()) else { return };
        let Some(clip) = self.montage.clips.get(cut.clip).cloned() else { return };
        let p = &mut self.montage.preview;
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
        let Some(cut) = self.montage.plan.as_ref().and_then(|p| p.cuts.get(i).cloned()) else { return };
        let Some(clip) = self.montage.clips.get(cut.clip).cloned() else { return };
        let wake = self.mail.wake_fn();
        let p = &mut self.montage.preview;
        (p.cur, p.t, p.started) = (i, cut.start, true);
        p.player = Some(media::play(&tools, &clip.probe, &clip.path, cut.start, move || wake()));
    }

    /// Plays the kept moments one after another, or stops.
    pub fn preview_toggle(&mut self) {
        if self.montage.preview.player.is_some() {
            self.montage.preview.player = None;
            return;
        }
        let kept = self.montage.kept();
        let cur = self.montage.preview.cur;
        let Some(first) = kept.first().copied() else { return };
        let start = if kept.contains(&cur) { cur } else { kept.iter().copied().find(|k| *k > cur).unwrap_or(first) };
        self.preview_play_from(start);
    }

    /// Goes to the previous (`-1`) or next (`1`) kept moment, playing on if it was playing.
    pub fn preview_step(&mut self, step: i32) {
        let kept = self.montage.kept();
        let cur = self.montage.preview.cur;
        let at = kept.iter().position(|&k| k >= cur);
        let target = match (step, at) {
            (1, Some(p)) if kept.get(p) == Some(&cur) => kept.get(p + 1),
            (1, Some(p)) => kept.get(p),
            (_, Some(p)) => p.checked_sub(1).and_then(|q| kept.get(q)),
            (1, None) => None,
            (_, None) => kept.last(),
        };
        if let Some(&i) = target {
            self.preview_goto(i);
        }
    }

    /// Looks at one moment: playing on from it if the preview was playing and the moment is kept.
    pub fn preview_goto(&mut self, i: usize) {
        if self.montage.preview.player.is_some() && self.montage.kept().contains(&i) {
            self.preview_play_from(i);
        } else {
            self.preview_select(i);
        }
    }

    /// Draws a decoded frame with the colour of its clip.
    fn preview_show(&mut self, clip: usize, frame: &Frame) {
        let Some(grade) = self.montage.plan.as_ref().and_then(|p| p.grades.get(clip).copied()) else { return };
        let p = &mut self.montage.preview;
        if p.lut.as_ref().is_none_or(|(c, _)| *c != clip) {
            p.lut = Some((clip, Lut::build(17, &grade, None)));
        }
        let graded = p.lut.as_ref().map(|(_, l)| l.apply(&frame.rgb)).unwrap_or_default();
        p.frame = Some(Frame { width: frame.width, height: frame.height, rgb: graded });
        p.shown += 1;
    }

    /// Moves playback along: new frames to the screen, and on to the next kept moment at the end of one. True when anything changed.
    pub(crate) fn preview_tick(&mut self) -> bool {
        let Some(cut) = self.montage.plan.as_ref().and_then(|p| p.cuts.get(self.montage.preview.cur).cloned()) else { return false };
        let (mut latest, mut ended) = (None, false);
        if let Some(player) = &self.montage.preview.player {
            loop {
                match player.rx.try_recv() {
                    Ok(item) => latest = Some(item),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        ended = true;
                        break;
                    }
                }
            }
        }
        let mut changed = false;
        if let Some((t, frame)) = latest {
            self.preview_show(cut.clip, &frame);
            self.montage.preview.t = t;
            ended |= t >= cut.end;
            changed = true;
        }
        if ended && self.montage.preview.player.is_some() {
            let cur = self.montage.preview.cur;
            match self.montage.kept().into_iter().find(|k| *k > cur) {
                Some(next) => self.preview_play_from(next),
                None => self.montage.preview.player = None,
            }
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::engine::tests::{isolate_saves, wait};
    use crate::media::Tools;
    use crate::media::tests::{make_video, scratch};
    use crate::pack::ThumbShape;

    /// An engine with three real clips added and a plan written by hand (no AI is asked), the way a finished plan looks.
    pub(crate) fn engine_with_montage(dir: &Path) -> Option<Engine> {
        isolate_saves();
        let tools = Tools::find()?;
        let clips: Vec<PathBuf> = [("a.mp4", "1920x1080"), ("b.mp4", "1280x720"), ("c.mp4", "1920x1080")]
            .iter()
            .map(|(name, size)| {
                make_video(&tools, &dir.join(name), size, 6);
                dir.join(name)
            })
            .collect();
        let mut engine = Engine::new(|| {});
        engine.settings.out_dir = Some(dir.to_path_buf());
        engine.montage = MontageState::new();
        engine.add_clips(clips);
        wait(&mut engine, 60, |e| e.montage.clips.len() == 3 && e.montage.thumbs.len() == 3 && e.montage.probing == 0);
        // The clips are probed on separate threads and arrive in any order; the tests want a, b, c.
        engine.montage.clips.sort_by(|a, b| a.path.cmp(&b.path));
        let cut = |clip, start, end, score, why: &str| Cut { clip, start, end, score, why: why.into() };
        let cuts = vec![cut(0, 1.0, 3.0, 0.9, "ace"), cut(1, 1.5, 3.5, 0.6, "clutch"), cut(2, 2.0, 4.0, 0.8, "flick"), cut(0, 3.5, 5.0, 0.4, "peek")];
        engine.montage.plan = Some(PlanData { cuts, grades: vec![Grade::default(); 3], title: "Test Reel".into(), music_query: "energetic".into(), tempo: "fast".into(), reused: false });
        engine.montage.keep = vec![true; 4];
        engine.montage.title = "Test Reel".into();
        engine.montage.sound = Sound::OriginalOnly;
        engine.montage.out_dir = Some(dir.to_path_buf());
        Some(engine)
    }

    #[test]
    fn clips_are_previewed_made_remembered_and_changed() {
        let dir = scratch("montage-engine");
        let Some(mut engine) = engine_with_montage(&dir) else { return };
        let tools = engine.tools.clone().unwrap();

        // A still picture of one moment, in its clip's colour, and then playing through the kept moments.
        engine.preview_select(1);
        wait(&mut engine, 30, |e| e.montage.preview.frame.is_some());
        let still = engine.montage.preview.frame.clone().unwrap();
        assert_eq!((still.width, still.height), (960, 540), "the clip is 1280x720");
        engine.montage.keep[2] = false;
        engine.preview_toggle();
        assert!(engine.montage.playing());
        wait(&mut engine, 30, |e| e.montage.preview.t > 1.6);
        engine.preview_toggle();
        assert!(!engine.montage.playing());
        // Stepping skips the moment that is unticked.
        engine.preview_select(1);
        engine.preview_step(1);
        assert_eq!(engine.montage.preview.cur, 3);
        engine.montage.keep[2] = true;

        // Making the montage writes a 16:9 file with the original sound, and remembers the moments as rendered.
        engine.montage.make_vertical = false;
        engine.render_montage();
        assert!(engine.montage.render.is_some());
        wait(&mut engine, 180, |e| e.montage.render.is_none() && !e.montage.made.is_empty());
        let made = engine.montage.made.clone();
        assert_eq!(made.len(), 1, "{}", engine.status);
        let out = media::probe(&tools, &made[0]).unwrap();
        assert_eq!((out.width, out.height), (1920, 1080));
        assert!(out.duration > 5.0 && out.duration < 8.0, "{}", out.duration);
        assert_eq!(engine.montage.rendered.len(), 4);
        assert!(made[0].file_name().unwrap().to_string_lossy().starts_with("Test_Reel_16x9"));

        // Publishing a montage: its own pack, drawn and saved under the montage's name, kept apart from a single video's.
        engine.set_target(Target::Montage);
        assert!(engine.publish.pack.is_none() && engine.stash.pack.is_none());
        engine.publish.pack = Some(engine.montage_pack_for_test());
        engine.publish.pack_gen += 1;
        engine.redraw_thumbnails(vec![ThumbShape::Wide, ThumbShape::Tall]);
        wait(&mut engine, 60, |e| e.publish.thumb.is_some() && e.publish.thumb_tall.is_some() && !e.publish.working());
        engine.save_pack();
        assert!(!engine.status_is_error, "{}", engine.status);
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        for want in ["Test_Reel_upload_pack.txt", "Test_Reel_thumbnail.jpg", "Test_Reel_thumbnail_shorts.jpg"] {
            assert!(names.iter().any(|n| n == want), "{want} missing in {names:?}");
        }
        assert!(engine.montage_pack().is_some() && engine.single_pack().is_none());

        // It all comes back in a new session: the clips, the plan and the choices.
        engine.autosave();
        let mut again = Engine::new(|| {});
        wait(&mut again, 60, |e| e.montage.clips.len() == 3 && e.montage.probing == 0);
        assert_eq!(again.montage.title, "Test Reel");
        assert_eq!(again.montage.plan.as_ref().map(|p| p.cuts.len()), Some(4));
        assert_eq!(again.montage.keep, vec![true; 4]);
        assert!(!again.montage.make_vertical && again.montage.sound == Sound::OriginalOnly);
        // The montage's pack is back too, and its pictures are drawn again when Publish shows it.
        assert!(again.montage_pack().is_some() && again.publish.pack.is_none());
        again.set_target(Target::Montage);
        wait(&mut again, 60, |e| e.publish.thumb.is_some() && e.publish.thumb_tall.is_some() && !e.publish.working());
        again.set_target(Target::Single);
        assert!(again.publish.pack.is_none() && again.stash.pack.is_some());

        // Taking a clip out clears the plan, which no longer fits.
        again.remove_clip(0);
        assert!(again.montage.plan.is_none() && again.montage.keep.is_empty() && again.montage.clips.len() == 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod demo {
    use super::*;
    use crate::engine::tests::wait;

    /// A developer aid, not a test: writes a montage session for the real clips named in `VE_DEMO_CLIPS` (`;` apart) into the
    /// saved folder named by `VIDEO_ENHANCER_SAVED`, with a hand-made plan and pack, so the app can be opened on it.
    #[test]
    #[ignore]
    fn write_demo_montage() {
        let (Ok(clips), Ok(out)) = (std::env::var("VE_DEMO_CLIPS"), std::env::var("VE_DEMO_OUT")) else { return };
        let mut engine = Engine::new(|| {});
        let paths: Vec<PathBuf> = clips.split(';').map(PathBuf::from).collect();
        engine.add_clips(paths.clone());
        wait(&mut engine, 60, |e| e.montage.clips.len() == paths.len() && e.montage.probing == 0);
        engine.montage.clips.sort_by_key(|c| paths.iter().position(|p| *p == c.path));
        let mut cuts = Vec::new();
        for (i, clip) in engine.montage.clips.iter().enumerate() {
            let d = clip.probe.duration;
            let len = (d * 0.2).clamp(2.0, 4.0);
            cuts.push(Cut { clip: i, start: d * 0.15, end: d * 0.15 + len, score: 0.9 - i as f32 * 0.15, why: ["opening fight", "clutch round", "last-second win"][i % 3].into() });
            cuts.push(Cut { clip: i, start: d * 0.55, end: d * 0.55 + len, score: 0.5 + i as f32 * 0.1, why: ["quiet peek", "big push", "ace"][i % 3].into() });
        }
        let n = engine.montage.clips.len();
        engine.montage.keep = vec![true; cuts.len()];
        engine.montage.plan = Some(PlanData { cuts, grades: vec![Grade::default(); n], title: "Clutch Night".into(), music_query: "energetic electronic".into(), tempo: "fast".into(), reused: false });
        engine.montage.title = "Clutch Night".into();
        engine.montage.sound = Sound::OriginalOnly;
        engine.montage.out_dir = Some(PathBuf::from(out));
        engine.montage.make_vertical = false;
        engine.set_target(Target::Montage);
        engine.publish.pack = Some(engine.montage_pack_for_test());
        engine.set_target(Target::Single);
        engine.autosave();
    }
}
