//! The video enhancer, built on gpui. This is the second front end on `ve-core`: the grading, FFmpeg, AI,
//! thumbnail and saving code is shared with the egui app, and the editing session itself (`ve_core::engine`)
//! is shared too. Only what you see and touch lives here.
//!
//! Ported so far: the window shell, Start, Source, Look, Export and Publish, and the montage (Clips, Plan, Make and its Publish).

mod controls;
mod export;
mod look;
mod montage;
mod publish;
mod settings;
mod theme;

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::TitleBar;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ve_core::engine::{Engine, clock};

use controls::{Knobs, picture, primary_button, secondary_button};
use montage::MontageUi;
use publish::PublishUi;
use settings::SettingsUi;
use ve_core::publish::Target;
use theme::{ACCENT, ACCENT_TEXT, BAD, CARD, CARD_DEEP, INPUT, LINE, LINE_SOFT, LINE_STRONG, OK, ON_ACCENT, PANEL, RAIL_TEXT, RAISED, TEXT, TEXT2, TEXT3, WARN, WINDOW, accent_soft, colour, tint};

// The sizes of the fixed parts of the window. The screens work out where things are from these.
pub const RAIL: f32 = 92.0;
pub const TOP: f32 = 56.0;
pub const STATUS: f32 = 30.0;
pub const INSPECTOR: f32 = 372.0;
pub const TIMELINE: f32 = 168.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Single,
    Montage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Source,
    Look,
    Export,
    Publish,
}

/// How the before and after pictures are shown together.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    After,
    Before,
    Side,
    Slide,
}

/// Something being dragged with the mouse. While it is, the whole window follows the pointer.
#[derive(Clone, Copy, PartialEq)]
pub enum Drag {
    /// The divider between before and after.
    Split,
    TrimIn,
    TrimOut,
    Scrub,
    /// The words on the thumbnail, being moved.
    Words,
    /// The 9:16 picture in the Export step, to move it. Holds where the pointer was last.
    Phone { x: f32, y: f32 },
}

pub struct Shell {
    pub engine: Engine,
    pub knobs: Knobs,
    pub mode: Mode,
    pub step: Step,
    pub compare: Compare,
    pub split: f32,
    pub drag: Option<Drag>,
    pub before_img: Option<Arc<RenderImage>>,
    pub after_img: Option<Arc<RenderImage>>,
    pub look_imgs: Vec<Arc<RenderImage>>,
    seen_view: u64,
    seen_looks: u64,
    pub more_colour: bool,
    pub finish_open: bool,
    pub confirm_omni: bool,
    pub notes: Entity<InputState>,
    pub publish_ui: PublishUi,
    pub montage_ui: MontageUi,
    pub settings_ui: SettingsUi,
    /// The size of the window, remembered from the last draw so mouse positions can be turned into places on screen.
    pub viewport: (f32, f32),
    had_source: bool,
}

