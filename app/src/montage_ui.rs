//! The Montage screens: Clips, Plan and Make, with the summary panel and the preview. Only the drawing is here.
//! Everything the montage does (probing clips, the AI plan, music, the preview player, rendering, saving) is
//! `ve_core::montage_engine`, shared with the gpui front end.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Align, Color32, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, pos2, vec2};

use crate::grade::LOOKS;
use crate::media::BlurLayout;
use crate::montage::{Clip, Cut, Sound, Style, Vertical};
use crate::theme::{self, Icon, Kind};
use crate::{App, Step, ai};
use ve_core::montage_engine::MusicSource;

/// The pictures the montage screens have loaded, and what the preview is showing.
#[derive(Default)]
pub struct MontageUi {
    pub thumbs: HashMap<PathBuf, egui::TextureHandle>,
    pub preview: Option<egui::TextureHandle>,
    pub seen_thumbs: u64,
    pub seen_shown: u64,
    /// Whether the preview shows the 9:16 picture rather than the 16:9 one.
    pub phone: bool,
}

fn clock(t: f64) -> String {
    format!("{}:{:02.0}", (t / 60.0) as u32, t % 60.0)
}

impl App {
    /// Turns what the montage engine holds into pictures egui can draw, when they changed.
    pub fn sync_montage(&mut self, ctx: &egui::Context) {
        let m = &self.engine.montage;
        if m.thumbs_gen != self.mon_ui.seen_thumbs {
            self.mon_ui.seen_thumbs = m.thumbs_gen;
            self.mon_ui.thumbs = m
                .thumbs
                .iter()
                .map(|(path, frame)| (path.clone(), ctx.load_texture(format!("clip-{}", path.display()), egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb), egui::TextureOptions::LINEAR)))
                .collect();
        }
        if m.preview.shown != self.mon_ui.seen_shown {
            self.mon_ui.seen_shown = m.preview.shown;
            self.mon_ui.preview = m.preview.frame.as_ref().map(|f| ctx.load_texture("montage-preview", egui::ColorImage::from_rgb([f.width, f.height], &f.rgb), egui::TextureOptions::LINEAR));
        }
    }

    pub fn montage_screen(&mut self, ui: &mut egui::Ui, step: Step) {
        self.montage_summary(ui, step);
        egui::CentralPanel::default().frame(Frame::new().fill(theme::WINDOW)).show(ui, |ui| {
            // VE_SCROLL=900 starts the page scrolled down, for screenshots of the lower sections.
            let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
            if let Some(y) = std::env::var("VE_SCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
                area = area.vertical_scroll_offset(y);
            }
            area.show(ui, |ui| {
                Frame::new().inner_margin(Margin { left: 28, right: 28, top: 16, bottom: 28 }).show(ui, |ui| match step {
                    Step::Source => self.clips_step(ui),
                    Step::Look => self.plan_step(ui),
                    _ => self.make_step(ui),
                });
            });
        });
    }

    /// The picture, the play controls and the moments as one clickable bar.
    fn preview_card(&mut self, ui: &mut egui::Ui) {
        let Some(plan) = &self.engine.montage.plan else { return };
        let cuts = plan.cuts.clone();
        let (cur, playing, phone) = (self.engine.montage.preview.cur.min(cuts.len().saturating_sub(1)), self.engine.montage.playing(), self.mon_ui.phone);
        let tex = self.mon_ui.preview.clone();
        let kept: Vec<usize> = self.engine.montage.kept();
        let (mut select, mut toggle, mut step) = (None, false, 0i32);
        let mut phone_now = phone;
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::bold("Preview", 14.0));
                ui.label(theme::muted("A quick silent look. Music, fades and exact colour come when you make it."));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    theme::segmented(ui, &mut phone_now, &[(true, "9:16"), (false, "16:9")], false);
                });
            });
            let (region, _) = ui.allocate_exact_size(vec2(ui.available_width(), 330.0), Sense::hover());
            ui.painter().rect_filled(region, 12, Color32::from_rgb(0x0B, 0x0B, 0x0D));
            if let Some(tex) = &tex {
                let aspect = tex.size()[0] as f32 / tex.size()[1].max(1) as f32;
                if phone_now {
                    let h = region.height() - 20.0;
                    paint_phone(ui, Rect::from_center_size(region.center(), vec2(h * 9.0 / 16.0, h)), tex, self.engine.montage.vertical, self.engine.montage.blur);
                } else {
                    let w = (region.width() - 20.0).min((region.height() - 20.0) * aspect);
                    let r = Rect::from_center_size(region.center(), vec2(w, w / aspect));
                    egui::Image::new(egui::load::SizedTexture::new(tex.id(), r.size())).corner_radius(10).paint_at(ui, r);
                }
            } else {
                ui.painter().text(region.center(), egui::Align2::CENTER_CENTER, if self.engine.montage.preview.busy { "Getting the picture…" } else { "Pick a moment" }, egui::FontId::proportional(14.0), theme::TEXT3);
            }
            let kept_total: f64 = kept.iter().map(|&i| cuts[i].len()).sum();
            let done: f64 = kept.iter().filter(|&&i| i < cur).map(|&i| cuts[i].len()).sum::<f64>() + if kept.contains(&cur) { (self.engine.montage.preview.t - cuts[cur].start).clamp(0.0, cuts[cur].len()) } else { 0.0 };
            ui.horizontal(|ui| {
                if theme::round_button(ui, if playing { Icon::Pause } else { Icon::Play }, if playing { "Pause" } else { "Play" }).clicked() {
                    toggle = true;
                }
                if theme::small_button(ui, "Previous").clicked() {
                    step = -1;
                }
                if theme::small_button(ui, "Next").clicked() {
                    step = 1;
                }
                ui.label(theme::mono(format!("{} / {}", clock(done), clock(kept_total)), 13.0).color(theme::TEXT));
                if let Some(cut) = cuts.get(cur) {
                    let name = self.engine.montage.clips.get(cut.clip).map_or_else(String::new, Clip::name);
                    ui.add(egui::Label::new(theme::muted(format!("Moment {} of {} · {} · {}", cur + 1, cuts.len(), name, cut.why))).truncate());
                }
            });
            // The moments as one bar: width is length, colour is the clip. Click one to look at it.
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
            let total: f64 = cuts.iter().map(Cut::len).sum::<f64>().max(0.1);
            let mut x = rect.left();
            let mut head = None;
            for (i, cut) in cuts.iter().enumerate() {
                let w = (rect.width() as f64 * cut.len() / total) as f32;
                let colour = clip_colour(cut.clip);
                let block = Rect::from_min_size(pos2(x, rect.top()), vec2((w - 2.0).max(2.0), rect.height()));
                let on = self.engine.montage.keep.get(i).copied().unwrap_or(true);
                ui.painter().rect_filled(block, 5, if on { colour } else { theme::tint(colour, 40) });
                if i == cur {
                    ui.painter().rect_stroke(block.expand(1.0), 6, Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Outside);
                    head = Some(block.left() + block.width() * ((self.engine.montage.preview.t - cut.start) / cut.len().max(0.1)).clamp(0.0, 1.0) as f32);
                }
                if response.clicked() && response.interact_pointer_pos().is_some_and(|p| p.x >= x && p.x < x + w) {
                    select = Some(i);
                }
                x += w;
            }
            if let Some(h) = head {
                ui.painter().rect_filled(Rect::from_min_max(pos2(h - 1.0, rect.top() - 4.0), pos2(h + 1.0, rect.bottom() + 4.0)), 1, Color32::WHITE);
            }
            ui.horizontal_wrapped(|ui| {
                for (i, clip) in self.engine.montage.clips.iter().enumerate() {
                    let (dot, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, clip_colour(i));
                    ui.label(theme::muted(format!("{}. {}", i + 1, clip.name())));
                }
            });
        });
        self.mon_ui.phone = phone_now;
        if step != 0 {
            let at = kept.iter().position(|&k| k >= cur);
            let target = match (step, at) {
                (1, Some(p)) if kept.get(p) == Some(&cur) => kept.get(p + 1),
                (1, Some(p)) => kept.get(p),
                (_, Some(p)) => p.checked_sub(1).and_then(|q| kept.get(q)),
                (1, None) => None,
                (_, None) => kept.last(),
            };
            select = target.copied().or(select);
        }
        if let Some(i) = select {
            self.engine.preview_goto(i);
        } else if toggle {
            self.engine.preview_toggle();
        } else if tex.is_none() && !self.engine.montage.preview.busy && !self.engine.montage.preview.started && let Some(first) = kept.first() {
            self.engine.preview_select(*first);
            // Developer aid: VE_PLAY=1 starts playing at once, for screenshots.
            if std::env::var_os("VE_PLAY").is_some() {
                self.engine.preview_toggle();
            }
        }
    }

    // ------------------------------------------------------------------ summary

    fn montage_summary(&mut self, ui: &mut egui::Ui, step: Step) {
        let mut plan = false;
        let mut make = false;
        let mut publish = false;
        egui::Panel::right("montage-summary").exact_size(372.0).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(20, 0))).show(ui, |ui| {
            egui::Panel::bottom("montage-footer").exact_size(if self.engine.montage.made.is_empty() { 84.0 } else { 140.0 }).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(0, 16))).show_separator_line(true).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                let m = &self.engine.montage;
                let working = m.busy.is_some() || m.render.is_some();
                let width = ui.available_width();
                if m.plan.is_none() {
                    ui.add_enabled_ui(!m.clips.is_empty() && self.engine.has_openai && !working, |ui| {
                        plan = theme::button(ui, if m.busy.is_some() { "Planning…" } else { "Plan with AI" }, Kind::Primary, width).clicked();
                    });
                } else {
                    ui.add_enabled_ui(!working, |ui| {
                        make = theme::button(ui, if m.render.is_some() { "Making the montage…" } else if m.made.is_empty() { "Make the montage" } else { "Make it again" }, if m.made.is_empty() { Kind::Primary } else { Kind::Secondary }, width).clicked();
                    });
                    if !m.made.is_empty() {
                        publish = theme::button_with_icon(ui, "Continue to Publish", Some(Icon::ArrowRight), Kind::Primary, width).clicked();
                    }
                }
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(16.0);
                ui.label(theme::bold("Your montage", 15.0));
                let m = &self.engine.montage;
                theme::card(ui, |ui| {
                    summary_row(ui, "Clips", &if m.clips.is_empty() { "none yet".to_string() } else { format!("{} · {}", m.clips.len(), clock(m.total())) });
                    summary_row(ui, "Style", m.style.name());
                    summary_row(ui, "Length", &format!("about {}", clock(m.target)));
                    summary_row(ui, "Sound", m.sound.name());
                    if m.sound.uses_music() {
                        let music = match (m.source, m.tracks.get(m.track), &m.file) {
                            (MusicSource::File, _, Some(f)) => f.file_name().map_or_else(|| "your file".to_string(), |n| n.to_string_lossy().into_owned()),
                            (MusicSource::File, _, None) => "no file chosen".to_string(),
                            (MusicSource::Online, Some(t), _) => t.title.clone(),
                            (MusicSource::Online, None, _) => "found after planning".to_string(),
                        };
                        summary_row(ui, "Music", &music);
                    }
                    summary_row(ui, "Look", m.look.map_or("Natural (fix only)", |i| LOOKS[i].name));
                    let shapes = match (m.make_wide, m.make_vertical) {
                        (true, true) => "16:9 and 9:16",
                        (true, false) => "16:9",
                        (false, true) => "9:16",
                        (false, false) => "nothing ticked",
                    };
                    summary_row(ui, "Makes", shapes);
                    if let Some(p) = &m.plan {
                        let kept = m.keep.iter().filter(|k| **k).count();
                        summary_row(ui, "Moments", &format!("{kept} of {} kept", p.cuts.len()));
                    }
                });
                match (step, self.engine.montage.plan.is_some()) {
                    (Step::Source, false) => {
                        ui.label(theme::muted("Next, the AI looks at small pictures from each clip and picks the best moments. Your video files are not uploaded."));
                    }
                    (_, true) if self.engine.montage.made.is_empty() => {
                        ui.label(theme::muted("Untick any moment you don't want, then make the montage."));
                    }
                    _ => {}
                }
                if !self.engine.has_openai && self.engine.montage.plan.is_none() {
                    theme::card_with(ui, theme::tint(theme::WARN, 18), theme::tint(theme::WARN, 70), |ui| {
                        ui.label(theme::bold("OpenAI key needed", 13.5));
                        ui.label(theme::soft("Planning uses your own OpenAI key."));
                        if theme::small_button(ui, "Add it in Settings").clicked() {
                            self.show_settings = true;
                        }
                    });
                }
                if !self.engine.montage.made.is_empty() {
                    theme::card_with(ui, theme::tint(theme::OK, 18), theme::tint(theme::OK, 70), |ui| {
                        ui.label(theme::bold("Saved", 14.0));
                        for file in &self.engine.montage.made {
                            ui.label(RichText::new(file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())).size(12.5).color(theme::TEXT2));
                        }
                        if theme::small_button(ui, "Show in folder").clicked()
                            && let Some(parent) = self.engine.montage.made[0].parent()
                        {
                            crate::screens::show_in_folder(parent);
                        }
                    });
                }
                ui.add_space(16.0);
            });
        });
        if plan {
            self.step = Step::Look;
            self.engine.plan_montage();
        }
        if make {
            self.step = Step::Export;
            self.engine.render_montage();
        }
        if publish {
            self.step = Step::Publish;
        }
    }

    // ------------------------------------------------------------------- clips

    fn clips_step(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::bold("Clips", 18.0));
            ui.label(theme::soft("Drop several clips anywhere in this window. The order is the order they play in."));
        });
        ui.add_space(6.0);
        let mut action: Option<(usize, i32)> = None;
        let mut add = false;
        let gap = 14.0;
        let width = ((ui.available_width() - gap * 2.0) / 3.0).floor();
        let count = self.engine.montage.clips.len();
        let tiles = count + 1;
        for row in 0..tiles.div_ceil(3) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for col in 0..3 {
                    let i = row * 3 + col;
                    if i < count {
                        let (name, probe_text, duration) = {
                            let c = &self.engine.montage.clips[i];
                            (c.name(), format!("{}×{}", c.probe.width, c.probe.height), c.probe.duration)
                        };
                        let thumb = self.mon_ui.thumbs.get(&self.engine.montage.clips[i].path).cloned();
                        ui.allocate_ui_with_layout(vec2(width, width * 9.0 / 16.0 + 74.0), Layout::top_down(Align::LEFT), |ui| {
                            let (rect, _) = ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::hover());
                            ui.painter().rect_filled(rect, 12, theme::INPUT);
                            if let Some(tex) = thumb {
                                egui::Image::new(egui::load::SizedTexture::new(tex.id(), rect.size())).corner_radius(12).paint_at(ui, rect);
                            } else {
                                theme::paint_icon(ui.painter(), rect.center(), 28.0, Icon::Film, theme::TEXT3, 1.5);
                            }
                            let badge = ui.painter().layout_no_wrap(clock(duration), egui::FontId::new(12.0, egui::FontFamily::Monospace), theme::TEXT);
                            let br = Rect::from_min_size(pos2(rect.right() - badge.size().x - 22.0, rect.bottom() - 30.0), badge.size() + vec2(16.0, 8.0));
                            ui.painter().rect_filled(br, 6, Color32::from_black_alpha(190));
                            ui.painter().galley(br.center() - badge.size() / 2.0, badge, theme::TEXT);
                            let number = Rect::from_min_size(rect.min + vec2(10.0, 10.0), vec2(26.0, 26.0));
                            ui.painter().circle_filled(number.center(), 13.0, theme::ACCENT);
                            ui.painter().text(number.center(), egui::Align2::CENTER_CENTER, format!("{}", i + 1), egui::FontId::new(13.0, theme::bold_family()), theme::ON_ACCENT);
                            ui.add(egui::Label::new(theme::bold(&name, 13.5)).truncate());
                            ui.label(theme::mono(probe_text, 12.0).color(theme::TEXT3));
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if i > 0 && theme::small_button(ui, "Earlier").clicked() {
                                    action = Some((i, -1));
                                }
                                if i + 1 < count && theme::small_button(ui, "Later").clicked() {
                                    action = Some((i, 1));
                                }
                                if theme::small_button(ui, "Remove").clicked() {
                                    action = Some((i, 0));
                                }
                            });
                        });
                    } else if i == count {
                        let (rect, response) = ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::click());
                        let painter = ui.painter();
                        painter.rect(rect, 12, if response.hovered() { theme::CARD } else { Color32::from_rgb(0x14, 0x15, 0x18) }, Stroke::new(2.0, theme::LINE_STRONG), egui::StrokeKind::Inside);
                        theme::paint_icon(painter, rect.center() - vec2(0.0, 12.0), 26.0, Icon::Plus, theme::ACCENT_TEXT, 2.0);
                        painter.text(rect.center() + vec2(0.0, 22.0), egui::Align2::CENTER_CENTER, if self.engine.montage.probing > 0 { "Reading clips…" } else { "Add clips" }, egui::FontId::new(14.0, theme::bold_family()), theme::TEXT);
                        if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            add = true;
                        }
                    }
                }
            });
            ui.add_space(8.0);
        }
        if add && let Some(paths) = rfd::FileDialog::new().add_filter("Video", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "wmv"]).pick_files() {
            self.engine.add_clips(paths);
        }
        if let Some((i, step)) = action {
            match step {
                0 => {
                    let gone = self.engine.montage.clips.remove(i);
                    self.engine.montage.thumbs.remove(&gone.path);
                }
                s => self.engine.montage.clips.swap(i, (i as i32 + s) as usize),
            }
            self.engine.montage.plan = None;
            self.engine.montage.tracks.clear();
        }

        ui.add_space(8.0);
        ui.label(theme::bold("Style", 15.0));
        let gap = 14.0;
        let width = ((ui.available_width() - gap * 2.0) / 3.0).floor();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for style in Style::ALL {
                if choice_card(ui, width, style.name(), style.blurb(), self.engine.montage.style == style).clicked() {
                    self.engine.montage.style = style;
                }
            }
        });
        ui.add_space(10.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let half = ((ui.available_width() - gap) / 2.0).floor();
            ui.allocate_ui_with_layout(vec2(half, 0.0), Layout::top_down(Align::LEFT), |ui| {
                theme::card(ui, |ui| {
                    ui.label(theme::bold("Length", 14.0));
                    ui.label(theme::muted("About how long the finished montage should be"));
                    ui.horizontal_wrapped(|ui| {
                        for (label, secs) in [("30s", 30.0), ("1 min", 60.0), ("2 min", 120.0), ("3 min", 180.0), ("5 min", 300.0)] {
                            let on = (self.engine.montage.target - secs).abs() < 0.5;
                            if chip(ui, label, on).clicked() {
                                self.engine.montage.target = secs;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(theme::soft("Exactly"));
                        ui.add(egui::DragValue::new(&mut self.engine.montage.target).range(10.0..=900.0).speed(1.0).suffix(" seconds"));
                    });
                });
            });
            ui.allocate_ui_with_layout(vec2(half, 0.0), Layout::top_down(Align::LEFT), |ui| {
                theme::card(ui, |ui| {
                    ui.label(theme::bold("Sound", 14.0));
                    ui.label(theme::muted(self.engine.montage.sound.name()));
                    theme::segmented(ui, &mut self.engine.montage.sound, &[(Sound::MusicAndOriginal, "Music + game"), (Sound::OriginalOnly, "Game only"), (Sound::MusicOnly, "Music only")], true);
                });
            });
        });
        ui.add_space(4.0);
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::bold("Colour look", 14.0));
                ui.label(theme::muted("Each clip is corrected on its own, then given this look"));
            });
            egui::ComboBox::from_id_salt("m_look").selected_text(self.engine.montage.look.map_or("Natural (fix only)", |i| LOOKS[i].name)).width(260.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.engine.montage.look, None, "Natural (fix only)");
                for (i, look) in LOOKS.iter().enumerate() {
                    ui.selectable_value(&mut self.engine.montage.look, Some(i), look.name);
                }
            });
            ui.add_space(4.0);
            ui.label(theme::bold("Anything special?", 14.0));
            ui.add(egui::TextEdit::multiline(&mut self.engine.settings.style_notes).desired_rows(2).desired_width(f32::INFINITY).margin(Margin::symmetric(12, 9)));
        });
        if self.engine.montage.sound.uses_music() {
            self.music_card(ui);
        }
    }

    fn music_card(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::icon(ui, Icon::Music, 16.0, theme::ACCENT_TEXT);
                ui.label(theme::bold("Music", 14.0));
            });
            theme::segmented(ui, &mut self.engine.montage.source, &[(MusicSource::Online, "NCS music (no copyright claims)"), (MusicSource::File, "Use my own file")], false);
            match self.engine.montage.source {
                MusicSource::Online => {
                    ui.label(theme::muted("Music from NCS (NoCopyrightSounds), made for creators: free in monetised YouTube and Twitch videos with no copyright strikes. The AI chooses the mood. NCS asks you to credit the song in your description, so the exact credit text is saved next to the montage and added to your upload pack."));
                    if self.engine.montage.plan.is_some() {
                        let mut again = false;
                        ui.horizontal(|ui| {
                            ui.label(theme::soft("Mood"));
                            ui.add(egui::TextEdit::singleline(&mut self.engine.montage.music_query).desired_width(200.0).margin(Margin::symmetric(10, 7)));
                            ui.add_enabled_ui(!self.engine.montage.searching, |ui| again = theme::small_button(ui, "Search again").clicked());
                            if self.engine.montage.searching {
                                ui.spinner();
                            }
                        });
                        if again {
                            self.engine.search_music();
                        }
                        let mut picked = None;
                        for (i, t) in self.engine.montage.tracks.iter().enumerate().take(8) {
                            let on = self.engine.montage.track == i;
                            let response = Frame::new().fill(if on { theme::accent_soft(24) } else { theme::INPUT }).stroke(Stroke::new(if on { 1.5 } else { 1.0 }, if on { theme::ACCENT } else { theme::LINE_STRONG })).corner_radius(10).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.add(egui::Label::new(theme::bold(&t.title, 13.0)).truncate());
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        theme::pill(ui, if t.instrumental { "Instrumental" } else { "With vocals" }, theme::TEXT2, theme::CARD, None);
                                    });
                                });
                                ui.label(theme::muted(format!("{} · {}", t.creator, t.source)));
                            }).response.interact(Sense::click());
                            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                picked = Some(i);
                            }
                        }
                        if let Some(i) = picked {
                            self.engine.montage.track = i;
                        }
                    } else {
                        ui.label(theme::soft("After planning, songs that fit your montage appear here."));
                    }
                }
                MusicSource::File => {
                    ui.horizontal(|ui| {
                        if theme::button(ui, "Choose music file", Kind::Secondary, 0.0).clicked()
                            && let Some(f) = rfd::FileDialog::new().add_filter("Audio", &["mp3", "wav", "m4a", "aac", "ogg", "flac"]).pick_file()
                        {
                            self.engine.montage.file = Some(f);
                        }
                        ui.add(egui::Label::new(theme::mono(self.engine.montage.file.as_ref().map_or_else(|| "No file chosen".to_string(), |f| f.display().to_string()), 12.0).color(theme::TEXT2)).truncate());
                    });
                    ui.label(theme::muted("Make sure you have the right to use this song in your video."));
                }
            }
        });
    }

    // -------------------------------------------------------------------- plan

    fn plan_step(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::bold("Plan", 18.0));
            ui.label(theme::soft("The moments the AI chose, in play order. Untick any you don't want."));
        });
        ui.add_space(6.0);
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::icon(ui, Icon::Sparkle, 16.0, theme::ACCENT_TEXT);
                ui.label(theme::bold("AI plan", 14.0));
                ui.label(theme::muted("uses your own OpenAI key"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("m_model").selected_text(ai::OPENAI_MODELS[self.engine.settings.openai_model].1).width(150.0).show_ui(ui, |ui| {
                        for (i, (_, name)) in ai::OPENAI_MODELS.iter().enumerate() {
                            ui.selectable_value(&mut self.engine.settings.openai_model, i, *name);
                        }
                    });
                });
            });
            ui.horizontal(|ui| {
                let ready = !self.engine.montage.clips.is_empty() && self.engine.montage.busy.is_none() && self.engine.montage.render.is_none();
                ui.add_enabled_ui(ready && self.engine.has_openai, |ui| {
                    if theme::button(ui, if self.engine.montage.plan.is_some() { "Plan again" } else { "Plan with AI" }, Kind::Secondary, 0.0).on_hover_text("Sends small pictures from each clip to OpenAI (about 10 per clip). Your video files are not uploaded.").clicked() {
                        self.engine.plan_montage();
                    }
                });
                if let Some(text) = &self.engine.montage.busy {
                    ui.spinner();
                    ui.label(theme::soft(text));
                }
            });
            theme::checkbox(ui, &mut self.engine.settings.reuse_ai, "Reuse saved AI answers when nothing changed (no tokens spent)");
        });
        let Some(plan) = &self.engine.montage.plan else {
            if self.engine.montage.clips.is_empty() {
                ui.label(theme::muted("Add clips on the first step, then plan."));
            }
            return;
        };
        let _ = plan;
        self.preview_card(ui);
        let Some(plan) = &self.engine.montage.plan else { return };
        ui.add_space(4.0);
        ui.label(theme::muted(format!("Tempo the AI suggests: {}", plan.tempo)));
        if self.engine.montage.style == Style::Story {
            theme::card(ui, |ui| {
                ui.label(theme::bold("Title shown at the start", 14.0));
                ui.add(egui::TextEdit::singleline(&mut self.engine.montage.title).desired_width(f32::INFINITY).margin(Margin::symmetric(12, 9)));
            });
        }
        let mut view = None;
        theme::card(ui, |ui| {
            ui.label(theme::bold("Moments", 14.0));
            for (i, cut) in plan.cuts.iter().enumerate() {
                ui.horizontal(|ui| {
                    theme::checkbox(ui, &mut self.engine.montage.keep[i], "");
                    let (dot, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, clip_colour(cut.clip));
                    ui.label(theme::mono(format!("{} to {}", clock(cut.start), clock(cut.end)), 12.5).color(theme::TEXT2));
                    ui.label(theme::mono(format!("{:.0}%", cut.score * 100.0), 12.5).color(if cut.score > 0.7 { theme::OK } else { theme::TEXT3 }));
                    if theme::small_button(ui, "View").clicked() {
                        view = Some(i);
                    }
                    ui.add(egui::Label::new(theme::soft(&cut.why)).truncate());
                });
            }
        });
        if let Some(i) = view {
            self.engine.preview_select(i);
        }
    }

    // -------------------------------------------------------------------- make

    fn make_step(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::bold("Make", 18.0));
            ui.label(theme::soft("Choose the shapes and where they are saved."));
        });
        ui.add_space(6.0);
        self.preview_card(ui);
        let working = self.engine.montage.render.is_some();
        ui.add_enabled_ui(!working, |ui| {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::checkbox(ui, &mut self.engine.montage.make_wide, "");
                    ui.label(theme::bold("16:9 video", 14.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::muted("For YouTube")));
                });
            });
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::checkbox(ui, &mut self.engine.montage.make_vertical, "");
                    ui.label(theme::bold("9:16 video", 14.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::muted("Shorts, Reels, TikTok")));
                });
                if self.engine.montage.make_vertical {
                    theme::segmented(ui, &mut self.engine.montage.vertical, &[(Vertical::Follow, "Follow"), (Vertical::Centre, "Centre"), (Vertical::Blur, "Blurred")], true);
                    ui.label(theme::muted(match self.engine.montage.vertical {
                        Vertical::Follow => "A calm virtual camera that pans toward movement and detail.",
                        Vertical::Centre => "Always the middle of the picture.",
                        Vertical::Blur => "The whole picture over a blurred copy of itself. Zoom trims the sides so it fills more of the tall frame.",
                    }));
                    if self.engine.montage.vertical == Vertical::Blur {
                        theme::slider_row(ui, "Zoom", &mut self.engine.montage.blur.zoom, 1.0..=2.0, 1.0, false);
                        theme::slider_row(ui, "Left / right", &mut self.engine.montage.blur.x, 0.0..=1.0, 0.0, false);
                        theme::slider_row(ui, "Up / down", &mut self.engine.montage.blur.y, 0.0..=1.0, 0.0, false);
                        if theme::small_button(ui, "Centre and reset").clicked() {
                            self.engine.montage.blur = BlurLayout::default();
                        }
                    }
                }
            });
            theme::card(ui, |ui| {
                ui.label(theme::bold("Speed", 14.0));
                self.encoder_picker(ui, false);
            });
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::icon(ui, Icon::Folder, 18.0, theme::TEXT3);
                    let shown = self.engine.montage.out_dir.clone().or_else(|| self.engine.montage.clips.first().and_then(|c| c.path.parent().map(PathBuf::from)));
                    let text = shown.map_or_else(|| "next to the first clip".to_string(), |d| d.display().to_string());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::small_button(ui, "Change").clicked()
                            && let Some(dir) = rfd::FileDialog::new().pick_folder()
                        {
                            self.engine.montage.out_dir = Some(dir);
                        }
                        ui.add(egui::Label::new(theme::mono(text, 12.0).color(theme::TEXT2)).truncate());
                    });
                });
            });
        });
        if self.engine.montage.plan.is_none() {
            ui.label(theme::muted("Plan with AI first, on the Plan step."));
        }
    }
}

