//! The small controls every screen uses: buttons, choices, tick boxes, and the sliders that are tied to the engine.

use std::sync::Arc;

use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ve_core::engine::Engine;
use ve_core::media::Frame;

use crate::Shell;
use crate::theme::{ACCENT, CARD, INPUT, LINE, LINE_STRONG, ON_ACCENT, PANEL, RAISED, TEXT, TEXT2, TEXT3, colour};

// ----------------------------------------------------------------------------------- pictures

/// A video frame as a picture gpui can draw. gpui wants the colours in the order blue, green, red.
pub fn picture(frame: &Frame) -> Arc<RenderImage> {
    let (w, h) = (frame.width as u32, frame.height as u32);
    let mut bgra = Vec::with_capacity(frame.rgb.len() / 3 * 4);
    for p in frame.rgb.chunks_exact(3) {
        bgra.extend_from_slice(&[p[2], p[1], p[0], 255]);
    }
    let buffer = image::RgbaImage::from_raw(w, h, bgra).expect("the frame is as big as it says");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// The biggest picture of this shape that fits in a box.
pub fn fit(aspect: f32, box_w: f32, box_h: f32) -> (f32, f32) {
    let (box_w, box_h) = (box_w.max(10.0), box_h.max(10.0));
    if box_w / box_h > aspect { (box_h * aspect, box_h) } else { (box_w, box_w / aspect) }
}

// ------------------------------------------------------------------------------------ buttons

pub fn primary_button(id: impl Into<ElementId>, label: &str) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(48.0))
        .px(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(12.0))
        .bg(colour(ACCENT))
        .text_color(colour(ON_ACCENT))
        .text_size(px(15.0))
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .hover(|s| s.bg(colour(0xFF7D54)))
        .active(|s| s.bg(colour(0xE5592B)))
        .child(label.to_string())
}

pub fn secondary_button(id: impl Into<ElementId>, label: &str) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(40.0))
        .px(px(18.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .bg(colour(INPUT))
        .border_1()
        .border_color(colour(LINE_STRONG))
        .text_color(colour(TEXT))
        .text_size(px(13.5))
        .cursor_pointer()
        .hover(|s| s.bg(colour(RAISED)))
        .child(label.to_string())
}

pub fn small_button(id: impl Into<ElementId>, label: &str) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(32.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .bg(colour(INPUT))
        .border_1()
        .border_color(colour(LINE_STRONG))
        .text_color(colour(TEXT))
        .text_size(px(12.5))
        .cursor_pointer()
        .hover(|s| s.bg(colour(RAISED)))
        .child(label.to_string())
}

/// A button that can't be pressed right now.
pub fn disabled(button: Stateful<Div>) -> Stateful<Div> {
    // The hover colour was already set when the button was made, and gpui allows it only once.
    button.opacity(0.45).cursor_default()
}

/// Options in a row; the chosen one is raised.
pub fn segmented<T: Copy + PartialEq + 'static>(id: &'static str, items: &[(T, &'static str)], current: T, full_width: bool, cx: &mut Context<Shell>, apply: impl Fn(&mut Shell, T) + Clone + 'static) -> Div {
    let mut row = div().flex().p(px(3.0)).rounded(px(12.0)).bg(colour(PANEL)).border_1().border_color(colour(LINE)).when(full_width, |s| s.w_full());
    for (i, (value, label)) in items.iter().enumerate() {
        let (value, on) = (*value, *value == current);
        let apply = apply.clone();
        row = row.child(
            div()
                .id((id, i))
                .when(full_width, |s| s.flex_1())
                .h(px(34.0))
                .px(px(14.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(9.0))
                .text_size(px(13.0))
                .cursor_pointer()
                .when(on, |s| s.bg(colour(RAISED)).text_color(colour(TEXT)).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |s| s.text_color(colour(TEXT2)).hover(|s| s.bg(colour(CARD))))
                .child(*label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    apply(this, value);
                    cx.notify();
                })),
        );
    }
    row
}

/// A tick box with a label.
pub fn checkbox(id: impl Into<ElementId>, label: &str, on: bool, cx: &mut Context<Shell>, toggle: impl Fn(&mut Shell) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(10.0))
        .cursor_pointer()
        .child(
            div()
                .size(px(20.0))
                .flex_none()
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .when(on, |s| s.bg(colour(ACCENT)).child(crate::theme::icon("check", 14.0, ON_ACCENT)))
                .when(!on, |s| s.border_2().border_color(colour(0x4A4B53))),
        )
        .child(div().text_size(px(14.0)).text_color(colour(if on { TEXT } else { TEXT2 })).child(label.to_string()))
        .on_click(cx.listener(move |this, _, _, cx| {
            toggle(this);
            cx.notify();
        }))
}

