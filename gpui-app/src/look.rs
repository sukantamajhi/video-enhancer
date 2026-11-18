//! The Look step: the before and after picture, the eight looks, the trim timeline and the adjustments.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ve_core::engine::clock;
use ve_core::grade::LOOKS;

use crate::controls::{Knob, fit, primary_button, secondary_button, segmented, small_button, switch};
use crate::theme::{self, ACCENT, ACCENT_TEXT, BAD, CARD, INPUT, LINE, LINE_SOFT, LINE_STRONG, OK, ON_ACCENT, PANEL, RAISED, TEXT, TEXT2, TEXT3, WARN, accent_soft, colour, tint};
use crate::{Compare, Drag, INSPECTOR, RAIL, STATUS, Shell, TIMELINE, TOP, clock_short};

// The look screen's own measurements, so mouse positions can be turned into places on the picture and the timeline.
const PAD_TOP: f32 = 16.0;
const HEADER: f32 = 36.0;
const GAP: f32 = 12.0;
const STRIP: f32 = 150.0;
const PAD_BOTTOM: f32 = 14.0;
const SIDE: f32 = 24.0;
const BAR_INSET: f32 = 10.0;

/// Where the picture sits in the window, and how big it is.
struct Frame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    region_w: f32,
    region_h: f32,
}

impl Shell {
    fn look_frame(&self) -> Frame {
        let (vw, vh) = self.viewport;
        let region_w = vw - RAIL - INSPECTOR - 2.0 * SIDE;
        let region_h = (vh - TOP - STATUS - TIMELINE) - PAD_TOP - HEADER - GAP - GAP - STRIP - PAD_BOTTOM;
        let aspect = self.engine.raw.as_ref().map_or(16.0 / 9.0, |f| f.width as f32 / f.height.max(1) as f32);
        let (w, h) = fit(aspect, region_w, region_h);
        Frame { x: RAIL + SIDE + (region_w - w) / 2.0, y: TOP + PAD_TOP + HEADER + GAP + (region_h.max(10.0) - h) / 2.0, w, h, region_w, region_h: region_h.max(10.0) }
    }

    /// The timeline's bar: where it starts and how wide it is, in window coordinates.
    fn bar_span(&self) -> (f32, f32) {
        (RAIL + SIDE + BAR_INSET, self.viewport.0 - RAIL - 2.0 * SIDE - 2.0 * BAR_INSET)
    }

    fn x_of(&self, t: f64) -> f32 {
        let duration = self.engine.source.as_ref().map_or(1.0, |s| s.probe.duration.max(0.01));
        let (left, width) = self.bar_span();
        left + width * (t / duration) as f32
    }

    fn t_of(&self, x: f32) -> f64 {
        let duration = self.engine.source.as_ref().map_or(1.0, |s| s.probe.duration);
        let (left, width) = self.bar_span();
        (((x - left) / width).clamp(0.0, 1.0) as f64) * duration
    }

    pub fn drag_split(&mut self, x: f32) {
        let f = self.look_frame();
        self.split = ((x - f.x) / f.w).clamp(0.02, 0.98);
    }

    pub fn drag_timeline(&mut self, x: f32) {
        let duration = self.engine.source.as_ref().map_or(0.0, |s| s.probe.duration);
        let t = self.t_of(x);
        match self.drag {
            Some(Drag::TrimIn) => self.engine.trim.0 = t.min(self.engine.trim.1 - 0.5).max(0.0),
            Some(Drag::TrimOut) => self.engine.trim.1 = t.max(self.engine.trim.0 + 0.5).min(duration),
            Some(Drag::Scrub) => self.engine.seek(t),
            _ => {}
        }
    }

    pub fn finish_drag(&mut self, drag: Drag) {
        if drag == Drag::Words {
            // Let go of the words: draw the plain thumbnail again with them in the new place.
            self.restyle();
        }
        if drag == Drag::Scrub {
            // One last, exact request for where the pointer was let go.
            let t = self.engine.t;
            self.engine.seek(t);
        }
    }