impl Shell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine = Engine::new(|| {});
        let notes = cx.new(|cx| InputState::new(window, cx).default_value(engine.settings.style_notes.clone()));
        cx.subscribe(&notes, |this: &mut Shell, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.engine.settings.style_notes = state.read(cx).value().to_string();
            }
        })
        .detach();
        let knobs = Knobs::new(cx);
        let publish_ui = PublishUi::new(window, cx, &engine);
        let montage_ui = MontageUi::new(window, cx);
        let settings_ui = SettingsUi::new(window, cx);
        let mut shell = Self {
            engine,
            knobs,
            mode: Mode::Single,
            step: Step::Source,
            compare: Compare::Slide,
            split: 0.5,
            drag: None,
            before_img: None,
            after_img: None,
            look_imgs: Vec::new(),
            seen_view: 0,
            seen_looks: 0,
            more_colour: false,
            finish_open: false,
            confirm_omni: false,
            notes,
            publish_ui,
            montage_ui,
            settings_ui,
            viewport: (1360.0, 860.0),
            had_source: false,
        };
        // Pick up where the last session left off, as the egui app does.
        if let Some(path) = shell.engine.settings.last_video.clone().filter(|p| p.is_file()) {
            shell.engine.open(path);
        }
        // Everything slow happens on the engine's own threads. This loop takes in what they finished and saves once in a while.
        cx.spawn(async move |this, cx| {
            let mut ticks = 0u32;
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                ticks += 1;
                let alive = this.update(cx, |this, cx| {
                    let mut changed = this.engine.poll();
                    if this.engine.source.is_some() && !this.had_source {
                        this.had_source = true;
                        if this.mode == Mode::Single && this.step == Step::Source {
                            this.step = Step::Look;
                            // Developer aid (debug builds): VE_STEP=export|publish opens that step, for screenshots.
                            #[cfg(debug_assertions)]
                            match std::env::var("VE_STEP").as_deref() {
                                Ok("export") => this.step = Step::Export,
                                Ok("publish") => this.step = Step::Publish,
                                _ => {}
                            }
                        }
                        changed = true;
                    }
                    if ticks % 30 == 0 {
                        this.engine.autosave();
                    }
                    if changed {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        shell
    }

    /// Builds the pictures gpui draws from what the engine holds, only when it changed, and frees the ones it replaces.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.engine.view_gen != self.seen_view {
            self.seen_view = self.engine.view_gen;
            for old in [self.before_img.take(), self.after_img.take()].into_iter().flatten() {
                let _ = window.drop_image(old);
            }
            self.before_img = self.engine.raw.as_ref().map(picture);
            self.after_img = self.engine.graded.as_ref().map(picture);
        }
        if self.engine.looks_gen != self.seen_looks {
            self.seen_looks = self.engine.looks_gen;
            for old in self.look_imgs.drain(..) {
                let _ = window.drop_image(old);
            }
            self.look_imgs = self.engine.look_pictures.iter().map(picture).collect();
        }
        self.knobs.sync(&self.engine, window, cx);
    }

    fn open_path(&mut self, path: std::path::PathBuf) {
        self.engine.open(path);
        self.step = Step::Source;
        self.had_source = false;
    }

    fn choose_video(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    this.open_path(path);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(dir) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    this.engine.settings.out_dir = Some(dir);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn file_name(path: &std::path::Path) -> String {
        path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }
}

pub fn short_gpu(name: &str) -> String {
    name.replace("NVIDIA GeForce ", "").replace("NVIDIA ", "").replace("AMD Radeon ", "Radeon ").replace("(TM)", "").trim().to_string()
}

pub fn clock_short(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:02.0}", (t / 60.0) as u32, t % 60.0)
}

// ------------------------------------------------------------------------------------ pieces

/// One of the three "ready to go" cards on the Start screen.
fn ready_card(title: &str, good: Option<bool>, state: &str, detail: &str) -> Div {
    let hex = match good {
        Some(true) => OK,
        Some(false) => WARN,
        None => TEXT3,
    };
    div()
        .flex_1()
        .min_w(px(0.0))
        .min_h(px(130.0))
        .p(px(16.0))
        .rounded(px(14.0))
        .bg(colour(CARD_DEEP))
        .border_1()
        .border_color(colour(LINE_SOFT))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(div().size(px(26.0)).rounded_full().bg(tint(hex, 0.16)).flex().items_center().justify_center().child(theme::icon(if good == Some(true) { "check" } else { "triangle-alert" }, 14.0, hex)))
                .child(theme::heading(title.to_string(), 14.0))
                .child(div().flex_1().flex().justify_end().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(colour(hex)).child(state.to_string())),
        )
        .child(theme::soft(detail.to_string()))
}

