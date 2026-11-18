//! The montage: Clips, Plan and Make, with the summary panel on the right and a preview of the moments the AI chose.
//! The logic (probing clips, planning, music, rendering, the preview player) is `ve_core::montage_engine`; this is the look of it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ve_core::ai;
use ve_core::grade::LOOKS;
use ve_core::media::BlurLayout;
use ve_core::montage::{Cut, Sound, Style, Vertical};
use ve_core::montage_engine::MusicSource;

use crate::controls::{Knob, checkbox, disabled, fit, picture, primary_button, secondary_button, segmented, small_button};
use crate::export::{encoder_choices, show_in_folder};
use crate::theme::{self, ACCENT, ACCENT_TEXT, CARD, INPUT, LINE, LINE_SOFT, LINE_STRONG, OK, ON_ACCENT, PANEL, TEXT, TEXT2, TEXT3, WARN, accent_soft, colour, tint};
use crate::{INSPECTOR, Mode, RAIL, Shell, Step, clock_short};

const SIDE: f32 = 28.0;
const PREVIEW_H: f32 = 330.0;
const VIDEO_TYPES: [&str; 8] = ["mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "wmv"];

/// One colour per clip, so a moment can be matched to the clip it came from.
const CLIP_COLOURS: [u32; 8] = [0xFF6B3D, 0x4FB3FF, 0x62D08F, 0xF4BE4F, 0xB58CFF, 0xFF7BAC, 0x4FD6D0, 0xC7D45A];

fn clip_colour(i: usize) -> u32 {
    CLIP_COLOURS[i % CLIP_COLOURS.len()]
}

pub fn is_video(path: &Path) -> bool {
    path.extension().is_some_and(|e| VIDEO_TYPES.iter().any(|t| e.eq_ignore_ascii_case(t)))
}

/// The text boxes and pictures of the montage screens.
pub struct MontageUi {
    title: Entity<InputState>,
    mood: Entity<InputState>,
    /// What the boxes held when the engine last told them, so a change from either side is noticed once.
    last_title: String,
    last_mood: String,
    thumbs: HashMap<PathBuf, Arc<RenderImage>>,
    preview: Option<Arc<RenderImage>>,
    seen_thumbs: u64,
    seen_shown: u64,
    /// Whether the preview shows the 9:16 picture rather than the 16:9 one.
    phone: bool,
}

impl MontageUi {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Shown at the start of the story"));
        cx.subscribe(&title, |this: &mut Shell, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let text = state.read(cx).value().to_string();
                this.montage_ui.last_title = text.clone();
                this.engine.montage.title = text;
                cx.notify();
            }
        })
        .detach();
        let mood = cx.new(|cx| InputState::new(window, cx).placeholder("for example energetic electronic"));
        cx.subscribe(&mood, |this: &mut Shell, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let text = state.read(cx).value().to_string();
                this.montage_ui.last_mood = text.clone();
                this.engine.montage.music_query = text;
            }
        })
        .detach();
        Self { title, mood, last_title: String::new(), last_mood: String::new(), thumbs: HashMap::new(), preview: None, seen_thumbs: 0, seen_shown: 0, phone: false }
    }
}

// ----------------------------------------------------------------------------------- small pieces

fn chip(id: impl Into<ElementId>, label: &str, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(32.0))
        .px(px(13.0))
        .flex()
        .items_center()
        .rounded(px(16.0))
        .text_size(px(13.0))
        .cursor_pointer()
        .when(on, |s| s.bg(accent_soft(0.16)).border_1().border_color(colour(ACCENT)).text_color(colour(TEXT)).font_weight(FontWeight::SEMIBOLD))
        .when(!on, |s| s.bg(colour(INPUT)).border_1().border_color(colour(LINE_STRONG)).text_color(colour(TEXT2)).hover(|s| s.bg(colour(0x2B2C32))))
        .child(label.to_string())
}

/// A big choice with a title and a line under it.
fn choice_card(id: impl Into<ElementId>, title: &str, blurb: &str, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .min_w(px(0.0))
        .p(px(14.0))
        .rounded(px(12.0))
        .cursor_pointer()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .when(on, |s| s.bg(accent_soft(0.12)).border_2().border_color(colour(ACCENT)))
        .when(!on, |s| s.bg(colour(CARD)).border_1().border_color(colour(LINE_SOFT)).hover(|s| s.bg(colour(0x23242A))))
        .child(theme::heading(title.to_string(), 14.5))
        .child(theme::muted(blurb.to_string()))
}

