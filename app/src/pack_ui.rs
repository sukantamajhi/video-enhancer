//! The "Upload pack" panel, shared by the single-video and montage screens: the titles, description and hashtags,
//! the thumbnail with its look, and the search check. Only the drawing is here. Writing the pack, drawing and designing
//! the thumbnails and saving all happen in `ve_core::publish`, which both front ends share.

use eframe::egui::{self, Frame, Margin, RichText, Sense, Stroke, vec2};
use ve_core::publish::Target;

use crate::pack::{self, Level, PackFormat, TextStyle, ThumbShape};
use crate::theme::{self, Icon, Kind};
use crate::thumb_ai;
use crate::{App, Step};
#[allow(unused_imports)]
use crate::store;

/// What the Publish screen remembers about itself: the pictures it has loaded and which one is showing.
#[derive(Default)]
pub struct PackUi {
    tex: Option<egui::TextureHandle>,
    tex_tall: Option<egui::TextureHandle>,
    seen_thumb: u64,
    seen_pack: u64,
    seen_target: Option<Target>,
    /// Which of the two the preview shows.
    view_tall: bool,
    /// The crop slider moved and the picture hasn't been redrawn yet.
    tall_dirty: bool,
    /// A look control moved and the plain thumbnail hasn't been redrawn yet.
    style_dirty: bool,
}

enum Action {
    Write,
    Redraw,
    RedrawTall,
    /// Draw just these again.
    Restore(Vec<ThumbShape>),
    Save,
}

impl App {
    /// The Publish step: the words on the left, the search check and thumbnail on the right.
    pub fn publish_screen(&mut self, ui: &mut egui::Ui) {
        let target = self.engine.target;
        let facts = self.engine.montage_facts();
        let ready = match target {
            Target::Single => self.engine.source.is_some(),
            Target::Montage => facts.is_some(),
        };
        let estimate = target == Target::Montage && facts.is_some_and(|f| !f.rendered);
        let has_key = self.engine.has_openai;
        let ctx = ui.ctx().clone();
        let mut action: Option<Action> = None;
        let mut copy: Option<String> = None;
        let mut open_settings = false;
        let mut go_back = false;

        // The pictures: decode them into textures when the engine has new ones, and open on the tall one for a Short.
        let (busy, have) = {
            let st = &self.engine.publish;
            let ui_state = &mut self.pack_ui;
            if ui_state.seen_target != Some(target) {
                ui_state.seen_target = Some(target);
                (ui_state.seen_pack, ui_state.seen_thumb) = (u64::MAX, u64::MAX);
            }
            if st.pack_gen != ui_state.seen_pack {
                ui_state.seen_pack = st.pack_gen;
                ui_state.view_tall = st.pack.as_ref().is_some_and(|p| p.format == PackFormat::Short);
            }
            if st.thumb_gen != ui_state.seen_thumb {
                ui_state.seen_thumb = st.thumb_gen;
                let load = |bytes: &Option<Vec<u8>>, name: String| {
                    let picture = image::load_from_memory(bytes.as_deref()?).ok()?.to_rgba8();
                    Some(ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([picture.width() as usize, picture.height() as usize], &picture), egui::TextureOptions::LINEAR))
                };
                ui_state.tex = load(&st.thumb, format!("pack-thumb-{target:?}"));
                ui_state.tex_tall = load(&st.thumb_tall, format!("pack-thumb-tall-{target:?}"));
            }
            (st.busy.clone(), st.pack.is_some())
        };

