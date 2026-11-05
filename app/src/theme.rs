//! The look of the whole app: colours, fonts, icons and the small widgets every
//! screen is built from. The values come from the approved design; nothing else
//! in the program picks a colour or a size on its own.

use std::ops::RangeInclusive;
use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Painter, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};

// ------------------------------------------------------------------ colours

pub const WINDOW: Color32 = Color32::from_rgb(0x0F, 0x10, 0x12);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x17, 0x1A);
pub const CARD: Color32 = Color32::from_rgb(0x1D, 0x1E, 0x22);
pub const INPUT: Color32 = Color32::from_rgb(0x25, 0x26, 0x2B);
pub const RAISED: Color32 = Color32::from_rgb(0x2B, 0x2C, 0x32);
pub const LINE: Color32 = Color32::from_rgb(0x2D, 0x2E, 0x34);
pub const LINE_SOFT: Color32 = Color32::from_rgb(0x2A, 0x2B, 0x30);
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x3A, 0x3B, 0x42);
pub const TRACK: Color32 = Color32::from_rgb(0x33, 0x34, 0x3A);
pub const TEXT: Color32 = Color32::from_rgb(0xED, 0xEB, 0xE7);
pub const TEXT2: Color32 = Color32::from_rgb(0xB4, 0xB1, 0xAA);
pub const TEXT3: Color32 = Color32::from_rgb(0x8F, 0x8C, 0x86);
pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x3D);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xFF, 0x9A, 0x78);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x1B, 0x0C, 0x06);
pub const OK: Color32 = Color32::from_rgb(0x62, 0xD0, 0x8F);
pub const WARN: Color32 = Color32::from_rgb(0xF4, 0xBE, 0x4F);
pub const BAD: Color32 = Color32::from_rgb(0xFF, 0x7B, 0x72);

pub fn accent_soft(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(0xFF, 0x6B, 0x3D, alpha)
}

pub fn tint(colour: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

// -------------------------------------------------------------------- fonts

/// Heavier text. Fonts have no weights here, so bold is a font of its own.
pub fn bold_family() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, bold_family())).color(TEXT)
}

pub fn mono(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, FontFamily::Monospace))
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).size(12.5).color(TEXT3)
}

pub fn soft(text: impl Into<String>) -> RichText {
    RichText::new(text).size(13.0).color(TEXT2)
}

/// Loads the interface fonts. Manrope and JetBrains Mono are used if their files are in
/// `assets/fonts` next to the program; otherwise the system's Segoe UI and Cascadia Mono.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let windows = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from("C:/Windows"), PathBuf::from).join("Fonts");
    let beside_exe = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("assets").join("fonts")));
    let find = |own: &[&str], system: &[&str]| -> Option<Vec<u8>> {
        let own_dirs = [Some(PathBuf::from("assets/fonts")), beside_exe.clone()];
        own.iter().find_map(|f| own_dirs.iter().flatten().find_map(|d| std::fs::read(d.join(f)).ok())).or_else(|| system.iter().find_map(|f| std::fs::read(windows.join(f)).ok()))
    };
    let regular = find(&["Manrope-Medium.ttf", "Manrope-Regular.ttf"], &["segoeui.ttf"]);
    let strong = find(&["Manrope-ExtraBold.ttf", "Manrope-Bold.ttf"], &["seguisb.ttf", "segoeuib.ttf"]);
    let fixed = find(&["JetBrainsMono-Medium.ttf", "JetBrainsMono-Regular.ttf"], &["CascadiaMono.ttf", "consola.ttf"]);
    for (name, bytes) in [("ui", regular), ("ui-bold", strong), ("ui-mono", fixed)] {
        if let Some(bytes) = bytes {
            fonts.font_data.insert(name.into(), egui::FontData::from_owned(bytes).into());
        }
    }
    let defaults_of = |family: &FontFamily| fonts.families.get(family).cloned().unwrap_or_default();
    let (proportional, monospace) = (defaults_of(&FontFamily::Proportional), defaults_of(&FontFamily::Monospace));
    let with = |first: &[&str], rest: &Vec<String>| -> Vec<String> {
        first.iter().filter(|n| fonts.font_data.contains_key(**n)).map(|n| n.to_string()).chain(rest.iter().cloned()).collect()
    };
    let (p, b, m) = (with(&["ui"], &proportional), with(&["ui-bold", "ui"], &proportional), with(&["ui-mono"], &monospace));
    fonts.families.insert(FontFamily::Proportional, p);
    fonts.families.insert(bold_family(), b);
    fonts.families.insert(FontFamily::Monospace, m);
    ctx.set_fonts(fonts);
}