/// An on/off switch with a label.
pub fn switch(id: impl Into<ElementId>, label: &str, on: bool, cx: &mut Context<Shell>, toggle: impl Fn(&mut Shell) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(10.0))
        .cursor_pointer()
        .child(
            div()
                .w(px(38.0))
                .h(px(22.0))
                .flex_none()
                .rounded(px(11.0))
                .bg(colour(if on { ACCENT } else { 0x33343A }))
                .relative()
                .child(div().absolute().top(px(3.0)).left(px(if on { 19.0 } else { 3.0 })).size(px(16.0)).rounded_full().bg(colour(if on { ON_ACCENT } else { TEXT3 }))),
        )
        .child(div().text_size(px(13.0)).text_color(colour(TEXT2)).child(label.to_string()))
        .on_click(cx.listener(move |this, _, _, cx| {
            toggle(this);
            cx.notify();
        }))
}

// -------------------------------------------------------------------------------------- sliders

/// Every slider that changes something in the engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Knob {
    Strength,
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Warmth,
    Tint,
    Saturation,
    Vibrance,
    TealOrange,
    Fade,
    Sharpen,
    Denoise,
    BlurZoom,
    BlurX,
    BlurY,
    ManualX,
    OmniAmount,
    /// The framing of a montage's 9:16 video (its own, apart from the single video's).
    MontageZoom,
    MontageX,
    MontageY,
}

impl Knob {
    pub const ALL: [Knob; 21] = [
        Knob::Strength, Knob::Exposure, Knob::Contrast, Knob::Highlights, Knob::Shadows, Knob::Warmth, Knob::Tint, Knob::Saturation, Knob::Vibrance, Knob::TealOrange, Knob::Fade, Knob::Sharpen, Knob::Denoise, Knob::BlurZoom, Knob::BlurX, Knob::BlurY, Knob::ManualX, Knob::OmniAmount, Knob::MontageZoom, Knob::MontageX, Knob::MontageY,
    ];