/// The 9:16 picture as the montage will frame it. Follow is shown centred, because it is only worked out while rendering.
fn paint_phone(ui: &egui::Ui, rect: Rect, tex: &egui::TextureHandle, vertical: Vertical, blur: BlurLayout) {
    let aspect = tex.size()[0] as f32 / tex.size()[1].max(1) as f32;
    let radius = egui::CornerRadius::same(16);
    let draw = |at: Rect, uv: Rect, tint: Color32, r: egui::CornerRadius| {
        egui::Image::new(egui::load::SizedTexture::new(tex.id(), at.size())).uv(uv).tint(tint).corner_radius(r).paint_at(ui, at);
    };
    ui.painter().rect_stroke(rect.expand(3.0), 19, Stroke::new(2.0, theme::LINE_STRONG), egui::StrokeKind::Outside);
    if vertical == Vertical::Blur {
        let b = blur.clamped();
        let cover = ((9.0 / 16.0) / aspect).min(1.0);
        draw(rect, Rect::from_min_max(pos2(0.5 - cover / 2.0, 0.0), pos2(0.5 + cover / 2.0, 1.0)), Color32::from_gray(70), radius);
        let (fw, fh) = (rect.width() * b.zoom, rect.width() * b.zoom / aspect);
        let shown_h = fh.min(rect.height());
        let top = rect.top() + (rect.height() - shown_h) * b.y;
        let u_span = (1.0 / b.zoom).min(1.0);
        let u0 = (1.0 - u_span) * b.x;
        let _ = fw;
        draw(Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), shown_h)), Rect::from_min_max(pos2(u0, 0.0), pos2(u0 + u_span, 1.0)), Color32::WHITE, egui::CornerRadius::ZERO);
    } else {
        let share = ((9.0 / 16.0) / aspect).min(1.0);
        let u0 = (1.0 - share) / 2.0;
        draw(rect, Rect::from_min_max(pos2(u0, 0.0), pos2(u0 + share, 1.0)), Color32::WHITE, radius);
    }
}

