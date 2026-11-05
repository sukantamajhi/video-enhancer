#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod gpu;
mod grade;
mod media;
mod montage;
mod montage_ui;
mod music;
mod pack;
mod pack_ui;
mod sample;
mod screens;
mod store;
mod theme;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use eframe::egui;
use grade::{Grade, LOOKS, LUT_SIZE, LearnedLook, Lut};
use serde::{Deserialize, Serialize};
use media::{BlurLayout, Encoder, EncoderChoice, ExportJob, Finish, Frame, Framing, GrabOptions, Player, Probe, Shape, Tools};

/// What a background thread can tell the window.
enum Msg {
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
    Montage(montage_ui::Event),
    Encoders(Vec<Encoder>),
    /// The graphics card's name, or why it can't be used for colour grading.
    Gpu(Result<String, String>),
    EncodeInfo(String),
    Pack(pack_ui::Event),
}

/// How the finished video was made, for the message that follows an export.
fn encode_summary(seconds: u64, used: Encoder, fell_back: bool, colour_on_card: bool) -> String {
    let time = format!("{}:{:02}", seconds / 60, seconds % 60);
    let colour = if colour_on_card { ", with the colour grade done on the graphics card" } else { "" };
    if fell_back {
        format!(" Made in {time} on the CPU (the graphics-card encoder didn't work, so the CPU was used instead){colour}.")
    } else {
        format!(" Made in {time} with {}{colour}.", used.short())
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum Mode {
    Single,
    Montage,
}

/// Lets worker threads send a message and wake the window.
#[derive(Clone)]
struct Mailbox {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Mailbox {
    fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Compare {
    Split,
    Side,
    Before,
    After,
}

/// The four stops on the workflow rail. Video and Montage mean different things by the first three.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Source = 0,
    Look = 1,
    Export = 2,
    Publish = 3,
}

/// A video opened before, for the Start screen.
#[derive(Clone, Serialize, Deserialize)]
struct Recent {
    path: PathBuf,
    duration: f64,
    height: u32,
    fps: f64,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum VerticalFraming {
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

/// Choices that carry over from one run to the next.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    last_video: Option<PathBuf>,
    montage: bool,
    openai_model: usize,
    style_notes: String,
    reuse_ai: bool,
    wants_wide: bool,
    wide_height: u32,
    wants_vertical: bool,
    wants_lut: bool,
    wants_thumb: bool,
    out_dir: Option<PathBuf>,
    encoder: EncoderChoice,
    gpu_grade: bool,
    pack_topic: String,
    pack_format: pack::PackFormat,
    recent: Vec<Recent>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            last_video: None,
            montage: false,
            openai_model: 0,
            style_notes: ai::DEFAULT_STYLE.to_string(),
            reuse_ai: true,
            wants_wide: true,
            wide_height: 0,
            wants_vertical: true,
            wants_lut: false,
            wants_thumb: true,
            out_dir: None,
            encoder: EncoderChoice::Auto,
            gpu_grade: true,
            pack_topic: String::new(),
            pack_format: pack::PackFormat::Standard,
            recent: Vec::new(),
        }
    }
}

struct Source {
    path: PathBuf,
    probe: Probe,
}

struct ExportState {
    progress: f32,
    label: String,
    cancel: Arc<AtomicBool>,
}

struct App {
    mail: Mailbox,
    rx: Receiver<Msg>,
    tools: Option<Tools>,
    tools_download: Option<f32>,
    source: Option<Source>,
    t: f64,
    trim: (f64, f64),
    player: Option<Player>,

    raw: Option<Frame>,
    raw_changed: bool,
    grab_busy: bool,
    grab_wanted: Option<f64>,
    before_tex: Option<egui::TextureHandle>,
    after_tex: Option<egui::TextureHandle>,
    scope: Option<[[f32; 64]; 3]>,

    grade: Grade,
    auto_base: Grade,
    learned: Option<LearnedLook>,
    look: Option<usize>,
    finish: Finish,
    lut: Lut,
    lut_for: (Grade, Option<LearnedLook>),
    compare: Compare,
    split: f32,

    wants_wide: bool,
    wide_height: u32,
    wants_vertical: bool,
    framing: VerticalFraming,
    manual_x: f32,
    action: Option<Vec<(f64, f32)>>,
    action_busy: bool,
    wants_lut: bool,
    wants_thumb: bool,
    out_dir: Option<PathBuf>,
    export: Option<ExportState>,
    made: Vec<PathBuf>,

    style_notes: String,
    gemini_key: String,
    openai_key: String,
    has_gemini: bool,
    has_openai: bool,
    openai_model: usize,
    confirm_omni: bool,
    ai_busy: Option<String>,
    ai_result: String,

    status: String,
    status_is_error: bool,
    shot: Option<(PathBuf, u32)>,
    mode: Mode,
    mon: montage_ui::State,
    video_fp: Option<String>,
    pack1: pack_ui::PackState,
    encoders: Vec<Encoder>,
    encoder_choice: EncoderChoice,
    gpu_grade: bool,
    gpu_status: Option<Result<String, String>>,
    pack_topic: String,
    pack_format: pack::PackFormat,
    encode_info: String,
    blur: BlurLayout,
    reuse_ai: bool,
    last_save: f64,
    saved: [String; 3],
    step: Step,
    show_settings: bool,
    recent: Vec<Recent>,
    look_tex: Vec<egui::TextureHandle>,
    looks_dirty: bool,
    more_colour_open: bool,
    finish_open: bool,
    activity: Option<Vec<f32>>,
}

fn main() -> eframe::Result {
    // VE_SIZE=1360x1500 gives a taller window, so a long screen fits in one screenshot.
    let size: [f32; 2] = std::env::var("VE_SIZE").ok().and_then(|s| s.split_once('x').and_then(|(w, h)| Some([w.parse().ok()?, h.parse().ok()?]))).unwrap_or([1360.0, 860.0]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(size).with_min_inner_size([1000.0, 640.0]).with_title("Video Enhancer").with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Video Enhancer", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

fn clock(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        theme::apply(ctx);

        let (tx, rx) = mpsc::channel();
        let grade = Grade::default();
        let mut app = Self {
            mail: Mailbox { tx, ctx: ctx.clone() },
            rx,
            tools: Tools::find(),
            tools_download: None,
            source: None,
            t: 0.0,
            trim: (0.0, 0.0),
            player: None,
            raw: None,
            raw_changed: true,
            grab_busy: false,
            grab_wanted: None,
            before_tex: None,
            after_tex: None,
            scope: None,
            grade,
            auto_base: grade,
            learned: None,
            look: Some(0),
            finish: Finish::default(),
            lut: Lut::build(LUT_SIZE, &grade, None),
            lut_for: (grade, None),
            compare: Compare::Split,
            split: 0.5,
            wants_wide: true,
            wide_height: 0,
            wants_vertical: true,
            framing: VerticalFraming::Follow,
            manual_x: 0.5,
            action: None,
            action_busy: false,
            wants_lut: false,
            wants_thumb: true,
            out_dir: None,
            export: None,
            made: Vec::new(),
            style_notes: ai::DEFAULT_STYLE.to_string(),
            gemini_key: String::new(),
            openai_key: String::new(),
            has_gemini: ai::load_key(ai::Provider::Gemini).is_some(),
            has_openai: ai::load_key(ai::Provider::OpenAi).is_some(),
            openai_model: 0,
            confirm_omni: false,
            ai_busy: None,
            ai_result: String::new(),
            status: String::new(),
            status_is_error: false,
            shot: None,
            mode: if std::env::var_os("VE_MONTAGE").is_some() { Mode::Montage } else { Mode::Single },
            mon: montage_ui::State::new(),
            video_fp: None,
            pack1: pack_ui::PackState::default(),
            encoders: Vec::new(),
            encoder_choice: EncoderChoice::Auto,
            gpu_grade: true,
            gpu_status: None,
            pack_topic: String::new(),
            pack_format: pack::PackFormat::Standard,
            encode_info: String::new(),
            blur: BlurLayout::default(),
            reuse_ai: true,
            last_save: 0.0,
            saved: Default::default(),
            step: Step::Source,
            show_settings: false,
            recent: Vec::new(),
            look_tex: Vec::new(),
            looks_dirty: true,
            more_colour_open: false,
            finish_open: false,
            activity: None,
        };
        app.use_sample();
        app.status = if app.tools.is_some() {
            "Drop a video here, or press Open a video.".into()
        } else {
            "The video tools aren't installed yet. Press the download button on the left to get them (one time, about 110 MB).".into()
        };
        if let (Some(path), Ok(n)) = (std::env::var_os("VE_SCREENSHOT"), std::env::var("VE_SCREENSHOT_FRAMES")) {
            app.shot = Some((path.into(), n.parse().unwrap_or(12)));
        }
        app.restore();
        // Developer aid: VE_STEP=look|export|publish opens that step, for screenshots.
        if let Ok(step) = std::env::var("VE_STEP") {
            app.step = match step.as_str() {
                "source" => Step::Source,
                "look" => Step::Look,
                "export" => Step::Export,
                "publish" => Step::Publish,
                _ => app.step,
            };
        }
        app.detect_encoders();
        app.detect_gpu();
        app
    }

    /// Checks in the background which graphics-card encoders really work on this computer.
    fn detect_encoders(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let mail = self.mail.clone();
        std::thread::spawn(move || mail.send(Msg::Encoders(media::detect_encoders(&tools))));
    }

    /// Starts the graphics card and checks its colours against the processor's, in the background.
    fn detect_gpu(&mut self) {
        let mail = self.mail.clone();
        std::thread::spawn(move || mail.send(Msg::Gpu(gpu::shared().map(|g| g.name().to_string()))));
    }

    // ------------------------------------------------------------------ memory

    fn settings(&self) -> Settings {
        Settings {
            last_video: self.source.as_ref().map(|s| s.path.clone()),
            montage: self.mode == Mode::Montage,
            openai_model: self.openai_model,
            style_notes: self.style_notes.clone(),
            reuse_ai: self.reuse_ai,
            wants_wide: self.wants_wide,
            wide_height: self.wide_height,
            wants_vertical: self.wants_vertical,
            wants_lut: self.wants_lut,
            wants_thumb: self.wants_thumb,
            out_dir: self.out_dir.clone(),
            encoder: self.encoder_choice,
            gpu_grade: self.gpu_grade,
            pack_topic: self.pack_topic.clone(),
            pack_format: self.pack_format,
            recent: self.recent.clone(),
        }
    }

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
            pack: self.pack1.pack.clone(),
        })
    }

    /// Brings back the last session: settings, the last video, and the montage in progress.
    fn restore(&mut self) {
        if let Some(s) = store::load::<Settings>("settings") {
            self.openai_model = s.openai_model.min(ai::OPENAI_MODELS.len() - 1);
            self.style_notes = s.style_notes;
            self.reuse_ai = s.reuse_ai;
            self.wants_wide = s.wants_wide;
            self.wide_height = s.wide_height;
            self.wants_vertical = s.wants_vertical;
            self.wants_lut = s.wants_lut;
            self.wants_thumb = s.wants_thumb;
            self.out_dir = s.out_dir.filter(|d| d.is_dir());
            self.encoder_choice = s.encoder;
            self.gpu_grade = s.gpu_grade;
            self.pack_topic = s.pack_topic;
            self.pack_format = s.pack_format;
            self.recent = s.recent.into_iter().filter(|r| r.path.is_file()).collect();
            if std::env::var_os("VE_MONTAGE").is_none() && s.montage {
                self.mode = Mode::Montage;
            }
            if let Some(path) = s.last_video.filter(|p| p.is_file()) {
                self.open(path);
            }
        }
        self.restore_montage();
    }

    /// Writes anything that changed, at most once a second.
    fn autosave(&mut self, now: f64) {
        if now - self.last_save < 1.0 {
            return;
        }
        self.last_save = now;
        if let (Some(fp), Some(session)) = (&self.video_fp, self.video_session()) {
            let text = serde_json::to_string(&session).unwrap_or_default();
            if text != self.saved[0] {
                store::save(&format!("video-{fp}"), &session);
                self.saved[0] = text;
            }
        }
        let mut settings = self.settings();
        // Keep the last video when the sample picture is showing, so it still reopens next time.
        if settings.last_video.is_none() && self.video_fp.is_none() {
            settings.last_video = store::load::<Settings>("settings").and_then(|s| s.last_video);
        }
        let text = serde_json::to_string(&settings).unwrap_or_default();
        if text != self.saved[1] {
            store::save("settings", &settings);
            self.saved[1] = text;
        }
        if let Some(text) = self.mon.autosave_text()
            && text != self.saved[2]
        {
            self.mon.save();
            self.saved[2] = text;
        }
    }

    fn note(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_is_error = false;
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_is_error = true;
    }

    fn use_sample(&mut self) {
        let frame = sample::frame();
        self.auto_base = grade::auto_correct(&frame.rgb);
        self.grade = self.auto_base;
        self.look = Some(0);
        LOOKS[0].apply(&mut self.grade, self.auto_base.temperature);
        self.raw = Some(frame);
        self.raw_changed = true;
    }

    // ----------------------------------------------------------- background work

    fn open(&mut self, path: PathBuf) {
        let Some(tools) = self.tools.clone() else {
            self.fail("Install the video tools first (button on the left).");
            return;
        };
        self.player = None;
        self.note(format!("Opening {}…", path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())));
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let probe = media::probe(&tools, &path);
            mail.send(Msg::Opened(path, probe));
        });
    }

    fn start_tools_download(&mut self) {
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

    /// The eight looks as small pictures of the current frame, drawn with the same colour maths as the preview.
    fn refresh_looks(&mut self, ctx: &egui::Context) {
        let Some(raw) = &self.raw else { return };
        // Not while playing: new pictures every frame would cost more than they show.
        if !self.looks_dirty || self.player.is_some() {
            return;
        }
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
        let mut textures = Vec::with_capacity(LOOKS.len());
        for (i, look) in LOOKS.iter().enumerate() {
            let mut g = self.auto_base;
            g.strength = self.grade.strength;
            look.apply(&mut g, self.auto_base.temperature);
            let lut = Lut::build(17, &g, None);
            let image = egui::ColorImage::from_rgb([tw, th], &lut.apply(&small));
            textures.push(ctx.load_texture(format!("look-{i}"), image, egui::TextureOptions::LINEAR));
        }
        self.look_tex = textures;
        self.looks_dirty = false;
    }

    fn request_frame(&mut self, t: f64) {
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

    fn analyse(&mut self) {
        let (Some(tools), Some(source)) = (self.tools.clone(), &self.source) else {
            if let Some(frame) = &self.raw {
                self.auto_base = grade::auto_correct(&frame.rgb);
                self.apply_base();
            }
            return;
        };
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

    fn pick_look(&mut self, i: usize) {
        self.look = Some(i);
        self.learned = None;
        self.apply_base();
    }

    fn follow_action(&mut self) {
        let (Some(tools), Some((probe, path))) = (self.tools.clone(), self.source.as_ref().map(|s| (s.probe.clone(), s.path.clone()))) else { return };
        if self.action_busy {
            return;
        }
        self.action_busy = true;
        self.note("Finding where the action is, so the vertical video can follow it…");
        let mail = self.mail.clone();
        std::thread::spawn(move || mail.send(Msg::Action(media::analyse_action(&tools, &probe, &path))));
    }

    fn run_omni(&mut self) {
        let (Some(tools), Some(source), Some(key)) = (self.tools.clone(), &self.source, ai::load_key(ai::Provider::Gemini)) else { return };
        self.ai_busy = Some("Gemini Omni is grading a sample…".into());
        let (probe, path, mail, (a, b)) = (source.probe.clone(), source.path.clone(), self.mail.clone(), self.trim);
        let prompt = ai::omni_prompt(self.style_notes.trim());
        // The same video, range and wording always gives the same question, so its answer can be reused.
        let cache = format!("ai-omni-{}", store::hash(&["omni".into(), store::fingerprint(&path), format!("{a:.2}-{b:.2}"), prompt.clone()]));
        let reuse = self.reuse_ai;
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

    fn run_openai(&mut self) {
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else { return };
        let model = ai::OPENAI_MODELS[self.openai_model].0;
        self.ai_busy = Some(format!("{model} is looking at your video…"));
        let (mail, goal) = (self.mail.clone(), self.style_notes.trim().to_string());
        let source = self.source.as_ref().map(|s| (s.probe.clone(), s.path.clone()));
        let tools = self.tools.clone();
        let (a, b) = self.trim;
        let sample = self.raw.clone();
        let cache = source.as_ref().map(|(_, path)| format!("ai-suggest-{}", store::hash(&["suggest".into(), model.into(), goal.clone(), store::fingerprint(path), format!("{a:.2}-{b:.2}")])));
        let reuse = self.reuse_ai;
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

    fn start_export(&mut self) {
        let (Some(tools), Some(source)) = (self.tools.clone(), &self.source) else { return };
        if self.trim.1 - self.trim.0 < 0.2 {
            self.fail("The selected part is too short to export.");
            return;
        }
        let out_dir = self.out_dir.clone().or_else(|| source.path.parent().map(PathBuf::from)).unwrap_or_else(std::env::temp_dir);
        let stem = source.path.file_stem().map_or_else(|| "video".to_string(), |s| s.to_string_lossy().into_owned());
        let (probe, input, (start, end)) = (source.probe.clone(), source.path.clone(), self.trim);
        let (finish, grade, learned) = (self.finish, self.grade, self.learned.clone());
        let framing = match self.framing {
            VerticalFraming::Centre => Some(Framing::Position(0.5)),
            VerticalFraming::Manual => Some(Framing::Position(self.manual_x)),
            VerticalFraming::Blur => Some(Framing::Blur(self.blur.clamped())),
            VerticalFraming::Follow => self.action.clone().map(Framing::Follow),
        };
        let follow_needed = self.wants_vertical && self.framing == VerticalFraming::Follow && framing.is_none();
        let (wide, vertical, cube, thumb) = (self.wants_wide.then_some(self.wide_height), self.wants_vertical, self.wants_lut, self.wants_thumb);
        if wide.is_none() && !vertical && !cube && !thumb {
            self.fail("Tick at least one thing to export.");
            return;
        }
        let (cancel, mail, thumb_t) = (Arc::new(AtomicBool::new(false)), self.mail.clone(), self.t);
        let encoder = self.encoder_choice.resolve(&self.encoders);
        let on_card = self.gpu_grade && matches!(self.gpu_status, Some(Ok(_)));
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
                        self.pack1 = pack_ui::PackState::with(s.pack);
                    }
                    None => {
                        self.t = probe.duration * 0.1;
                        self.trim = (0.0, probe.duration);
                        self.action = None;
                        self.learned = None;
                        self.look = Some(0);
                        self.ai_result.clear();
                        self.pack1 = pack_ui::PackState::default();
                    }
                }
                let message = format!("{}  {}x{}  {}  {:.0} fps", path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()), probe.width, probe.height, clock(probe.duration), probe.fps);
                self.recent.retain(|r| r.path != path);
                self.recent.insert(0, Recent { path: path.clone(), duration: probe.duration, height: probe.height, fps: probe.fps });
                self.recent.truncate(6);
                if self.mode == Mode::Single && self.step == Step::Source {
                    self.step = Step::Look;
                }
                self.activity = None;
                self.looks_dirty = true;
                self.source = Some(Source { path, probe });
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

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        let mut latest = None;
        let mut ended = false;
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
            if t >= self.trim.1 {
                ended = true;
            }
        }
        if ended {
            self.stop();
        }
    }

    fn toggle_play(&mut self) {
        if self.player.is_some() {
            self.stop();
            return;
        }
        let (Some(tools), Some(source)) = (self.tools.clone(), &self.source) else { return };
        if self.t >= self.trim.1 - 0.2 {
            self.t = self.trim.0;
        }
        let ctx = self.mail.ctx.clone();
        self.player = Some(media::play(&tools, &source.probe, &source.path, self.t, move || ctx.request_repaint()));
    }

    fn stop(&mut self) {
        if self.player.take().is_some() {
            let t = self.t;
            self.request_frame(t);
        }
    }

    // ---------------------------------------------------------------- rendering

    fn refresh(&mut self, ctx: &egui::Context) {
        let key = (self.grade, self.learned.clone());
        let lut_changed = key != self.lut_for;
        if lut_changed {
            self.lut = Lut::build(LUT_SIZE, &key.0, key.1.as_ref());
            self.lut_for = key;
        }
        let Some(raw) = &self.raw else { return };
        if !(self.raw_changed || lut_changed) {
            return;
        }
        let size = [raw.width, raw.height];
        let options = egui::TextureOptions::LINEAR;
        if self.raw_changed {
            let image = egui::ColorImage::from_rgb(size, &raw.rgb);
            match &mut self.before_tex {
                Some(tex) if tex.size() == size => tex.set(image, options),
                slot => *slot = Some(ctx.load_texture("before", image, options)),
            }
        }
        let graded = self.lut.apply(&raw.rgb);
        self.scope = Some(grade::scope(&graded));
        let image = egui::ColorImage::from_rgb(size, &graded);
        match &mut self.after_tex {
            Some(tex) if tex.size() == size => tex.set(image, options),
            slot => *slot = Some(ctx.load_texture("after", image, options)),
        }
        self.raw_changed = false;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain();
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if self.mode == Mode::Montage {
            if !dropped.is_empty() {
                self.add_clips(dropped);
            }
        } else if dropped.len() > 1 {
            // Several clips at once can only mean a montage.
            self.switch_mode(Mode::Montage);
            self.add_clips(dropped);
        } else if let Some(path) = dropped.into_iter().next() {
            self.open(path);
        }
        if self.mode == Mode::Single && ctx.input(|i| i.key_pressed(egui::Key::Space)) && !ctx.egui_wants_keyboard_input() {
            self.toggle_play();
        }
        self.refresh(&ctx);
        self.refresh_looks(&ctx);
        self.autosave(ctx.input(|i| i.time));

        if self.mode != Mode::Montage || !matches!(self.step, Step::Look | Step::Export) {
            self.mon.stop_preview();
        }
        self.shell(ui);
        match (self.mode, self.step) {
            (Mode::Single, Step::Source) if self.source.is_none() => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::WINDOW)).show(ui, |ui| self.start_screen(ui));
            }
            (Mode::Single, Step::Source) => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::WINDOW)).show(ui, |ui| self.source_screen(ui));
            }
            (Mode::Single, Step::Look) => {
                self.timeline_panel(ui);
                self.look_inspector(ui);
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::WINDOW)).show(ui, |ui| self.look_center(ui));
            }
            (Mode::Single, Step::Export) => {
                self.export_inspector(ui);
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::WINDOW)).show(ui, |ui| self.export_center(ui));
            }
            (Mode::Single, Step::Publish) | (Mode::Montage, Step::Publish) => {
                let target = if self.mode == Mode::Single { pack_ui::Target::Single } else { pack_ui::Target::Montage };
                self.publish_screen(ui, target);
            }
            (Mode::Montage, step) => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::WINDOW)).show(ui, |ui| self.montage_screen(ui, step));
            }
        }
        self.toast(&ctx);
        self.settings_window(&ctx);

        if self.confirm_omni {
            egui::Window::new("Send a sample to Gemini Omni?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(&ctx, |ui| {
                ui.label("This uploads a 10-second sample of your video to Google and uses your Gemini key.");
                ui.label("It costs real money, roughly one dollar per run. The rest of your video is not uploaded.");
                ui.horizontal(|ui| {
                    if ui.button("Send it").clicked() {
                        self.confirm_omni = false;
                        self.run_omni();
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_omni = false;
                    }
                });
            });
        }

        // Developer aid: VE_SCREENSHOT=path.png VE_SCREENSHOT_FRAMES=12 saves a picture of the window and quits.
        if let Some((path, frames)) = &mut self.shot {
            if *frames > 0 {
                *frames -= 1;
                if *frames == 0 {
                    let path = path.clone();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                        let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                        if let Some(buffer) = image::RgbaImage::from_raw(image.width() as u32, image.height() as u32, rgba) {
                            let _ = buffer.save(&path);
                        }
                        std::process::exit(0);
                    })));
                }
                ctx.request_repaint();
            }
        }
        if self.player.is_some() || self.ai_busy.is_some() || self.export.is_some() || self.mon.working() || self.pack1.working() || self.tools_download.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
