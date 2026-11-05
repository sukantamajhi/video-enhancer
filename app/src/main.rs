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
mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use eframe::egui::{self, Color32, RichText, pos2, vec2};
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
    show_guide: bool,

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
    encode_info: String,
    blur: BlurLayout,
    reuse_ai: bool,
    last_save: f64,
    saved: [String; 3],
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
        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = Color32::from_rgb(214, 92, 51);
        visuals.hyperlink_color = Color32::from_rgb(240, 140, 100);
        ctx.set_visuals(visuals);
        ctx.global_style_mut(|s| {
            s.spacing.item_spacing = vec2(8.0, 6.0);
            s.spacing.slider_width = 150.0;
        });

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
            show_guide: true,
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
            encode_info: String::new(),
            blur: BlurLayout::default(),
            reuse_ai: true,
            last_save: 0.0,
            saved: Default::default(),
        };
        app.use_sample();
        app.status = if app.tools.is_some() {
            "Drop a video here, or press Open video. The sample picture below lets you try the controls.".into()
        } else {
            "The video tools aren't installed yet. Press the download button on the left to get them (one time, about 110 MB).".into()
        };
        if let (Some(path), Ok(n)) = (std::env::var_os("VE_SCREENSHOT"), std::env::var("VE_SCREENSHOT_FRAMES")) {
            app.shot = Some((path.into(), n.parse().unwrap_or(12)));
        }
        app.restore();
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

    fn preview(&mut self, ui: &mut egui::Ui) {
        let (Some(before), Some(after)) = (self.before_tex.clone(), self.after_tex.clone()) else { return };
        let aspect = before.size()[0] as f32 / before.size()[1] as f32;
        let avail = ui.available_size();
        let (w, h) = if avail.x / avail.y > aspect { (avail.y * aspect, avail.y) } else { (avail.x, avail.x / aspect) };
        let (rect, response) = ui.allocate_exact_size(vec2(w.max(10.0), h.max(10.0)), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let full = egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let label = |rect: egui::Rect, text: &str, align: egui::Align2| {
            let anchor = match align {
                egui::Align2::RIGHT_TOP => rect.right_top() + vec2(-10.0, 10.0),
                _ => rect.left_top() + vec2(10.0, 10.0),
            };
            let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), Color32::WHITE);
            let box_rect = align.anchor_size(anchor, galley.size()).expand2(vec2(6.0, 3.0));
            painter.rect_filled(box_rect, 4.0, Color32::from_black_alpha(150));
            painter.galley(box_rect.min + vec2(6.0, 3.0), galley, Color32::WHITE);
        };
        match self.compare {
            Compare::Before => {
                painter.image(before.id(), rect, full, Color32::WHITE);
                label(rect, "BEFORE", egui::Align2::LEFT_TOP);
            }
            Compare::After => {
                painter.image(after.id(), rect, full, Color32::WHITE);
                label(rect, "AFTER", egui::Align2::LEFT_TOP);
            }
            Compare::Split | Compare::Side => {
                if self.compare == Compare::Split {
                    if let Some(p) = response.interact_pointer_pos().filter(|_| response.dragged() || response.clicked()) {
                        self.split = ((p.x - rect.left()) / rect.width()).clamp(0.02, 0.98);
                    }
                    let x = rect.left() + rect.width() * self.split;
                    let left = egui::Rect::from_min_max(rect.min, pos2(x, rect.bottom()));
                    let right = egui::Rect::from_min_max(pos2(x, rect.top()), rect.max);
                    painter.image(before.id(), left, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(self.split, 1.0)), Color32::WHITE);
                    painter.image(after.id(), right, egui::Rect::from_min_max(pos2(self.split, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], egui::Stroke::new(2.0, Color32::WHITE));
                    painter.circle(pos2(x, rect.center().y), 11.0, Color32::from_rgb(214, 92, 51), egui::Stroke::new(2.0, Color32::WHITE));
                    label(rect, "BEFORE", egui::Align2::LEFT_TOP);
                    label(rect, "AFTER", egui::Align2::RIGHT_TOP);
                } else {
                    let half = egui::Rect::from_min_max(rect.min, pos2(rect.center().x - 2.0, rect.bottom()));
                    let other = half.translate(vec2(rect.width() / 2.0 + 2.0, 0.0));
                    painter.image(before.id(), half, full, Color32::WHITE);
                    painter.image(after.id(), other, full, Color32::WHITE);
                    label(half, "BEFORE", egui::Align2::LEFT_TOP);
                    label(other, "AFTER", egui::Align2::LEFT_TOP);
                }
            }
        }
        if let (true, Some(source)) = (self.show_guide && self.wants_vertical, &self.source) {
            let share = source.probe.vertical_share() as f32;
            if share < 1.0 && self.framing != VerticalFraming::Blur {
                let pos = match self.framing {
                    VerticalFraming::Manual => self.manual_x,
                    VerticalFraming::Follow => self.action.as_ref().map_or(0.5, |p| media::action_at(p, self.t)),
                    _ => 0.5,
                };
                let width = rect.width() * share;
                let left = rect.left() + (rect.width() - width) * pos;
                let dim = Color32::from_black_alpha(140);
                painter.rect_filled(egui::Rect::from_min_max(rect.min, pos2(left, rect.bottom())), 0.0, dim);
                painter.rect_filled(egui::Rect::from_min_max(pos2(left + width, rect.top()), rect.max), 0.0, dim);
                painter.rect_stroke(egui::Rect::from_min_max(pos2(left, rect.top()), pos2(left + width, rect.bottom())), 0.0, egui::Stroke::new(2.0, Color32::from_rgb(240, 140, 100)), egui::StrokeKind::Inside);
            }
        }
    }

    fn slider(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, name: &str) -> bool {
        ui.add(egui::Slider::new(value, range).text(name).fixed_decimals(2)).changed()
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        if self.tools.is_none() {
            ui.group(|ui| {
                ui.label(RichText::new("Video tools needed").strong());
                ui.label("This app uses FFmpeg to read and write video. Download it once (about 110 MB, checked for safety before use).");
                if let Some(p) = self.tools_download {
                    ui.add(egui::ProgressBar::new(p).show_percentage());
                } else if ui.button("Download video tools").clicked() {
                    self.tools_download = Some(0.0);
                    let mail = self.mail.clone();
                    std::thread::spawn(move || {
                        let progress = mail.clone();
                        let result = Tools::download(move |p| progress.send(Msg::ToolsProgress(p)));
                        mail.send(Msg::Tools(result));
                    });
                }
            });
            ui.add_space(6.0);
        }

        ui.heading("Look");
        ui.label(RichText::new("Pick a starting point. Auto-fix has already corrected exposure and white balance.").weak().small());
        let mut picked = None;
        ui.horizontal_wrapped(|ui| {
            for (i, look) in LOOKS.iter().enumerate() {
                if ui.selectable_label(self.look == Some(i), look.name).on_hover_text(look.note).clicked() {
                    picked = Some(i);
                }
            }
        });
        if let Some(i) = picked {
            self.pick_look(i);
        }
        ui.horizontal(|ui| {
            if ui.button("Auto-fix again").on_hover_text("Re-measures the brightness and colour cast of the selected part of the video").clicked() {
                self.analyse();
            }
            if ui.button("Reset").clicked() {
                self.look = None;
                self.learned = None;
                self.grade = Grade::default();
            }
        });
        Self::slider(ui, &mut self.grade.strength, 0.0..=1.0, "Overall amount");
        if let Some(learned) = &mut self.learned {
            ui.add(egui::Slider::new(&mut learned.amount, 0.0..=1.0).text("Omni look amount").fixed_decimals(2));
        }

        ui.add_space(4.0);
        egui::CollapsingHeader::new("Fine tune").default_open(false).show(ui, |ui| {
            let g = &mut self.grade;
            let mut touched = false;
            touched |= Self::slider(ui, &mut g.exposure, -1.0..=1.0, "Exposure");
            touched |= Self::slider(ui, &mut g.contrast, -0.5..=0.6, "Contrast");
            touched |= Self::slider(ui, &mut g.highlights, -1.0..=1.0, "Highlights");
            touched |= Self::slider(ui, &mut g.shadows, -1.0..=1.0, "Shadows");
            touched |= Self::slider(ui, &mut g.temperature, -1.0..=1.0, "Warmth");
            touched |= Self::slider(ui, &mut g.tint, -1.0..=1.0, "Tint");
            touched |= Self::slider(ui, &mut g.saturation, -0.6..=0.6, "Saturation");
            touched |= Self::slider(ui, &mut g.vibrance, 0.0..=0.8, "Vibrance");
            touched |= Self::slider(ui, &mut g.teal_orange, 0.0..=1.0, "Teal & orange");
            touched |= Self::slider(ui, &mut g.fade, 0.0..=0.2, "Fade");
            if touched {
                self.look = None;
            }
        });
        egui::CollapsingHeader::new("Finishing (applied when you export)").default_open(false).show(ui, |ui| {
            Self::slider(ui, &mut self.finish.sharpen, 0.0..=1.0, "Sharpen (slower export)");
            Self::slider(ui, &mut self.finish.denoise, 0.0..=1.0, "Noise cleanup");
            ui.checkbox(&mut self.finish.loudness, "Even out the sound (YouTube level)");
        });

        ui.add_space(8.0);
        ui.heading("AI helpers");
        ui.label(RichText::new("Optional. They need your own API key.").weak().small());
        ui.label("Style you want");
        ui.add(egui::TextEdit::multiline(&mut self.style_notes).desired_rows(2).desired_width(f32::INFINITY));
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.reuse_ai, "Reuse saved AI answers").on_hover_text("When nothing has changed, a saved answer is shown instead of asking the AI again. No cost, no tokens.");
            if ui.small_button("Forget saved answers").clicked() {
                let n = store::clear_ai_cache();
                self.note(format!("Forgot {n} saved AI answer(s). The next request will ask the AI again."));
            }
        });
        let busy = self.ai_busy.is_some();
        let have_video = self.source.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.add_enabled_ui(self.has_gemini && have_video, |ui| {
                    if ui.button("Grade with Gemini Omni").on_hover_text("Omni grades a 10-second sample from across your video. The app copies that look onto the real footage. Costs money.").clicked() {
                        self.confirm_omni = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("openai_model").selected_text(ai::OPENAI_MODELS[self.openai_model].1).show_ui(ui, |ui| {
                    for (i, (_, name)) in ai::OPENAI_MODELS.iter().enumerate() {
                        ui.selectable_value(&mut self.openai_model, i, *name);
                    }
                });
                ui.add_enabled_ui(self.has_openai, |ui| {
                    if ui.button("Suggest with OpenAI").on_hover_text("Sends 6 small pictures from your video and applies the slider values it suggests.").clicked() {
                        self.run_openai();
                    }
                });
            });
        });
        if let Some(text) = &self.ai_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(text);
            });
        }
        if !self.ai_result.is_empty() {
            ui.label(RichText::new(&self.ai_result).italics());
        }
        egui::CollapsingHeader::new(if self.has_gemini || self.has_openai { "API keys (saved)" } else { "API keys" }).show(ui, |ui| {
            ui.label(RichText::new("Stored in your computer's secure credential store, not in a file.").weak().small());
            for (provider, name, buffer, flag) in [
                (ai::Provider::Gemini, "Gemini", &mut self.gemini_key, &mut self.has_gemini),
                (ai::Provider::OpenAi, "OpenAI", &mut self.openai_key, &mut self.has_openai),
            ] {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(buffer).password(true).hint_text(format!("{name} key")).desired_width(150.0));
                    if ui.button("Save").clicked() {
                        match ai::save_key(provider, buffer) {
                            Ok(()) => {
                                buffer.clear();
                                *flag = ai::load_key(provider).is_some();
                            }
                            Err(e) => {
                                self.status = e;
                                self.status_is_error = true;
                            }
                        }
                    }
                    if *flag && ui.small_button("Remove").clicked() {
                        let _ = ai::save_key(provider, "");
                        *flag = ai::load_key(provider).is_some();
                    }
                });
            }
        });
    }

    /// Zoom and position of the main picture over the blurred background, with a
    /// live 9:16 preview. Drag the preview to move the picture.
    fn blur_controls(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Main picture").weak().small());
        ui.add(egui::Slider::new(&mut self.blur.zoom, 1.0..=2.0).text("zoom").fixed_decimals(2));
        let Some(tex) = self.after_tex.clone() else { return };
        let aspect = tex.size()[0] as f32 / tex.size()[1].max(1) as f32;
        let width = 150.0f32.min(ui.available_width());
        let (rect, response) = ui.allocate_exact_size(vec2(width, width * 16.0 / 9.0), egui::Sense::click_and_drag());
        let b = &mut self.blur;
        let (fw, fh) = (rect.width() * b.zoom, rect.width() * b.zoom / aspect);
        if response.dragged() {
            // Dragging the picture one way shows more of the other side of it.
            let d = response.drag_delta();
            if fw > rect.width() + 0.5 {
                b.x = (b.x - d.x / (fw - rect.width())).clamp(0.0, 1.0);
            }
            if fh < rect.height() - 0.5 {
                b.y = (b.y + d.y / (rect.height() - fh)).clamp(0.0, 1.0);
            }
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::BLACK);
        // The background is the same picture filling the frame, dimmed like the blur.
        let cover = ((9.0 / 16.0) / aspect).min(1.0);
        let uv = egui::Rect::from_min_max(pos2(0.5 - cover / 2.0, 0.0), pos2(0.5 + cover / 2.0, 1.0));
        painter.image(tex.id(), rect, uv, Color32::from_gray(80));
        let left = rect.left() - (fw - rect.width()) * b.x;
        let top = if fh > rect.height() { rect.top() - (fh - rect.height()) * 0.5 } else { rect.top() + (rect.height() - fh) * b.y };
        let picture = egui::Rect::from_min_size(pos2(left, top), vec2(fw, fh));
        painter.image(tex.id(), picture, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        // A crosshair on the middle of the main picture.
        let centre = picture.intersect(rect).center();
        for colour_width in [(Color32::BLACK, 3.0), (Color32::WHITE, 1.2)] {
            let stroke = egui::Stroke::new(colour_width.1, colour_width.0);
            painter.line_segment([centre - vec2(9.0, 0.0), centre + vec2(9.0, 0.0)], stroke);
            painter.line_segment([centre - vec2(0.0, 9.0), centre + vec2(0.0, 9.0)], stroke);
        }
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, Color32::from_gray(120)), egui::StrokeKind::Inside);
        ui.horizontal(|ui| {
            if ui.small_button("Centre").on_hover_text("Put the picture back in the middle").clicked() {
                self.blur.x = 0.5;
                self.blur.y = 0.5;
            }
            if ui.small_button("Reset zoom").clicked() {
                self.blur = BlurLayout::default();
            }
        });
        ui.label(RichText::new("Drag the preview to move the picture.").weak().small());
    }

    /// Which encoder writes the finished video. Shared by both screens.
    pub fn encoder_picker(&mut self, ui: &mut egui::Ui, show_grade: bool) {
        let using = self.encoder_choice.resolve(&self.encoders);
        let automatic = format!("Automatic ({})", using.short());
        ui.horizontal(|ui| {
            ui.label("Encoder");
            let shown = match self.encoder_choice {
                EncoderChoice::Auto => automatic.clone(),
                _ => using.name().to_string(),
            };
            egui::ComboBox::from_id_salt("encoder").selected_text(shown).width(200.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.encoder_choice, EncoderChoice::Auto, automatic);
                ui.selectable_value(&mut self.encoder_choice, EncoderChoice::Cpu, Encoder::Cpu.name());
                for (choice, encoder) in [(EncoderChoice::Nvidia, Encoder::Nvidia), (EncoderChoice::Amd, Encoder::Amd), (EncoderChoice::Intel, Encoder::Intel)] {
                    if self.encoders.contains(&encoder) {
                        ui.selectable_value(&mut self.encoder_choice, choice, encoder.name());
                    }
                }
            });
        });
        ui.label(RichText::new(if self.encoders.is_empty() { "No graphics-card encoder found here, so the CPU is used." } else { "Graphics-card encoding is much faster. Files come out slightly larger." }).weak().small());
        if show_grade {
            let ready = matches!(self.gpu_status, Some(Ok(_)));
            ui.add_enabled(ready, egui::Checkbox::new(&mut self.gpu_grade, "Grade colour on the graphics card"))
                .on_hover_text("Much faster than the processor, and identical to the preview. Single-video exports only for now; montages still grade with FFmpeg.");
            let status = match &self.gpu_status {
                None => "Checking the graphics card…".to_string(),
                Some(Ok(name)) => format!("Colour grading on: {name}"),
                Some(Err(why)) => format!("Not available here, so FFmpeg grades the colour: {why}"),
            };
            ui.label(RichText::new(status).weak().small());
        }
    }

    fn export_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Export");
        let have = self.source.is_some();
        if !have {
            ui.label(RichText::new("Open a video to export it.").weak());
        }
        ui.add_enabled_ui(have && self.export.is_none(), |ui| {
            ui.checkbox(&mut self.wants_wide, "16:9 video (YouTube)");
            if self.wants_wide {
                ui.indent("wide", |ui| {
                    egui::ComboBox::from_id_salt("wide_height").selected_text(match self.wide_height {
                        0 => "Same size as the original".to_string(),
                        h => format!("{h}p"),
                    }).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.wide_height, 0, "Same size as the original");
                        ui.selectable_value(&mut self.wide_height, 1080, "1080p");
                        ui.selectable_value(&mut self.wide_height, 1440, "1440p");
                        ui.selectable_value(&mut self.wide_height, 2160, "2160p (4K)");
                    });
                });
            }
            ui.checkbox(&mut self.wants_vertical, "9:16 video (Shorts, Reels, TikTok)");
            if self.wants_vertical {
                ui.indent("vertical", |ui| {
                    let mut follow_wanted = false;
                    ui.radio_value(&mut self.framing, VerticalFraming::Follow, "Follow the action").on_hover_text("A calm virtual camera that pans toward movement and detail");
                    ui.radio_value(&mut self.framing, VerticalFraming::Centre, "Centre");
                    ui.radio_value(&mut self.framing, VerticalFraming::Manual, "I'll choose where");
                    ui.radio_value(&mut self.framing, VerticalFraming::Blur, "Whole picture on a blurred background");
                    if self.framing == VerticalFraming::Manual {
                        ui.add(egui::Slider::new(&mut self.manual_x, 0.0..=1.0).text("left to right").show_value(false));
                    }
                    if self.framing == VerticalFraming::Blur {
                        self.blur_controls(ui);
                    }
                    if self.framing == VerticalFraming::Follow {
                        if self.action_busy {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Finding the action…");
                            });
                        } else if self.action.is_none() {
                            follow_wanted = ui.button("Find the action now").clicked();
                        } else {
                            ui.label(RichText::new("Action found. The box in the preview shows the crop.").weak().small());
                        }
                    }
                    ui.checkbox(&mut self.show_guide, "Show the 9:16 box in the preview");
                    if follow_wanted {
                        self.follow_action();
                    }
                });
            }
            ui.checkbox(&mut self.wants_thumb, "Thumbnail picture (current frame)");
            ui.checkbox(&mut self.wants_lut, "Colour file (.cube) for other editors");
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Save to");
            let shown = self.out_dir.clone().or_else(|| self.source.as_ref().and_then(|s| s.path.parent().map(PathBuf::from)));
            ui.label(RichText::new(shown.map_or_else(|| "next to the video".to_string(), |d| d.display().to_string())).small());
        });
        if ui.button("Choose folder…").clicked()
            && let Some(dir) = rfd::FileDialog::new().pick_folder()
        {
            self.out_dir = Some(dir);
        }
        ui.add_space(4.0);
        self.encoder_picker(ui, true);
        ui.add_space(6.0);
        if let Some(state) = &self.export {
            ui.label(&state.label);
            ui.add(egui::ProgressBar::new(state.progress).show_percentage());
            if ui.button("Cancel").clicked() {
                state.cancel.store(true, Ordering::Relaxed);
            }
        } else if ui.add_enabled(have, egui::Button::new(RichText::new("Export").strong().size(16.0)).fill(Color32::from_rgb(214, 92, 51)).min_size(vec2(ui.available_width(), 34.0))).clicked() {
            self.start_export();
        }
        if !self.made.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Made:").strong());
            for file in &self.made {
                ui.label(RichText::new(file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())).small());
            }
            if ui.button("Show in folder").clicked()
                && let Some(parent) = self.made[0].parent()
            {
                #[cfg(windows)]
                let program = "explorer";
                #[cfg(target_os = "macos")]
                let program = "open";
                #[cfg(all(not(windows), not(target_os = "macos")))]
                let program = "xdg-open";
                let _ = std::process::Command::new(program).arg(parent).spawn();
            }
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new("Upload pack (titles, description, thumbnail)").default_open(self.pack1.pack.is_some()).show(ui, |ui| self.pack_ui(ui, pack_ui::Target::Single));

        if let Some(scope) = &self.scope {
            ui.add_space(10.0);
            ui.label(RichText::new("Colour levels after grading").weak().small());
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 70.0), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, Color32::from_gray(18));
            for (ch, colour) in [Color32::from_rgb(235, 70, 70), Color32::from_rgb(70, 200, 90), Color32::from_rgb(80, 130, 255)].into_iter().enumerate() {
                let points: Vec<_> = scope[ch].iter().enumerate().map(|(i, v)| pos2(rect.left() + rect.width() * i as f32 / 63.0, rect.bottom() - 3.0 - (rect.height() - 8.0) * v)).collect();
                painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, colour.gamma_multiply(0.85))));
            }
        }
    }

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let Some(duration) = self.source.as_ref().map(|s| s.probe.duration) else {
            ui.label(RichText::new("Sample picture. Open a video to scrub and play.").weak());
            return;
        };
        ui.horizontal(|ui| {
            let playing = self.player.is_some();
            if ui.button(if playing { "Pause" } else { "Play" }).clicked() {
                self.toggle_play();
            }
            ui.label(format!("{} / {}", clock(self.t), clock(duration)));
            ui.label("  ");
            if ui.small_button("Start here").on_hover_text("Export begins at this moment").clicked() {
                self.trim.0 = self.t.min(self.trim.1 - 0.5).max(0.0);
            }
            if ui.small_button("End here").on_hover_text("Export ends at this moment").clicked() {
                self.trim.1 = self.t.max(self.trim.0 + 0.5).min(duration);
            }
            if ui.small_button("Whole video").clicked() {
                self.trim = (0.0, duration);
            }
            ui.label(RichText::new(format!("Exporting {} to {}", clock(self.trim.0), clock(self.trim.1))).weak());
        });
        let width = ui.available_width();
        ui.spacing_mut().slider_width = width - 20.0;
        let mut t = self.t;
        let response = ui.add(egui::Slider::new(&mut t, 0.0..=duration).show_value(false));
        // The selected part is drawn as an orange bar under the slider.
        let bar = ui.allocate_exact_size(vec2(width - 20.0, 4.0), egui::Sense::hover()).0;
        ui.painter().rect_filled(bar, 2.0, Color32::from_gray(50));
        let at = |s: f64| bar.left() + bar.width() * (s / duration.max(0.01)) as f32;
        ui.painter().rect_filled(egui::Rect::from_min_max(pos2(at(self.trim.0), bar.top()), pos2(at(self.trim.1), bar.bottom())), 2.0, Color32::from_rgb(214, 92, 51));
        if response.changed() {
            self.player = None;
            self.t = t;
            self.request_frame(t);
        }
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
        } else if let Some(path) = dropped.into_iter().next() {
            self.open(path);
        }
        if self.mode == Mode::Single && ctx.input(|i| i.key_pressed(egui::Key::Space)) && !ctx.egui_wants_keyboard_input() {
            self.toggle_play();
        }
        self.refresh(&ctx);
        self.autosave(ctx.input(|i| i.time));

        egui::Panel::top("bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Video Enhancer").strong().size(18.0));
                ui.separator();
                for (mode, name) in [(Mode::Single, "Single video"), (Mode::Montage, "Montage")] {
                    if ui.selectable_value(&mut self.mode, mode, name).clicked() {
                        self.player = None;
                        self.status_is_error = false;
                        self.status = match mode {
                            Mode::Montage => "Add several clips (or drop them here), choose your options, then press Plan with AI.".into(),
                            Mode::Single => "Drop a video here, or press Open video.".into(),
                        };
                    }
                }
                if self.mode == Mode::Single {
                    ui.separator();
                    if ui.button("Open video…").clicked()
                        && let Some(path) = rfd::FileDialog::new().add_filter("Video", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "wmv"]).pick_file()
                    {
                        self.open(path);
                    }
                    if self.source.is_some() && ui.button("Use sample picture").clicked() {
                        self.source = None;
                        self.video_fp = None;
                        self.player = None;
                        self.use_sample();
                    }
                    ui.separator();
                    for (mode, name) in [(Compare::Split, "Slide"), (Compare::Side, "Side by side"), (Compare::Before, "Before"), (Compare::After, "After")] {
                        ui.selectable_value(&mut self.compare, mode, name);
                    }
                }
            });
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            let colour = if self.status_is_error { Color32::from_rgb(255, 120, 110) } else { ui.visuals().text_color() };
            ui.label(RichText::new(&self.status).color(colour));
        });
        if self.mode == Mode::Montage {
            egui::CentralPanel::default().show(ui, |ui| self.montage_screen(ui));
        } else {
            egui::Panel::bottom("timeline").show(ui, |ui| self.timeline(ui));
            egui::Panel::left("controls").default_size(330.0).min_size(280.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui));
            });
            egui::Panel::right("export").default_size(250.0).min_size(220.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.export_panel(ui));
            });
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| self.preview(ui));
            });
        }

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
        if self.player.is_some() || self.ai_busy.is_some() || self.export.is_some() || self.mon.working() || self.pack1.working() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
