//! The look of the app: the approved design's colours, and the small pieces every screen is built from.
//! The values match `app/src/theme.rs` in the egui front end, so the two look alike.

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::*;

pub const WINDOW: u32 = 0x0F1012;
pub const PANEL: u32 = 0x16171A;
pub const CARD: u32 = 0x1D1E22;
pub const CARD_DEEP: u32 = 0x1A1B1E;
pub const INPUT: u32 = 0x25262B;
pub const RAISED: u32 = 0x2B2C32;
pub const LINE: u32 = 0x2D2E34;
pub const LINE_SOFT: u32 = 0x2A2B30;
pub const LINE_STRONG: u32 = 0x3A3B42;
pub const TEXT: u32 = 0xEDEBE7;
pub const TEXT2: u32 = 0xB4B1AA;
pub const TEXT3: u32 = 0x8F8C86;
pub const ACCENT: u32 = 0xFF6B3D;
pub const ACCENT_TEXT: u32 = 0xFF9A78;
pub const ON_ACCENT: u32 = 0x1B0C06;
pub const OK: u32 = 0x62D08F;
pub const WARN: u32 = 0xF4BE4F;
pub const BAD: u32 = 0xFF7B72;
pub const RAIL_TEXT: u32 = 0x9A978F;

pub fn colour(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// The accent colour at a given strength, for soft fills.
pub fn accent_soft(alpha: f32) -> Hsla {
    let mut c: Hsla = rgb(ACCENT).into();
    c.a = alpha;
    c
}

pub fn tint(hex: u32, alpha: f32) -> Hsla {
    let mut c: Hsla = rgb(hex).into();
    c.a = alpha;
    c
}

/// Makes the component library's own widgets (inputs, sliders, buttons) dark and orange like the rest of the app.
pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    theme.background = colour(WINDOW);
    theme.foreground = colour(TEXT);
    theme.border = colour(LINE_STRONG);
    theme.input = colour(LINE_STRONG);
    theme.muted = colour(INPUT);
    theme.muted_foreground = colour(TEXT3);
    theme.primary = colour(ACCENT);
    theme.primary_hover = colour(0xFF7D54);
    theme.primary_active = colour(0xE5592B);
    theme.primary_foreground = colour(ON_ACCENT);
    theme.ring = colour(ACCENT);
    theme.accent = colour(RAISED);
    theme.accent_foreground = colour(TEXT);
    theme.secondary = colour(INPUT);
    theme.secondary_hover = colour(RAISED);
    theme.secondary_active = colour(RAISED);
    theme.secondary_foreground = colour(TEXT);
    theme.slider_bar = colour(ACCENT);
    theme.slider_thumb = colour(TEXT);
    theme.popover = colour(CARD);
    theme.popover_foreground = colour(TEXT);
    theme.caret = colour(ACCENT);
    theme.selection = accent_soft(0.35);
    theme.scrollbar_thumb = colour(LINE_STRONG);
    theme.title_bar = colour(WINDOW);
    let _ = cx.theme();
}

/// An icon from the bundled Lucide set, such as "film" or "arrow-right".
pub fn icon(name: &str, size: f32, hex: u32) -> Svg {
    svg().path(format!("icons/{name}.svg")).size(px(size)).flex_none().text_color(colour(hex))
}

/// A group of controls on a card.
pub fn card() -> Div {
    div().bg(colour(CARD)).border_1().border_color(colour(LINE_SOFT)).rounded(px(12.0)).p(px(14.0))
}

/// A small rounded label: status, a count.
pub fn pill(text: impl Into<SharedString>, fg: u32, bg: Hsla) -> Div {
    div().px(px(11.0)).h(px(26.0)).flex().items_center().rounded(px(13.0)).bg(bg).text_color(colour(fg)).text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child(text.into())
}

/// The status badge in the top bar: a dot and a word.
pub fn status_pill(text: impl Into<SharedString>, hex: u32) -> Div {
    div()
        .h(px(30.0))
        .px(px(13.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(15.0))
        .bg(tint(hex, 0.12))
        .border_1()
        .border_color(tint(hex, 0.31))
        .text_color(colour(hex))
        .text_size(px(12.5))
        .font_weight(FontWeight::SEMIBOLD)
        .child(div().size(px(7.0)).rounded_full().bg(colour(hex)))
        .child(text.into())
}

pub fn muted(text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.5)).text_color(colour(TEXT3)).child(text.into())
}

pub fn soft(text: impl Into<SharedString>) -> Div {
    div().text_size(px(13.0)).text_color(colour(TEXT2)).child(text.into())
}

pub fn heading(text: impl Into<SharedString>, size: f32) -> Div {
    div().text_size(px(size)).font_weight(FontWeight::BOLD).text_color(colour(TEXT)).child(text.into())
}
