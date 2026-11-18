//! The editing session for one video, with no user interface in it: open a video, show a frame, grade it,
//! play it, ask the AI, and export. A front end holds an [`Engine`], calls its methods when something is
//! clicked, calls [`Engine::poll`] now and then to take in what the background threads have finished, and draws
//! what the engine holds.
//!
//! Work that takes time (probing, grabbing frames, the AI, exporting) runs on its own threads and reports back
//! through a channel, so the window never waits. The `wake` function given to [`Engine::new`] is called whenever
//! a thread has something to report, so the front end knows to poll.
//!
//! It saves and restores each video's grade and AI results exactly as the egui app does, so a video can be
//! started in one front end and finished in the other.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::grade::{self, Grade, LOOKS, LUT_SIZE, LearnedLook, Lut};
use crate::media::{self, BlurLayout, Encoder, ExportJob, Finish, Frame, Framing, GrabOptions, Player, Probe, Shape, Tools};
use crate::settings::{Recent, Settings};
use crate::montage_engine::{self, MontageState};
use crate::publish::{self, PublishState, Target};
use crate::{ai, gpu, pack, store};

/// What a background thread can tell the engine.
pub enum Msg {
    ToolsProgress(f32),
    Tools(Result<Tools, String>),
    Opened(PathBuf, Result<Probe, String>),
    Frame(Result<Frame, String>),
    Analysed(Result<Grade, String>),
    Action(Result<Vec<(f64, f32)>, String>),
    Note(String),
    /// The second field is true when a saved answer was reused (nothing was sent or paid for).
    Omni(Result<LearnedLook, String>, bool),
    Suggestion(Result<ai::Suggestion, String>, bool),
    Export(f32, String),
    ExportDone(Result<Vec<PathBuf>, String>),
    Encoders(Vec<Encoder>),
    /// The graphics card's name, or why it can't be used for colour grading.
    Gpu(Result<String, String>),
    EncodeInfo(String),
    Pack(publish::Event),
    Montage(montage_engine::Event),
}

/// How the 9:16 version frames the picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum VerticalFraming {
    Centre,
    Manual,
    Follow,
    Blur,
}

/// Everything remembered about one video, so reopening it (even after a restart)
/// brings back the grade and any AI results without paying for them again.
#[derive(Serialize, Deserialize)]
struct VideoSession {
    grade: Grade,
    auto_base: Grade,
    learned: Option<LearnedLook>,
    look: Option<usize>,
    finish: Finish,
    trim: (f64, f64),
    t: f64,
    ai_result: String,
    framing: VerticalFraming,
    manual_x: f32,
    action: Option<Vec<(f64, f32)>>,
    #[serde(default)]
    blur: BlurLayout,
    #[serde(default)]
    pack: Option<pack::Pack>,
}

pub struct Source {
    pub path: PathBuf,
    pub probe: Probe,
}

pub struct ExportState {
    pub progress: f32,
    pub label: String,
    pub cancel: Arc<AtomicBool>,
}

/// How the finished video was made, for the message that follows an export.
pub fn encode_summary(seconds: u64, used: Encoder, fell_back: bool, colour_on_card: bool) -> String {
    let time = format!("{}:{:02}", seconds / 60, seconds % 60);
    let colour = if colour_on_card { ", with the colour grade done on the graphics card" } else { "" };
    if fell_back {
        format!(" Made in {time} on the CPU (the graphics-card encoder didn't work, so the CPU was used instead){colour}.")
    } else {
        format!(" Made in {time} with {}{colour}.", used.short())
    }
}

#[derive(Clone)]
pub(crate) struct Mailbox {
    tx: Sender<Msg>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Mailbox {
    /// The function that wakes the window, for a thread that needs to call it itself.
    pub(crate) fn wake_fn(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.wake.clone()
    }

    pub(crate) fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        (self.wake)();
    }
}

pub struct Engine {
    pub(crate) mail: Mailbox,
    rx: Receiver<Msg>,
    pub settings: Settings,
    pub tools: Option<Tools>,
    pub tools_download: Option<f32>,
    pub source: Option<Source>,
    video_fp: Option<String>,
    /// Where the picture is, in seconds, and the part that will be exported.
    pub t: f64,
    pub trim: (f64, f64),
    player: Option<Player>,

    /// The frame as the camera saw it, and the same frame with the grade applied.
    pub raw: Option<Frame>,
    pub graded: Option<Frame>,
    /// Goes up whenever `raw` or `graded` changes, so a front end knows when to redraw the picture.
    pub view_gen: u64,
    raw_changed: bool,
    grab_busy: bool,
    grab_wanted: Option<f64>,
    /// The eight looks drawn from the current frame, and a counter for when they change.
    pub look_pictures: Vec<Frame>,
    pub looks_gen: u64,
    looks_dirty: bool,