/// Colours, text sizes and spacing for every widget, including the ones egui draws itself.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut v = egui::Visuals::dark();
    v.panel_fill = WINDOW;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, LINE_STRONG);
    v.extreme_bg_color = INPUT;
    v.faint_bg_color = CARD;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = accent_soft(90);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT_TEXT;
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x25, 0x26, 0x2B));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT2);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(10);
    }
    v.widgets.inactive.bg_fill = INPUT;
    v.widgets.inactive.weak_bg_fill = INPUT;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE_STRONG);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.bg_fill = RAISED;
    v.widgets.hovered.weak_bg_fill = RAISED;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, LINE_STRONG);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.active.bg_fill = RAISED;
    v.widgets.active.weak_bg_fill = RAISED;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.open.bg_fill = RAISED;
    v.widgets.open.weak_bg_fill = RAISED;
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(14.0, 8.0);
        s.spacing.interact_size.y = 36.0;
        s.spacing.combo_height = 300.0;
        s.text_styles.insert(egui::TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(egui::TextStyle::Button, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(egui::TextStyle::Small, FontId::new(12.5, FontFamily::Proportional));
        s.text_styles.insert(egui::TextStyle::Heading, FontId::new(18.0, bold_family()));
        s.text_styles.insert(egui::TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace));
    });
}

// -------------------------------------------------------------------- icons

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Icon {
    Film,
    Sliders,
    Export,
    Send,
    Check,
    Plus,
    Gear,
    Play,
    Pause,
    Sparkle,
    Bolt,
    Folder,
    Chevron,
    ArrowRight,
    Upload,
    Close,
    Warn,
    Music,
    Book,
    Beat,
}

/// Each icon is a few lines on a 24 by 24 grid, drawn with round-ended strokes.
fn lines_of(icon: Icon) -> Vec<Vec<(f32, f32)>> {
    let l = |pts: &[(f32, f32)]| pts.to_vec();
    match icon {
        Icon::Film => vec![l(&[(4., 5.), (20., 5.), (20., 19.), (4., 19.), (4., 5.)]), l(&[(8., 5.), (8., 19.)]), l(&[(16., 5.), (16., 19.)]), l(&[(4., 9.), (8., 9.)]), l(&[(4., 15.), (8., 15.)]), l(&[(16., 9.), (20., 9.)]), l(&[(16., 15.), (20., 15.)])],
        Icon::Sliders => vec![l(&[(4., 7.), (13., 7.)]), l(&[(17., 7.), (20., 7.)]), l(&[(4., 17.), (7., 17.)]), l(&[(11., 17.), (20., 17.)]), l(&[(13., 4.), (13., 10.)]), l(&[(7., 14.), (7., 20.)])],
        Icon::Export => vec![l(&[(12., 4.), (12., 15.)]), l(&[(7., 10.), (12., 15.), (17., 10.)]), l(&[(5., 20.), (19., 20.)])],
        Icon::Send => vec![l(&[(3., 11.), (21., 3.), (13., 21.), (10.5, 13.5), (3., 11.)])],
        Icon::Check => vec![l(&[(5., 12.5), (9.5, 17.), (19., 7.5)])],
        Icon::Plus => vec![l(&[(12., 5.), (12., 19.)]), l(&[(5., 12.), (19., 12.)])],
        Icon::Gear => vec![l(&[(12., 3.), (12., 6.)]), l(&[(12., 18.), (12., 21.)]), l(&[(3., 12.), (6., 12.)]), l(&[(18., 12.), (21., 12.)]), l(&[(5.6, 5.6), (7.7, 7.7)]), l(&[(16.3, 16.3), (18.4, 18.4)]), l(&[(18.4, 5.6), (16.3, 7.7)]), l(&[(7.7, 16.3), (5.6, 18.4)])],
        Icon::Play | Icon::Pause => Vec::new(),
        Icon::Sparkle => vec![l(&[(12., 3.), (14.1, 8.4), (20., 10.), (14.1, 11.6), (12., 17.), (9.9, 11.6), (4., 10.), (9.9, 8.4), (12., 3.)])],
        Icon::Bolt => vec![l(&[(13., 3.), (5., 14.), (11., 14.), (10., 21.), (18., 10.), (12., 10.), (13., 3.)])],
        Icon::Folder => vec![l(&[(3., 7.), (3., 18.), (21., 18.), (21., 9.), (11., 9.), (9., 6.), (3., 6.), (3., 7.)])],
        Icon::Chevron => vec![l(&[(6., 9.), (12., 15.), (18., 9.)])],
        Icon::ArrowRight => vec![l(&[(5., 12.), (19., 12.)]), l(&[(13., 6.), (19., 12.), (13., 18.)])],
        Icon::Upload => vec![l(&[(12., 16.), (12., 4.)]), l(&[(7., 9.), (12., 4.), (17., 9.)]), l(&[(5., 20.), (19., 20.)])],
        Icon::Close => vec![l(&[(7., 7.), (17., 17.)]), l(&[(17., 7.), (7., 17.)])],
        Icon::Warn => vec![l(&[(12., 7.), (12., 13.)]), l(&[(12., 17.), (12., 17.2)])],
        Icon::Music => vec![l(&[(9., 18.), (9., 6.), (19., 4.), (19., 16.)])],
        Icon::Book => vec![l(&[(5., 4.), (16., 4.), (19., 7.), (19., 20.), (8., 20.), (5., 17.), (5., 4.)]), l(&[(8., 8.), (15., 8.)]), l(&[(8., 12.), (15., 12.)])],
        Icon::Beat => vec![l(&[(4., 12.), (6., 12.)]), l(&[(8., 7.), (8., 17.)]), l(&[(12., 4.), (12., 20.)]), l(&[(16., 8.), (16., 16.)]), l(&[(20., 11.), (20., 13.)])],
    }
}