fn summary_row(label: &str, value: String) -> Div {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(px(12.0))
        .py(px(5.0))
        .child(div().flex_none().text_size(px(13.0)).text_color(colour(TEXT3)).child(label.to_string()))
        .child(div().min_w(px(0.0)).text_size(px(13.0)).text_color(colour(TEXT)).text_right().child(value))
}

fn dot(hex: u32) -> Div {
    div().size(px(11.0)).flex_none().rounded_full().bg(colour(hex))
}

fn clock(t: f64) -> String {
    clock_short(t)
}

/// The 9:16 picture as the montage will frame it. Follow is shown centred, because it is only worked out while rendering.
fn phone_picture(img: &Arc<RenderImage>, aspect: f32, ph: f32, vertical: Vertical, blur: BlurLayout) -> Div {
    let pw = ph * 9.0 / 16.0;
    let screen = div().relative().w(px(pw)).h(px(ph)).flex_none().rounded(px(16.0)).overflow_hidden().bg(colour(INPUT));
    if vertical == Vertical::Blur {
        let b = blur.clamped();
        let (cover_w, band_w) = (ph * aspect, pw * b.zoom);
        let band_h = band_w / aspect;
        screen
            .child(gpui_kit::img(img.clone()).absolute().left(px(-(cover_w - pw) / 2.0)).top(px(0.0)).w(px(cover_w)).h(px(ph)))
            .child(div().absolute().top(px(0.0)).left(px(0.0)).size_full().bg(gpui_kit::black().opacity(0.62)))
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px((ph - band_h) * b.y))
                    .w(px(pw))
                    .h(px(band_h))
                    .overflow_hidden()
                    .child(gpui_kit::img(img.clone()).absolute().left(px(-(band_w - pw) * b.x)).top(px(0.0)).w(px(band_w)).h(px(band_h))),
            )
    } else {
        let full_w = ph * aspect;
        screen.child(gpui_kit::img(img.clone()).absolute().left(px(-(full_w - pw).max(0.0) * 0.5)).top(px(0.0)).w(px(full_w.max(pw))).h(px(ph)))
    }
}

// ------------------------------------------------------------------------------------- the shell

impl Shell {
    /// How wide the middle column is, for the pictures and bars that are drawn to fit it.
    fn montage_width(&self) -> f32 {
        (self.viewport.0 - RAIL - INSPECTOR - 2.0 * SIDE).max(400.0)
    }