        // ---- right column: how it will search, and the thumbnail
        if have {
            egui::Panel::right("publish-side").exact_size(440.0).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(20, 0))).show(ui, |ui| {
                egui::Panel::bottom("publish-footer").exact_size(84.0).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(0, 16))).show_separator_line(true).show(ui, |ui| {
                    if theme::button(ui, "Save pack and thumbnails", Kind::Primary, ui.available_width()).clicked() {
                        action = Some(Action::Save);
                    }
                });
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(16.0);
                    let (mut model_i, mut quality_i) = (self.engine.settings.thumb_model, self.engine.settings.thumb_quality);
                    let (tex, tex_tall, mut view_tall, mut dirty) = (self.pack_ui.tex.clone(), self.pack_ui.tex_tall.clone(), self.pack_ui.view_tall, self.pack_ui.tall_dirty);
                    let st = &mut self.engine.publish;
                    let (ai_busy, ai_cost, ai_reused, ai_warnings) = (st.ai_busy.clone(), st.ai_cost, st.ai_reused, st.ai_warnings.clone());
                    let (thumb_busy, thumb_error) = (st.thumb_busy || st.tall_busy || st.ai_busy.is_some(), st.thumb_error.clone());
                    let released = !ui.input(|i| i.pointer.any_down());
                    let mut changed_x = false;
                    let mut ask_ai = false;
                    let mut style_changed = false;
                    let mut style_designed = false;
                    let mut redraw_both = false;
                    let mut view_shape = ThumbShape::Wide;
                    let Some(pack) = &mut st.pack else { return };
                    let checks = pack.seo_checks();
                    let good = checks.iter().filter(|c| c.level == Level::Good).count();
                    theme::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(theme::bold("Thumbnail", 15.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                theme::segmented(ui, &mut view_tall, &[(true, "9:16 Short"), (false, "16:9 video")], false);
                            });
                        });
                        let shape = if view_tall { ThumbShape::Tall } else { ThumbShape::Wide };
                        let mut style = pack.style(shape);
                        let before = style.clone();
                        let designed = if view_tall { pack.ai_tall.is_some() } else { pack.ai_wide.is_some() };
                        view_shape = shape;
                        style_designed = designed;

                        // The picture. On a free draft the words can be dragged to where they should sit.
                        let width = ui.available_width();
                        let shown = if view_tall { &tex_tall } else { &tex };
                        let region = if view_tall { 420.0 } else { width * 9.0 / 16.0 };
                        let (rect, response) = ui.allocate_exact_size(vec2(width, region), Sense::click_and_drag());
                        ui.painter().rect_filled(rect, 10, theme::INPUT);
                        let r = if view_tall { egui::Rect::from_center_size(rect.center(), vec2(420.0 * 9.0 / 16.0, 420.0)) } else { rect };
                        if let Some(tex) = shown {
                            egui::Image::new(egui::load::SizedTexture::new(tex.id(), r.size())).corner_radius(10).paint_at(ui, r);
                        } else if thumb_busy {
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Drawing the thumbnail…", egui::FontId::proportional(14.0), theme::TEXT2);
                        }
                        if view_tall {
                            // Shorts covers the top and the bottom of the picture with its title and buttons.
                            let shade = egui::Color32::from_rgba_unmultiplied(255, 70, 60, 46);
                            let (top, bottom) = (egui::Rect::from_min_size(r.min, vec2(r.width(), r.height() * 0.10)), egui::Rect::from_min_max(egui::pos2(r.left(), r.bottom() - r.height() * 0.25), r.max));
                            for zone in [top, bottom] {
                                ui.painter().rect_filled(zone, 0, shade);
                            }
                            ui.painter().text(bottom.center(), egui::Align2::CENTER_CENTER, "Shorts covers this", egui::FontId::proportional(11.5), egui::Color32::from_rgba_unmultiplied(255, 200, 190, 200));
                        }
                        let words = pack.thumb_text.clone();
                        if !designed && !words.is_empty() {
                            let layout = pack::text_layout(&words, shape, &style);
                            if let Some(p) = response.interact_pointer_pos().filter(|_| response.dragged() || response.clicked()) {
                                style.x = ((p.x - r.left()) / r.width()).clamp(0.0, 1.0);
                                style.y = ((p.y - r.top()) / r.height()).clamp(0.0, 1.0);
                            }
                            if response.hovered() || response.dragged() {
                                ui.ctx().set_cursor_icon(if response.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
                                let (out_w, out_h) = (shape.size().0 as f32, shape.size().1 as f32);
                                let (cx, cy) = pack::text_centre(&layout, shape, &style);
                                let size = vec2(layout.width / out_w * r.width(), layout.height / out_h * r.height());
                                let guide = egui::Rect::from_center_size(egui::pos2(r.left() + cx / out_w * r.width(), r.top() + cy / out_h * r.height()), size).expand(4.0);
                                ui.painter().rect_stroke(guide, 4, Stroke::new(1.5, theme::ACCENT), egui::StrokeKind::Outside);
                            }
                        }

                        // A plain frame with words on it is only a draft. The AI design is the real thumbnail.
                        ui.horizontal(|ui| {
                            if designed {
                                theme::pill(ui, "AI designed", theme::ACCENT_TEXT, theme::accent_soft(40), None);
                            } else {
                                theme::pill(ui, "Free draft", theme::TEXT2, theme::INPUT, None);
                                ui.label(theme::muted("Just your frame and words."));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_enabled_ui(has_key && !thumb_busy, |ui| {
                                    if theme::small_button(ui, if designed { "Design again" } else { "Design with AI" }).clicked() {
                                        ask_ai = true;
                                    }
                                });
                            });
                        });
                        if view_tall {
                            let mut x = pack.thumb_x.unwrap_or(0.5);
                            if theme::slider_row(ui, "Where to cut the tall picture (left / right)", &mut x, 0.0..=1.0, 0.5, false) {
                                pack.thumb_x = Some(x);
                                changed_x = true;
                            }
                            ui.label(theme::muted("The Short thumbnail is cut out of the wide frame. Slide it to keep the action in."));
                        }
                        if let Some(e) = &thumb_error {
                            ui.label(RichText::new(format!("The thumbnail couldn't be drawn: {e}")).color(theme::BAD).size(12.5));
                        }
                        ui.horizontal(|ui| {
                            ui.label(theme::soft("Words on it"));
                            ui.add(egui::TextEdit::singleline(&mut pack.thumb_text).desired_width(ui.available_width() - 120.0).margin(Margin::symmetric(10, 8)));
                            ui.add_enabled_ui(!thumb_busy, |ui| {
                                if theme::small_button(ui, "Redraw (free)").clicked() {
                                    action = Some(Action::Redraw);
                                }
                            });
                        });

                        // How the words look. On a free draft the picture follows as soon as a control is let go.
                        egui::CollapsingHeader::new(theme::bold("Words: place, size and colours", 13.5)).default_open(true).id_salt(("text-style", view_tall)).show(ui, |ui| {
                            ui.label(theme::muted(if designed { "This is an AI design. These choices are sent to the AI the next time you press Design again." } else { "Drag the words on the picture, or use the controls. The picture redraws when you let go." }));
                            theme::slider_row(ui, "Left / right", &mut style.x, 0.0..=1.0, 0.5, false);
                            theme::slider_row(ui, "Up / down", &mut style.y, 0.0..=1.0, 0.5, false);
                            theme::slider_row(ui, "Size", &mut style.size, 0.4..=1.8, 1.0, false);
                            theme::slider_row(ui, "Outline thickness", &mut style.outline_width, 0.0..=2.5, 1.0, false);
                            theme::slider_row(ui, "Shade behind the words", &mut style.shade, 0.0..=1.0, 0.0, false);
                            swatches(ui, "Words", &mut style.fill, &[[0xFF, 0xE2, 0x00], [0xFF, 0xFF, 0xFF], [0xFF, 0x6B, 0x3D], [0x00, 0xE5, 0xFF], [0x7C, 0xFF, 0x4F], [0xFF, 0x3B, 0x30], [0xFF, 0x4F, 0xB8]]);
                            swatches(ui, "Outline", &mut style.outline, &[[0, 0, 0], [0x0B, 0x1B, 0x3A], [0xFF, 0xFF, 0xFF], [0x4A, 0x00, 0x00]]);
                            ui.horizontal(|ui| {
                                let fonts = pack::available_fonts();
                                ui.label(theme::soft("Font"));
                                egui::ComboBox::from_id_salt("thumb_font").selected_text(style.font.clone()).width(140.0).show_ui(ui, |ui| {
                                    for f in fonts {
                                        ui.selectable_value(&mut style.font, f.clone(), f);
                                    }
                                });
                                ui.label(theme::soft("Lines"));
                                egui::ComboBox::from_id_salt("thumb_lines").selected_text(["Auto", "One line", "Two lines"][style.lines.min(2) as usize]).width(100.0).show_ui(ui, |ui| {
                                    for (n, name) in ["Auto", "One line", "Two lines"].into_iter().enumerate() {
                                        ui.selectable_value(&mut style.lines, n as u8, name);
                                    }
                                });
                            });
                            theme::checkbox(ui, &mut style.shadow, "Shadow behind the letters");
                            ui.horizontal(|ui| {
                                if theme::small_button(ui, "Reset").clicked() {
                                    style = TextStyle::default_for(shape);
                                }
                                if theme::small_button(ui, "Use this look on both thumbnails").on_hover_text("Copies the font, size, colours and outline to the other thumbnail. Each keeps its own position.").clicked() {
                                    let other = if view_tall { ThumbShape::Wide } else { ThumbShape::Tall };
                                    let mut copy = pack.style(other);
                                    (copy.size, copy.fill, copy.outline, copy.outline_width, copy.shadow, copy.shade, copy.font, copy.lines) = (style.size, style.fill, style.outline, style.outline_width, style.shadow, style.shade, style.font.clone(), style.lines);
                                    pack.set_style(other, copy);
                                    redraw_both = pack.ai_wide.is_none() && pack.ai_tall.is_none();
                                }
                            });
                        });
                        let style = style.clamped();
                        if style != before {
                            pack.set_style(shape, style);
                            style_changed = true;
                        }
                        ui.add_space(2.0);
                        let count = pack.candidates.len();
                        ui.label(theme::muted("Pick the frame"));
                        let gap = 6.0;
                        let chip = ((ui.available_width() - gap * 7.0) / 8.0).max(24.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for i in 0..count.min(8) {
                                let (r, response) = ui.allocate_exact_size(vec2(chip, chip * 0.7), Sense::click());
                                let on = pack.thumb_pick == i;
                                ui.painter().rect(r, 7, if on { theme::accent_soft(40) } else { theme::INPUT }, Stroke::new(if on { 2.0 } else { 1.0 }, if on { theme::ACCENT } else { theme::LINE_STRONG }), egui::StrokeKind::Inside);
                                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, format!("{}", i + 1), egui::FontId::new(13.0, theme::bold_family()), if on { theme::ACCENT_TEXT } else { theme::TEXT2 });
                                if response.on_hover_text(pack.candidates[i].label.clone()).clicked() && !thumb_busy && !on {
                                    pack.thumb_pick = i;
                                    action = Some(Action::Redraw);
                                }
                            }
                        });
                        if !pack.reason.is_empty() {
                            ui.label(theme::muted(format!("Why this frame: {}", pack.reason)));
                        }
                    });
                    theme::card_with(ui, theme::accent_soft(14), theme::accent_soft(70), |ui| {
                        ui.horizontal(|ui| {
                            theme::icon(ui, Icon::Sparkle, 16.0, theme::ACCENT_TEXT);
                            ui.label(theme::bold("Design both with AI", 14.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if pack.ai_wide.is_some() || pack.ai_tall.is_some() {
                                    theme::pill(ui, "AI designed", theme::ACCENT_TEXT, theme::accent_soft(40), None);
                                }
                            });
                        });
                        ui.label(theme::muted("Sends the frame you picked (a 16:9 and a 9:16 version), your words and the game to OpenAI, which designs a thumbnail for each shape. The pictures stay true to your video. It costs money, and you see the real cost afterwards."));
                        ui.label(theme::soft("Anything to show off? (optional)"));
                        ui.add(egui::TextEdit::singleline(&mut pack.thumb_note).hint_text("for example: make the explosion bigger").desired_width(f32::INFINITY).margin(Margin::symmetric(10, 8)));
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            egui::ComboBox::from_id_salt("thumb_model").selected_text(thumb_ai::IMAGE_MODELS[model_i.min(1)].1).width(170.0).show_ui(ui, |ui| {
                                for (i, (_, name)) in thumb_ai::IMAGE_MODELS.iter().enumerate() {
                                    ui.selectable_value(&mut model_i, i, *name);
                                }
                            });
                            egui::ComboBox::from_id_salt("thumb_quality").selected_text(thumb_ai::QUALITIES[quality_i.min(2)].1).width(150.0).show_ui(ui, |ui| {
                                for (i, (_, name)) in thumb_ai::QUALITIES.iter().enumerate() {
                                    ui.selectable_value(&mut quality_i, i, *name);
                                }
                            });
                        });
                        ui.add_enabled_ui(has_key && !thumb_busy, |ui| {
                            if theme::button_with_icon(ui, if ai_busy.is_some() { "Designing…" } else { "Make both with AI" }, None, Kind::Primary, ui.available_width()).clicked() {
                                ask_ai = true;
                            }
                        });
                        if !has_key {
                            ui.label(theme::muted("Needs your OpenAI key (Settings, top right)."));
                        }
                        if let Some(text) = &ai_busy {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(theme::soft(text));
                            });
                        }
                        for warning in &ai_warnings {
                            ui.add(egui::Label::new(RichText::new(warning).size(12.5).color(theme::WARN)).wrap());
                        }
                        if ai_cost > 0.0 && ai_busy.is_none() {
                            ui.label(theme::muted(format!("This time it cost about ${ai_cost:.2} (from OpenAI's usage report).")));
                        } else if ai_reused && ai_busy.is_none() {
                            ui.label(theme::muted("Reused your saved pictures. Nothing was charged."));
                        }
                    });
                    theme::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            theme::ring(ui, good, checks.len());
                            ui.vertical(|ui| {
                                ui.label(theme::bold("Search check", 15.0));
                                ui.label(theme::muted("Updates as you edit"));
                                ui.label(theme::soft(if good == checks.len() { "Everything YouTube likes is in place." } else { "A few things could help it get found." }));
                            });
                        });
                        ui.add_space(4.0);
                        for check in &checks {
                            let level = match check.level {
                                Level::Good => theme::Level::Good,
                                Level::Warn => theme::Level::Warn,
                                Level::Bad => theme::Level::Bad,
                            };
                            theme::status_row(ui, level, &check.text);
                        }
                    });
                    ui.add_space(16.0);
                    self.pack_ui.view_tall = view_tall;
                    (self.engine.settings.thumb_model, self.engine.settings.thumb_quality) = (model_i, quality_i);
                    if ask_ai {
                        self.confirm_ai_thumb = true;
                    }
                    let (thumb_busy_now, tall_busy_now) = (self.engine.publish.thumb_busy, self.engine.publish.tall_busy);
                    let st = &mut self.pack_ui;
                    dirty |= changed_x;
                    // Redraw once the slider is let go, not on every pixel of the drag.
                    if dirty && released && !tall_busy_now {
                        dirty = false;
                        action = Some(Action::RedrawTall);
                    }
                    st.tall_dirty = dirty;
                    // The look of the words: redraw the plain picture once the control is let go. An AI design is left alone.
                    let mut style_dirty = st.style_dirty || (style_changed && !style_designed);
                    if style_dirty && released && !thumb_busy_now && !tall_busy_now && action.is_none() {
                        style_dirty = false;
                        action = Some(Action::Restore(vec![view_shape]));
                    }
                    st.style_dirty = style_dirty;
                    if redraw_both && action.is_none() {
                        action = Some(Action::Restore(vec![ThumbShape::Wide, ThumbShape::Tall]));
                    }
                });
            });
        }

        // ---- left column
        egui::CentralPanel::default().frame(Frame::new().fill(theme::WINDOW)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                Frame::new().inner_margin(Margin { left: 28, right: 28, top: 16, bottom: 24 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::bold("Publish", 18.0));
                        ui.label(theme::soft("Titles, description and thumbnails (wide and Short) that help people find it"));
                    });
                    ui.add_space(8.0);
                    theme::card(ui, |ui| {
                        ui.label(theme::bold("Tell the AI what this is", 14.0));
                        ui.horizontal(|ui| {
                            ui.label(theme::soft("Game or topic"));
                            ui.add(egui::TextEdit::singleline(&mut self.engine.settings.pack_topic).hint_text("for example Valorant").desired_width(220.0).margin(Margin::symmetric(10, 8)))
                                .on_hover_text("The game's exact name is the strongest search keyword for a gaming video, and it is used exactly as you type it. Leave it empty and the AI names a game only if it can clearly see which one.");
                        });
                        theme::segmented(ui, &mut self.engine.settings.pack_format, &[(PackFormat::Standard, "Standard video"), (PackFormat::Short, "YouTube Short")], false);
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(ready && has_key && busy.is_none(), |ui| {
                                if theme::button(ui, if have { "Write it again" } else { "Write my upload pack" }, Kind::Primary, 0.0).on_hover_text("Three titles, a description with chapters, hashtags and a thumbnail. Sends about 8 small pictures to OpenAI.").clicked() {
                                    action = Some(Action::Write);
                                }
                            });
                            if let Some(text) = &busy {
                                ui.spinner();
                                ui.label(theme::soft(text));
                            }
                        });
                        if !has_key {
                            ui.horizontal(|ui| {
                                ui.label(theme::muted("Needs your OpenAI key."));
                                if theme::small_button(ui, "Add it in Settings").clicked() {
                                    open_settings = true;
                                }
                            });
                        }
                        if !ready {
                            ui.horizontal(|ui| {
                                ui.label(theme::muted(if target == Target::Single { "Open a video first." } else { "Plan the montage first." }));
                                if theme::small_button(ui, "Go back").clicked() {
                                    go_back = true;
                                }
                            });
                        }
                        if estimate && ready {
                            ui.label(theme::muted("Make the montage first so the chapter times match the finished video exactly. Until then they are estimates."));
                        }
                    });

                    let Some(pack) = &mut self.engine.publish.pack else { return };
                    let good_len = if pack.format == PackFormat::Short { 50 } else { 60 };
                    ui.add_space(4.0);
                    theme::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(theme::bold("Titles", 14.0));
                            ui.label(theme::muted("Pick one and edit it freely"));
                        });
                        for i in 0..pack.titles.len() {
                            let chosen = pack.title_pick == i;
                            ui.horizontal(|ui| {
                                let (dot, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                                theme::radio_dot(ui.painter(), dot.center(), chosen);
                                if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    pack.title_pick = i;
                                }
                                let n = pack.titles[i].chars().count();
                                ui.add(egui::TextEdit::singleline(&mut pack.titles[i]).desired_width(ui.available_width() - 140.0).margin(Margin::symmetric(10, 9)));
                                let colour = if n == 0 || n > pack::MAX_TITLE { theme::BAD } else if pack::title_length(&pack.titles[i]) <= good_len { theme::OK } else { theme::WARN };
                                theme::pill(ui, &format!("{n}/{}", pack::MAX_TITLE), colour, theme::tint(colour, 30), None);
                                if theme::small_button(ui, "Copy").clicked() {
                                    copy = Some(pack.titles[i].clone());
                                }
                            });
                        }
                    });
                    theme::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(theme::bold("Description", 14.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if theme::small_button(ui, "Copy description").clicked() {
                                    copy = Some(pack.description());
                                }
                            });
                        });
                        ui.label(theme::muted("The first two lines show in search, so the intro leads with what people look for"));
                        ui.add(egui::TextEdit::multiline(&mut pack.intro).desired_rows(4).desired_width(f32::INFINITY).margin(Margin::symmetric(12, 10)));
                        ui.label(theme::bold(format!("Chapters ({})", pack.chapters.len()), 13.0));
                        if pack.chapters.is_empty() {
                            ui.label(theme::muted("Too short for chapters (YouTube needs 3, each 10 s or more)."));
                        }
                        for chapter in &pack.chapters {
                            ui.horizontal(|ui| {
                                ui.label(theme::mono(pack::stamp(chapter.at), 12.5).color(theme::ACCENT_TEXT));
                                ui.label(theme::soft(&chapter.title));
                            });
                        }
                        if !pack.credit.trim().is_empty() {
                            ui.add_space(2.0);
                            ui.label(theme::muted(format!("Music credit added: {}", pack.credit.lines().next().unwrap_or(""))));
                        }
                    });
                    theme::card(ui, |ui| {
                        ui.label(theme::bold("Keywords and tags", 14.0));
                        if !pack.primary_keyword.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(theme::muted("Focus"));
                                theme::pill(ui, &pack.primary_keyword, theme::ACCENT_TEXT, theme::accent_soft(36), None);
                                if !pack.secondary_keywords.is_empty() {
                                    ui.label(theme::muted("Related"));
                                    for k in &pack.secondary_keywords {
                                        theme::pill(ui, k, theme::TEXT2, theme::INPUT, None);
                                    }
                                }
                            });
                        }
                        if !pack.hashtags.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(theme::muted("Hashtags"));
                                for h in &pack.hashtags {
                                    theme::pill(ui, h, theme::TEXT2, theme::INPUT, None);
                                }
                            });
                        }
                        if !pack.tags.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(theme::muted(format!("Hidden tags ({})", pack.tags.len())));
                                ui.add(egui::Label::new(theme::soft(pack.tags.join(", "))).wrap());
                            });
                            if theme::small_button(ui, "Copy tags").clicked() {
                                copy = Some(pack.tags.join(", "));
                            }
                        }
                    });
                });
            });
        });

        if open_settings {
            self.show_settings = true;
        }
        if go_back {
            self.step = if target == Target::Single { Step::Export } else { Step::Source };
        }
        if let Some(text) = copy {
            ctx.copy_text(text);
            self.engine.note_copied();
        }
        match action {
            Some(Action::Write) => self.engine.start_pack(),
            Some(Action::Redraw) => {
                if let Some(pack) = &mut self.engine.publish.pack {
                    pack.thumb_text = pack::clean_thumb_text(&pack.thumb_text);
                }
                self.engine.redraw_thumbnails(vec![ThumbShape::Wide, ThumbShape::Tall]);
            }
            Some(Action::RedrawTall) => self.engine.redraw_thumbnails(vec![ThumbShape::Tall]),
            Some(Action::Restore(shapes)) => self.engine.redraw_thumbnails(shapes),
            Some(Action::Save) => self.engine.save_pack(),
            None => {}
        }
    }
}

/// A row of colour choices with a picker for any other colour. True when the colour changed.
fn swatches(ui: &mut egui::Ui, label: &str, current: &mut [u8; 3], presets: &[[u8; 3]]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(theme::soft(label));
        for preset in presets {
            let (r, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
            ui.painter().circle_filled(r.center(), 10.0, egui::Color32::from_rgb(preset[0], preset[1], preset[2]));
            if *current == *preset {
                ui.painter().circle_stroke(r.center(), 12.0, Stroke::new(2.0, theme::ACCENT));
            } else {
                ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.0, theme::LINE_STRONG));
            }
            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                *current = *preset;
                changed = true;
            }
        }
        changed |= ui.color_edit_button_srgb(current).changed();
    });
    changed
}