pub fn paint_icon(painter: &Painter, centre: egui::Pos2, size: f32, icon: Icon, colour: Color32, width: f32) {
    let at = |(x, y): (f32, f32)| centre + vec2((x - 12.0) / 24.0 * size, (y - 12.0) / 24.0 * size);
    let stroke = Stroke::new(width, colour);
    match icon {
        Icon::Play => {
            painter.add(Shape::convex_polygon(vec![at((7., 4.)), at((20., 12.)), at((7., 20.))], colour, Stroke::NONE));
            return;
        }
        Icon::Pause => {
            for x in [7.0, 14.0] {
                painter.rect_filled(Rect::from_min_max(at((x, 5.)), at((x + 3.5, 19.))), 1.0, colour);
            }
            return;
        }
        _ => {}
    }
    for line in lines_of(icon) {
        let points: Vec<_> = line.into_iter().map(at).collect();
        painter.add(Shape::line(points, stroke));
        // Round the ends and corners a little.
    }
    if icon == Icon::Gear {
        painter.circle_stroke(centre, size * 3.0 / 24.0, stroke);
    }
}

/// A square space for an icon, drawn in `colour`.
pub fn icon(ui: &mut Ui, icon: Icon, size: f32, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_icon(ui.painter(), rect.center(), size, icon, colour, 1.75);
}

// ------------------------------------------------------------------- buttons

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Ghost,
}

