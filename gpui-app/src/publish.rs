//! The Publish step: the words on the left (titles, description, tags), and on the right the thumbnails,
//! how the words on them look, the AI design, and the search check.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ve_core::engine::Engine;
use ve_core::media::Frame;
use ve_core::pack::{self, Level, TextStyle, ThumbShape};
use ve_core::publish::Target;
use ve_core::thumb_ai;

use crate::controls::{disabled, picture, primary_button, secondary_button, segmented, small_button, switch};
use crate::theme::{self, ACCENT, ACCENT_TEXT, BAD, CARD, INPUT, LINE, LINE_SOFT, LINE_STRONG, OK, PANEL, TEXT, TEXT2, TEXT3, WARN, accent_soft, colour, tint};
use crate::{Drag, Shell, Step};

const RIGHT: f32 = 440.0;

/// The sliders in the thumbnail card. They always act on the thumbnail being looked at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PubKnob {
    TextX,
    TextY,
    TextSize,
    TextOutline,
    TextShade,
    CropX,
}

impl PubKnob {
    const ALL: [PubKnob; 6] = [PubKnob::TextX, PubKnob::TextY, PubKnob::TextSize, PubKnob::TextOutline, PubKnob::TextShade, PubKnob::CropX];

    fn range(self) -> (f32, f32) {
        match self {
            PubKnob::TextX | PubKnob::TextY | PubKnob::TextShade | PubKnob::CropX => (0.0, 1.0),
            PubKnob::TextSize => (0.4, 1.8),
            PubKnob::TextOutline => (0.0, 2.5),
        }
    }

    fn label(self) -> &'static str {
        match self {
            PubKnob::TextX => "Left / right",
            PubKnob::TextY => "Up / down",
            PubKnob::TextSize => "Size",
            PubKnob::TextOutline => "Outline thickness",
            PubKnob::TextShade => "Shade behind the words",
            PubKnob::CropX => "Where to cut the tall picture (left / right)",
        }
    }

    fn get(self, style: &TextStyle, crop: f32) -> f32 {
        match self {
            PubKnob::TextX => style.x,
            PubKnob::TextY => style.y,
            PubKnob::TextSize => style.size,
            PubKnob::TextOutline => style.outline_width,
            PubKnob::TextShade => style.shade,
            PubKnob::CropX => crop,
        }
    }
}

/// The text boxes, sliders and pictures of the Publish step.
pub struct PublishUi {
    topic: Entity<InputState>,
    titles: Vec<Entity<InputState>>,
    intro: Entity<TextareaState>,
    words: Entity<InputState>,
    note: Entity<InputState>,
    knobs: Vec<(PubKnob, Entity<SliderState>)>,
    wide_img: Option<Arc<RenderImage>>,
    tall_img: Option<Arc<RenderImage>>,
    seen_pack: u64,
    seen_thumb: u64,
    seen_target: Target,
    pub view_tall: bool,
    pub confirm_ai: bool,
    /// Where the thumbnail picture is on screen, as of the last draw, so a drag on it can be turned into a place on the picture.
    pic_bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl PublishUi {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>, engine: &Engine) -> Self {
        let topic = cx.new(|cx| InputState::new(window, cx).placeholder("for example Valorant").default_value(engine.settings.pack_topic.clone()));
        cx.subscribe(&topic, |this: &mut Shell, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.engine.settings.pack_topic = state.read(cx).value().to_string();
            }
        })
        .detach();
        let titles = (0..3)
            .map(|i| {
                let state = cx.new(|cx| InputState::new(window, cx));
                cx.subscribe(&state, move |this: &mut Shell, state, event: &InputEvent, cx| {
                    if let InputEvent::Change = event
                        && let Some(p) = &mut this.engine.publish.pack
                        && let Some(t) = p.titles.get_mut(i)
                    {
                        *t = state.read(cx).value().to_string();
                        cx.notify();
                    }
                })
                .detach();
                state
            })
            .collect();
        let intro = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8));
        cx.subscribe(&intro, |this: &mut Shell, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event
                && let Some(p) = &mut this.engine.publish.pack
            {
                p.intro = state.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        let words = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe(&words, |this: &mut Shell, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event
                && let Some(p) = &mut this.engine.publish.pack
            {
                p.thumb_text = state.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("for example: make the explosion bigger"));
        cx.subscribe(&note, |this: &mut Shell, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event
                && let Some(p) = &mut this.engine.publish.pack
            {
                p.thumb_note = state.read(cx).value().to_string();
            }
        })
        .detach();
        let knobs = PubKnob::ALL
            .into_iter()
            .map(|knob| {
                let (lo, hi) = knob.range();
                let state = cx.new(|_| SliderState::new().min(lo).max(hi).step(0.005).default_value(lo));
                cx.subscribe(&state, move |this: &mut Shell, _, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(v) => {
                        this.set_pub_knob(knob, v.start());
                        cx.notify();
                    }
                    // Let go: draw the plain thumbnail again with the new look.
                    SliderEvent::Release(_) => this.restyle(),
                })
                .detach();
                (knob, state)
            })
            .collect();
        Self { topic, titles, intro, words, note, knobs, wide_img: None, tall_img: None, seen_pack: 0, seen_thumb: 0, seen_target: Target::Single, view_tall: false, confirm_ai: false, pic_bounds: Rc::new(Cell::new(Bounds::default())) }
    }

    fn knob_state(&self, knob: PubKnob) -> &Entity<SliderState> {
        &self.knobs.iter().find(|(k, _)| *k == knob).expect("every knob has a slider").1
    }
}