    pub fn choose_clips(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                this.update(cx, |this, cx| {
                    this.engine.add_clips(paths.into_iter().filter(|p| is_video(p)).collect());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn choose_music(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(file) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    this.engine.montage.file = Some(file);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn choose_montage_folder(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(dir) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    this.engine.montage.out_dir = Some(dir);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Builds the pictures gpui draws from what the montage engine holds, when they changed, and keeps the text boxes in step.
    pub fn sync_montage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let m = &self.engine.montage;
        let ui = &mut self.montage_ui;
        if m.thumbs_gen != ui.seen_thumbs {
            ui.seen_thumbs = m.thumbs_gen;
            for (_, old) in ui.thumbs.drain() {
                let _ = window.drop_image(old);
            }
            ui.thumbs = m.thumbs.iter().map(|(path, frame)| (path.clone(), picture(frame))).collect();
        }
        if m.preview.shown != ui.seen_shown {
            ui.seen_shown = m.preview.shown;
            if let Some(old) = ui.preview.take() {
                let _ = window.drop_image(old);
            }
            ui.preview = m.preview.frame.as_ref().map(picture);
        }
        if m.title != ui.last_title {
            ui.last_title = m.title.clone();
            let text = m.title.clone();
            ui.title.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        if m.music_query != ui.last_mood {
            ui.last_mood = m.music_query.clone();
            let text = m.music_query.clone();
            ui.mood.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        // The first time the plan is on screen, show its first moment.
        if self.mode == Mode::Montage && matches!(self.step, Step::Look | Step::Export) {
            let m = &self.engine.montage;
            if m.plan.is_some() && m.preview.frame.is_none() && !m.preview.busy && !m.preview.started
                && let Some(first) = m.kept().first().copied()
            {
                self.engine.preview_select(first);
            }
        }
    }

    pub fn montage_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, body) = match self.step {
            Step::Source => ("montage-clips", self.clips_step(cx).into_any_element()),
            Step::Look => ("montage-plan", self.plan_step(cx).into_any_element()),
            _ => ("montage-make", self.make_step(cx).into_any_element()),
        };
        div()
            .id("montage-drop")
            .size_full()
            .flex()
            .drag_over::<ExternalPaths>(|s, _, _, _| s.bg(accent_soft(0.05)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.engine.add_clips(paths.paths().iter().filter(|p| is_video(p)).cloned().collect());
                this.step = Step::Source;
                cx.notify();
            }))
            .child(div().id(id).flex_1().min_w(px(0.0)).h_full().overflow_y_scroll().child(div().px(px(SIDE)).pt(px(16.0)).pb(px(24.0)).flex().flex_col().gap(px(12.0)).child(body)))
            .child(self.montage_summary(cx))
    }

    // ------------------------------------------------------------------------------------ clips

    fn clip_tiles(&self, cx: &mut Context<Self>) -> Div {
        let m = &self.engine.montage;
        let width = ((self.montage_width() - 28.0) / 3.0).floor();
        let height = (width * 9.0 / 16.0).floor();
        let count = m.clips.len();
        let mut grid = div().flex().flex_wrap().gap(px(14.0));
        for (i, clip) in m.clips.iter().enumerate() {
            let picture = div().relative().w(px(width)).h(px(height)).flex_none().rounded(px(12.0)).overflow_hidden().bg(colour(INPUT)).flex().items_center().justify_center();
            let picture = match self.montage_ui.thumbs.get(&clip.path) {
                Some(img) => picture.child(gpui_kit::img(img.clone()).w(px(width)).h(px(height))),
                None => picture.child(theme::icon("film", 28.0, TEXT3)),
            };
            let picture = picture
                .child(div().absolute().left(px(10.0)).top(px(10.0)).size(px(26.0)).rounded_full().bg(colour(ACCENT)).flex().items_center().justify_center().text_size(px(13.0)).font_weight(FontWeight::BOLD).text_color(colour(ON_ACCENT)).child(format!("{}", i + 1)))
                .child(div().absolute().right(px(10.0)).bottom(px(10.0)).px(px(8.0)).py(px(3.0)).rounded(px(6.0)).bg(gpui_kit::black().opacity(0.74)).font_family("Consolas").text_size(px(12.0)).child(clock(clip.probe.duration)));
            let mut buttons = div().flex().gap(px(6.0));
            if i > 0 {
                buttons = buttons.child(small_button(("clip-earlier", i), "Earlier").on_click(cx.listener(move |this, _, _, cx| {
                    this.engine.move_clip(i, -1);
                    cx.notify();
                })));
            }
            if i + 1 < count {
                buttons = buttons.child(small_button(("clip-later", i), "Later").on_click(cx.listener(move |this, _, _, cx| {
                    this.engine.move_clip(i, 1);
                    cx.notify();
                })));
            }
            buttons = buttons.child(small_button(("clip-remove", i), "Remove").on_click(cx.listener(move |this, _, _, cx| {
                this.engine.remove_clip(i);
                cx.notify();
            })));
            grid = grid.child(
                div()
                    .w(px(width))
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(picture)
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::BOLD).whitespace_nowrap().overflow_hidden().text_ellipsis().child(clip.name()))
                    .child(div().font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT3)).child(format!("{}×{}", clip.probe.width, clip.probe.height)))
                    .child(buttons),
            );
        }
        let reading = m.probing > 0;
        grid.child(
            div()
                .id("add-clips")
                .w(px(width))
                .h(px(height))
                .rounded(px(12.0))
                .bg(colour(0x141518))
                .border_2()
                .border_color(colour(LINE_STRONG))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .cursor_pointer()
                .hover(|s| s.bg(colour(CARD)))
                .child(theme::icon("plus", 26.0, ACCENT_TEXT))
                .child(div().text_size(px(14.0)).font_weight(FontWeight::BOLD).child(if reading { "Reading clips…" } else { "Add clips" }))
                .on_click(cx.listener(|this, _, _, cx| this.choose_clips(cx))),
        )
    }

    fn length_card(&self, cx: &mut Context<Self>) -> Div {
        let target = self.engine.montage.target;
        let mut chips = div().flex().flex_wrap().gap(px(8.0));
        for (i, (label, secs)) in [("30s", 30.0), ("1 min", 60.0), ("2 min", 120.0), ("3 min", 180.0), ("5 min", 300.0)].into_iter().enumerate() {
            chips = chips.child(chip(("length", i), label, (target - secs).abs() < 0.5).on_click(cx.listener(move |this, _, _, cx| {
                this.engine.montage.target = secs;
                cx.notify();
            })));
        }
        theme::card()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(theme::heading("Length", 14.0))
            .child(theme::muted("About how long the finished montage should be"))
            .child(chips)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(theme::soft("Exactly"))
                    .child(small_button("length-less", "−10 s").on_click(cx.listener(|this, _, _, cx| {
                        this.engine.montage.target = (this.engine.montage.target - 10.0).clamp(10.0, 900.0);
                        cx.notify();
                    })))
                    .child(div().w(px(90.0)).text_center().font_family("Consolas").text_size(px(13.0)).child(format!("{target:.0} seconds")))
                    .child(small_button("length-more", "+10 s").on_click(cx.listener(|this, _, _, cx| {
                        this.engine.montage.target = (this.engine.montage.target + 10.0).clamp(10.0, 900.0);
                        cx.notify();
                    }))),
            )
    }

    fn look_card(&self, cx: &mut Context<Self>) -> Div {
        let look = self.engine.montage.look;
        let mut chips = div().flex().flex_wrap().gap(px(8.0)).child(chip("look-none", "Natural (fix only)", look.is_none()).on_click(cx.listener(|this, _, _, cx| {
            this.engine.montage.look = None;
            cx.notify();
        })));
        for (i, l) in LOOKS.iter().enumerate() {
            chips = chips.child(chip(("look", i), l.name, look == Some(i)).on_click(cx.listener(move |this, _, _, cx| {
                this.engine.montage.look = Some(i);
                cx.notify();
            })));
        }
        theme::card()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().flex().items_baseline().gap(px(10.0)).child(theme::heading("Colour look", 14.0)).child(theme::muted("Each clip is corrected on its own, then given this look")))
            .child(chips)
            .child(div().pt(px(4.0)).child(theme::heading("Anything special?", 14.0)))
            .child(Input::new(&self.notes))
    }

    fn music_card(&self, cx: &mut Context<Self>) -> Div {
        let m = &self.engine.montage;
        let mut card = theme::card()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().flex().items_center().gap(px(8.0)).child(theme::icon("music", 16.0, ACCENT_TEXT)).child(theme::heading("Music", 14.0)))
            .child(div().flex().child(segmented("music-source", &[(MusicSource::Online, "NCS music (no copyright claims)"), (MusicSource::File, "Use my own file")], m.source, false, cx, |this, v| this.engine.montage.source = v)));
        match m.source {
            MusicSource::Online => {
                card = card.child(theme::muted("Music from NCS (NoCopyrightSounds), made for creators: free in monetised YouTube and Twitch videos with no copyright strikes. The AI chooses the mood. NCS asks you to credit the song in your description, so the exact credit text is saved next to the montage and added to your upload pack."));
                if m.plan.is_some() {
                    let search = {
                        let b = small_button("music-search", "Search again");
                        if m.searching { disabled(b) } else { b.on_click(cx.listener(|this, _, _, cx| {
                            this.engine.search_music();
                            cx.notify();
                        })) }
                    };
                    card = card.child(div().flex().items_center().gap(px(10.0)).child(theme::soft("Mood")).child(div().w(px(240.0)).child(Input::new(&self.montage_ui.mood))).child(search).when(m.searching, |s| s.child(theme::muted("Searching…"))));
                    let mut list = div().flex().flex_col().gap(px(6.0));
                    for (i, t) in m.tracks.iter().enumerate().take(8) {
                        let on = m.track == i;
                        list = list.child(
                            div()
                                .id(("track", i))
                                .px(px(12.0))
                                .py(px(8.0))
                                .rounded(px(10.0))
                                .cursor_pointer()
                                .when(on, |s| s.bg(accent_soft(0.10)).border_2().border_color(colour(ACCENT)))
                                .when(!on, |s| s.bg(colour(INPUT)).border_1().border_color(colour(LINE_STRONG)).hover(|s| s.bg(colour(0x2B2C32))))
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(div().flex().items_center().justify_between().gap(px(10.0)).child(div().min_w(px(0.0)).text_size(px(13.0)).font_weight(FontWeight::BOLD).whitespace_nowrap().overflow_hidden().text_ellipsis().child(t.title.clone())).child(theme::pill(if t.instrumental { "Instrumental" } else { "With vocals" }, TEXT2, colour(CARD))))
                                .child(theme::muted(format!("{} · {}", t.creator, t.source)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.engine.montage.track = i;
                                    cx.notify();
                                })),
                        );
                    }
                    card = card.child(list);
                } else {
                    card = card.child(theme::soft("After planning, songs that fit your montage appear here."));
                }
            }
            MusicSource::File => {
                let shown = m.file.as_ref().map_or_else(|| "No file chosen".to_string(), |f| f.display().to_string());
                card = card
                    .child(div().flex().items_center().gap(px(12.0)).child(secondary_button("choose-music", "Choose music file").on_click(cx.listener(|this, _, _, cx| this.choose_music(cx)))).child(div().flex_1().min_w(px(0.0)).font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT2)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(shown)))
                    .child(theme::muted("Make sure you have the right to use this song in your video."));
            }
        }
        card
    }

    fn clips_step(&self, cx: &mut Context<Self>) -> Div {
        let m = &self.engine.montage;
        let mut styles = div().flex().gap(px(14.0));
        for (i, style) in Style::ALL.into_iter().enumerate() {
            styles = styles.child(choice_card(("style", i), style.name(), style.blurb(), m.style == style).on_click(cx.listener(move |this, _, _, cx| {
                this.engine.montage.style = style;
                cx.notify();
            })));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(div().h(px(36.0)).flex().items_center().gap(px(12.0)).child(theme::heading("Clips", 18.0)).child(theme::soft("Drop several clips anywhere in this window. The order is the order they play in.")))
            .child(self.clip_tiles(cx))
            .child(div().pt(px(6.0)).child(theme::heading("Style", 15.0)))
            .child(styles)
            .child(
                div().flex().gap(px(14.0)).child(self.length_card(cx)).child(
                    theme::card().flex_1().min_w(px(0.0)).flex().flex_col().gap(px(8.0)).child(theme::heading("Sound", 14.0)).child(theme::muted(m.sound.name())).child(segmented("sound", &[(Sound::MusicAndOriginal, "Music + game"), (Sound::OriginalOnly, "Game only"), (Sound::MusicOnly, "Music only")], m.sound, true, cx, |this, v| this.engine.montage.sound = v)),
                ),
            )
            .child(self.look_card(cx))
            .when(m.sound.uses_music(), |s| s.child(self.music_card(cx)))
    }

    // -------------------------------------------------------------------------------------- plan

    fn plan_step(&self, cx: &mut Context<Self>) -> Div {
        let e = &self.engine;
        let m = &e.montage;
        let working = m.busy.is_some() || m.render.is_some();
        let ready = !m.clips.is_empty() && !working && e.has_openai;
        let plan_button = {
            let b = secondary_button("plan-again", if m.plan.is_some() { "Plan again" } else { "Plan with AI" });
            if ready { b.on_click(cx.listener(|this, _, _, cx| {
                this.engine.plan_montage();
                cx.notify();
            })) } else { disabled(b) }
        };
        let models: Vec<(usize, &'static str)> = ai::OPENAI_MODELS.iter().enumerate().map(|(i, (_, name))| (i, *name)).collect();
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(div().h(px(36.0)).flex().items_center().gap(px(12.0)).child(theme::heading("Plan", 18.0)).child(theme::soft("The moments the AI chose, in play order. Untick any you don't want.")))
            .child(
                theme::card()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(div().flex().items_center().gap(px(8.0)).child(theme::icon("sparkles", 16.0, ACCENT_TEXT)).child(theme::heading("AI plan", 14.0)).child(theme::muted("uses your own OpenAI key")))
                    .child(div().flex().child(segmented("plan-model", &models, e.settings.openai_model, false, cx, |this, v| this.engine.settings.openai_model = v)))
                    .child(div().flex().items_center().gap(px(12.0)).child(plan_button).when_some(m.busy_text(), |s, text| s.child(theme::soft(text))))
                    .child(checkbox("reuse-ai", "Reuse saved AI answers when nothing changed (no tokens spent)", e.settings.reuse_ai, cx, |this| this.engine.settings.reuse_ai = !this.engine.settings.reuse_ai))
                    .when(!e.has_openai, |c| c.child(self.key_hint("Needs your OpenAI key.", "plan-key", cx))),
            );
        let Some(plan) = &m.plan else {
            if m.clips.is_empty() {
                page = page.child(theme::muted("Add clips on the first step, then plan."));
            }
            return page;
        };
        page = page.child(self.preview_card(cx)).child(theme::muted(format!("Tempo the AI suggests: {}", plan.tempo)));
        if m.style == Style::Story {
            page = page.child(theme::card().flex().flex_col().gap(px(8.0)).child(theme::heading("Title shown at the start", 14.0)).child(Input::new(&self.montage_ui.title)));
        }
        let mut moments = theme::card().flex().flex_col().gap(px(6.0)).child(theme::heading("Moments", 14.0));
        for (i, cut) in plan.cuts.iter().enumerate() {
            let on = m.keep.get(i).copied().unwrap_or(true);
            moments = moments.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(checkbox(("keep", i), "", on, cx, move |this| {
                        if let Some(k) = this.engine.montage.keep.get_mut(i) {
                            *k = !*k;
                        }
                    }))
                    .child(dot(clip_colour(cut.clip)))
                    .child(div().flex_none().font_family("Consolas").text_size(px(12.5)).text_color(colour(TEXT2)).child(format!("{} to {}", clock(cut.start), clock(cut.end))))
                    .child(div().flex_none().w(px(40.0)).font_family("Consolas").text_size(px(12.5)).text_color(colour(if cut.score > 0.7 { OK } else { TEXT3 })).child(format!("{:.0}%", cut.score * 100.0)))
                    .child(small_button(("view-moment", i), "View").on_click(cx.listener(move |this, _, _, cx| {
                        this.engine.preview_select(i);
                        cx.notify();
                    })))
                    .child(div().flex_1().min_w(px(0.0)).text_size(px(13.0)).text_color(colour(TEXT2)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(cut.why.clone())),
            );
        }
        page.child(moments)
    }

    // ---------------------------------------------------------------------------------- preview

    fn preview_card(&self, cx: &mut Context<Self>) -> Div {
        let e = &self.engine;
        let m = &e.montage;
        let Some(plan) = &m.plan else { return div() };
        let cuts: &[Cut] = &plan.cuts;
        let cur = m.preview.cur.min(cuts.len().saturating_sub(1));
        let playing = m.playing();
        let kept = m.kept();
        let phone = self.montage_ui.phone;
        let region_w = self.montage_width() - 28.0;
        let region = div().w_full().h(px(PREVIEW_H)).rounded(px(12.0)).bg(colour(0x0B0B0D)).flex().items_center().justify_center();
        let region = match (&self.montage_ui.preview, &m.preview.frame) {
            (Some(img), Some(frame)) => {
                let aspect = frame.width as f32 / frame.height.max(1) as f32;
                if phone {
                    region.child(phone_picture(img, aspect, PREVIEW_H - 20.0, m.vertical, m.blur))
                } else {
                    let (w, h) = fit(aspect, region_w - 20.0, PREVIEW_H - 20.0);
                    region.child(div().w(px(w)).h(px(h)).rounded(px(10.0)).overflow_hidden().child(gpui_kit::img(img.clone()).w(px(w)).h(px(h))))
                }
            }
            _ => region.child(theme::muted(if m.preview.busy { "Getting the picture…" } else { "Pick a moment" })),
        };
        let kept_total: f64 = kept.iter().map(|&i| cuts[i].len()).sum();
        let done: f64 = kept.iter().filter(|&&i| i < cur).map(|&i| cuts[i].len()).sum::<f64>() + if kept.contains(&cur) { (m.preview.t - cuts[cur].start).clamp(0.0, cuts[cur].len()) } else { 0.0 };
        let round = div()
            .id("preview-play")
            .size(px(40.0))
            .flex_none()
            .rounded_full()
            .bg(colour(ACCENT))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(colour(0xFF7D54)))
            .child(theme::icon(if playing { "pause" } else { "play" }, 17.0, ON_ACCENT))
            .on_click(cx.listener(|this, _, _, cx| {
                this.engine.preview_toggle();
                cx.notify();
            }));
        let name = cuts.get(cur).and_then(|c| m.clips.get(c.clip)).map(|c| c.name()).unwrap_or_default();
        let controls = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(round)
            .child(small_button("preview-prev", "Previous").on_click(cx.listener(|this, _, _, cx| {
                this.engine.preview_step(-1);
                cx.notify();
            })))
            .child(small_button("preview-next", "Next").on_click(cx.listener(|this, _, _, cx| {
                this.engine.preview_step(1);
                cx.notify();
            })))
            .child(div().flex_none().font_family("Consolas").text_size(px(13.0)).child(format!("{} / {}", clock(done), clock(kept_total))))
            .child(div().flex_1().min_w(px(0.0)).text_size(px(12.5)).text_color(colour(TEXT3)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(cuts.get(cur).map_or_else(String::new, |c| format!("Moment {} of {} · {} · {}", cur + 1, cuts.len(), name, c.why))));
        // The moments as one bar: width is length, colour is the clip. Click one to look at it.
        let total: f64 = cuts.iter().map(Cut::len).sum::<f64>().max(0.1);
        let mut bar = div().h(px(46.0)).flex().gap(px(2.0)).overflow_hidden();
        for (i, cut) in cuts.iter().enumerate() {
            let w = ((region_w as f64 * cut.len() / total) as f32 - 2.0).max(2.0);
            let hex = clip_colour(cut.clip);
            let on = m.keep.get(i).copied().unwrap_or(true);
            let block = div()
                .id(("moment", i))
                .relative()
                .w(px(w))
                .h(px(46.0))
                .flex_none()
                .rounded(px(5.0))
                .cursor_pointer()
                .bg(if on { colour(hex) } else { tint(hex, 0.25) })
                .when(i == cur, |s| s.border_2().border_color(white()))
                .when(i == cur, |s| {
                    let at = ((m.preview.t - cut.start) / cut.len().max(0.1)).clamp(0.0, 1.0) as f32;
                    s.child(div().absolute().left(px(w * at)).top(px(-4.0)).w(px(2.0)).h(px(54.0)).bg(white()))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.engine.preview_goto(i);
                    cx.notify();
                }));
            bar = bar.child(block);
        }
        let mut legend = div().flex().flex_wrap().gap_x(px(14.0)).gap_y(px(4.0));
        for (i, clip) in m.clips.iter().enumerate() {
            legend = legend.child(div().flex().items_center().gap(px(6.0)).child(dot(clip_colour(i))).child(theme::muted(format!("{}. {}", i + 1, clip.name()))));
        }
        theme::card()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(theme::heading("Preview", 14.0))
                    .child(div().flex_1().min_w(px(0.0)).child(theme::muted("A quick silent look. Music, fades and exact colour come when you make it.")))
                    .child(segmented("preview-shape", &[(true, "9:16"), (false, "16:9")], phone, false, cx, |this, v| this.montage_ui.phone = v)),
            )
            .child(region)
            .child(controls)
            .child(bar)
            .child(legend)
    }

    // ---------------------------------------------------------------------------------- make

    fn make_step(&self, cx: &mut Context<Self>) -> Div {
        let e = &self.engine;
        let m = &e.montage;
        let k = &self.knobs;
        let folder = m.out_dir.clone().or_else(|| m.clips.first().and_then(|c| c.path.parent().map(PathBuf::from))).map_or_else(|| "next to the first clip".to_string(), |d| d.display().to_string());
        let help = match m.vertical {
            Vertical::Follow => "A calm virtual camera that pans toward movement and detail.",
            Vertical::Centre => "Always the middle of the picture.",
            Vertical::Blur => "The whole picture over a blurred copy of itself. Zoom trims the sides so it fills more of the tall frame.",
        };
        let mut tall = theme::card()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().flex().items_center().justify_between().child(checkbox("make-tall", "9:16 video", m.make_vertical, cx, |this| this.engine.montage.make_vertical = !this.engine.montage.make_vertical)).child(theme::muted("Shorts, Reels, TikTok")));
        if m.make_vertical {
            tall = tall.child(segmented("montage-framing", &[(Vertical::Follow, "Follow"), (Vertical::Centre, "Centre"), (Vertical::Blur, "Blurred")], m.vertical, true, cx, |this, v| this.engine.montage.vertical = v)).child(theme::muted(help));
            if m.vertical == Vertical::Blur {
                tall = tall.child(k.row(Knob::MontageZoom, e, false)).child(k.row(Knob::MontageX, e, false)).child(k.row(Knob::MontageY, e, false)).child(div().flex().child(small_button("montage-blur-reset", "Centre and reset").on_click(cx.listener(|this, _, _, cx| {
                    this.engine.montage.blur = BlurLayout::default();
                    cx.notify();
                }))));
            }
        }
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(div().h(px(36.0)).flex().items_center().gap(px(12.0)).child(theme::heading("Make", 18.0)).child(theme::soft("Choose the shapes and where they are saved.")));
        if m.plan.is_some() {
            page = page.child(self.preview_card(cx));
        } else {
            page = page.child(theme::muted("Plan with AI first, on the Plan step."));
        }
        page.child(theme::card().flex().items_center().justify_between().child(checkbox("make-wide", "16:9 video", m.make_wide, cx, |this| this.engine.montage.make_wide = !this.engine.montage.make_wide)).child(theme::muted("For YouTube")))
            .child(tall)
            .child(theme::card().flex().flex_col().gap(px(10.0)).child(theme::heading("Speed", 14.0)).child(div().flex().child(segmented("montage-encoder", &encoder_choices(e), e.settings.encoder, false, cx, |this, v| this.engine.settings.encoder = v))))
            .child(
                theme::card()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(theme::icon("folder", 18.0, TEXT3))
                    .child(div().flex_1().min_w(px(0.0)).font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT2)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(folder))
                    .child(small_button("montage-folder", "Change").on_click(cx.listener(|this, _, _, cx| this.choose_montage_folder(cx)))),
            )
    }

    // -------------------------------------------------------------------------------- summary

    fn montage_summary(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let e = &self.engine;
        let m = &e.montage;
        let working = m.busy.is_some() || m.render.is_some();
        let mut footer = div().px(px(20.0)).py(px(16.0)).border_t_1().border_color(colour(LINE)).flex().flex_col().gap(px(10.0));
        if m.plan.is_none() {
            let b = primary_button("montage-plan", if m.busy.is_some() { "Planning…" } else { "Plan with AI" }).w_full();
            footer = footer.child(if !m.clips.is_empty() && e.has_openai && !working {
                b.on_click(cx.listener(|this, _, _, cx| {
                    this.step = Step::Look;
                    this.engine.plan_montage();
                    cx.notify();
                }))
            } else {
                disabled(b)
            });
        } else {
            let label = if m.render.is_some() { "Making the montage…" } else if m.made.is_empty() { "Make the montage" } else { "Make it again" };
            let b = if m.made.is_empty() { primary_button("montage-make", label) } else { secondary_button("montage-make", label) }.w_full();
            footer = footer.child(if !working {
                b.on_click(cx.listener(|this, _, _, cx| {
                    this.step = Step::Export;
                    this.engine.render_montage();
                    cx.notify();
                }))
            } else {
                disabled(b)
            });
            if !m.made.is_empty() {
                footer = footer.child(primary_button("montage-publish", "Continue to Publish").w_full().on_click(cx.listener(|this, _, _, cx| {
                    this.step = Step::Publish;
                    this.engine.montage.stop_preview();
                    cx.notify();
                })));
            }
        }
        let music = match (m.source, m.tracks.get(m.track), &m.file) {
            (MusicSource::File, _, Some(f)) => f.file_name().map_or_else(|| "your file".to_string(), |n| n.to_string_lossy().into_owned()),
            (MusicSource::File, _, None) => "no file chosen".to_string(),
            (MusicSource::Online, Some(t), _) => t.title.clone(),
            (MusicSource::Online, None, _) => "found after planning".to_string(),
        };
        let shapes = match (m.make_wide, m.make_vertical) {
            (true, true) => "16:9 and 9:16",
            (true, false) => "16:9",
            (false, true) => "9:16",
            (false, false) => "nothing ticked",
        };
        let mut rows = theme::card()
            .flex()
            .flex_col()
            .child(summary_row("Clips", if m.clips.is_empty() { "none yet".to_string() } else { format!("{} · {}", m.clips.len(), clock(m.total())) }))
            .child(summary_row("Style", m.style.name().to_string()))
            .child(summary_row("Length", format!("about {}", clock(m.target))))
            .child(summary_row("Sound", m.sound.name().to_string()))
            .when(m.sound.uses_music(), |c| c.child(summary_row("Music", music)))
            .child(summary_row("Look", m.look.map_or("Natural (fix only)", |i| LOOKS[i].name).to_string()))
            .child(summary_row("Makes", shapes.to_string()));
        if let Some(p) = &m.plan {
            rows = rows.child(summary_row("Moments", format!("{} of {} kept", m.keep.iter().filter(|k| **k).count(), p.cuts.len())));
        }
        let mut body = div().py(px(16.0)).flex().flex_col().gap(px(10.0)).child(theme::heading("Your montage", 15.0)).child(rows);
        if m.plan.is_none() {
            body = body.child(theme::muted("Next, the AI looks at small pictures from each clip and picks the best moments. Your video files are not uploaded."));
        } else if m.made.is_empty() {
            body = body.child(theme::muted("Untick any moment you don't want, then make the montage."));
        }
        if !e.has_openai && m.plan.is_none() {
            body = body.child(theme::card().bg(tint(WARN, 0.07)).border_color(tint(WARN, 0.27)).flex().flex_col().gap(px(6.0)).child(theme::heading("OpenAI key needed", 13.5)).child(theme::soft("Planning uses your own OpenAI key. It is kept in the Windows credential store.")).child(div().flex().child(small_button("summary-key", "Open Settings").on_click(cx.listener(|this, _, _, cx| {
                this.settings_ui.open = true;
                cx.notify();
            })))));
        }
        if !m.made.is_empty() {
            let mut saved = theme::card().bg(tint(OK, 0.08)).border_color(tint(OK, 0.28)).flex().flex_col().gap(px(6.0)).child(theme::heading("Saved", 14.0));
            for file in &m.made {
                saved = saved.child(div().text_size(px(12.5)).text_color(colour(TEXT2)).child(Shell::file_name(file)));
            }
            body = body.child(saved.child(small_button("montage-show-folder", "Show in folder").on_click(cx.listener(|this, _, _, _| {
                if let Some(parent) = this.engine.montage.made.first().and_then(|f| f.parent()) {
                    show_in_folder(parent);
                }
            }))));
        }
        div()
            .w(px(INSPECTOR))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(colour(PANEL))
            .border_l_1()
            .border_color(colour(LINE))
            .child(div().id("montage-summary").flex_1().min_h(px(0.0)).overflow_y_scroll().px(px(20.0)).child(body))
            .child(footer)
    }
}