fn pointer(response: Response) -> Response {
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A button of the three kinds in the design. `min_width` of 0 fits the text.
pub fn button(ui: &mut Ui, text: &str, kind: Kind, min_width: f32) -> Response {
    button_with_icon(ui, text, None, kind, min_width)
}

pub fn button_with_icon(ui: &mut Ui, text: &str, trailing: Option<Icon>, kind: Kind, min_width: f32) -> Response {
    let (height, size, radius) = if kind == Kind::Primary { (48.0, 15.0, 12) } else { (40.0, 13.5, 10) };
    let enabled = ui.is_enabled();
    let family = if kind == Kind::Primary { bold_family() } else { FontFamily::Proportional };
    let text_colour = match (kind, enabled) {
        (Kind::Primary, true) => ON_ACCENT,
        (Kind::Primary, false) => tint(ON_ACCENT, 140),
        (Kind::Secondary, true) => TEXT,
        (Kind::Ghost, true) => TEXT2,
        (_, false) => Color32::from_rgb(0x6F, 0x6D, 0x68),
    };
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::new(size, family), text_colour);
    let extra = if trailing.is_some() { 26.0 } else { 0.0 };
    let width = (galley.size().x + extra + if kind == Kind::Ghost { 24.0 } else { 36.0 }).max(min_width);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let (hover, down) = (response.hovered() && enabled, response.is_pointer_button_down_on() && enabled);
    let (fill, stroke) = match (kind, enabled) {
        (Kind::Primary, true) => (if down { Color32::from_rgb(0xE5, 0x59, 0x2B) } else if hover { Color32::from_rgb(0xFF, 0x7D, 0x54) } else { ACCENT }, Stroke::NONE),
        (Kind::Primary, false) => (accent_soft(60), Stroke::NONE),
        (Kind::Secondary, true) => (if hover { RAISED } else { INPUT }, Stroke::new(1.0, LINE_STRONG)),
        (Kind::Secondary, false) => (CARD, Stroke::new(1.0, LINE)),
        (Kind::Ghost, _) => (if hover { CARD } else { Color32::TRANSPARENT }, Stroke::NONE),
    };
    let painter = ui.painter();
    painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
    let centre_x = rect.center().x - extra / 2.0;
    painter.galley(pos2(centre_x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0), galley.clone(), text_colour);
    if let Some(i) = trailing {
        paint_icon(painter, pos2(centre_x + galley.size().x / 2.0 + 18.0, rect.center().y), 16.0, i, text_colour, 2.2);
    }
    if response.has_focus() {
        painter.rect_stroke(rect.expand(2.0), radius + 2, Stroke::new(2.0, ACCENT), StrokeKind::Outside);
    }
    pointer(response)
}

/// A small secondary button, 32 high, for things beside a field.
pub fn small_button(ui: &mut Ui, text: &str) -> Response {
    let enabled = ui.is_enabled();
    let colour = if enabled { TEXT } else { Color32::from_rgb(0x6F, 0x6D, 0x68) };
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::new(12.5, FontFamily::Proportional), colour);
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 24.0, 32.0), Sense::click());
    let hover = response.hovered() && enabled;
    let painter = ui.painter();
    painter.rect(rect, 8, if hover { RAISED } else { INPUT }, Stroke::new(1.0, LINE_STRONG), StrokeKind::Inside);
    painter.galley(rect.center() - galley.size() / 2.0, galley, colour);
    pointer(response)
}

/// A square icon button, 40 by 40.
pub fn icon_button(ui: &mut Ui, glyph: Icon, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
    let hover = response.hovered();
    let painter = ui.painter();
    painter.rect(rect, 10, if hover { RAISED } else { CARD }, Stroke::new(1.0, LINE), StrokeKind::Inside);
    paint_icon(painter, rect.center(), 18.0, glyph, TEXT2, 1.75);
    pointer(response.on_hover_text(label))
}

/// A round primary button with an icon, for play and pause.
pub fn round_button(ui: &mut Ui, glyph: Icon, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 20.0, if response.hovered() { Color32::from_rgb(0xFF, 0x7D, 0x54) } else { ACCENT });
    paint_icon(painter, rect.center(), 16.0, glyph, ON_ACCENT, 2.0);
    pointer(response.on_hover_text(label))
}

// ------------------------------------------------------------- small pieces

/// A rounded label: status, licence, a count.
pub fn pill(ui: &mut Ui, text: &str, colour: Color32, fill: Color32, line: Option<Color32>) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::new(12.5, bold_family()), colour);
    let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x + 22.0, 26.0), Sense::hover());
    let painter = ui.painter();
    painter.rect(rect, 13, fill, line.map_or(Stroke::NONE, |c| Stroke::new(1.0, c)), StrokeKind::Inside);
    painter.galley(rect.center() - galley.size() / 2.0, galley, colour);
}