    // ------------------------------------------------------------------ the picture

    fn label(text: &str, accent: bool) -> Div {
        div()
            .px(px(10.0))
            .h(px(26.0))
            .flex()
            .items_center()
            .rounded(px(7.0))
            .text_size(px(12.0))
            .font_weight(FontWeight::BOLD)
            .when(accent, |s| s.bg(colour(ACCENT)).text_color(colour(ON_ACCENT)))
            .when(!accent, |s| s.bg(gpui_kit::black().opacity(0.75)).text_color(colour(TEXT)))
            .child(text.to_string())
    }

    fn preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let f = self.look_frame();
        let (Some(before), Some(after)) = (self.before_img.clone(), self.after_img.clone()) else {
            return div().h(px(f.region_h)).flex().items_center().justify_center().child(theme::muted(if self.engine.source.is_some() { "Getting the picture…" } else { "Open a video to see it here." })).into_any_element();
        };
        let (w, h) = (f.w, f.h);
        let picture = div().relative().w(px(w)).h(px(h)).rounded(px(14.0)).overflow_hidden().border_1().border_color(colour(LINE)).bg(colour(INPUT));
        let picture = match self.compare {
            Compare::Before => picture.child(gpui_kit::img(before).w(px(w)).h(px(h))).child(div().absolute().left(px(14.0)).top(px(14.0)).child(Self::label("BEFORE", false))),
            Compare::After => picture.child(gpui_kit::img(after).w(px(w)).h(px(h))).child(div().absolute().left(px(14.0)).top(px(14.0)).child(Self::label("AFTER", true))),
            Compare::Side => {
                let half = (w - 6.0) / 2.0;
                // Each half shows the middle of its picture's width, so a wide picture isn't squashed.
                picture
                    .child(div().absolute().left(px(0.0)).top(px(0.0)).w(px(half)).h(px(h)).overflow_hidden().child(gpui_kit::img(before).absolute().left(px(-(w - half) / 2.0)).top(px(0.0)).w(px(w)).h(px(h))))
                    .child(div().absolute().left(px(half + 6.0)).top(px(0.0)).w(px(half)).h(px(h)).overflow_hidden().child(gpui_kit::img(after).absolute().left(px(-(w - half) / 2.0)).top(px(0.0)).w(px(w)).h(px(h))))
                    .child(div().absolute().left(px(14.0)).top(px(14.0)).child(Self::label("BEFORE", false)))
                    .child(div().absolute().left(px(half + 20.0)).top(px(14.0)).child(Self::label("AFTER", true)))
            }
            Compare::Slide => {
                let at = self.split * w;
                picture
                    .child(gpui_kit::img(after).absolute().left(px(0.0)).top(px(0.0)).w(px(w)).h(px(h)))
                    .child(div().absolute().left(px(0.0)).top(px(0.0)).w(px(at)).h(px(h)).overflow_hidden().child(gpui_kit::img(before).absolute().left(px(0.0)).top(px(0.0)).w(px(w)).h(px(h))))
                    .child(div().absolute().left(px(at - 1.0)).top(px(0.0)).w(px(2.0)).h(px(h)).bg(white()))
                    .child(div().absolute().left(px(at - 18.0)).top(px(h / 2.0 - 18.0)).size(px(36.0)).rounded_full().bg(colour(ACCENT)).border_2().border_color(white()).flex().items_center().justify_center().child(theme::icon("arrow-right", 16.0, ON_ACCENT)))
                    .child(div().absolute().left(px(14.0)).top(px(14.0)).child(Self::label("BEFORE", false)))
                    .child(div().absolute().right(px(14.0)).top(px(14.0)).child(Self::label("AFTER", true)))
                    .cursor_ew_resize()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        this.drag = Some(Drag::Split);
                        this.drag_split(f32::from(event.position.x));
                        cx.notify();
                    }))
            }
        };
        div().h(px(f.region_h)).flex().items_center().justify_center().child(picture).into_any_element()
    }

    fn looks_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (vw, _) = self.viewport;
        let total = vw - RAIL - INSPECTOR - 2.0 * SIDE;
        let gap = 10.0;
        let w = ((total - 6.0 - gap * 7.0) / 8.0).floor();
        let mut row = div().flex().gap(px(gap));
        for (i, look) in LOOKS.iter().enumerate() {
            let on = self.engine.look == Some(i);
            let thumb_h = (w - 8.0) * 9.0 / 16.0;
            let card = div()
                .id(("look", i))
                .w(px(w))
                .h(px(thumb_h + 40.0))
                .p(px(4.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .rounded(px(11.0))
                .cursor_pointer()
                .border_1()
                .when(on, |s| s.bg(accent_soft(0.10)).border_color(colour(ACCENT)))
                .when(!on, |s| s.bg(colour(CARD)).border_color(colour(LINE_SOFT)).hover(|s| s.bg(colour(RAISED))))
                .child(match self.look_imgs.get(i) {
                    Some(img) => div().w(px(w - 10.0)).h(px(thumb_h)).rounded(px(7.0)).overflow_hidden().child(gpui_kit::img(img.clone()).w(px(w - 10.0)).h(px(thumb_h))),
                    None => div().w(px(w - 10.0)).h(px(thumb_h)).rounded(px(7.0)).bg(colour(INPUT)),
                })
                .child(div().px(px(4.0)).text_size(px(12.5)).font_weight(FontWeight::BOLD).whitespace_nowrap().overflow_hidden().text_ellipsis().child(look.name))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.engine.pick_look(i);
                    cx.notify();
                }));
            row = row.child(card);
        }
        div()
            .h(px(STRIP))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(theme::heading("Looks", 13.0))
                    .child(theme::pill("Auto-fix on", OK, tint(OK, 0.12)))
                    .child(theme::muted("Exposure and white balance are already corrected")),
            )
            .child(row)
    }

    // ------------------------------------------------------------------- the timeline

    fn timeline(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let e = &self.engine;
        let panel = div().h(px(TIMELINE)).flex_none().px(px(SIDE)).py(px(12.0)).flex().flex_col().gap(px(8.0)).bg(colour(0x131416)).border_t_1().border_color(colour(LINE));
        let Some(source) = &e.source else { return panel.child(theme::muted("Open a video to scrub and play.")) };
        let duration = source.probe.duration;
        let playing = e.playing();
        let (t, trim) = (e.t, e.trim);
        let header = div()
            .h(px(40.0))
            .flex()
            .items_center()
            .gap(px(14.0))
            .child(
                div()
                    .id("play")
                    .size(px(40.0))
                    .rounded_full()
                    .bg(colour(ACCENT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(colour(0xFF7D54)))
                    .child(theme::icon(if playing { "pause" } else { "play" }, 16.0, ON_ACCENT))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.engine.toggle_play();
                        cx.notify();
                    })),
            )
            .child(div().font_family("Consolas").text_size(px(14.0)).child(clock(t)))
            .child(div().font_family("Consolas").text_size(px(14.0)).text_color(colour(TEXT3)).child(format!("/ {}", clock(duration))))
            .child(theme::pill(format!("Exporting {} to {} · {}", clock_short(trim.0), clock_short(trim.1), clock_short(trim.1 - trim.0)), ACCENT_TEXT, accent_soft(0.14)))
            .child(div().flex_1())
            .child(small_button("start-here", "Start here").on_click(cx.listener(|this, _, _, cx| {
                let t = this.engine.t;
                this.engine.trim.0 = t.min(this.engine.trim.1 - 0.5).max(0.0);
                cx.notify();
            })))
            .child(small_button("end-here", "End here").on_click(cx.listener(|this, _, _, cx| {
                let (t, duration) = (this.engine.t, this.engine.source.as_ref().map_or(0.0, |s| s.probe.duration));
                this.engine.trim.1 = t.max(this.engine.trim.0 + 0.5).min(duration);
                cx.notify();
            })))
            .child(div().id("whole").px(px(12.0)).h(px(32.0)).flex().items_center().text_size(px(12.5)).text_color(colour(TEXT2)).cursor_pointer().rounded(px(8.0)).hover(|s| s.bg(colour(CARD))).child("Whole video").on_click(cx.listener(|this, _, _, cx| {
                let duration = this.engine.source.as_ref().map_or(0.0, |s| s.probe.duration);
                this.engine.trim = (0.0, duration);
                cx.notify();
            })));
        // The bar itself is drawn straight onto the window: a row of bars, the part left out dimmed, the two handles and the playhead.
        let bar = canvas(
            |bounds, _, _| bounds,
            move |bounds, _: Bounds<Pixels>, window, _| {
                let (left, top, width, height) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height));
                let mut quad = |x: f32, y: f32, w: f32, h: f32, radius: f32, c: Hsla| {
                    window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(w.max(0.0)), px(h.max(0.0)))), c).corner_radii(px(radius)));
                };
                quad(left, top, width, height, 10.0, colour(CARD));
                let (inner_left, inner_width) = (left + BAR_INSET, width - 2.0 * BAR_INSET);
                let (a, b) = (inner_left + inner_width * (trim.0 / duration) as f32, inner_left + inner_width * (trim.1 / duration) as f32);
                let mid = top + height / 2.0;
                let bars = (inner_width / 6.0) as usize;
                for i in 0..bars {
                    let x = inner_left + i as f32 * 6.0 + 2.0;
                    let half = (4.0 + 0.35 * height * 0.38).min(height * 0.45);
                    let inside = x >= a && x <= b;
                    quad(x, mid - half, 3.0, half * 2.0, 1.5, colour(if inside { 0x6F7280 } else { 0x3A3B42 }));
                }
                let dim: Hsla = rgba(0x0F1012B4).into();
                quad(left, top, a - left, height, 10.0, dim);
                quad(b, top, left + width - b, height, 10.0, dim);
                for x in [a, b] {
                    quad(x - 3.0, top, 6.0, height, 3.0, colour(ACCENT));
                }
                let head = inner_left + inner_width * (t / duration.max(0.01)) as f32;
                quad(head - 1.0, top, 2.0, height, 0.0, white());
            },
        )
        .size_full();
        panel.child(header).child(
            div()
                .id("timeline-bar")
                .flex_1()
                .min_h(px(40.0))
                .cursor_pointer()
                .child(bar)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    let x = f32::from(event.position.x);
                    let (a, b) = (this.x_of(this.engine.trim.0), this.x_of(this.engine.trim.1));
                    this.drag = Some(if (x - a).abs() < 9.0 { Drag::TrimIn } else if (x - b).abs() < 9.0 { Drag::TrimOut } else { Drag::Scrub });
                    this.drag_timeline(x);
                    cx.notify();
                })),
        )
    }

    // ------------------------------------------------------------------- the adjustments

    fn section(title: &str, reset: Option<Stateful<Div>>) -> Div {
        div().flex().items_center().justify_between().child(theme::heading(title.to_string(), 14.0)).children(reset)
    }

    fn inspector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let e = &self.engine;
        let k = &self.knobs;
        let learned = e.learned.is_some();
        let light = theme::card().flex().flex_col().gap(px(8.0)).child(Self::section(
            "Light",
            Some(small_button("reset-light", "Reset").on_click(cx.listener(|this, _, _, cx| {
                for knob in [Knob::Exposure, Knob::Contrast, Knob::Highlights, Knob::Shadows] {
                    knob.set(&mut this.engine, 0.0);
                }
                cx.notify();
            }))),
        ))
        .child(k.row(Knob::Exposure, e, true))
        .child(k.row(Knob::Contrast, e, true))
        .child(k.row(Knob::Highlights, e, true))
        .child(k.row(Knob::Shadows, e, true));
        let colour_card = theme::card()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(Self::section(
                "Colour",
                Some(small_button("reset-colour", "Reset").on_click(cx.listener(|this, _, _, cx| {
                    for knob in [Knob::Warmth, Knob::Tint, Knob::Saturation, Knob::Vibrance, Knob::TealOrange, Knob::Fade] {
                        knob.set(&mut this.engine, 0.0);
                    }
                    cx.notify();
                }))),
            ))
            .child(k.row(Knob::Warmth, e, true))
            .child(k.row(Knob::Tint, e, true))
            .child(k.row(Knob::Saturation, e, true))
            .child(k.row(Knob::Vibrance, e, true))
            .when(self.more_colour, |s| s.child(k.row(Knob::TealOrange, e, false)).child(k.row(Knob::Fade, e, false)))
            .child(small_button("more-colour", if self.more_colour { "Fewer colour controls" } else { "More colour controls" }).on_click(cx.listener(|this, _, _, cx| {
                this.more_colour = !this.more_colour;
                cx.notify();
            })));
        let finish = theme::card()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .id("finish-head")
                    .flex()
                    .items_baseline()
                    .gap(px(10.0))
                    .cursor_pointer()
                    .child(theme::heading("Finish", 14.0))
                    .child(theme::muted(format!("Sharpen {} · Sound level {}", if e.finish.sharpen > 0.01 { "on" } else { "off" }, if e.finish.loudness { "on" } else { "off" })))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.finish_open = !this.finish_open;
                        cx.notify();
                    })),
            )
            .when(self.finish_open, |s| {
                s.child(k.row(Knob::Sharpen, e, false)).child(k.row(Knob::Denoise, e, false)).child(switch("loudness", "Even out the sound (YouTube level)", e.finish.loudness, cx, |this| this.engine.finish.loudness = !this.engine.finish.loudness))
            });
        let busy = e.ai_busy.is_some();
        let have_video = e.source.is_some();
        let gemini = {
            let b = div().id("omni").flex_1().h(px(52.0)).px(px(12.0)).flex().flex_col().justify_center().rounded(px(10.0)).bg(colour(CARD)).border_1().border_color(colour(LINE_STRONG)).cursor_pointer().hover(|s| s.bg(colour(RAISED)))
                .child(div().text_size(px(13.0)).font_weight(FontWeight::BOLD).child("Gemini Omni"))
                .child(theme::muted("Copies a look · about $1"));
            if e.has_gemini && have_video && !busy { b.on_click(cx.listener(|this, _, _, cx| { this.confirm_omni = true; cx.notify(); })) } else { b.opacity(0.45).cursor_default() }
        };
        let openai = {
            let b = div().id("suggest").flex_1().h(px(52.0)).px(px(12.0)).flex().flex_col().justify_center().rounded(px(10.0)).bg(colour(CARD)).border_1().border_color(colour(LINE_STRONG)).cursor_pointer().hover(|s| s.bg(colour(RAISED)))
                .child(div().text_size(px(13.0)).font_weight(FontWeight::BOLD).child("OpenAI suggest"))
                .child(theme::muted("Sets the sliders for you"));
            if e.has_openai && !busy { b.on_click(cx.listener(|this, _, _, cx| { this.engine.run_openai(); cx.notify(); })) } else { b.opacity(0.45).cursor_default() }
        };
        let models = ve_core::ai::OPENAI_MODELS.iter().map(|(_, name)| name.split(':').next().unwrap_or(name).trim()).collect::<Vec<_>>();
        let ai = div()
            .p(px(14.0))
            .rounded(px(12.0))
            .bg(accent_soft(0.08))
            .border_1()
            .border_color(accent_soft(0.28))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().flex().items_center().gap(px(8.0)).child(theme::icon("sparkles", 16.0, ACCENT_TEXT)).child(theme::heading("AI assist", 14.0)).child(theme::muted("uses your own keys")))
            .child(theme::muted("What kind of look do you want?"))
            .child(gpui_kit::component::input::Input::new(&self.notes))
            .child(div().flex().gap(px(8.0)).child(gemini).child(openai))
            .when(!e.has_openai && !e.has_gemini, |s| s.child(self.key_hint("No AI key is saved yet.", "look-key", cx)))
            .child(div().flex().items_center().gap(px(8.0)).child(theme::muted("Model")).child(segmented("model", &[(0usize, models[0]), (1, models[1]), (2, models[2])], e.settings.openai_model, false, cx, |this, v| this.engine.settings.openai_model = v)))
            .when_some(e.ai_busy.clone(), |s, text| s.child(div().text_size(px(13.0)).text_color(colour(TEXT2)).child(text)))
            .when(!e.ai_result.is_empty(), |s| s.child(div().text_size(px(13.0)).italic().text_color(colour(TEXT2)).child(e.ai_result.clone())));
        let mut amount = theme::card().flex().flex_col().gap(px(8.0)).child(k.row(Knob::Strength, e, false));
        if learned {
            amount = amount.child(k.row(Knob::OmniAmount, e, false));
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
            .child(
                div().id("inspector-scroll").flex_1().min_h(px(0.0)).overflow_y_scroll().px(px(20.0)).child(
                    div()
                        .py(px(16.0))
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(div().flex().items_baseline().justify_between().child(theme::heading("Adjust", 15.0)).child(theme::muted(e.look.map_or("Your own mix", |i| LOOKS[i].name))))
                        .child(amount)
                        .child(light)
                        .child(colour_card)
                        .child(finish)
                        .child(ai),
                ),
            )
            .child(div().px(px(20.0)).py(px(16.0)).border_t_1().border_color(colour(LINE)).child(primary_button("to-export", "Continue to Export").w_full().on_click(cx.listener(|this, _, _, cx| {
                this.step = crate::Step::Export;
                this.engine.stop_playing();
                cx.notify();
            }))))
    }

    pub fn look_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let centre = div()
            .flex_1()
            .min_w(px(0.0))
            .px(px(SIDE))
            .pt(px(PAD_TOP))
            .pb(px(PAD_BOTTOM))
            .flex()
            .flex_col()
            .gap(px(GAP))
            .child(
                div()
                    .h(px(HEADER))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(theme::heading("Look", 18.0))
                    .child(theme::soft("Drag the divider to compare"))
                    .child(div().flex_1())
                    .child(segmented("compare", &[(Compare::After, "After"), (Compare::Before, "Before"), (Compare::Side, "Side by side"), (Compare::Slide, "Slide")], self.compare, false, cx, |this, v| this.compare = v)),
            )
            .child(self.preview(cx))
            .child(self.looks_strip(cx));
        div().size_full().flex().flex_col().child(div().flex_1().min_h(px(0.0)).flex().child(centre).child(self.inspector(cx))).child(self.timeline(cx))
    }

    /// Asks before sending a sample of the video to Gemini, which costs real money.
    pub fn omni_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("omni-overlay")
            .occlude()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full()
            .bg(gpui_kit::black().opacity(0.55))
            .flex()
            .items_center()
            .justify_center()
            .on_click(|_, _, _| {})
            .child(
                theme::card()
                    .w(px(480.0))
                    .p(px(22.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(theme::heading("Send a sample to Gemini Omni?", 17.0))
                    .child(theme::soft("This uploads a 10-second sample of your video to Google and uses your Gemini key."))
                    .child(theme::soft("It costs real money, roughly one dollar per run. The rest of your video is not uploaded."))
                    .child(
                        div()
                            .flex()
                            .gap(px(10.0))
                            .pt(px(6.0))
                            .child(primary_button("omni-send", "Send it").on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_omni = false;
                                this.engine.run_omni();
                                cx.notify();
                            })))
                            .child(secondary_button("omni-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_omni = false;
                                cx.notify();
                            }))),
                    ),
            )
    }
}

#[allow(dead_code)]
fn unused() -> (u32, u32, u32) {
    (BAD, WARN, 0)
}