/// The three style cards.
fn choice_card(ui: &mut egui::Ui, width: f32, title: &str, blurb: &str, selected: bool) -> egui::Response {
    let galley = ui.painter().layout(blurb.to_string(), egui::FontId::proportional(13.0), theme::TEXT2, width - 32.0);
    let height = 52.0 + galley.size().y + 16.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height.max(120.0)), Sense::click());
    let painter = ui.painter();
    let fill = if selected { theme::accent_soft(26) } else if response.hovered() { theme::RAISED } else { theme::CARD };
    painter.rect(rect, 12, fill, Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { theme::ACCENT } else { theme::LINE_SOFT }), egui::StrokeKind::Inside);
    theme::radio_dot(painter, pos2(rect.left() + 26.0, rect.top() + 26.0), selected);
    painter.text(pos2(rect.left() + 46.0, rect.top() + 26.0), egui::Align2::LEFT_CENTER, title, egui::FontId::new(14.5, theme::bold_family()), theme::TEXT);
    painter.galley(pos2(rect.left() + 16.0, rect.top() + 52.0), galley, theme::TEXT2);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small choice chip, like "1 min".
fn chip(ui: &mut egui::Ui, label: &str, on: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::new(13.0, theme::bold_family()), if on { theme::ON_ACCENT } else { theme::TEXT2 });
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 28.0, 32.0), Sense::click());
    ui.painter().rect(rect, 16, if on { theme::ACCENT } else { theme::INPUT }, Stroke::new(1.0, if on { theme::ACCENT } else { theme::LINE_STRONG }), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, theme::TEXT);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn summary_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(theme::muted(label));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(value).size(13.0).color(theme::TEXT)).truncate());
        });
    });
}

/// One colour per clip, so the plan bar and the moment list match.
fn clip_colour(i: usize) -> Color32 {
    const PALETTE: [Color32; 6] = [Color32::from_rgb(0xFF, 0x6B, 0x3D), Color32::from_rgb(0x5B, 0xA8, 0xFF), Color32::from_rgb(0x62, 0xD0, 0x8F), Color32::from_rgb(0xF4, 0xBE, 0x4F), Color32::from_rgb(0xC0, 0x84, 0xFC), Color32::from_rgb(0xFF, 0x8F, 0xB8)];
    PALETTE[i % PALETTE.len()]
}