pub fn status_pill(ui: &mut Ui, text: &str, colour: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::new(12.5, bold_family()), colour);
    let (rect, _) = ui.allocate_exact_size(vec2(galley.size().x + 40.0, 30.0), Sense::hover());
    let painter = ui.painter();
    painter.rect(rect, 15, tint(colour, 30), Stroke::new(1.0, tint(colour, 80)), StrokeKind::Inside);
    painter.circle_filled(pos2(rect.left() + 15.0, rect.center().y), 3.5, colour);
    painter.galley(pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0), galley, colour);
}

/// A group of controls on a card.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new().fill(CARD).stroke(Stroke::new(1.0, LINE_SOFT)).corner_radius(12).inner_margin(Margin::same(14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    }).inner
}

/// A card with a different fill and outline, for the selected or highlighted one.
pub fn card_with<R>(ui: &mut Ui, fill: Color32, line: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new().fill(fill).stroke(Stroke::new(1.0, line)).corner_radius(12).inner_margin(Margin::same(14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    }).inner
}

/// Options in a row; the chosen one is raised. Returns true when the choice changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, items: &[(T, &str)], full_width: bool) -> bool {
    let height = 36.0;
    let galleys: Vec<_> = items
        .iter()
        .map(|(v, label)| {
            let (family, colour) = if *v == *value { (bold_family(), TEXT) } else { (FontFamily::Proportional, TEXT2) };
            ui.painter().layout_no_wrap(label.to_string(), FontId::new(13.0, family), colour)
        })
        .collect();
    let total = if full_width { ui.available_width() } else { galleys.iter().map(|g| g.size().x + 32.0).sum::<f32>() + 6.0 + 2.0 * (items.len() as f32 - 1.0) };
    let (outer, _) = ui.allocate_exact_size(vec2(total, height + 6.0), Sense::hover());
    ui.painter().rect(outer, 11, CARD, Stroke::new(1.0, LINE), StrokeKind::Inside);
    let inner = outer.shrink(3.0);
    let item_w = |i: usize| if full_width { (inner.width() - 2.0 * (items.len() as f32 - 1.0)) / items.len() as f32 } else { galleys[i].size().x + 32.0 };
    let mut x = inner.left();
    let mut changed = false;
    for (i, (v, label)) in items.iter().enumerate() {
        let rect = Rect::from_min_size(pos2(x, inner.top()), vec2(item_w(i), height));
        let response = ui.interact(rect, ui.scope_id().with(("seg", i, *label)), Sense::click());
        let selected = *v == *value;
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 8, RAISED);
        } else if response.hovered() {
            painter.rect_filled(rect, 8, tint(RAISED, 120));
        }
        painter.galley(rect.center() - galleys[i].size() / 2.0, galleys[i].clone(), if selected { TEXT } else { TEXT2 });
        if pointer(response).clicked() && !selected {
            *value = *v;
            changed = true;
        }
        x += rect.width() + 2.0;
    }
    changed
}

/// An on/off switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 11, if *on { ACCENT } else { TRACK });
    let x = if *on { rect.right() - 11.0 } else { rect.left() + 11.0 };
    painter.circle_filled(pos2(x, rect.center().y), 8.0, if *on { ON_ACCENT } else { TEXT3 });
    pointer(response)
}

pub fn toggle_row(ui: &mut Ui, on: &mut bool, label: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = toggle(ui, on).changed();
        ui.label(RichText::new(label).size(13.0).color(TEXT2));
    });
    changed
}

/// A tick box with a label.
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> bool {
    let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::new(14.0, FontFamily::Proportional), if *on { TEXT } else { TEXT2 });
    let (rect, response) = ui.allocate_exact_size(vec2(30.0 + galley.size().x, 24.0), Sense::click());
    let box_rect = Rect::from_min_size(pos2(rect.left(), rect.center().y - 10.0), vec2(20.0, 20.0));
    let painter = ui.painter();
    if *on {
        painter.rect_filled(box_rect, 6, ACCENT);
        paint_icon(painter, box_rect.center(), 14.0, Icon::Check, ON_ACCENT, 3.0);
    } else {
        painter.rect_stroke(box_rect, 6, Stroke::new(2.0, Color32::from_rgb(0x4A, 0x4B, 0x53)), StrokeKind::Inside);
    }
    painter.galley(pos2(box_rect.right() + 10.0, rect.center().y - galley.size().y / 2.0), galley, TEXT);
    let clicked = pointer(response).clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

