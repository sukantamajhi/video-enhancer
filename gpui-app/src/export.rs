//! The Export step: what will be saved (a wide and a phone picture with your look), and the options for saving it.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ve_core::engine::{Engine, VerticalFraming};
use ve_core::media::{self, Encoder, EncoderChoice};

use crate::controls::{Knob, checkbox, disabled, primary_button, secondary_button, segmented, small_button, switch};
use crate::theme::{self, ACCENT, CARD, INPUT, LINE, LINE_STRONG, OK, PANEL, RAISED, TEXT, TEXT2, TEXT3, WARN, colour, tint};
use crate::{Drag, INSPECTOR, RAIL, STATUS, Shell, Step, TOP, clock_short};

const PAD_TOP: f32 = 16.0;
const HEADER: f32 = 36.0;
const ESTIMATE: f32 = 64.0;
const SIDE: f32 = 28.0;

/// How big the two previews are drawn.
struct Sizes {
    wide_w: f32,
    wide_h: f32,
    phone_w: f32,
    phone_h: f32,
}

impl Shell {
    fn export_sizes(&self) -> Sizes {
        let (vw, vh) = self.viewport;
        let region_w = vw - RAIL - INSPECTOR - 2.0 * SIDE;
        let region_h = vh - TOP - STATUS - PAD_TOP - HEADER - ESTIMATE - 12.0 - 12.0 - 14.0;
        let phone_h = (region_h - 70.0).clamp(300.0, 444.0);
        let phone_w = phone_h * 9.0 / 16.0;
        let wide_w = (region_w - phone_w - 60.0).clamp(300.0, 560.0);
        Sizes { wide_w, wide_h: wide_w * 9.0 / 16.0, phone_w, phone_h }
    }

    /// Moves the picture inside the phone, when the pointer moves while dragging it.
    pub fn drag_phone(&mut self, dx: f32, dy: f32, x: f32, y: f32) {
        let s = self.export_sizes();
        let aspect = self.aspect();
        let e = &mut self.engine;
        match e.framing {
            VerticalFraming::Blur => {
                let b = e.blur.clamped();
                let (fw, fh) = (s.phone_w * b.zoom, s.phone_w * b.zoom / aspect);
                if fw > s.phone_w + 0.5 {
                    e.blur.x = (b.x - dx / (fw - s.phone_w)).clamp(0.0, 1.0);
                }
                if fh < s.phone_h - 0.5 {
                    e.blur.y = (b.y + dy / (s.phone_h - fh)).clamp(0.0, 1.0);
                }
            }
            VerticalFraming::Manual => e.manual_x = (e.manual_x - dx / (s.phone_w * 1.6)).clamp(0.0, 1.0),
            _ => {}
        }
        self.drag = Some(Drag::Phone { x, y });
    }

    fn aspect(&self) -> f32 {
        self.engine.graded.as_ref().map_or(16.0 / 9.0, |f| f.width as f32 / f.height.max(1) as f32)
    }