    pub fn range(self) -> (f32, f32) {
        match self {
            Knob::Strength | Knob::TealOrange | Knob::Sharpen | Knob::Denoise | Knob::BlurX | Knob::BlurY | Knob::ManualX | Knob::OmniAmount | Knob::MontageX | Knob::MontageY => (0.0, 1.0),
            Knob::Exposure | Knob::Highlights | Knob::Shadows | Knob::Warmth | Knob::Tint => (-1.0, 1.0),
            Knob::Contrast => (-0.5, 0.6),
            Knob::Saturation => (-0.6, 0.6),
            Knob::Vibrance => (0.0, 0.8),
            Knob::Fade => (0.0, 0.2),
            Knob::BlurZoom | Knob::MontageZoom => (1.0, 2.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Knob::Strength => "Overall amount",
            Knob::Exposure => "Exposure",
            Knob::Contrast => "Contrast",
            Knob::Highlights => "Highlights",
            Knob::Shadows => "Shadows",
            Knob::Warmth => "Warmth",
            Knob::Tint => "Tint",
            Knob::Saturation => "Saturation",
            Knob::Vibrance => "Vibrance",
            Knob::TealOrange => "Teal and orange",
            Knob::Fade => "Fade",
            Knob::Sharpen => "Sharpen (slower export)",
            Knob::Denoise => "Noise cleanup",
            Knob::BlurZoom | Knob::MontageZoom => "Zoom",
            Knob::BlurX | Knob::MontageX => "Left / right",
            Knob::BlurY | Knob::MontageY => "Up / down",
            Knob::ManualX => "Left / right",
            Knob::OmniAmount => "Omni look amount",
        }
    }

    /// Whether moving it makes the grade something other than one of the named looks.
    fn is_grade_slider(self) -> bool {
        !matches!(self, Knob::Strength | Knob::Sharpen | Knob::Denoise | Knob::BlurZoom | Knob::BlurX | Knob::BlurY | Knob::ManualX | Knob::OmniAmount | Knob::MontageZoom | Knob::MontageX | Knob::MontageY)
    }

    pub fn get(self, e: &Engine) -> f32 {
        match self {
            Knob::Strength => e.grade.strength,
            Knob::Exposure => e.grade.exposure,
            Knob::Contrast => e.grade.contrast,
            Knob::Highlights => e.grade.highlights,
            Knob::Shadows => e.grade.shadows,
            Knob::Warmth => e.grade.temperature,
            Knob::Tint => e.grade.tint,
            Knob::Saturation => e.grade.saturation,
            Knob::Vibrance => e.grade.vibrance,
            Knob::TealOrange => e.grade.teal_orange,
            Knob::Fade => e.grade.fade,
            Knob::Sharpen => e.finish.sharpen,
            Knob::Denoise => e.finish.denoise,
            Knob::BlurZoom => e.blur.zoom,
            Knob::BlurX => e.blur.x,
            Knob::BlurY => e.blur.y,
            Knob::ManualX => e.manual_x,
            Knob::OmniAmount => e.learned.as_ref().map_or(1.0, |l| l.amount),
            Knob::MontageZoom => e.montage.blur.zoom,
            Knob::MontageX => e.montage.blur.x,
            Knob::MontageY => e.montage.blur.y,
        }
    }

    pub fn set(self, e: &mut Engine, v: f32) {
        match self {
            Knob::Strength => e.grade.strength = v,
            Knob::Exposure => e.grade.exposure = v,
            Knob::Contrast => e.grade.contrast = v,
            Knob::Highlights => e.grade.highlights = v,
            Knob::Shadows => e.grade.shadows = v,
            Knob::Warmth => e.grade.temperature = v,
            Knob::Tint => e.grade.tint = v,
            Knob::Saturation => e.grade.saturation = v,
            Knob::Vibrance => e.grade.vibrance = v,
            Knob::TealOrange => e.grade.teal_orange = v,
            Knob::Fade => e.grade.fade = v,
            Knob::Sharpen => e.finish.sharpen = v,
            Knob::Denoise => e.finish.denoise = v,
            Knob::BlurZoom => e.blur.zoom = v,
            Knob::BlurX => e.blur.x = v,
            Knob::BlurY => e.blur.y = v,
            Knob::ManualX => e.manual_x = v,
            Knob::OmniAmount => {
                if let Some(l) = &mut e.learned {
                    l.amount = v;
                }
            }
            Knob::MontageZoom => e.montage.blur.zoom = v,
            Knob::MontageX => e.montage.blur.x = v,
            Knob::MontageY => e.montage.blur.y = v,
        }
        if self.is_grade_slider() {
            e.grade_touched();
        }
    }
}

/// The slider widgets, one per knob.
pub struct Knobs {
    states: Vec<(Knob, Entity<SliderState>)>,
}

impl Knobs {
    pub fn new(cx: &mut Context<Shell>) -> Self {
        let states = Knob::ALL
            .into_iter()
            .map(|knob| {
                let (lo, hi) = knob.range();
                let state = cx.new(|_| SliderState::new().min(lo).max(hi).step(0.005).default_value(lo.max(0.0).min(hi)));
                cx.subscribe(&state, move |this: &mut Shell, _, event: &SliderEvent, cx| {
                    // A change made by the user. (Moving a slider from code never sends this.)
                    if let SliderEvent::Change(value) = event {
                        knob.set(&mut this.engine, value.start());
                        cx.notify();
                    }
                })
                .detach();
                (knob, state)
            })
            .collect();
        Self { states }
    }

    /// Moves every slider to what the engine holds, for when a look or the AI changed the grade.
    pub fn sync(&self, engine: &Engine, window: &mut Window, cx: &mut App) {
        for (knob, state) in &self.states {
            let want = knob.get(engine);
            if (state.read(cx).value().start() - want).abs() > 1e-4 {
                state.update(cx, |s, cx| s.set_value(want, window, cx));
            }
        }
    }

    pub fn state(&self, knob: Knob) -> &Entity<SliderState> {
        &self.states.iter().find(|(k, _)| *k == knob).expect("every knob has a slider").1
    }

    /// A label, its number, and the slider under them.
    pub fn row(&self, knob: Knob, engine: &Engine, signed: bool) -> Div {
        let v = knob.get(engine);
        let shown = if signed && v > 0.0 { format!("+{v:.2}") } else { format!("{v:.2}") };
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(div().flex().justify_between().items_center().child(div().text_size(px(13.0)).text_color(colour(TEXT2)).child(knob.label())).child(div().font_family("Consolas").text_size(px(12.0)).text_color(colour(TEXT)).child(shown)))
            .child(Slider::new(self.state(knob)))
    }
}