/// The round dot of a radio choice, filled when chosen.
pub fn radio_dot(painter: &Painter, centre: egui::Pos2, selected: bool) {
    painter.circle_stroke(centre, 9.0, Stroke::new(2.0, if selected { ACCENT } else { Color32::from_rgb(0x4A, 0x4B, 0x53) }));
    if selected {
        painter.circle_filled(centre, 4.5, ACCENT);
    }
}

/// A slider the full width of its space. The coloured part runs from `origin` to the handle.
pub fn slider(ui: &mut Ui, value: &mut f32, range: RangeInclusive<f32>, origin: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    let before = *value;
    if let Some(p) = response.interact_pointer_pos().filter(|_| response.dragged() || response.clicked()) {
        let t = ((p.x - rect.left() - 8.0) / (rect.width() - 16.0)).clamp(0.0, 1.0);
        *value = lo + t * (hi - lo);
    }
    let x_of = |v: f32| rect.left() + 8.0 + (v - lo) / (hi - lo) * (rect.width() - 16.0);
    let (a, b) = (x_of(origin.clamp(lo, hi)), x_of(*value));
    let painter = ui.painter();
    let mid = rect.center().y;
    painter.rect_filled(Rect::from_min_max(pos2(rect.left() + 8.0, mid - 2.0), pos2(rect.right() - 8.0, mid + 2.0)), 2, TRACK);
    painter.rect_filled(Rect::from_min_max(pos2(a.min(b), mid - 2.0), pos2(a.max(b), mid + 2.0)), 2, ACCENT);
    painter.circle_filled(pos2(b, mid + 1.0), 8.0, Color32::from_black_alpha(90));
    painter.circle_filled(pos2(b, mid), 8.0, TEXT);
    pointer(response);
    *value != before
}

/// A label with its value, and the slider under them.
pub fn slider_row(ui: &mut Ui, label: &str, value: &mut f32, range: RangeInclusive<f32>, origin: f32, signed: bool) -> bool {
    let shown = if signed && *value > 0.0 { format!("+{:.2}", *value) } else { format!("{:.2}", *value) };
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.0).color(TEXT2));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(mono(shown, 12.0).color(TEXT)));
    });
    ui.spacing_mut().item_spacing.y = 3.0;
    let changed = slider(ui, value, range, origin);
    ui.spacing_mut().item_spacing.y = 8.0;
    changed
}

/// A step of a check list: a round icon, a word, and the sentence. Never colour alone.
pub fn status_row(ui: &mut Ui, level: Level, text: &str) {
    let (colour, glyph, word) = match level {
        Level::Good => (OK, Icon::Check, "OK"),
        Level::Warn => (WARN, Icon::Warn, "Check"),
        Level::Bad => (BAD, Icon::Close, "Fix"),
    };
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 10.0, tint(colour, 40));
        paint_icon(ui.painter(), rect.center(), 12.0, glyph, colour, 2.6);
        ui.add_sized([40.0, 20.0], egui::Label::new(RichText::new(word).font(FontId::new(11.5, bold_family())).color(colour)));
        ui.add(egui::Label::new(RichText::new(text).size(13.0).color(Color32::from_rgb(0xD8, 0xD5, 0xCE))).wrap());
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Good,
    Warn,
    Bad,
}

/// A progress ring with a count in the middle, like "7/8".
pub fn ring(ui: &mut Ui, good: usize, total: usize) {
    let (rect, _) = ui.allocate_exact_size(vec2(76.0, 76.0), Sense::hover());
    let painter = ui.painter();
    let (centre, radius) = (rect.center(), 31.0);
    painter.circle_stroke(centre, radius, Stroke::new(7.0, TRACK));
    let fraction = if total == 0 { 0.0 } else { good as f32 / total as f32 };
    let steps = 64;
    let upto = (steps as f32 * fraction).round() as usize;
    if upto > 0 {
        let points: Vec<_> = (0..=upto).map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * (i as f32 / steps as f32);
            centre + vec2(a.cos(), a.sin()) * radius
        }).collect();
        painter.add(Shape::line(points, Stroke::new(7.0, if good == total { OK } else { WARN })));
    }
    painter.text(centre, Align2::CENTER_CENTER, format!("{good}/{total}"), FontId::new(18.0, bold_family()), TEXT);
}
