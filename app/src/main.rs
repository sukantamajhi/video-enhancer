#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The egui front end. The editing session itself (opening a video, grading, playing, the AI, exporting, the montage,
//! the upload pack, saving) is `ve_core::engine::Engine`, shared with the gpui front end. This file holds the window:
//! what is on screen, the pictures egui draws from the engine's frames, and the dialogs.

mod montage_ui;
mod pack_ui;
mod screens;
mod theme;

// The shared logic lives in the core crate. These names keep every `crate::media::...` path working.
use ve_core::engine::{Engine, VerticalFraming};
use ve_core::publish::Target;
use ve_core::{ai, grade, media, montage, pack, store, thumb_ai};

use std::path::PathBuf;

use eframe::egui;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Single,
    Montage,
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

struct App {
    engine: Engine,
    mode: Mode,
    step: Step,
    compare: Compare,
    split: f32,
    /// The pictures egui draws, made from the engine's frames when the engine says they changed.
    before_tex: Option<egui::TextureHandle>,
    after_tex: Option<egui::TextureHandle>,
    look_tex: Vec<egui::TextureHandle>,
    seen_view: u64,
    seen_looks: u64,
    seen_source: Option<PathBuf>,
    pack_ui: pack_ui::PackUi,
    mon_ui: montage_ui::MontageUi,
    gemini_key: String,
    openai_key: String,
    confirm_omni: bool,
    confirm_ai_thumb: bool,
    show_settings: bool,
    more_colour_open: bool,
    finish_open: bool,
    last_save: f64,
    shot: Option<(PathBuf, u32)>,
    /// Developer aid: start the AI thumbnails as soon as there is a pack to design them from.
    auto_ai: bool,
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
        let waker = ctx.clone();
        let mut engine = Engine::new(move || waker.request_repaint());
        engine.use_sample();
        // Things that were remembered but are gone are left out.
        engine.settings.out_dir = engine.settings.out_dir.take().filter(|d| d.is_dir());
        engine.status = if engine.tools.is_some() {
            "Drop a video here, or press Open a video.".into()
        } else {
            "The video tools aren't installed yet. Press the download button on the left to get them (one time, about 110 MB).".into()
        };
        let mode = if std::env::var_os("VE_MONTAGE").is_some() || engine.settings.montage { Mode::Montage } else { Mode::Single };
        let mut app = Self {
            engine,
            mode,
            step: Step::Source,
            compare: Compare::Split,
            split: 0.5,
            before_tex: None,
            after_tex: None,
            look_tex: Vec::new(),
            seen_view: 0,
            seen_looks: 0,
            seen_source: None,
            pack_ui: pack_ui::PackUi::default(),
            mon_ui: montage_ui::MontageUi::default(),
            gemini_key: String::new(),
            openai_key: String::new(),
            confirm_omni: false,
            confirm_ai_thumb: false,
            show_settings: false,
            more_colour_open: false,
            finish_open: false,
            last_save: 0.0,
            shot: None,
            auto_ai: std::env::var_os("VE_AUTO_AI").is_some(),
        };
        if let (Some(path), Ok(n)) = (std::env::var_os("VE_SCREENSHOT"), std::env::var("VE_SCREENSHOT_FRAMES")) {
            app.shot = Some((path.into(), n.parse().unwrap_or(12)));
        }
        if let Some(path) = app.engine.settings.last_video.clone().filter(|p| p.is_file()) {
            app.engine.open(path);
        }
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
        // Developer aid: VE_CONFIRM_AI=1 opens the "design with OpenAI?" question, for screenshots.
        app.confirm_ai_thumb = std::env::var_os("VE_CONFIRM_AI").is_some();
        app
    }

    /// Which video the Publish step is about: the montage's own pack on the montage's Publish step, the single video's otherwise.
    fn target(&self) -> Target {
        if self.mode == Mode::Montage && self.step == Step::Publish { Target::Montage } else { Target::Single }
    }

    /// Writes anything that changed, at most once a second.
    fn autosave(&mut self, now: f64) {
        if now - self.last_save < 1.0 {
            return;
        }
        self.last_save = now;
        self.engine.settings.montage = self.mode == Mode::Montage;
        self.engine.autosave();
    }

    pub fn switch_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.engine.stop_playing();
        self.engine.montage.stop_preview();
        self.step = Step::Source;
        self.engine.status_is_error = false;
        self.engine.status = match mode {
            Mode::Montage => "Add several clips (or drop them here), choose your options, then plan with AI.".into(),
            Mode::Single => "Drop a video here, or press Open a video.".into(),
        };
    }

    /// Turns the engine's frames into textures, whenever the engine says the picture changed.
    fn sync_textures(&mut self, ctx: &egui::Context) {
        let e = &self.engine;
        let options = egui::TextureOptions::LINEAR;
        if e.view_gen != self.seen_view {
            self.seen_view = e.view_gen;
            for (frame, slot, name) in [(&e.raw, &mut self.before_tex, "before"), (&e.graded, &mut self.after_tex, "after")] {
                let Some(frame) = frame else { continue };
                let size = [frame.width, frame.height];
                let image = egui::ColorImage::from_rgb(size, &frame.rgb);
                match slot {
                    Some(tex) if tex.size() == size => tex.set(image, options),
                    slot => *slot = Some(ctx.load_texture(name, image, options)),
                }
            }
        }
        if e.looks_gen != self.seen_looks {
            self.seen_looks = e.looks_gen;
            self.look_tex = e.look_pictures.iter().enumerate().map(|(i, f)| ctx.load_texture(format!("look-{i}"), egui::ColorImage::from_rgb([f.width, f.height], &f.rgb), options)).collect();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.engine.poll();
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if self.mode == Mode::Montage {
            if !dropped.is_empty() {
                self.engine.add_clips(dropped);
            }
        } else if dropped.len() > 1 {
            // Several clips at once can only mean a montage.
            self.switch_mode(Mode::Montage);
            self.engine.add_clips(dropped);
        } else if let Some(path) = dropped.into_iter().next() {
            self.engine.open(path);
        }
        // A video that has just opened takes you to the Look step, as long as you were still on Source.
        let now_open = self.engine.source.as_ref().map(|s| s.path.clone());
        if now_open != self.seen_source {
            if now_open.is_some() && self.mode == Mode::Single && self.step == Step::Source {
                self.step = Step::Look;
            }
            self.seen_source = now_open;
        }
        if self.mode == Mode::Single && ctx.input(|i| i.key_pressed(egui::Key::Space)) && !ctx.egui_wants_keyboard_input() {
            self.engine.toggle_play();
        }
        self.engine.set_target(self.target());
        self.sync_textures(&ctx);
        self.sync_montage(&ctx);
        self.autosave(ctx.input(|i| i.time));
        // Developer aid: VE_AUTO_AI=1 starts the AI thumbnails once there is a pack, for testing against a stand-in server.
        if self.auto_ai && self.step == Step::Publish && self.engine.publish.pack.is_some() {
            self.auto_ai = false;
            self.engine.start_ai_thumbnails();
        }

        if self.mode != Mode::Montage || !matches!(self.step, Step::Look | Step::Export) {
            self.engine.montage.stop_preview();
        }
        self.shell(ui);
        match (self.mode, self.step) {
            (Mode::Single, Step::Source) if self.engine.source.is_none() => {
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
            (_, Step::Publish) => self.publish_screen(ui),
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
                        self.engine.run_omni();
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_omni = false;
                    }
                });
            });
        }

        if self.confirm_ai_thumb {
            egui::Window::new("Design the thumbnails with OpenAI?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(&ctx, |ui| {
                ui.set_width(470.0);
                ui.label(theme::soft("This sends these to OpenAI, using your key:"));
                ui.label(theme::soft("  •  the frame you picked from your video, as a 16:9 picture and a 9:16 picture"));
                ui.label(theme::soft("  •  the words for the thumbnail and the game or topic"));
                ui.label(theme::soft("  •  anything you typed under \"Anything to show off\""));
                ui.add_space(4.0);
                let s = &self.engine.settings;
                ui.label(theme::soft(format!("Model: {}. Quality: {}.", thumb_ai::IMAGE_MODELS[s.thumb_model.min(1)].1, thumb_ai::QUALITIES[s.thumb_quality.min(2)].1)));
                ui.label(theme::soft("OpenAI charges for each of the two pictures, by size and quality (image output costs $30 per million tokens). The app shows the real cost afterwards, taken from OpenAI's own usage report."));
                ui.label(theme::soft("If the lettering comes out misspelled, one more try is made automatically, and it is charged too. Asking again with nothing changed reuses the saved pictures and costs nothing."));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if theme::button(ui, "Send it", theme::Kind::Primary, 0.0).clicked() {
                        self.confirm_ai_thumb = false;
                        self.engine.start_ai_thumbnails();
                    }
                    if theme::button(ui, "Cancel", theme::Kind::Secondary, 0.0).clicked() {
                        self.confirm_ai_thumb = false;
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
        let e = &self.engine;
        if e.playing() || e.ai_busy.is_some() || e.export.is_some() || e.montage.working() || e.publish_working() || e.tools_download.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