    pub grade: Grade,
    pub auto_base: Grade,
    pub learned: Option<LearnedLook>,
    pub look: Option<usize>,
    pub finish: Finish,
    lut: Lut,
    lut_for: (Grade, Option<LearnedLook>),

    pub framing: VerticalFraming,
    pub manual_x: f32,
    pub action: Option<Vec<(f64, f32)>>,
    pub action_busy: bool,
    pub blur: BlurLayout,
    pub export: Option<ExportState>,
    pub made: Vec<PathBuf>,
    pub encoders: Vec<Encoder>,
    pub gpu_status: Option<Result<String, String>>,
    pub(crate) encode_info: String,

    pub has_openai: bool,
    pub has_gemini: bool,
    pub ai_busy: Option<String>,
    pub ai_result: String,

    pub status: String,
    pub status_is_error: bool,
    /// The Publish step: the upload pack and its thumbnails.
    pub publish: PublishState,
    /// The upload pack of the other video (the montage while a single video is on screen, and the other way round).
    pub(crate) stash: PublishState,
    /// Which video the Publish step is about.
    pub target: Target,
    pub montage: MontageState,
    pub(crate) saved_text: [String; 3],
}

impl Engine {
    /// Starts an engine: reads the saved settings, finds the video tools, and checks the graphics card and encoders in the background.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let mail = Mailbox { tx, wake: Arc::new(wake) };
        let settings = Settings::load();
        let grade = Grade::default();
        let mut engine = Self {
            mail,
            rx,
            settings,
            tools: Tools::find(),
            tools_download: None,
            source: None,
            video_fp: None,
            t: 0.0,
            trim: (0.0, 0.0),
            player: None,
            raw: None,
            graded: None,
            view_gen: 0,
            raw_changed: true,
            grab_busy: false,
            grab_wanted: None,
            look_pictures: Vec::new(),
            looks_gen: 0,
            looks_dirty: true,
            grade,
            auto_base: grade,
            learned: None,
            look: Some(0),
            finish: Finish::default(),
            lut: Lut::build(LUT_SIZE, &grade, None),
            lut_for: (grade, None),
            framing: VerticalFraming::Follow,
            manual_x: 0.5,
            action: None,
            action_busy: false,
            blur: BlurLayout::default(),
            export: None,
            made: Vec::new(),
            encoders: Vec::new(),
            gpu_status: None,
            encode_info: String::new(),
            has_openai: ai::load_key(ai::Provider::OpenAi).is_some(),
            has_gemini: ai::load_key(ai::Provider::Gemini).is_some(),
            ai_busy: None,
            ai_result: String::new(),
            status: "Drop a video here, or press Open a video.".into(),
            status_is_error: false,
            publish: PublishState::default(),
            stash: PublishState::default(),
            target: Target::Single,
            montage: MontageState::new(),
            saved_text: Default::default(),
        };
        engine.settings.openai_model = engine.settings.openai_model.min(ai::OPENAI_MODELS.len() - 1);
        engine.detect_gpu();
        engine.detect_encoders();
        engine.restore_montage();
        engine
    }

    /// Shows the built-in sample picture, so the Look and Export screens have something to draw before a video is opened.
    pub fn use_sample(&mut self) {
        let frame = crate::sample::frame();
        self.auto_base = grade::auto_correct(&frame.rgb);
        self.grade = self.auto_base;
        self.look = Some(0);
        LOOKS[0].apply(&mut self.grade, self.auto_base.temperature);
        self.raw = Some(frame);
        self.raw_changed = true;
        self.looks_dirty = true;
    }

    pub fn refresh_keys(&mut self) {
        self.has_openai = ai::load_key(ai::Provider::OpenAi).is_some();
        self.has_gemini = ai::load_key(ai::Provider::Gemini).is_some();
    }

    fn detect_encoders(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let mail = self.mail.clone();
        std::thread::spawn(move || mail.send(Msg::Encoders(media::detect_encoders(&tools))));
    }

    fn detect_gpu(&mut self) {
        let mail = self.mail.clone();
        std::thread::spawn(move || mail.send(Msg::Gpu(gpu::shared().map(|g| g.name().to_string()))));
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_is_error = false;
    }

    pub fn fail(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_is_error = true;
    }

    pub fn playing(&self) -> bool {
        self.player.is_some()
    }

    /// The encoder that will really be used, given the choice and what works on this computer.
    pub fn encoder(&self) -> Encoder {
        self.settings.encoder.resolve(&self.encoders)
    }

    /// True when the colour grade will be done on the graphics card.
    pub fn colour_on_card(&self) -> bool {
        self.settings.gpu_grade && matches!(self.gpu_status, Some(Ok(_)))
    }

    // ------------------------------------------------------------------ memory

    fn video_session(&self) -> Option<VideoSession> {
        self.source.as_ref()?;
        Some(VideoSession {
            grade: self.grade,
            auto_base: self.auto_base,
            learned: self.learned.clone(),
            look: self.look,
            finish: self.finish,
            trim: self.trim,
            t: self.t,
            ai_result: self.ai_result.clone(),
            framing: self.framing,
            manual_x: self.manual_x,
            action: self.action.clone(),
            blur: self.blur,
            pack: self.single_pack(),
        })
    }

    /// The single video's upload pack, wherever it is held at the moment.
    pub fn single_pack(&self) -> Option<pack::Pack> {
        if self.target == Target::Single { self.publish.pack.clone() } else { self.stash.pack.clone() }
    }

    fn set_single_publish(&mut self, state: PublishState) {
        if self.target == Target::Single { self.publish = state } else { self.stash = state }
    }

    /// Writes anything that changed: this video's grade, the montage and the settings. Cheap to call often.
    pub fn autosave(&mut self) {
        if let (Some(fp), Some(session)) = (&self.video_fp, self.video_session()) {
            let text = serde_json::to_string(&session).unwrap_or_default();
            if text != self.saved_text[0] {
                store::save(&format!("video-{fp}"), &session);
                self.saved_text[0] = text;
            }
        }
        self.autosave_montage();
        let mut settings = self.settings.clone();
        settings.last_video = self.source.as_ref().map(|s| s.path.clone()).or(settings.last_video);
        let text = serde_json::to_string(&settings).unwrap_or_default();
        if text != self.saved_text[1] {
            store::save("settings", &settings);
            self.saved_text[1] = text;
        }
    }

    // ----------------------------------------------------------- background work

    pub fn start_tools_download(&mut self) {
        if self.tools_download.is_some() {
            return;
        }
        self.tools_download = Some(0.0);
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let progress = mail.clone();
            let result = Tools::download(move |p| progress.send(Msg::ToolsProgress(p)));
            mail.send(Msg::Tools(result));
        });
    }

    pub fn open(&mut self, path: PathBuf) {
        let Some(tools) = self.tools.clone() else {
            self.fail("Install the video tools first.");
            return;
        };
        self.stop_playing();
        self.note(format!("Opening {}…", path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())));
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let probe = media::probe(&tools, &path);
            mail.send(Msg::Opened(path, probe));
        });
    }

    /// Asks for the frame at `t`. If one is already being fetched, the newest request waits its turn.
    pub fn request_frame(&mut self, t: f64) {
        let (Some(tools), Some(source)) = (self.tools.clone(), &self.source) else { return };
        if self.grab_busy {
            self.grab_wanted = Some(t);
            return;
        }
        self.grab_busy = true;
        let (probe, path, mail) = (source.probe.clone(), source.path.clone(), self.mail.clone());
        std::thread::spawn(move || {
            let options = GrabOptions { width: 960, finish: Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
            mail.send(Msg::Frame(media::grab_frame(&tools, &probe, &path, t, &options)));
        });
    }

    /// Moves to a moment in the video and shows its picture.
    pub fn seek(&mut self, t: f64) {
        let duration = self.source.as_ref().map_or(0.0, |s| s.probe.duration);
        self.player = None;
        self.t = t.clamp(0.0, duration);
        let t = self.t;
        self.request_frame(t);
    }

    /// Measures the selected part of the video and sets the automatic fix.
    pub fn analyse(&mut self) {
        let (Some(tools), Some(source)) = (self.tools.clone(), &self.source) else { return };
        let (probe, path, mail, (a, b)) = (source.probe.clone(), source.path.clone(), self.mail.clone(), self.trim);
        std::thread::spawn(move || {
            let options = GrabOptions { width: 480, finish: Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
            let mut all = Vec::new();
            for t in media::spread_times(a, b, 6) {
                match media::grab_frame(&tools, &probe, &path, t, &options) {
                    Ok(f) => all.extend(f.rgb),
                    Err(e) => return mail.send(Msg::Analysed(Err(e))),
                }
            }
            mail.send(Msg::Analysed(Ok(grade::auto_correct(&all))));
        });
    }

    fn apply_base(&mut self) {
        let strength = self.grade.strength;
        self.grade = self.auto_base;
        self.grade.strength = strength;
        if let Some(i) = self.look {
            LOOKS[i].apply(&mut self.grade, self.auto_base.temperature);
        }
    }

    pub fn pick_look(&mut self, i: usize) {
        self.look = Some(i.min(LOOKS.len() - 1));
        self.learned = None;
        self.apply_base();
    }

    /// Call after a slider changed the grade by hand: it is no longer exactly one of the named looks.
    pub fn grade_touched(&mut self) {
        self.look = None;
    }

    pub fn follow_action(&mut self) {
        let (Some(tools), Some((probe, path))) = (self.tools.clone(), self.source.as_ref().map(|s| (s.probe.clone(), s.path.clone()))) else { return };
        if self.action_busy {
            return;
        }
        self.action_busy = true;
        self.note("Finding where the action is, so the vertical video can follow it…");
        let mail = self.mail.clone();
        std::thread::spawn(move || mail.send(Msg::Action(media::analyse_action(&tools, &probe, &path))));
    }

    pub fn run_omni(&mut self) {
        let (Some(tools), Some(source), Some(key)) = (self.tools.clone(), &self.source, ai::load_key(ai::Provider::Gemini)) else { return };
        self.ai_busy = Some("Gemini Omni is grading a sample…".into());
        let (probe, path, mail, (a, b)) = (source.probe.clone(), source.path.clone(), self.mail.clone(), self.trim);
        let prompt = ai::omni_prompt(self.settings.style_notes.trim());
        // The same video, range and wording always gives the same question, so its answer can be reused.
        let cache = format!("ai-omni-{}", store::hash(&["omni".into(), store::fingerprint(&path), format!("{a:.2}-{b:.2}"), prompt.clone()]));
        let reuse = self.settings.reuse_ai;
        std::thread::spawn(move || {
            if reuse && let Some(look) = store::load::<LearnedLook>(&cache) {
                return mail.send(Msg::Omni(Ok(look), true));
            }
            let work = std::env::temp_dir().join(format!("video-enhancer-omni-{}", std::process::id()));
            let result = (|| {
                std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
                let pieces = 4;
                let each = ((b - a) / pieces as f64).min(ai::OMNI_MAX_SECONDS / pieces as f64 - 0.1).max(0.3);
                let (sample, graded) = (work.join("sample.mp4"), work.join("graded.mp4"));
                mail.send(Msg::Note("Cutting a short sample from across the video…".into()));
                media::cut_montage(&tools, &probe, &path, &media::spread_times(a, b, pieces), each, &sample)?;
                mail.send(Msg::Note("Sending the sample to Gemini Omni. This usually takes a minute or two…".into()));
                std::fs::write(&graded, ai::omni_grade(&key, &sample, &prompt)?).map_err(|e| e.to_string())?;
                mail.send(Msg::Note("Learning the colour look from Omni's version…".into()));
                ai::learn_from_omni(&tools, &sample, &graded)
            })();
            std::fs::remove_dir_all(&work).ok();
            if let Ok(look) = &result {
                store::save(&cache, look);
            }
            mail.send(Msg::Omni(result, false));
        });
    }

    pub fn run_openai(&mut self) {
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else { return };
        let model = ai::OPENAI_MODELS[self.settings.openai_model].0;
        self.ai_busy = Some(format!("{model} is looking at your video…"));
        let (mail, goal) = (self.mail.clone(), self.settings.style_notes.trim().to_string());
        let source = self.source.as_ref().map(|s| (s.probe.clone(), s.path.clone()));
        let tools = self.tools.clone();
        let (a, b) = self.trim;
        let sample = self.raw.clone();
        let cache = source.as_ref().map(|(_, path)| format!("ai-suggest-{}", store::hash(&["suggest".into(), model.into(), goal.clone(), store::fingerprint(path), format!("{a:.2}-{b:.2}")])));
        let reuse = self.settings.reuse_ai;
        std::thread::spawn(move || {
            if reuse && let Some(saved) = cache.as_ref().and_then(|c| store::load::<ai::Suggestion>(c)) {
                return mail.send(Msg::Suggestion(Ok(saved), true));
            }
            let result = (|| {
                let mut frames = Vec::new();
                if let (Some(tools), Some((probe, path))) = (&tools, &source) {
                    let options = GrabOptions { width: 640, finish: Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
                    for t in media::spread_times(a, b, 6) {
                        frames.push(media::grab_frame(tools, probe, path, t, &options)?);
                    }
                } else if let Some(frame) = sample {
                    frames.push(frame);
                }
                ai::openai_suggest(&key, model, &frames, &goal)
            })();
            if let (Ok(found), Some(cache)) = (&result, &cache) {
                store::save(cache, found);
            }
            mail.send(Msg::Suggestion(result, false));
        });
    }

    pub fn start_export(&mut self) {
        let (Some(tools), Some(source)) = (self.tools.clone(), &self.source) else { return };
        if self.trim.1 - self.trim.0 < 0.2 {
            self.fail("The selected part is too short to export.");
            return;
        }
        let s = &self.settings;
        let out_dir = s.out_dir.clone().or_else(|| source.path.parent().map(PathBuf::from)).unwrap_or_else(std::env::temp_dir);
        let stem = source.path.file_stem().map_or_else(|| "video".to_string(), |s| s.to_string_lossy().into_owned());
        let (probe, input, (start, end)) = (source.probe.clone(), source.path.clone(), self.trim);
        let (finish, grade, learned) = (self.finish, self.grade, self.learned.clone());
        let framing = match self.framing {
            VerticalFraming::Centre => Some(Framing::Position(0.5)),
            VerticalFraming::Manual => Some(Framing::Position(self.manual_x)),
            VerticalFraming::Blur => Some(Framing::Blur(self.blur.clamped())),
            VerticalFraming::Follow => self.action.clone().map(Framing::Follow),
        };
        let follow_needed = s.wants_vertical && self.framing == VerticalFraming::Follow && framing.is_none();
        let (wide, vertical, cube, thumb) = (s.wants_wide.then_some(s.wide_height), s.wants_vertical, s.wants_lut, s.wants_thumb);
        if wide.is_none() && !vertical && !cube && !thumb {
            self.fail("Tick at least one thing to export.");
            return;
        }
        let (cancel, mail, thumb_t) = (Arc::new(AtomicBool::new(false)), self.mail.clone(), self.t);
        let encoder = self.encoder();
        let on_card = self.colour_on_card();
        self.export = Some(ExportState { progress: 0.0, label: "Getting ready…".into(), cancel: cancel.clone() });
        self.made.clear();
        std::thread::spawn(move || {
            let began = std::time::Instant::now();
            let work = std::env::temp_dir().join(format!("video-enhancer-export-{}", std::process::id()));
            let result = (|| -> Result<Vec<PathBuf>, String> {
                std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
                let lut = Lut::build(LUT_SIZE, &grade, learned.as_ref());
                lut.write_cube(&work.join("look.cube")).map_err(|e| e.to_string())?;
                let framing = match framing {
                    Some(f) => f,
                    None if follow_needed => Framing::Follow(media::analyse_action(&tools, &probe, &input)?),
                    None => Framing::Position(0.5),
                };
                let mut jobs: Vec<(String, Shape, PathBuf)> = Vec::new();
                if let Some(height) = wide {
                    jobs.push(("16:9 video".into(), Shape::Wide { height }, media::free_name(&out_dir, &stem, "_16x9", "mp4")));
                }
                if vertical {
                    jobs.push(("9:16 video".into(), Shape::Vertical(framing), media::free_name(&out_dir, &stem, "_9x16", "mp4")));
                }
                let mut made = Vec::new();
                let steps = jobs.len().max(1) as f32;
                let mut fell_back = false;
                let mut on_card_used = false;
                let had_jobs = !jobs.is_empty();
                for (i, (label, shape, output)) in jobs.into_iter().enumerate() {
                    let job = ExportJob { input: input.clone(), probe: probe.clone(), start, end, shape, finish, lut_file: "look.cube".into(), workdir: work.clone(), output: output.clone(), encoder, gpu_lut: on_card.then(|| Arc::new(lut.clone())) };
                    let name = label.clone();
                    let used = media::export(&tools, &job, &cancel, |f| mail.send(Msg::Export((i as f32 + f) / steps, format!("Making the {name} ({})…", encoder.short()))))?;
                    fell_back |= used.encoder != encoder;
                    on_card_used |= used.gpu_grade;
                    made.push(output);
                }
                if had_jobs {
                    mail.send(Msg::EncodeInfo(encode_summary(began.elapsed().as_secs(), encoder, fell_back, on_card_used)));
                }
                if cube {
                    let target = media::free_name(&out_dir, &stem, "_look", "cube");
                    std::fs::copy(work.join("look.cube"), &target).map_err(|e| e.to_string())?;
                    made.push(target);
                }
                if thumb {
                    mail.send(Msg::Export(0.98, "Saving a thumbnail…".into()));
                    let options = GrabOptions { width: 1920, finish, crop_wide: true };
                    let frame = media::grab_frame(&tools, &probe, &input, thumb_t, &options)?;
                    let graded = Frame { width: frame.width, height: frame.height, rgb: lut.apply(&frame.rgb) };
                    let target = media::free_name(&out_dir, &stem, "_thumbnail", "jpg");
                    std::fs::write(&target, ai::encode_jpeg(&graded, 93)?).map_err(|e| e.to_string())?;
                    made.push(target);
                }
                Ok(made)
            })();
            std::fs::remove_dir_all(&work).ok();
            mail.send(Msg::ExportDone(result));
        });
    }

    // ------------------------------------------------------------------ playing

    pub fn toggle_play(&mut self) {
        if self.player.is_some() {
            self.stop_playing();
            return;
        }
        let (Some(tools), Some(source)) = (self.tools.clone(), &self.source) else { return };
        if self.t >= self.trim.1 - 0.2 {
            self.t = self.trim.0;
        }
        let wake = self.mail.wake.clone();
        self.player = Some(media::play(&tools, &source.probe, &source.path, self.t, move || wake()));
    }

    /// Stops playing and shows a full-quality picture of where it stopped.
    pub fn stop_playing(&mut self) {
        if self.player.take().is_some() {
            let t = self.t;
            self.request_frame(t);
        }
    }

    // ------------------------------------------------------------------ messages

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::ToolsProgress(p) => self.tools_download = Some(p),
            Msg::Tools(result) => {
                self.tools_download = None;
                match result {
                    Ok(tools) => {
                        self.tools = Some(tools);
                        self.detect_encoders();
                        self.note("Video tools are ready. Open a video to begin.");
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::Opened(path, Err(e)) => self.fail(format!("{}: {e}", path.display())),
            Msg::Opened(path, Ok(probe)) => {
                let mut warnings = Vec::new();
                if probe.hdr {
                    warnings.push("HDR video was converted to standard colour");
                }
                if !probe.is_wide() {
                    warnings.push("this isn't 16:9, so the wide version will be cropped");
                }
                let fp = store::fingerprint(&path);
                let saved: Option<VideoSession> = store::load(&format!("video-{fp}"));
                let restored = saved.is_some();
                self.video_fp = Some(fp);
                match saved {
                    Some(s) => {
                        let ok = s.trim.0 >= 0.0 && s.trim.1 <= probe.duration + 0.5 && s.trim.1 - s.trim.0 >= 0.2;
                        self.trim = if ok { s.trim } else { (0.0, probe.duration) };
                        self.t = s.t.clamp(0.0, probe.duration);
                        (self.grade, self.auto_base, self.learned, self.look) = (s.grade, s.auto_base, s.learned, s.look);
                        (self.finish, self.ai_result, self.framing, self.manual_x, self.action) = (s.finish, s.ai_result, s.framing, s.manual_x, s.action);
                        self.blur = s.blur.clamped();
                        self.set_single_publish(PublishState::with(s.pack));
                    }
                    None => {
                        self.t = probe.duration * 0.1;
                        self.trim = (0.0, probe.duration);
                        self.action = None;
                        self.learned = None;
                        self.look = Some(0);
                        self.finish = Finish::default();
                        self.ai_result.clear();
                        self.set_single_publish(PublishState::default());
                    }
                }
                let message = format!("{}  {}x{}  {}  {:.0} fps", path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()), probe.width, probe.height, clock(probe.duration), probe.fps);
                self.settings.recent.retain(|r| r.path != path);
                self.settings.recent.insert(0, Recent { path: path.clone(), duration: probe.duration, height: probe.height, fps: probe.fps });
                self.settings.recent.truncate(6);
                self.settings.last_video = Some(path.clone());
                self.looks_dirty = true;
                self.raw = None;
                self.graded = None;
                self.made.clear();
                self.source = Some(Source { path, probe });
                // Developer aid (debug builds): VE_SEED_PACK=file.json gives a video without a pack the pack in that file, for testing Publish.
                #[cfg(debug_assertions)]
                if self.publish.pack.is_none()
                    && let Some(file) = std::env::var_os("VE_SEED_PACK")
                    && let Ok(text) = std::fs::read_to_string(file)
                    && let Ok(seeded) = serde_json::from_str::<pack::Pack>(&text)
                {
                    self.set_single_publish(PublishState::with(Some(seeded)));
                }
                if self.target == Target::Single {
                    self.restore_thumbnails();
                }
                let t = self.t;
                self.request_frame(t);
                if restored {
                    warnings.insert(0, "restored your earlier grade and AI results");
                } else {
                    self.analyse();
                }
                self.note(if warnings.is_empty() { message } else { format!("{message}  ({})", warnings.join("; ")) });
            }
            Msg::Frame(result) => {
                self.grab_busy = false;
                match result {
                    Ok(frame) if self.player.is_none() => {
                        self.raw = Some(frame);
                        self.raw_changed = true;
                        self.looks_dirty = true;
                    }
                    Ok(_) => {}
                    Err(e) => self.fail(e),
                }
                if let Some(t) = self.grab_wanted.take() {
                    self.request_frame(t);
                }
            }
            Msg::Analysed(Ok(g)) => {
                self.auto_base = g;
                self.looks_dirty = true;
                self.apply_base();
            }
            Msg::Analysed(Err(e)) => self.fail(e),
            Msg::Action(result) => {
                self.action_busy = false;
                match result {
                    Ok(points) => {
                        self.action = Some(points);
                        self.note("Found the action. The vertical video will follow it smoothly.");
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::Note(text) => self.note(text),
            Msg::Omni(result, reused) => {
                self.ai_busy = None;
                match result {
                    Ok(look) => {
                        self.learned = Some(look);
                        self.grade = Grade::default();
                        self.look = None;
                        self.ai_result = "Gemini Omni's colour look is applied to your whole video. Use the slider to blend it.".into();
                        self.note(if reused { "Reused your saved Omni result. Nothing was sent and nothing was charged." } else { "Done. Omni's look was copied onto your real footage. It is saved, so it will still be here next time." });
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::Suggestion(result, reused) => {
                self.ai_busy = None;
                match result {
                    Ok(s) => {
                        self.grade = s.apply_over(&self.auto_base);
                        self.learned = None;
                        self.look = None;
                        self.ai_result = format!("{}: {}", s.look, s.reasoning);
                        self.note(if reused { "Reused your saved suggestion. Nothing was sent and no tokens were used." } else { "Done. The suggested grade is applied and saved. Adjust anything you like." });
                    }
                    Err(e) => self.fail(e),
                }
            }
            Msg::Export(progress, label) => {
                if let Some(e) = &mut self.export {
                    e.progress = progress;
                    e.label = label;
                }
            }
            Msg::Encoders(found) => self.encoders = found,
            Msg::Gpu(result) => self.gpu_status = Some(result),
            Msg::EncodeInfo(text) => self.encode_info = text,
            Msg::Pack(event) => self.pack_event(event),
            Msg::Montage(event) => self.montage_event(event),
            Msg::ExportDone(result) => {
                self.export = None;
                match result {
                    Ok(files) => {
                        let how = std::mem::take(&mut self.encode_info);
                        self.note(format!("Finished. {} file(s) saved.{how}", files.len()));
                        self.made = files;
                    }
                    Err(e) if e == "Cancelled" => self.note("Export cancelled. Nothing half-finished was left behind."),
                    Err(e) => self.fail(format!("Export failed: {e}")),
                }
            }
        }
    }

    /// Takes in everything the background threads have finished, keeps the picture and looks up to date,
    /// and returns true when anything changed that is worth redrawing.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
            changed = true;
        }
        let (mut latest, mut ended) = (None, false);
        if let Some(player) = &self.player {
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
        if let Some((t, frame)) = latest {
            self.t = t;
            self.raw = Some(frame);
            self.raw_changed = true;
            changed = true;
            if t >= self.trim.1 {
                ended = true;
            }
        }
        if ended {
            self.stop_playing();
            changed = true;
        }
        changed |= self.preview_tick();
        changed |= self.refresh();
        changed
    }

    /// Rebuilds the graded picture when the frame or the grade changed, and the eight look pictures when paused.
    fn refresh(&mut self) -> bool {
        let key = (self.grade, self.learned.clone());
        let lut_changed = key != self.lut_for;
        if lut_changed {
            self.lut = Lut::build(LUT_SIZE, &key.0, key.1.as_ref());
            self.lut_for = key;
        }
        let mut changed = false;
        if let Some(raw) = &self.raw
            && (self.raw_changed || lut_changed)
        {
            self.graded = Some(Frame { width: raw.width, height: raw.height, rgb: self.lut.apply(&raw.rgb) });
            self.raw_changed = false;
            self.view_gen += 1;
            changed = true;
        }
        // Not while playing: new pictures every frame would cost more than they show.
        if self.looks_dirty && self.player.is_none() && self.raw.is_some() {
            self.look_pictures = self.draw_looks();
            self.looks_gen += 1;
            self.looks_dirty = false;
            changed = true;
        }
        changed
    }

    /// The eight looks as small pictures of the current frame, drawn with the same colour maths as the preview.
    fn draw_looks(&self) -> Vec<Frame> {
        let Some(raw) = &self.raw else { return Vec::new() };
        let (w, h) = (raw.width.max(1), raw.height.max(1));
        let tw = 192usize.min(w);
        let th = (tw * h / w).max(1);
        let mut small = Vec::with_capacity(tw * th * 3);
        for y in 0..th {
            for x in 0..tw {
                let at = ((y * h / th) * w + x * w / tw) * 3;
                small.extend_from_slice(&raw.rgb[at..at + 3]);
            }
        }
        LOOKS
            .iter()
            .map(|look| {
                let mut g = self.auto_base;
                g.strength = self.grade.strength;
                look.apply(&mut g, self.auto_base.temperature);
                Frame { width: tw, height: th, rgb: Lut::build(17, &g, None).apply(&small) }
            })
            .collect()
    }
}

pub fn clock(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::media::tests::{make_video, scratch};

    /// Keeps every test's saves in one scratch folder, away from the real ones. Set once, because the setting is global.
    pub(crate) fn isolate_saves() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let dir = std::env::temp_dir().join(format!("ve-engine-saves-{}", std::process::id()));
            // No test in this crate reads the saved folder in any other way.
            unsafe { std::env::set_var("VIDEO_ENHANCER_SAVED", dir) };
        });
    }

    /// Polls until `done` is true or the time runs out.
    pub(crate) fn wait(engine: &mut Engine, seconds: u64, done: impl Fn(&Engine) -> bool) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < end {
            engine.poll();
            if done(engine) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        panic!("timed out: {}", engine.status);
    }

    #[test]
    fn a_video_opens_shows_a_graded_picture_and_the_looks_and_exports() {
        let dir = scratch("engine");
        isolate_saves();
        let video = dir.join("clip.mp4");
        let Some(tools) = Tools::find() else { return };
        make_video(&tools, &video, "1920x1080", 6);
        let mut engine = Engine::new(|| {});
        engine.settings.out_dir = Some(dir.clone());
        engine.settings.wants_thumb = false;
        engine.open(video.clone());
        // The picture, the automatic fix (which sets the first look's grade), and the eight look pictures.
        wait(&mut engine, 60, |e| e.source.is_some() && e.graded.is_some() && e.grade != Grade::default() && e.look_pictures.len() == LOOKS.len());
        let source = engine.source.as_ref().unwrap();
        assert_eq!((source.probe.width, source.probe.height), (1920, 1080));
        assert_eq!(engine.trim.1.round(), 6.0);
        let (raw, graded) = (engine.raw.clone().unwrap(), engine.graded.clone().unwrap());
        assert_eq!((raw.width, raw.height), (graded.width, graded.height));
        assert!(raw.rgb != graded.rgb, "the first look changes the picture");
        // Picking another look regrades the same frame, and the sliders do too.
        let before = engine.view_gen;
        engine.pick_look(3);
        engine.poll();
        assert!(engine.view_gen > before && engine.look == Some(3));
        let after_look = engine.graded.clone().unwrap().rgb;
        engine.grade.exposure = 0.6;
        engine.grade_touched();
        engine.poll();
        assert!(engine.look.is_none() && engine.graded.clone().unwrap().rgb != after_look);
        // The eight look pictures are small and differ from each other.
        assert!(engine.look_pictures.iter().all(|f| f.width <= 192));
        assert!(engine.look_pictures[0].rgb != engine.look_pictures[3].rgb, "the looks differ");
        // Seeking fetches a new frame.
        let before_seek = engine.view_gen;
        engine.seek(4.0);
        wait(&mut engine, 30, |e| e.view_gen > before_seek);
        // Export one wide video of the middle part.
        engine.trim = (1.0, 3.0);
        engine.settings.wants_vertical = false;
        engine.start_export();
        assert!(engine.export.is_some());
        wait(&mut engine, 120, |e| e.export.is_none() && !e.made.is_empty());
        let made = engine.made.clone();
        assert_eq!(made.len(), 1, "{}", engine.status);
        assert!(made[0].is_file() && std::fs::metadata(&made[0]).unwrap().len() > 10_000);
        let out = media::probe(&tools, &made[0]).unwrap();
        assert!((out.duration - 2.0).abs() < 0.3, "{}", out.duration);
        // The grade is saved with the video and comes back when it is opened again.
        engine.autosave();
        let mut again = Engine::new(|| {});
        again.open(video);
        wait(&mut again, 60, |e| e.source.is_some());
        assert_eq!(again.trim.0.round(), 1.0);
        assert!((again.grade.exposure - 0.6).abs() < 1e-6);
        std::fs::remove_dir_all(&dir).ok();
    }
}