impl Shell {
    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let e = &self.engine;
        let project = match (&self.mode, &e.source) {
            (Mode::Single, Some(s)) => div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(div().text_size(px(14.0)).child(Self::file_name(&s.path)))
                .child(div().font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT3)).child(format!("{}×{} · {:.0} fps · {}", s.probe.width, s.probe.height, s.probe.fps, clock(s.probe.duration)))),
            (Mode::Single, None) => theme::muted("No video yet"),
            (Mode::Montage, _) => div().text_size(px(14.0)).child(if e.montage.title.trim().is_empty() { "New montage".to_string() } else { e.montage.title.clone() }),
        };
        let badge = match &e.gpu_status {
            Some(Ok(name)) => theme::status_pill(if e.colour_on_card() { format!("{} · colour on graphics card", short_gpu(name)) } else { short_gpu(name) }, OK),
            Some(Err(_)) => theme::status_pill("No graphics-card grading", WARN),
            None => theme::status_pill("Checking graphics card", TEXT3),
        };
        let segment = |id: &'static str, label: &'static str, mode: Mode, on: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .occlude()
                .w(px(100.0))
                .h(px(34.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(9.0))
                .text_size(px(13.5))
                .cursor_pointer()
                .when(on, |s| s.bg(colour(RAISED)).text_color(colour(TEXT)).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |s| s.text_color(colour(TEXT2)).hover(|s| s.bg(colour(CARD))))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mode = mode;
                    this.step = Step::Source;
                    this.engine.stop_playing();
                    this.engine.montage.stop_preview();
                    cx.notify();
                }))
        };
        // The window's own title bar: it drags the window, double-click maximises, and the buttons at the right are the
        // system's minimise, maximise and close. Everything else in the bar is ours.
        let content = div()
            .size_full()
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(div().size(px(30.0)).rounded(px(9.0)).bg(colour(ACCENT)).flex().items_center().justify_center().child(theme::icon("play", 15.0, ON_ACCENT)))
                    .child(theme::heading("Video Enhancer", 15.0))
                    .child(div().w(px(1.0)).h(px(22.0)).bg(colour(LINE)))
                    .child(project),
            )
            .child(
                div()
                    .p(px(3.0))
                    .flex()
                    .rounded(px(12.0))
                    .bg(colour(PANEL))
                    .border_1()
                    .border_color(colour(LINE))
                    .child(segment("mode-video", "Video", Mode::Single, self.mode == Mode::Single, cx))
                    .child(segment("mode-montage", "Montage", Mode::Montage, self.mode == Mode::Montage, cx)),
            )
            .child(
                div().flex_1().flex().justify_end().items_center().gap(px(10.0)).child(badge).child(
                    div()
                        .id("open-settings")
                        // On Windows the bar's drag area would take the click as a press on the title bar; this keeps it ours.
                        .occlude()
                        .size(px(34.0))
                        .rounded(px(9.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(colour(CARD)))
                        .child(theme::icon("settings", 18.0, TEXT2))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings_ui.open = true;
                            cx.notify();
                        })),
                ),
            );
        TitleBar::new().h(px(TOP)).bg(colour(WINDOW)).border_color(colour(LINE)).child(content)
    }

    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let labels = match self.mode {
            Mode::Single => ["Source", "Look", "Export", "Publish"],
            Mode::Montage => ["Clips", "Plan", "Make", "Publish"],
        };
        let icons = ["film", "sliders-horizontal", "download", "send"];
        let steps = [Step::Source, Step::Look, Step::Export, Step::Publish];
        let single = self.mode == Mode::Single;
        let m = &self.engine.montage;
        let done = if single {
            [self.engine.source.is_some(), self.step as u8 > Step::Look as u8, !self.engine.made.is_empty(), self.engine.single_pack().is_some()]
        } else {
            [!m.clips.is_empty(), m.plan.is_some(), !m.made.is_empty(), self.engine.montage_pack().is_some()]
        };
        let mut rail = div().w(px(RAIL)).flex_none().flex().flex_col().gap(px(6.0)).px(px(8.0)).py(px(14.0)).border_r_1().border_color(colour(LINE));
        for i in 0..4 {
            let active = self.step == steps[i];
            let hex = if active { ACCENT_TEXT } else { RAIL_TEXT };
            let step = steps[i];
            rail = rail.child(
                div()
                    .id(("rail", i))
                    .relative()
                    .h(px(76.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(7.0))
                    .rounded(px(12.0))
                    .cursor_pointer()
                    .when(active, |s| s.bg(accent_soft(0.13)))
                    .when(!active, |s| s.hover(|s| s.bg(colour(CARD))))
                    .child(theme::icon(icons[i], 22.0, hex))
                    .child(div().text_size(px(12.5)).font_weight(FontWeight::BOLD).text_color(colour(hex)).child(labels[i]))
                    .when(active, |s| s.child(div().absolute().left(px(0.0)).top(px(18.0)).w(px(3.0)).h(px(40.0)).rounded(px(2.0)).bg(colour(ACCENT))))
                    .when(done[i] && !active, |s| s.child(div().absolute().right(px(10.0)).top(px(8.0)).size(px(14.0)).rounded_full().bg(colour(OK)).flex().items_center().justify_center().child(theme::icon("check", 9.0, WINDOW))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.step = step;
                        if step != Step::Look {
                            this.engine.stop_playing();
                        }
                        if !matches!(step, Step::Look | Step::Export) {
                            this.engine.montage.stop_preview();
                        }
                        cx.notify();
                    })),
            );
        }
        rail.child(div().flex_1()).child(div().flex().justify_center().child(theme::muted("Autosaved")))
    }

    fn status_bar(&self) -> impl IntoElement {
        div()
            .h(px(STATUS))
            .px(px(20.0))
            .flex()
            .flex_none()
            .items_center()
            .border_t_1()
            .border_color(colour(LINE))
            .text_size(px(12.5))
            .text_color(colour(if self.engine.status_is_error { BAD } else { TEXT3 }))
            .overflow_hidden()
            .whitespace_nowrap()
            .child(self.engine.status.clone())
    }

    fn start_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let e = &self.engine;
        let tools_ok = e.tools.is_some();
        let gpu_card = match &e.gpu_status {
            Some(Ok(name)) => ready_card("Graphics card", Some(true), "Ready", &format!("{}. Colour grading and encoding both run on the card ({}), about 5 times faster.", short_gpu(name), e.encoder().short())),
            Some(Err(why)) => ready_card("Graphics card", Some(false), "Not used", &format!("Not used: {why}. FFmpeg grades the colour instead.")),
            None => ready_card("Graphics card", None, "Checking", "Checking…"),
        };
        let keys_card = if e.has_openai {
            ready_card("AI keys", Some(true), "Ready", if e.has_gemini { "OpenAI and Gemini keys are saved." } else { "OpenAI key saved. Gemini is optional." })
        } else {
            ready_card("AI keys", Some(false), "OpenAI missing", "Add your own OpenAI key for montage plans and upload packs. Keys stay in Windows Credential Manager.")
        };
        let tools_card = match (tools_ok, e.tools_download) {
            (true, _) => ready_card("Video tools", Some(true), "Installed", "FFmpeg reads, grades and writes your video."),
            (false, Some(p)) => ready_card("Video tools", Some(false), "Downloading", &format!("Getting FFmpeg: {:.0}%", p * 100.0)),
            (false, None) => ready_card("Video tools", Some(false), "Needed", "FFmpeg reads, grades and writes your video. Press Download to get it (about 110 MB, checked for safety)."),
        };
        let mut recent = div().flex().gap(px(14.0));
        for (i, item) in e.settings.recent.iter().take(3).enumerate() {
            let path = item.path.clone();
            recent = recent.child(
                div()
                    .id(("recent", i))
                    .flex_1()
                    .min_w(px(0.0))
                    .h(px(76.0))
                    .p(px(8.0))
                    .flex()
                    .gap(px(12.0))
                    .items_center()
                    .rounded(px(14.0))
                    .bg(colour(CARD_DEEP))
                    .border_1()
                    .border_color(colour(LINE_SOFT))
                    .cursor_pointer()
                    .hover(|s| s.bg(colour(RAISED)))
                    .child(div().w(px(104.0)).h(px(60.0)).flex_none().rounded(px(9.0)).bg(colour(INPUT)).flex().items_center().justify_center().child(theme::icon("film", 22.0, TEXT3)))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(div().text_size(px(14.0)).font_weight(FontWeight::BOLD).whitespace_nowrap().text_ellipsis().overflow_hidden().child(Self::file_name(&item.path)))
                            .child(div().font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT3)).child(format!("{} · {}p{:.0}", clock(item.duration), item.height, item.fps)))
                            .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(colour(OK)).child("Look and AI results saved")),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if path.is_file() {
                            this.open_path(path.clone());
                            cx.notify();
                        }
                    })),
            );
        }
        let drop_zone = div()
            .id("drop-zone")
            .h(px(300.0))
            .rounded(px(20.0))
            .bg(colour(0x141518))
            .border_2()
            .border_color(colour(LINE_STRONG))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.0))
            .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(colour(ACCENT)).bg(accent_soft(0.08)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                let files = paths.paths();
                if files.len() > 1 {
                    this.mode = Mode::Montage;
                    this.step = Step::Source;
                    this.engine.add_clips(files.iter().filter(|p| montage::is_video(p)).cloned().collect());
                } else if let Some(path) = files.first() {
                    this.open_path(path.clone());
                }
                cx.notify();
            }))
            .child(div().size(px(64.0)).rounded(px(18.0)).bg(accent_soft(0.14)).flex().items_center().justify_center().child(theme::icon("upload", 30.0, ACCENT_TEXT)))
            .child(theme::heading("Drop your gameplay here", 28.0))
            .child(div().text_size(px(15.0)).text_color(colour(TEXT2)).child("Drop one video to colour-grade it for YouTube, or several clips to cut a montage."))
            .child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .pt(px(10.0))
                    .child(if tools_ok {
                        primary_button("open-video", "Open a video").w(px(170.0)).on_click(cx.listener(|this, _, _, cx| this.choose_video(cx)))
                    } else {
                        primary_button("download-tools", "Download video tools").w(px(210.0)).on_click(cx.listener(|this, _, _, cx| {
                            this.engine.start_tools_download();
                            cx.notify();
                        }))
                    })
                    .child(secondary_button("start-montage", "Start a montage").w(px(170.0)).on_click(cx.listener(|this, _, _, cx| {
                        this.mode = Mode::Montage;
                        this.step = Step::Source;
                        cx.notify();
                    }))),
            );
        div()
            .id("start-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(1000.0))
                    .mx_auto()
                    .px(px(8.0))
                    .py(px(36.0))
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(drop_zone)
                    .child(div().pt(px(14.0)).flex().items_baseline().gap(px(10.0)).child(theme::heading("Ready to go", 15.0)).child(theme::muted("Checked on this computer")))
                    .child(div().flex().gap(px(14.0)).child(tools_card).child(gpu_card).child(keys_card))
                    .when(!e.settings.recent.is_empty(), |s| {
                        s.child(div().pt(px(12.0)).flex().items_baseline().gap(px(10.0)).child(theme::heading("Pick up where you left off", 15.0)).child(theme::muted("Grades and AI results are saved, so nothing is paid for twice")))
                            .child(recent)
                    }),
            )
    }

    /// The Source step once a video is open: what it is, and a way to swap it.
    fn source_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(source) = &self.engine.source else { return div().into_any_element() };
        let probe = &source.probe;
        let mut notes = div().flex().gap(px(8.0)).pt(px(4.0));
        if probe.hdr {
            notes = notes.child(theme::pill("HDR, converted to standard colour", WARN, tint(WARN, 0.12)));
        }
        if !probe.is_wide() {
            notes = notes.child(theme::pill("Not 16:9, the wide version is cropped", WARN, tint(WARN, 0.12)));
        }
        if probe.has_audio {
            notes = notes.child(theme::pill("Has sound", TEXT2, colour(INPUT)));
        }
        let thumb = div().w(px(220.0)).h(px(124.0)).flex_none().rounded(px(10.0)).overflow_hidden().bg(colour(INPUT)).flex().items_center().justify_center();
        let thumb = match &self.before_img {
            Some(img) => thumb.child(gpui_kit::img(img.clone()).w(px(220.0)).h(px(124.0))),
            None => thumb.child(theme::icon("film", 34.0, TEXT3)),
        };
        div()
            .size_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(760.0))
                    .py(px(36.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(theme::heading("Source", 18.0))
                    .child(
                        theme::card().flex().gap(px(16.0)).child(thumb).child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(theme::heading(Self::file_name(&source.path), 17.0))
                                .child(div().font_family("Consolas").text_size(px(12.5)).text_color(colour(TEXT2)).child(format!("{}×{} · {:.0} fps · {}", probe.width, probe.height, probe.fps, clock(probe.duration))))
                                .child(notes),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .pt(px(6.0))
                            .child(secondary_button("other-video", "Open a different video").on_click(cx.listener(|this, _, _, cx| this.choose_video(cx))))
                            .child(primary_button("to-look", "Continue to Look").on_click(cx.listener(|this, _, _, cx| {
                                this.step = Step::Look;
                                cx.notify();
                            }))),
                    ),
            )
            .into_any_element()
    }

    /// While something is being dragged, every mouse move anywhere in the window steers it.
    fn dragging(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        match (self.drag, self.step) {
            (Some(Drag::Split), Step::Look) => self.drag_split(x),
            (Some(Drag::TrimIn | Drag::TrimOut | Drag::Scrub), Step::Look) => self.drag_timeline(x),
            (Some(Drag::Words), Step::Publish) => self.drag_words(x, y),
            (Some(Drag::Phone { x: x0, y: y0 }), Step::Export) => self.drag_phone(x - x0, y - y0, x, y),
            _ => return,
        }
        cx.notify();
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        self.viewport = (f32::from(size.width), f32::from(size.height));
        self.sync(window, cx);
        // Publish is about the montage on the montage's own steps, and about the single video everywhere else.
        self.engine.set_target(if self.mode == Mode::Montage && self.step == Step::Publish { Target::Montage } else { Target::Single });
        self.sync_publish(window, cx);
        self.sync_montage(window, cx);
        let content = match (self.mode, self.step) {
            (Mode::Single, Step::Source) if self.engine.source.is_none() => self.start_screen(cx).into_any_element(),
            (Mode::Single, Step::Source) => self.source_screen(cx).into_any_element(),
            (Mode::Single, Step::Look) => self.look_screen(cx).into_any_element(),
            (Mode::Single, Step::Export) => self.export_screen(cx).into_any_element(),
            (Mode::Single, Step::Publish) => self.publish_screen(cx).into_any_element(),
            (Mode::Montage, Step::Publish) => self.publish_screen(cx).into_any_element(),
            (Mode::Montage, _) => self.montage_screen(cx).into_any_element(),
        };
        div()
            .size_full()
            .bg(colour(WINDOW))
            .text_color(colour(TEXT))
            .text_size(px(14.0))
            .flex()
            .flex_col()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    this.dragging(event.position, cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| {
                if let Some(drag) = this.drag.take() {
                    this.finish_drag(drag);
                    cx.notify();
                }
            }))
            .child(self.top_bar(cx))
            .child(div().flex_1().min_h(px(0.0)).flex().child(self.rail(cx)).child(div().flex_1().min_w(px(0.0)).child(content)))
            .child(self.status_bar())
            .child(self.toast(cx))
            .when(self.confirm_omni, |s| s.child(self.omni_dialog(cx)))
            .when(self.settings_ui.open, |s| s.child(self.settings_dialog(cx)))
            .when(self.publish_ui.confirm_ai, |s| s.child(self.ai_thumb_dialog(cx)))
    }
}

fn main() {
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(|cx| {
        gpui_kit::init(cx);
        theme::apply(cx);
        // The title bar is drawn by the app (see `top_bar`), so the system's is left out.
        let mut titlebar = TitleBar::title_bar_options();
        titlebar.title = Some("Video Enhancer".into());
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1360.0), px(860.0)), cx))),
            window_min_size: Some(size(px(1000.0), px(640.0))),
            titlebar: Some(titlebar),
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Shell::new(window, cx))).expect("failed to open the window");
        cx.activate(true);
    });
}