    /// The 9:16 picture as it will be framed, for each of the four ways of framing it.
    fn phone(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.export_sizes();
        let (pw, ph) = (s.phone_w, s.phone_h);
        let e = &self.engine;
        let aspect = self.aspect();
        let screen = div().relative().w(px(pw)).h(px(ph)).rounded(px(30.0)).overflow_hidden().bg(colour(INPUT));
        let screen = match &self.after_img {
            None => screen,
            Some(img) if e.framing == VerticalFraming::Blur => {
                let b = e.blur.clamped();
                // The picture, enlarged to fill the tall frame and darkened, sits behind the whole picture.
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
                    .child(crosshair((pw / 2.0, (ph - band_h) * b.y + band_h / 2.0)))
            }
            Some(img) => {
                let pos = match e.framing {
                    VerticalFraming::Manual => e.manual_x,
                    VerticalFraming::Follow => e.action.as_ref().map_or(0.5, |p| media::action_at(p, e.t)),
                    _ => 0.5,
                };
                let full_w = ph * aspect;
                screen.child(gpui_kit::img(img.clone()).absolute().left(px(-(full_w - pw).max(0.0) * pos)).top(px(0.0)).w(px(full_w.max(pw))).h(px(ph)))
            }
        };
        let draggable = matches!(e.framing, VerticalFraming::Blur | VerticalFraming::Manual);
        div()
            .id("phone")
            .p(px(6.0))
            .rounded(px(36.0))
            .bg(colour(CARD))
            .border_1()
            .border_color(colour(0x34353C))
            .child(screen)
            .when(draggable, |s| {
                s.cursor_grab().on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.drag = Some(Drag::Phone { x: f32::from(event.position.x), y: f32::from(event.position.y) });
                    cx.notify();
                }))
            })
    }

    fn estimate(&self) -> (String, String) {
        let e = &self.engine;
        let jobs = u8::from(e.settings.wants_wide) + u8::from(e.settings.wants_vertical);
        let on_card = e.colour_on_card();
        let hardware = e.encoder() != Encoder::Cpu;
        let speed = match (on_card, hardware) {
            (true, true) => 5.0,
            (true, false) => 2.6,
            (false, true) => 2.4,
            (false, false) => 1.8,
        };
        let seconds = (e.trim.1 - e.trim.0).max(0.0) / speed * f64::from(jobs.max(1));
        let headline = if seconds < 90.0 { format!("About {:.0} seconds", seconds.max(5.0)) } else { format!("About {:.0} minutes", (seconds / 60.0).ceil()) };
        let enc = e.encoder();
        let detail = format!(
            "for {} of video, with {} and colour on {}",
            clock_short((e.trim.1 - e.trim.0).max(0.0)),
            if enc == Encoder::Cpu { "the CPU encoder".to_string() } else { format!("the {} encoder", enc.short()) },
            if on_card { "the graphics card" } else { "the processor" }
        );
        (headline, detail)
    }

    fn wide_spec(&self) -> String {
        let e = &self.engine;
        let size = match (e.settings.wide_height, &e.source) {
            (0, Some(s)) => format!("{}×{}", if s.probe.is_wide() { s.probe.width } else { media::even(s.probe.height as f64 * 16.0 / 9.0) }, s.probe.height),
            (h, _) => format!("{}×{}", media::even(h as f64 * 16.0 / 9.0), h),
        };
        format!("{size} · MP4 · with sound")
    }

    fn previews(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.export_sizes();
        let e = &self.engine;
        let (want_wide, want_tall) = (e.settings.wants_wide, e.settings.wants_vertical);
        let how = match e.framing {
            VerticalFraming::Follow => "follows the action",
            VerticalFraming::Centre => "centred",
            VerticalFraming::Manual => "your framing",
            VerticalFraming::Blur => "blurred background",
        };
        let wide = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(match &self.after_img {
                Some(img) => div().w(px(s.wide_w)).h(px(s.wide_h)).rounded(px(14.0)).overflow_hidden().border_1().border_color(colour(LINE)).child(gpui_kit::img(img.clone()).w(px(s.wide_w)).h(px(s.wide_h))),
                None => div().w(px(s.wide_w)).h(px(s.wide_h)).rounded(px(14.0)).bg(colour(INPUT)),
            })
            .child(div().flex().items_baseline().gap(px(12.0)).child(theme::heading("16:9 video", 14.0)).child(div().font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT3)).child(self.wide_spec())));
        let tall = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(self.phone(cx))
            .child(div().flex().flex_col().gap(px(3.0)).child(theme::heading("9:16 video", 14.0)).child(div().font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT3)).child(format!("1080×1920 · {how}"))));
        div().flex_1().min_h(px(0.0)).flex().items_center().justify_center().gap(px(48.0)).when(want_wide, |s| s.child(wide)).when(want_tall, |s| s.child(tall)).when(!want_wide && !want_tall, |s| s.child(theme::muted("Tick a video format on the right.")))
    }

    fn framing_controls(&self, cx: &mut Context<Self>) -> Div {
        let e = &self.engine;
        let k = &self.knobs;
        let help = match e.framing {
            VerticalFraming::Follow => "A calm virtual camera that pans toward movement and detail.",
            VerticalFraming::Centre => "Always the middle of the picture.",
            VerticalFraming::Manual => "You choose where. Drag the phone preview left and right.",
            VerticalFraming::Blur => "The whole picture over a blurred copy of itself. Drag the phone preview to move it.",
        };
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(segmented("framing", &[(VerticalFraming::Follow, "Follow"), (VerticalFraming::Centre, "Centre"), (VerticalFraming::Manual, "Choose"), (VerticalFraming::Blur, "Blurred")], e.framing, true, cx, |this, v| this.engine.framing = v))
            .child(theme::muted(help));
        match e.framing {
            VerticalFraming::Blur => {
                col = col.child(k.row(Knob::BlurZoom, e, false)).child(k.row(Knob::BlurX, e, false)).child(k.row(Knob::BlurY, e, false)).child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(small_button("blur-centre", "Centre").on_click(cx.listener(|this, _, _, cx| {
                            (this.engine.blur.x, this.engine.blur.y) = (0.5, 0.5);
                            cx.notify();
                        })))
                        .child(small_button("blur-reset", "Reset zoom").on_click(cx.listener(|this, _, _, cx| {
                            this.engine.blur = media::BlurLayout::default();
                            cx.notify();
                        }))),
                );
            }
            VerticalFraming::Manual => col = col.child(k.row(Knob::ManualX, e, false)),
            VerticalFraming::Follow => {
                col = col.child(if e.action_busy {
                    theme::soft("Finding the action…").into_any_element()
                } else if e.action.is_none() {
                    small_button("find-action", "Find the action now").on_click(cx.listener(|this, _, _, cx| {
                        this.engine.follow_action();
                        cx.notify();
                    })).into_any_element()
                } else {
                    theme::muted("Action found. The preview shows the crop.").into_any_element()
                });
            }
            VerticalFraming::Centre => {}
        }
        col
    }

    fn options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let e = &self.engine;
        let s = &e.settings;
        let sizes = [(0u32, "Original"), (1080, "1080p"), (1440, "1440p"), (2160, "4K")];
        let encoders = encoder_choices(e);
        let folder = s.out_dir.clone().or_else(|| e.source.as_ref().and_then(|x| x.path.parent().map(std::path::PathBuf::from))).map_or_else(|| "next to the video".to_string(), |d| d.display().to_string());
        let speed_pill = if e.colour_on_card() && e.encoder() != Encoder::Cpu { theme::pill("About 5× real time", OK, tint(OK, 0.12)) } else { theme::pill("Slower without the graphics card", WARN, tint(WARN, 0.12)) };
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                theme::card()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(div().flex().items_center().justify_between().child(checkbox("want-wide", "16:9 video", s.wants_wide, cx, |this| this.engine.settings.wants_wide = !this.engine.settings.wants_wide)).child(theme::muted("For YouTube")))
                    .when(s.wants_wide, |c| c.child(div().flex().items_center().gap(px(10.0)).child(theme::soft("Size")).child(segmented("wide-size", &sizes, s.wide_height, false, cx, |this, v| this.engine.settings.wide_height = v)))),
            )
            .child(
                theme::card()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(div().flex().items_center().justify_between().child(checkbox("want-tall", "9:16 video", s.wants_vertical, cx, |this| this.engine.settings.wants_vertical = !this.engine.settings.wants_vertical)).child(theme::muted("Shorts, Reels, TikTok")))
                    .when(s.wants_vertical, |c| c.child(self.framing_controls(cx))),
            )
            .child(
                theme::card()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(theme::heading("Extras", 14.0))
                    .child(checkbox("want-thumb", "Thumbnail from this frame", s.wants_thumb, cx, |this| this.engine.settings.wants_thumb = !this.engine.settings.wants_thumb))
                    .child(checkbox("want-lut", "Colour file (.cube) for other editors", s.wants_lut, cx, |this| this.engine.settings.wants_lut = !this.engine.settings.wants_lut)),
            )
            .child(
                theme::card()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(div().flex().items_center().justify_between().child(theme::heading("Speed", 14.0)).child(speed_pill))
                    .child(div().flex().items_center().gap(px(10.0)).child(theme::soft("Encoder")).child(segmented("encoder", &encoders, s.encoder, false, cx, |this, v| this.engine.settings.encoder = v)))
                    .child(theme::muted(if e.encoders.is_empty() { "No graphics-card encoder found here, so the CPU is used." } else { "Graphics-card encoding is much faster. Files come out slightly larger." }))
                    .child(if matches!(e.gpu_status, Some(Ok(_))) {
                        switch("gpu-grade", "Grade colour on the graphics card", s.gpu_grade, cx, |this| this.engine.settings.gpu_grade = !this.engine.settings.gpu_grade).into_any_element()
                    } else {
                        theme::muted("Colour is graded by FFmpeg: the graphics card isn't available here.").into_any_element()
                    }),
            )
            .child(
                theme::card().flex().items_center().gap(px(10.0)).child(theme::icon("folder", 18.0, TEXT3)).child(div().flex_1().min_w(px(0.0)).font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT2)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(folder)).child(small_button("change-folder", "Change").on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx)))),
            )
            .when(!e.made.is_empty(), |c| {
                let mut list = theme::card().flex().flex_col().gap(px(6.0)).bg(tint(OK, 0.08)).border_color(tint(OK, 0.28)).child(theme::heading("Saved", 14.0));
                for file in &e.made {
                    list = list.child(div().text_size(px(12.5)).text_color(colour(TEXT2)).child(Shell::file_name(file)));
                }
                c.child(list.child(small_button("show-folder", "Show in folder").on_click(cx.listener(|this, _, _, _| {
                    if let Some(parent) = this.engine.made.first().and_then(|f| f.parent()) {
                        show_in_folder(parent);
                    }
                }))))
            })
    }

    pub fn export_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let e = &self.engine;
        let (headline, detail) = self.estimate();
        let jobs = u8::from(e.settings.wants_wide) + u8::from(e.settings.wants_vertical);
        let label = match (jobs, e.settings.wants_thumb) {
            (0, _) => "Export".to_string(),
            (1, true) => "Export 1 video and a thumbnail".to_string(),
            (1, false) => "Export 1 video".to_string(),
            (_, true) => "Export 2 videos and a thumbnail".to_string(),
            (_, false) => "Export 2 videos".to_string(),
        };
        let can_export = e.source.is_some() && e.export.is_none();
        let start = {
            let b = primary_button("export", &label).w_full();
            if can_export {
                b.on_click(cx.listener(|this, _, _, cx| {
                    this.engine.start_export();
                    cx.notify();
                }))
            } else {
                disabled(b)
            }
        };
        let centre = div()
            .flex_1()
            .min_w(px(0.0))
            .px(px(SIDE))
            .pt(px(PAD_TOP))
            .pb(px(14.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(div().h(px(HEADER)).flex().items_center().gap(px(12.0)).child(theme::heading("Export", 18.0)).child(theme::soft("This is what will be saved. Both previews use your look.")))
            .child(self.previews(cx))
            .child(
                theme::card()
                    .h(px(ESTIMATE))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(theme::icon("zap", 18.0, OK))
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::BOLD).child(headline))
                    .child(theme::soft(detail)),
            );
        let inspector = div()
            .w(px(INSPECTOR))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(colour(PANEL))
            .border_l_1()
            .border_color(colour(LINE))
            .child(div().id("export-scroll").flex_1().min_h(px(0.0)).overflow_y_scroll().px(px(20.0)).child(div().py(px(16.0)).child(self.options(cx))))
            .child(
                div()
                    .px(px(20.0))
                    .py(px(16.0))
                    .border_t_1()
                    .border_color(colour(LINE))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(start)
                    .when(!e.made.is_empty(), |s| s.child(secondary_button("to-publish", "Continue to Publish").w_full().on_click(cx.listener(|this, _, _, cx| {
                        this.step = Step::Publish;
                        cx.notify();
                    })))),
            );
        div().size_full().flex().child(centre).child(inspector)
    }

    /// The running export, as a small card over the bottom left, with a way to stop it.
    pub fn toast(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The single video's export, or the montage being made: both show the same card.
        let (progress, label, cancel, what) = match (&self.engine.export, &self.engine.montage.render) {
            (Some(x), _) => (x.progress, x.label.clone(), x.cancel.clone(), "Writing the video"),
            (None, Some((p, label, cancel))) => (*p, label.clone(), cancel.clone(), "Making the montage"),
            (None, None) => return div().into_any_element(),
        };
        let p = progress.clamp(0.0, 1.0);
        div()
            .absolute()
            .left(px(RAIL + 24.0))
            .bottom(px(STATUS + 16.0))
            .w(px(440.0))
            .p(px(16.0))
            .rounded(px(16.0))
            .bg(colour(0x212226))
            .border_1()
            .border_color(colour(LINE_STRONG))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().flex().justify_between().child(theme::heading(what, 14.0)).child(div().font_family("Consolas").text_size(px(13.0)).child(format!("{:.0}%", p * 100.0))))
            .child(div().h(px(8.0)).rounded(px(4.0)).bg(colour(0x33343A)).child(div().h(px(8.0)).rounded(px(4.0)).bg(colour(ACCENT)).w(relative(p))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().flex_1().min_w(px(0.0)).text_size(px(12.5)).text_color(colour(TEXT3)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(label))
                    .child(small_button("cancel-export", "Cancel").on_click(cx.listener(move |_, _, _, cx| {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }
}

/// The encoders to offer: automatic, the processor, and every graphics-card encoder found here.
pub fn encoder_choices(e: &Engine) -> Vec<(EncoderChoice, &'static str)> {
    let mut encoders: Vec<(EncoderChoice, &'static str)> = vec![(EncoderChoice::Auto, "Auto"), (EncoderChoice::Cpu, "CPU")];
    for (choice, encoder, name) in [(EncoderChoice::Nvidia, Encoder::Nvidia, "NVIDIA"), (EncoderChoice::Amd, Encoder::Amd, "AMD"), (EncoderChoice::Intel, Encoder::Intel, "Intel")] {
        if e.encoders.contains(&encoder) {
            encoders.push((choice, name));
        }
    }
    encoders
}

/// The small cross that marks the middle of the main picture.
fn crosshair(at: (f32, f32)) -> Div {
    div().absolute().left(px(at.0 - 14.0)).top(px(at.1 - 14.0)).size(px(28.0)).flex().items_center().justify_center().child(div().size(px(14.0)).rounded_full().border_2().border_color(white()))
}

pub fn show_in_folder(path: &std::path::Path) {
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program).arg(path).spawn();
}

#[allow(dead_code)]
fn unused(_: &Engine) -> (u32, u32) {
    (RAISED, TEXT)
}