fn decode(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let rgb = image::load_from_memory(bytes).ok()?.to_rgb8();
    Some(picture(&Frame { width: rgb.width() as usize, height: rgb.height() as usize, rgb: rgb.into_raw() }))
}

impl Shell {
    pub fn view_shape(&self) -> ThumbShape {
        if self.publish_ui.view_tall { ThumbShape::Tall } else { ThumbShape::Wide }
    }

    /// Whether the thumbnail being looked at was designed by the AI (and so isn't redrawn when the look of the words changes).
    fn designed(&self) -> bool {
        self.engine.publish.pack.as_ref().is_some_and(|p| if self.publish_ui.view_tall { p.ai_tall.is_some() } else { p.ai_wide.is_some() })
    }

    /// Keeps the text boxes and the pictures in step with the engine's pack.
    pub fn sync_publish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Showing the other video's pack: everything is refilled, whatever the counters say.
        if self.engine.target != self.publish_ui.seen_target {
            self.publish_ui.seen_target = self.engine.target;
            (self.publish_ui.seen_pack, self.publish_ui.seen_thumb) = (u64::MAX, u64::MAX);
        }
        let st = &self.engine.publish;
        let ui = &mut self.publish_ui;
        if st.pack_gen != ui.seen_pack {
            ui.seen_pack = st.pack_gen;
            let pack = st.pack.clone();
            ui.view_tall = pack.as_ref().is_some_and(|p| p.format == pack::PackFormat::Short);
            for (i, state) in ui.titles.iter().enumerate() {
                let text = pack.as_ref().and_then(|p| p.titles.get(i).cloned()).unwrap_or_default();
                state.update(cx, |s, cx| s.set_value(text, window, cx));
            }
            let (intro, words, note) = pack.as_ref().map_or_else(Default::default, |p| (p.intro.clone(), p.thumb_text.clone(), p.thumb_note.clone()));
            ui.intro.update(cx, |s, cx| s.set_value(intro, window, cx));
            ui.words.update(cx, |s, cx| s.set_value(words, window, cx));
            ui.note.update(cx, |s, cx| s.set_value(note, window, cx));
        }
        if st.thumb_gen != ui.seen_thumb {
            ui.seen_thumb = st.thumb_gen;
            for old in [ui.wide_img.take(), ui.tall_img.take()].into_iter().flatten() {
                let _ = window.drop_image(old);
            }
            ui.wide_img = st.thumb.as_deref().and_then(decode);
            ui.tall_img = st.thumb_tall.as_deref().and_then(decode);
        }
        // The sliders show the look of the thumbnail being viewed.
        if let Some(pack) = &st.pack {
            let (style, crop) = (pack.style(if ui.view_tall { ThumbShape::Tall } else { ThumbShape::Wide }), pack.thumb_x.unwrap_or(0.5));
            for (knob, state) in &ui.knobs {
                let want = knob.get(&style, crop);
                if (state.read(cx).value().start() - want).abs() > 1e-4 {
                    state.update(cx, |s, cx| s.set_value(want, window, cx));
                }
            }
        }
    }

    fn set_pub_knob(&mut self, knob: PubKnob, v: f32) {
        let shape = self.view_shape();
        let Some(pack) = &mut self.engine.publish.pack else { return };
        if knob == PubKnob::CropX {
            pack.thumb_x = Some(v);
            return;
        }
        let mut style = pack.style(shape);
        match knob {
            PubKnob::TextX => style.x = v,
            PubKnob::TextY => style.y = v,
            PubKnob::TextSize => style.size = v,
            PubKnob::TextOutline => style.outline_width = v,
            PubKnob::TextShade => style.shade = v,
            PubKnob::CropX => {}
        }
        pack.set_style(shape, style);
    }

    /// Changes the look of the words for the thumbnail being viewed, and draws it again.
    fn edit_style(&mut self, change: impl FnOnce(&mut TextStyle)) {
        let shape = self.view_shape();
        if let Some(pack) = &mut self.engine.publish.pack {
            let mut style = pack.style(shape);
            change(&mut style);
            pack.set_style(shape, style);
        }
        self.restyle();
    }

    /// Draws the plain thumbnail again after the look of the words changed. A thumbnail the AI designed is left alone.
    pub fn restyle(&mut self) {
        if !self.designed() && !self.engine.publish.thumb_busy_for(self.view_shape()) {
            let shape = self.view_shape();
            self.engine.redraw_thumbnails(vec![shape]);
        }
    }

    /// Moves the words to where the pointer is on the picture.
    pub fn drag_words(&mut self, x: f32, y: f32) {
        let b = self.publish_ui.pic_bounds.get();
        let (left, top, w, h) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
        if w < 1.0 || h < 1.0 {
            return;
        }
        let (px, py) = (((x - left) / w).clamp(0.0, 1.0), ((y - top) / h).clamp(0.0, 1.0));
        let shape = self.view_shape();
        if let Some(pack) = &mut self.engine.publish.pack {
            let mut style = pack.style(shape);
            (style.x, style.y) = (px, py);
            pack.set_style(shape, style);
        }
    }

    // --------------------------------------------------------------------------- left side

    fn titles_card(&self, cx: &mut Context<Self>) -> Div {
        let Some(pack) = &self.engine.publish.pack else { return div() };
        let good_len = if pack.format == pack::PackFormat::Short { 50 } else { 60 };
        let mut card = theme::card().flex().flex_col().gap(px(8.0)).child(div().flex().items_baseline().gap(px(10.0)).child(theme::heading("Titles", 14.0)).child(theme::muted("Pick one and edit it freely")));
        for (i, title) in pack.titles.iter().enumerate().take(3) {
            let n = title.chars().count();
            let hex = if n == 0 || n > pack::MAX_TITLE { BAD } else if pack::title_length(title) <= good_len { OK } else { WARN };
            let chosen = pack.title_pick == i;
            let text = title.clone();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .id(("title-pick", i))
                            .size(px(24.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(div().size(px(18.0)).rounded_full().border_2().border_color(colour(if chosen { ACCENT } else { 0x4A4B53 })).flex().items_center().justify_center().when(chosen, |s| s.child(div().size(px(8.0)).rounded_full().bg(colour(ACCENT)))))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(p) = &mut this.engine.publish.pack {
                                    p.title_pick = i;
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().min_w(px(0.0)).child(Input::new(&self.publish_ui.titles[i])))
                    .child(theme::pill(format!("{n}/{}", pack::MAX_TITLE), hex, tint(hex, 0.12)))
                    .child(small_button(("title-copy", i), "Copy").on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        this.engine.note_copied();
                        cx.notify();
                    }))),
            );
        }
        card
    }

    fn description_card(&self, cx: &mut Context<Self>) -> Div {
        let Some(pack) = &self.engine.publish.pack else { return div() };
        let mut chapters = div().flex().flex_col().gap(px(4.0));
        if pack.chapters.is_empty() {
            chapters = chapters.child(theme::muted("Too short for chapters (YouTube needs 3, each 10 s or more)."));
        }
        for c in &pack.chapters {
            chapters = chapters.child(div().flex().gap(px(10.0)).child(div().font_family("Consolas").text_size(px(12.5)).text_color(colour(ACCENT_TEXT)).child(pack::stamp(c.at))).child(theme::soft(c.title.clone())));
        }
        theme::card()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div().flex().items_center().justify_between().child(theme::heading("Description", 14.0)).child(small_button("copy-description", "Copy description").on_click(cx.listener(|this, _, _, cx| {
                    if let Some(p) = &this.engine.publish.pack {
                        cx.write_to_clipboard(ClipboardItem::new_string(p.description()));
                    }
                    this.engine.note_copied();
                    cx.notify();
                }))),
            )
            .child(theme::muted("The first two lines show in search, so the intro leads with what people look for"))
            .child(Textarea::new(&self.publish_ui.intro))
            .child(theme::heading(format!("Chapters ({})", pack.chapters.len()), 13.0))
            .child(chapters)
            .when(!pack.credit.trim().is_empty(), |c| c.child(theme::muted(format!("Music credit added: {}", pack.credit.lines().next().unwrap_or("")))))
    }

    fn tags_card(&self, cx: &mut Context<Self>) -> Div {
        let Some(pack) = &self.engine.publish.pack else { return div() };
        let pills = |items: &[String], fg: u32, bg: Hsla| {
            let mut row = div().flex().flex_wrap().gap(px(6.0));
            for item in items {
                row = row.child(theme::pill(item.clone(), fg, bg));
            }
            row
        };
        let tags = pack.tags.join(", ");
        theme::card()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(theme::heading("Keywords and tags", 14.0))
            .when(!pack.primary_keyword.is_empty(), |c| {
                c.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.0))
                        .child(theme::muted("Focus"))
                        .child(theme::pill(pack.primary_keyword.clone(), ACCENT_TEXT, accent_soft(0.14)))
                        .when(!pack.secondary_keywords.is_empty(), |s| s.child(theme::muted("Related")).child(pills(&pack.secondary_keywords, TEXT2, colour(INPUT)))),
                )
            })
            .when(!pack.hashtags.is_empty(), |c| c.child(div().flex().flex_wrap().items_center().gap(px(8.0)).child(theme::muted("Hashtags")).child(pills(&pack.hashtags, TEXT2, colour(INPUT)))))
            .when(!pack.tags.is_empty(), |c| {
                c.child(theme::muted(format!("Hidden tags ({})", pack.tags.len()))).child(theme::soft(tags.clone())).child(small_button("copy-tags", "Copy tags").on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(tags.clone()));
                    this.engine.note_copied();
                    cx.notify();
                })))
            })
    }

    fn write_card(&self, cx: &mut Context<Self>) -> Div {
        let e = &self.engine;
        let have = e.publish.pack.is_some();
        let montage = e.target == Target::Montage;
        let ready = if montage { e.montage_facts().is_some() } else { e.source.is_some() };
        let busy = e.publish.busy.clone();
        let can = ready && e.has_openai && busy.is_none();
        let write = {
            let b = primary_button("write-pack", if have { "Write it again" } else { "Write my upload pack" });
            if can {
                b.on_click(cx.listener(|this, _, _, cx| {
                    this.engine.start_pack();
                    cx.notify();
                }))
            } else {
                disabled(b)
            }
        };
        theme::card()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(theme::heading("Tell the AI what this is", 14.0))
            .child(div().flex().items_center().gap(px(10.0)).child(theme::soft("Game or topic")).child(div().w(px(240.0)).child(Input::new(&self.publish_ui.topic))))
            .child(segmented("pack-format", &[(pack::PackFormat::Standard, "Standard video"), (pack::PackFormat::Short, "YouTube Short")], e.settings.pack_format, false, cx, |this, v| this.engine.settings.pack_format = v))
            .child(div().flex().items_center().gap(px(12.0)).child(write).when_some(busy, |s, text| s.child(theme::soft(text))))
            .when(!e.has_openai, |c| c.child(self.key_hint("Needs your OpenAI key.", "pack-key", cx)))
            .when(!ready, |c| c.child(theme::muted(if montage { "Plan the montage first." } else { "Open a video first." })))
            .when(ready && montage && e.montage.rendered.is_empty(), |c| c.child(theme::muted("The chapter times are estimates until you make the montage.")))
    }

    // -------------------------------------------------------------------------- right side

    fn swatches(&self, id: &'static str, label: &str, current: [u8; 3], presets: &[[u8; 3]], cx: &mut Context<Self>, apply: impl Fn(&mut TextStyle, [u8; 3]) + Clone + 'static) -> Div {
        let mut row = div().flex().items_center().gap(px(8.0)).child(div().w(px(56.0)).child(theme::soft(label.to_string())));
        for (i, preset) in presets.iter().enumerate() {
            let preset = *preset;
            let apply = apply.clone();
            let on = current == preset;
            row = row.child(
                div()
                    .id((id, i))
                    .size(px(26.0))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(on, |s| s.border_2().border_color(colour(ACCENT)))
                    .when(!on, |s| s.border_1().border_color(colour(LINE_STRONG)))
                    .child(div().size(px(18.0)).rounded_full().bg(rgb(u32::from(preset[0]) << 16 | u32::from(preset[1]) << 8 | u32::from(preset[2]))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_style(|s| apply(s, preset));
                        cx.notify();
                    })),
            );
        }
        row
    }

    fn knob_row(&self, knob: PubKnob) -> Div {
        let (style, crop) = self.engine.publish.pack.as_ref().map_or((TextStyle::default_for(ThumbShape::Wide), 0.5), |p| (p.style(self.view_shape()), p.thumb_x.unwrap_or(0.5)));
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(div().flex().justify_between().child(div().text_size(px(13.0)).text_color(colour(TEXT2)).child(knob.label())).child(div().font_family("Consolas").text_size(px(12.0)).child(format!("{:.2}", knob.get(&style, crop)))))
            .child(Slider::new(self.publish_ui.knob_state(knob)))
    }

    fn thumbnail_card(&self, cx: &mut Context<Self>) -> Div {
        let Some(pack) = &self.engine.publish.pack else { return div() };
        let st = &self.engine.publish;
        let tall = self.publish_ui.view_tall;
        let shape = self.view_shape();
        let style = pack.style(shape);
        let designed = self.designed();
        let busy = st.thumb_busy || st.tall_busy || st.ai_busy.is_some();
        let (w, h) = if tall { (420.0 * 9.0 / 16.0, 420.0) } else { (RIGHT - 40.0 - 28.0, (RIGHT - 40.0 - 28.0) * 9.0 / 16.0) };
        let img = if tall { &self.publish_ui.tall_img } else { &self.publish_ui.wide_img };
        let bounds = self.publish_ui.pic_bounds.clone();
        let words_on = !pack.thumb_text.trim().is_empty();
        let guide = if self.drag == Some(Drag::Words) || (!designed && words_on) {
            // The box around the words, where they will be drawn.
            let layout = pack::text_layout(&pack.thumb_text, shape, &style);
            let (cx_, cy_) = pack::text_centre(&layout, shape, &style);
            let (out_w, out_h) = (shape.size().0 as f32, shape.size().1 as f32);
            let (gw, gh) = (layout.width / out_w * w, layout.height / out_h * h);
            Some(div().absolute().left(px(cx_ / out_w * w - gw / 2.0 - 4.0)).top(px(cy_ / out_h * h - gh / 2.0 - 4.0)).w(px(gw + 8.0)).h(px(gh + 8.0)).rounded(px(4.0)).border_1().border_color(colour(ACCENT)))
        } else {
            None
        };
        let mut picture = div().relative().w(px(w)).h(px(h)).rounded(px(10.0)).overflow_hidden().bg(colour(INPUT));
        picture = match img {
            Some(img) => picture.child(gpui_kit::img(img.clone()).w(px(w)).h(px(h))),
            None => picture.child(div().size_full().flex().items_center().justify_center().child(theme::muted(if busy { "Drawing the thumbnail…" } else { "No picture yet" }))),
        };
        if tall {
            // Shorts covers the top and the bottom of the picture with its title and buttons.
            picture = picture
                .child(div().absolute().left(px(0.0)).top(px(0.0)).w(px(w)).h(px(h * 0.10)).bg(rgba(0xFF463C2E)))
                .child(div().absolute().left(px(0.0)).bottom(px(0.0)).w(px(w)).h(px(h * 0.25)).bg(rgba(0xFF463C2E)).flex().items_center().justify_center().text_size(px(11.5)).text_color(rgba(0xFFC8BEC8)).child("Shorts covers this"));
        }
        if !designed && words_on {
            picture = picture.cursor_grab();
        }
        // The canvas reports where the picture is on screen.
        picture = picture.child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().top(px(0.0)).left(px(0.0)).size_full());
        picture = picture.children(guide.filter(|_| !designed));
        let picture = div().id("thumb-picture").flex().justify_center().py(px(8.0)).rounded(px(10.0)).bg(colour(INPUT)).child(picture.on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            if !this.designed() && this.engine.publish.pack.as_ref().is_some_and(|p| !p.thumb_text.trim().is_empty()) {
                this.drag = Some(Drag::Words);
                this.drag_words(f32::from(event.position.x), f32::from(event.position.y));
                cx.notify();
            }
        })));

        let fonts = pack::available_fonts();
        let font_items: Vec<(&'static str, &'static str)> = fonts.iter().map(|f| (f.as_str(), f.as_str())).collect();
        let style_card = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(theme::heading("Words: place, size and colours", 13.5))
            .child(theme::muted(if designed { "This is an AI design. These choices are sent to the AI the next time you press Design again." } else { "Drag the words on the picture, or use the controls. The picture redraws when you let go." }))
            .child(self.knob_row(PubKnob::TextX))
            .child(self.knob_row(PubKnob::TextY))
            .child(self.knob_row(PubKnob::TextSize))
            .child(self.knob_row(PubKnob::TextOutline))
            .child(self.knob_row(PubKnob::TextShade))
            .child(self.swatches("fill", "Words", style.fill, &[[0xFF, 0xE2, 0x00], [0xFF, 0xFF, 0xFF], [0xFF, 0x6B, 0x3D], [0x00, 0xE5, 0xFF], [0x7C, 0xFF, 0x4F], [0xFF, 0x3B, 0x30], [0xFF, 0x4F, 0xB8]], cx, |s, c| s.fill = c))
            .child(self.swatches("outline", "Outline", style.outline, &[[0, 0, 0], [0x0B, 0x1B, 0x3A], [0xFF, 0xFF, 0xFF], [0x4A, 0x00, 0x00]], cx, |s, c| s.outline = c))
            .child(div().flex().items_center().gap(px(8.0)).child(theme::soft("Font")).child(segmented("font", &font_items, fonts.iter().find(|f| **f == style.font).map_or("", |f| f.as_str()), false, cx, |this, v| this.edit_style(|s| s.font = v.to_string()))))
            .child(div().flex().items_center().gap(px(8.0)).child(theme::soft("Lines")).child(segmented("lines", &[(0u8, "Auto"), (1, "One"), (2, "Two")], style.lines, false, cx, |this, v| this.edit_style(|s| s.lines = v))))
            .child(switch("shadow", "Shadow behind the letters", style.shadow, cx, |this| this.edit_style(|s| s.shadow = !s.shadow)))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(small_button("style-reset", "Reset").on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_style(|s| *s = TextStyle::default_for(shape));
                        cx.notify();
                    })))
                    .child(small_button("style-both", "Use this look on both").on_click(cx.listener(move |this, _, _, cx| {
                        let other = if shape == ThumbShape::Wide { ThumbShape::Tall } else { ThumbShape::Wide };
                        let mut redraw = false;
                        if let Some(p) = &mut this.engine.publish.pack {
                            let from = p.style(shape);
                            let mut copy = p.style(other);
                            (copy.size, copy.fill, copy.outline, copy.outline_width, copy.shadow, copy.shade, copy.font, copy.lines) = (from.size, from.fill, from.outline, from.outline_width, from.shadow, from.shade, from.font.clone(), from.lines);
                            p.set_style(other, copy);
                            redraw = if other == ThumbShape::Wide { p.ai_wide.is_none() } else { p.ai_tall.is_none() };
                        }
                        if redraw {
                            this.engine.redraw_thumbnails(vec![other]);
                        }
                        cx.notify();
                    }))),
            );

        let mut chips = div().flex().gap(px(6.0));
        for i in 0..pack.candidates.len().min(8) {
            let on = pack.thumb_pick == i;
            chips = chips.child(
                div()
                    .id(("frame", i))
                    .flex_1()
                    .h(px(34.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.0))
                    .cursor_pointer()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::BOLD)
                    .when(on, |s| s.bg(accent_soft(0.16)).border_2().border_color(colour(ACCENT)).text_color(colour(ACCENT_TEXT)))
                    .when(!on, |s| s.bg(colour(INPUT)).border_1().border_color(colour(LINE_STRONG)).text_color(colour(TEXT2)))
                    .child(format!("{}", i + 1))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(p) = &mut this.engine.publish.pack
                            && p.thumb_pick != i
                        {
                            p.thumb_pick = i;
                            this.engine.redraw_thumbnails(vec![ThumbShape::Wide, ThumbShape::Tall]);
                        }
                        cx.notify();
                    })),
            );
        }

        theme::card()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().flex().items_center().justify_between().child(theme::heading("Thumbnail", 15.0)).child(segmented("view", &[(true, "9:16 Short"), (false, "16:9 video")], tall, false, cx, |this, v| this.publish_ui.view_tall = v)))
            .child(picture)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(if designed { theme::pill("AI designed", ACCENT_TEXT, accent_soft(0.16)) } else { theme::pill("Free draft", TEXT2, colour(INPUT)) })
                    .when(!designed, |s| s.child(theme::muted("Just your frame and words.")))
                    .child(div().flex_1())
                    .child({
                        let b = small_button("design-quick", if designed { "Design again" } else { "Design with AI" });
                        if self.engine.has_openai && !busy { b.on_click(cx.listener(|this, _, _, cx| { this.publish_ui.confirm_ai = true; cx.notify(); })) } else { disabled(b) }
                    }),
            )
            .when(tall, |c| c.child(self.knob_row(PubKnob::CropX)).child(theme::muted("The Short thumbnail is cut out of the wide frame. Slide it to keep the action in.")))
            .when_some(st.thumb_error.clone(), |c, e| c.child(div().text_size(px(12.5)).text_color(colour(BAD)).child(format!("The thumbnail couldn't be drawn: {e}"))))
            .child(
                div().flex().items_center().gap(px(8.0)).child(theme::soft("Words on it")).child(div().flex_1().min_w(px(0.0)).child(Input::new(&self.publish_ui.words))).child({
                    let b = small_button("redraw", "Redraw (free)");
                    if busy { disabled(b) } else { b.on_click(cx.listener(|this, _, _, cx| {
                        this.engine.redraw_thumbnails(vec![ThumbShape::Wide, ThumbShape::Tall]);
                        cx.notify();
                    })) }
                }),
            )
            .child(style_card)
            .child(theme::muted("Pick the frame"))
            .child(chips)
            .when(!pack.reason.is_empty(), |c| c.child(theme::muted(format!("Why this frame: {}", pack.reason))))
    }

    fn ai_card(&self, cx: &mut Context<Self>) -> Div {
        let e = &self.engine;
        let Some(pack) = &e.publish.pack else { return div() };
        let st = &e.publish;
        let busy = st.thumb_busy || st.tall_busy || st.ai_busy.is_some();
        let models: Vec<(usize, &'static str)> = thumb_ai::IMAGE_MODELS.iter().enumerate().map(|(i, (_, n))| (i, n.split(':').next().unwrap_or(n))).collect();
        let qualities: Vec<(usize, &'static str)> = thumb_ai::QUALITIES.iter().enumerate().map(|(i, (_, n))| (i, n.split(':').next().unwrap_or(n))).collect();
        let make = {
            let b = primary_button("design-both", if st.ai_busy.is_some() { "Designing…" } else { "Make both with AI" }).w_full();
            if e.has_openai && !busy { b.on_click(cx.listener(|this, _, _, cx| { this.publish_ui.confirm_ai = true; cx.notify(); })) } else { disabled(b) }
        };
        div()
            .p(px(14.0))
            .rounded(px(12.0))
            .bg(accent_soft(0.08))
            .border_1()
            .border_color(accent_soft(0.28))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(theme::icon("sparkles", 16.0, ACCENT_TEXT))
                    .child(theme::heading("Design both with AI", 14.0))
                    .child(div().flex_1())
                    .when(pack.ai_wide.is_some() || pack.ai_tall.is_some(), |s| s.child(theme::pill("AI designed", ACCENT_TEXT, accent_soft(0.16)))),
            )
            .child(theme::muted("Sends the frame you picked (a 16:9 and a 9:16 version), your words and the game to OpenAI, which designs a thumbnail for each shape. The pictures stay true to your video. It costs money, and you see the real cost afterwards."))
            .child(theme::soft("Anything to show off? (optional)"))
            .child(Input::new(&self.publish_ui.note))
            .child(div().flex().items_center().gap(px(8.0)).child(segmented("thumb-model", &models, e.settings.thumb_model, false, cx, |this, v| this.engine.settings.thumb_model = v)).child(segmented("thumb-quality", &qualities, e.settings.thumb_quality, false, cx, |this, v| this.engine.settings.thumb_quality = v)))
            .child(make)
            .when(!e.has_openai, |c| c.child(theme::muted("Needs your OpenAI key.")))
            .when_some(st.ai_busy.clone(), |c, text| c.child(theme::soft(text)))
            .children(st.ai_warnings.iter().map(|w| div().text_size(px(12.5)).text_color(colour(WARN)).child(w.clone())))
            .when(st.ai_cost > 0.0 && st.ai_busy.is_none(), |c| c.child(theme::muted(format!("This time it cost about ${:.2} (from OpenAI's usage report).", st.ai_cost))))
            .when(st.ai_reused && st.ai_busy.is_none() && st.ai_cost <= 0.0, |c| c.child(theme::muted("Reused your saved pictures. Nothing was charged.")))
    }

    fn checks_card(&self) -> Div {
        let Some(pack) = &self.engine.publish.pack else { return div() };
        let checks = pack.seo_checks();
        let good = checks.iter().filter(|c| c.level == Level::Good).count();
        let all = good == checks.len();
        let hex = if all { OK } else { WARN };
        let mut rows = div().flex().flex_col().gap(px(8.0));
        for check in &checks {
            let (hex, icon, word) = match check.level {
                Level::Good => (OK, "check", "OK"),
                Level::Warn => (WARN, "triangle-alert", "Check"),
                Level::Bad => (BAD, "x", "Fix"),
            };
            rows = rows.child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(div().size(px(20.0)).flex_none().rounded_full().bg(tint(hex, 0.16)).flex().items_center().justify_center().child(theme::icon(icon, 12.0, hex)))
                    .child(div().w(px(40.0)).flex_none().text_size(px(11.5)).font_weight(FontWeight::BOLD).text_color(colour(hex)).child(word))
                    .child(div().flex_1().min_w(px(0.0)).text_size(px(13.0)).text_color(colour(0xD8D5CE)).child(check.text.clone())),
            );
        }
        theme::card().flex().flex_col().gap(px(10.0)).child(
            div()
                .flex()
                .items_center()
                .gap(px(14.0))
                .child(div().size(px(64.0)).flex_none().rounded_full().border_4().border_color(colour(hex)).flex().items_center().justify_center().text_size(px(17.0)).font_weight(FontWeight::BOLD).child(format!("{good}/{}", checks.len())))
                .child(div().flex().flex_col().gap(px(2.0)).child(theme::heading("Search check", 15.0)).child(theme::muted("Updates as you edit")).child(theme::soft(if all { "Everything YouTube likes is in place." } else { "A few things could help it get found." }))),
        ).child(rows)
    }

    pub fn publish_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let have = self.engine.publish.pack.is_some();
        let left = div().id("publish-left").flex_1().min_w(px(0.0)).h_full().overflow_y_scroll().child(
            div()
                .px(px(28.0))
                .pt(px(16.0))
                .pb(px(24.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(div().h(px(36.0)).flex().items_center().gap(px(12.0)).child(theme::heading("Publish", 18.0)).child(theme::soft(if self.engine.target == Target::Montage { "For your montage: titles, description, music credit and thumbnails" } else { "Titles, description and thumbnails (wide and Short) that help people find it" })))
                .child(self.write_card(cx))
                .child(self.titles_card(cx))
                .child(self.description_card(cx))
                .child(self.tags_card(cx)),
        );
        let save = {
            let b = primary_button("save-pack", "Save pack and thumbnails").w_full();
            if have { b.on_click(cx.listener(|this, _, _, cx| { this.engine.save_pack(); cx.notify(); })) } else { disabled(b) }
        };
        let right = div()
            .w(px(RIGHT))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(colour(PANEL))
            .border_l_1()
            .border_color(colour(LINE))
            .child(div().id("publish-right").flex_1().min_h(px(0.0)).overflow_y_scroll().px(px(20.0)).child(div().py(px(16.0)).flex().flex_col().gap(px(10.0)).child(self.thumbnail_card(cx)).child(self.ai_card(cx)).child(self.checks_card()).when(!have, |c| c.child(theme::card().child(theme::soft("Write the upload pack first. The thumbnails and the search check appear here."))))))
            .child(div().px(px(20.0)).py(px(16.0)).border_t_1().border_color(colour(LINE)).child(save));
        div().size_full().flex().child(left).child(right)
    }

    /// Asks before sending the frame and words to OpenAI, which costs real money.
    pub fn ai_thumb_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = &self.engine.settings;
        div()
            .id("ai-overlay")
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
                    .w(px(500.0))
                    .p(px(22.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(theme::heading("Design the thumbnails with OpenAI?", 17.0))
                    .child(theme::soft("This sends these to OpenAI, using your key:"))
                    .child(theme::soft("  •  the frame you picked from your video, as a 16:9 picture and a 9:16 picture"))
                    .child(theme::soft("  •  the words for the thumbnail and the game or topic"))
                    .child(theme::soft("  •  anything you typed under \"Anything to show off\""))
                    .child(theme::soft(format!("Model: {}. Quality: {}.", thumb_ai::IMAGE_MODELS[s.thumb_model.min(1)].1, thumb_ai::QUALITIES[s.thumb_quality.min(2)].1)))
                    .child(theme::soft("OpenAI charges for each of the two pictures, by size and quality (image output costs $30 per million tokens). The app shows the real cost afterwards, taken from OpenAI's own usage report."))
                    .child(theme::soft("If the lettering comes out misspelled, one more try is made automatically, and it is charged too. Asking again with nothing changed reuses the saved pictures and costs nothing."))
                    .child(
                        div()
                            .flex()
                            .gap(px(10.0))
                            .pt(px(6.0))
                            .child(primary_button("ai-send", "Send it").on_click(cx.listener(|this, _, _, cx| {
                                this.publish_ui.confirm_ai = false;
                                this.engine.start_ai_thumbnails();
                                cx.notify();
                            })))
                            .child(secondary_button("ai-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                                this.publish_ui.confirm_ai = false;
                                cx.notify();
                            }))),
                    ),
            )
    }
}

#[allow(dead_code)]
fn unused() -> (u32, u32, Step) {
    (CARD, LINE_SOFT, Step::Publish)
}

#[allow(dead_code)]
fn unused2() -> u32 {
    TEXT + TEXT3
}
