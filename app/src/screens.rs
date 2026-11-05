//! The window's shell and the video screens: top bar, workflow rail, status bar,
//! progress toast, and the Start, Source, Look and Export screens.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use eframe::egui::{self, Align, Align2, Color32, FontId, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::media::{self, Encoder, EncoderChoice};
use crate::theme::{self, Icon, Kind};
use crate::{App, Compare, Mode, Step, VerticalFraming, ai, clock, store};

fn video_filter() -> &'static [&'static str] {
    &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "wmv"]
}

/// "NVIDIA GeForce RTX 3060" becomes "RTX 3060".
fn short_gpu(name: &str) -> String {
    name.replace("NVIDIA GeForce ", "").replace("NVIDIA ", "").replace("AMD Radeon ", "Radeon ").replace("(TM)", "").trim().to_string()
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

impl App {
    // ------------------------------------------------------------------ shell

    pub fn shell(&mut self, ui: &mut Ui) {
        self.top_bar(ui);
        egui::Panel::bottom("status").exact_size(30.0).show_separator_line(true).frame(Frame::new().fill(theme::WINDOW).inner_margin(Margin::symmetric(20, 0))).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let colour = if self.status_is_error { theme::BAD } else { theme::TEXT3 };
                ui.add(egui::Label::new(RichText::new(&self.status).size(12.5).color(colour)).truncate());
            });
        });
        self.rail(ui);
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        egui::Panel::top("bar").exact_size(56.0).frame(Frame::new().fill(theme::WINDOW).inner_margin(Margin::symmetric(20, 0))).show(ui, |ui| {
            let bar = ui.max_rect();
            ui.horizontal_centered(|ui| {
                let (logo, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                ui.painter().rect_filled(logo, 9, theme::ACCENT);
                theme::paint_icon(ui.painter(), logo.center() + vec2(1.0, 0.0), 15.0, Icon::Play, theme::ON_ACCENT, 2.0);
                ui.label(theme::bold("Video Enhancer", 15.0));
                ui.add_space(4.0);
                let (sep, _) = ui.allocate_exact_size(vec2(1.0, 22.0), Sense::hover());
                ui.painter().rect_filled(sep, 0, theme::LINE);
                match (self.mode, &self.source) {
                    (Mode::Single, Some(s)) => {
                        ui.label(RichText::new(s.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())).size(14.0).color(theme::TEXT));
                        if bar.width() > 1180.0 {
                            ui.label(theme::mono(format!("{}×{} · {:.0} fps · {}", s.probe.width, s.probe.height, s.probe.fps, clock(s.probe.duration)), 12.0).color(theme::TEXT3));
                        }
                    }
                    (Mode::Single, None) => {
                        ui.label(theme::muted("No video yet"));
                    }
                    (Mode::Montage, _) => {
                        ui.label(RichText::new("New montage").size(14.0).color(theme::TEXT));
                        ui.label(theme::mono(self.mon.summary_line(), 12.0).color(theme::TEXT3));
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::icon_button(ui, Icon::Gear, "Settings and API keys").clicked() {
                        self.show_settings = true;
                    }
                    ui.add_space(4.0);
                    match &self.gpu_status {
                        Some(Ok(name)) => theme::status_pill(ui, &if self.gpu_grade { format!("{} · colour on graphics card", short_gpu(name)) } else { short_gpu(name) }, theme::OK),
                        Some(Err(_)) => theme::status_pill(ui, "No graphics-card grading", theme::WARN),
                        None => theme::status_pill(ui, "Checking graphics card", theme::TEXT3),
                    }
                });
            });
            // The mode switch sits in the middle of the bar.
            let mut mode = self.mode;
            let seg = Rect::from_center_size(bar.center(), vec2(210.0, 42.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(seg), |ui| {
                if theme::segmented(ui, &mut mode, &[(Mode::Single, "Video"), (Mode::Montage, "Montage")], true) {
                    self.switch_mode(mode);
                }
            });
        });
    }

    pub fn switch_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.player = None;
        self.mon.stop_preview();
        self.step = Step::Source;
        self.status_is_error = false;
        self.status = match mode {
            Mode::Montage => "Add several clips (or drop them here), choose your options, then plan with AI.".into(),
            Mode::Single => "Drop a video here, or press Open a video.".into(),
        };
    }

    fn rail(&mut self, ui: &mut Ui) {
        let labels = match self.mode {
            Mode::Single => ["Source", "Look", "Export", "Publish"],
            Mode::Montage => ["Clips", "Plan", "Make", "Publish"],
        };
        let icons = [if self.mode == Mode::Single { Icon::Film } else { Icon::Film }, if self.mode == Mode::Single { Icon::Sliders } else { Icon::Sparkle }, Icon::Export, Icon::Send];
        let steps = [Step::Source, Step::Look, Step::Export, Step::Publish];
        let done = match self.mode {
            Mode::Single => [self.source.is_some(), self.step as u8 > Step::Look as u8, !self.made.is_empty(), self.pack1.pack.is_some()],
            Mode::Montage => [self.mon.has_clips(), self.mon.has_plan(), self.mon.has_made(), self.mon.has_pack()],
        };
        egui::Panel::left("rail").exact_size(92.0).frame(Frame::new().fill(theme::WINDOW).inner_margin(Margin::symmetric(8, 14))).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for i in 0..4 {
                let active = self.step == steps[i];
                let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 76.0), Sense::click());
                let painter = ui.painter();
                if active {
                    painter.rect_filled(rect, 12, theme::accent_soft(33));
                    painter.rect_filled(Rect::from_min_max(pos2(rect.left(), rect.top() + 18.0), pos2(rect.left() + 3.0, rect.bottom() - 18.0)), 2, theme::ACCENT);
                } else if response.hovered() {
                    painter.rect_filled(rect, 12, theme::CARD);
                }
                let colour = if active { theme::ACCENT_TEXT } else { Color32::from_rgb(0x9A, 0x97, 0x8F) };
                theme::paint_icon(painter, pos2(rect.center().x, rect.top() + 28.0), 22.0, icons[i], colour, 1.75);
                painter.text(pos2(rect.center().x, rect.top() + 55.0), Align2::CENTER_CENTER, labels[i], FontId::new(12.5, theme::bold_family()), colour);
                if done[i] && !active {
                    let badge = pos2(rect.right() - 20.0, rect.top() + 16.0);
                    painter.circle_filled(badge, 7.0, theme::OK);
                    theme::paint_icon(painter, badge, 9.0, Icon::Check, theme::WINDOW, 3.4);
                }
                if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.step = steps[i];
                }
            }
            ui.with_layout(Layout::bottom_up(Align::Center), |ui| ui.label(theme::muted("Autosaved").size(11.5)));
        });
    }

    /// Shows the running job, whichever it is, as a small card over the bottom left.
    pub fn toast(&mut self, ctx: &egui::Context) {
        let job: Option<(String, Option<f32>, String, Option<std::sync::Arc<std::sync::atomic::AtomicBool>>)> = if let Some(e) = &self.export {
            Some((e.label.clone(), Some(e.progress), "Writing the video".into(), Some(e.cancel.clone())))
        } else if let Some((p, label, cancel)) = self.mon.render_info() {
            Some((label, Some(p), "Rendering the montage".into(), Some(cancel)))
        } else if let Some(text) = self.ai_busy.clone().or_else(|| self.mon.busy_text()) {
            Some((text, None, "Working with the AI".into(), None))
        } else {
            self.pack_busy_text().map(|t| (t, None, "Writing your upload pack".into(), None))
        };
        let Some((label, progress, title, cancel)) = job else { return };
        #[allow(deprecated)]
        let id = egui::Id::new("toast");
        egui::Area::new(id).anchor(Align2::LEFT_BOTTOM, vec2(116.0, -46.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            Frame::new().fill(Color32::from_rgb(0x21, 0x22, 0x26)).stroke(Stroke::new(1.0, theme::LINE_STRONG)).corner_radius(16).inner_margin(Margin::symmetric(18, 14)).shadow(egui::Shadow { offset: [0, 12], blur: 36, spread: 0, color: Color32::from_black_alpha(140) }).show(ui, |ui| {
                ui.set_width(440.0);
                ui.horizontal(|ui| {
                    ui.label(theme::bold(&title, 14.0));
                    if let Some(p) = progress {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::mono(format!("{:.0}%", p * 100.0), 13.0)));
                    } else {
                        ui.spinner();
                    }
                });
                if let Some(p) = progress {
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
                    ui.painter().rect_filled(rect, 4, theme::TRACK);
                    ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * p.clamp(0.0, 1.0), 8.0)), 4, theme::ACCENT);
                }
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(theme::muted(label)).truncate());
                    if let Some(c) = &cancel {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::small_button(ui, "Cancel").clicked() {
                                c.store(true, Ordering::Relaxed);
                            }
                        });
                    }
                });
            });
        });
    }

    /// API keys and housekeeping, in a window of its own.
    pub fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings").open(&mut open).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.set_width(430.0);
            ui.label(theme::bold("AI keys", 15.0));
            ui.label(theme::muted("Stored in your computer's secure credential store, never in a file."));
            ui.add_space(4.0);
            for (provider, name, hint) in [(ai::Provider::OpenAi, "OpenAI", "Montage plans, upload packs and look suggestions"), (ai::Provider::Gemini, "Gemini", "Optional: copies a look from Gemini Omni")] {
                let has = if provider == ai::Provider::OpenAi { self.has_openai } else { self.has_gemini };
                let mut save = false;
                let mut remove = false;
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::bold(name, 14.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if has {
                                theme::status_pill(ui, "Saved", theme::OK);
                            } else {
                                theme::status_pill(ui, "Not added", theme::TEXT3);
                            }
                        });
                    });
                    ui.label(theme::muted(hint));
                    let buffer = if provider == ai::Provider::OpenAi { &mut self.openai_key } else { &mut self.gemini_key };
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(buffer).password(true).hint_text(format!("Paste your {name} key")).desired_width(250.0).margin(Margin::symmetric(12, 10)));
                        save = theme::small_button(ui, "Save").clicked();
                        if has {
                            remove = theme::small_button(ui, "Remove").clicked();
                        }
                    });
                });
                if save {
                    let buffer = if provider == ai::Provider::OpenAi { self.openai_key.clone() } else { self.gemini_key.clone() };
                    match ai::save_key(provider, &buffer) {
                        Ok(()) => {
                            if provider == ai::Provider::OpenAi {
                                self.openai_key.clear();
                                self.has_openai = ai::load_key(provider).is_some();
                            } else {
                                self.gemini_key.clear();
                                self.has_gemini = ai::load_key(provider).is_some();
                            }
                        }
                        Err(e) => self.fail(e),
                    }
                }
                if remove {
                    let _ = ai::save_key(provider, "");
                    if provider == ai::Provider::OpenAi {
                        self.has_openai = ai::load_key(provider).is_some();
                    } else {
                        self.has_gemini = ai::load_key(provider).is_some();
                    }
                }
            }
            ui.add_space(6.0);
            ui.label(theme::bold("Saved AI answers", 15.0));
            theme::toggle_row(ui, &mut self.reuse_ai, "Reuse a saved answer when nothing has changed (no tokens spent)");
            if theme::small_button(ui, "Forget all saved answers").clicked() {
                let n = store::clear_ai_cache();
                self.note(format!("Forgot {n} saved AI answer(s). The next request will ask the AI again."));
            }
            if self.tools.is_none() {
                ui.add_space(6.0);
                ui.label(theme::bold("Video tools", 15.0));
                self.tools_card(ui);
            }
        });
        if !open {
            self.show_settings = false;
        }
    }

    fn tools_card(&mut self, ui: &mut Ui) {
        theme::card(ui, |ui| {
            ui.label(theme::bold("Video tools needed", 14.0));
            ui.label(theme::soft("FFmpeg reads and writes your video. Download it once (about 110 MB, checked for safety before use)."));
            if let Some(p) = self.tools_download {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
                ui.painter().rect_filled(rect, 4, theme::TRACK);
                ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * p.clamp(0.0, 1.0), 8.0)), 4, theme::ACCENT);
            } else if theme::button(ui, "Download video tools", Kind::Primary, 0.0).clicked() {
                self.start_tools_download();
            }
        });
    }

    // ------------------------------------------------------------------ Start

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Video", video_filter()).pick_file() {
            self.open(path);
        }
    }

    pub fn start_screen(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(1000.0);
                ui.add_space(36.0);
                // Drop zone.
                let (zone, _) = ui.allocate_exact_size(vec2(ui.available_width(), 300.0), Sense::hover());
                let painter = ui.painter();
                painter.rect(zone, 20, Color32::from_rgb(0x14, 0x15, 0x18), Stroke::new(2.0, theme::LINE_STRONG), StrokeKind::Inside);
                painter.rect_filled(Rect::from_min_size(zone.min + vec2(2.0, 2.0), vec2(zone.width() - 4.0, 110.0)), CornerRadiusTop, theme::accent_soft(18));
                let c = zone.center_top() + vec2(0.0, 62.0);
                painter.rect_filled(Rect::from_center_size(c, vec2(64.0, 64.0)), 18, theme::accent_soft(36));
                theme::paint_icon(painter, c, 30.0, Icon::Upload, theme::ACCENT_TEXT, 1.75);
                painter.text(zone.center_top() + vec2(0.0, 128.0), Align2::CENTER_CENTER, "Drop your gameplay here", FontId::new(28.0, theme::bold_family()), theme::TEXT);
                painter.text(zone.center_top() + vec2(0.0, 166.0), Align2::CENTER_CENTER, "Drop one video to colour-grade it for YouTube, or several clips to cut a montage.", FontId::new(15.0, egui::FontFamily::Proportional), theme::TEXT2);
                let buttons = Rect::from_center_size(zone.center_top() + vec2(0.0, 236.0), vec2(360.0, 48.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(buttons).layout(Layout::left_to_right(Align::Center)), |ui| {
                    ui.add_enabled_ui(self.tools.is_some(), |ui| {
                        if theme::button(ui, "Open a video", Kind::Primary, 170.0).clicked() {
                            self.open_dialog();
                        }
                    });
                    if theme::button(ui, "Start a montage", Kind::Secondary, 170.0).clicked() {
                        self.switch_mode(Mode::Montage);
                    }
                });
                ui.add_space(28.0);

                ui.add_space(8.0);
                ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::bold("Ready to go", 15.0));
                        ui.label(theme::muted("Checked on this computer"));
                    });
                    ui.add_space(4.0);
                    let width = (ui.available_width() - 28.0) / 3.0;
                    let tools_ok = self.tools.is_some();
                    let gpu = self.gpu_status.clone();
                    let (has_openai, has_gemini) = (self.has_openai, self.has_gemini);
                    let encoder = self.encoder_choice.resolve(&self.encoders);
                    let mut open_settings = false;
                    let mut download = false;
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        ready_card(ui, width, "Video tools", if tools_ok { Some(true) } else { Some(false) }, if tools_ok { "Installed" } else { "Needed" }, "FFmpeg reads, grades and writes your video.", if tools_ok { None } else { Some("Download video tools") }, &mut download);
                        let (state, detail) = match &gpu {
                            Some(Ok(name)) => (Some(true), format!("{}. Colour grading and encoding both run on the card, about 5 times faster.{}", short_gpu(name), if encoder == Encoder::Cpu { "" } else { "" })),
                            Some(Err(why)) => (Some(false), format!("Not used: {why}. FFmpeg grades the colour instead.")),
                            None => (None, "Checking…".to_string()),
                        };
                        ready_card(ui, width, "Graphics card", state, match state { Some(true) => "Ready", Some(false) => "Not used", None => "Checking" }, &detail, None, &mut download);
                        let (state, label, detail) = if has_openai { (Some(true), "Ready", if has_gemini { "OpenAI and Gemini keys are saved." } else { "OpenAI key saved. Gemini is optional." }) } else { (Some(false), "OpenAI missing", "Add your own OpenAI key for montage plans and upload packs. Keys stay in Windows Credential Manager.") };
                        ready_card(ui, width, "AI keys", state, label, detail, if has_openai { None } else { Some("Add a key") }, &mut open_settings);
                    });
                    if download {
                        self.start_tools_download();
                    }
                    if open_settings {
                        self.show_settings = true;
                    }
                    if let Some(p) = self.tools_download {
                        ui.add_space(6.0);
                        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
                        ui.painter().rect_filled(rect, 4, theme::TRACK);
                        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * p.clamp(0.0, 1.0), 8.0)), 4, theme::ACCENT);
                    }

                    if !self.recent.is_empty() {
                        ui.add_space(26.0);
                        ui.horizontal(|ui| {
                            ui.label(theme::bold("Pick up where you left off", 15.0));
                            ui.label(theme::muted("Grades and AI results are saved, so nothing is paid for twice"));
                        });
                        ui.add_space(4.0);
                        let mut chosen = None;
                        let width = (ui.available_width() - 28.0) / 3.0;
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 14.0;
                            for item in self.recent.iter().take(3) {
                                let (rect, response) = ui.allocate_exact_size(vec2(width, 76.0), Sense::click());
                                let painter = ui.painter();
                                painter.rect(rect, 14, if response.hovered() { theme::RAISED } else { Color32::from_rgb(0x1A, 0x1B, 0x1E) }, Stroke::new(1.0, theme::LINE_SOFT), StrokeKind::Inside);
                                let thumb = Rect::from_min_size(rect.min + vec2(8.0, 8.0), vec2(104.0, 60.0));
                                painter.rect_filled(thumb, 9, theme::INPUT);
                                theme::paint_icon(painter, thumb.center(), 22.0, Icon::Film, theme::TEXT3, 1.5);
                                let name = item.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                                painter.text(pos2(thumb.right() + 12.0, rect.top() + 22.0), Align2::LEFT_CENTER, ellipsize(&name, 24), FontId::new(14.0, theme::bold_family()), theme::TEXT);
                                painter.text(pos2(thumb.right() + 12.0, rect.top() + 42.0), Align2::LEFT_CENTER, format!("{} · {}p{:.0}", clock(item.duration), item.height, item.fps), FontId::new(12.0, egui::FontFamily::Monospace), theme::TEXT3);
                                painter.text(pos2(thumb.right() + 12.0, rect.top() + 60.0), Align2::LEFT_CENTER, "Look and AI results saved", FontId::new(12.0, theme::bold_family()), theme::OK);
                                if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    chosen = Some(item.path.clone());
                                }
                            }
                        });
                        if let Some(path) = chosen.filter(|p| p.is_file()) {
                            self.open(path);
                        }
                    }
                });
            });
        });
    }

    /// The Source step once a video is open: what it is, and a way to swap it.
    pub fn source_screen(&mut self, ui: &mut Ui) {
        let Some(source) = &self.source else { return };
        let (name, probe) = (source.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()), source.probe.clone());
        let mut replace = false;
        let mut next = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(760.0);
                ui.add_space(36.0);
                ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
                    ui.label(theme::bold("Source", 18.0));
                    ui.add_space(6.0);
                    theme::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (thumb, _) = ui.allocate_exact_size(vec2(220.0, 124.0), Sense::hover());
                            if let Some(tex) = &self.before_tex {
                                ui.put(thumb, egui::Image::new(egui::load::SizedTexture::new(tex.id(), thumb.size())).corner_radius(10).fit_to_exact_size(thumb.size()));
                            }
                            ui.vertical(|ui| {
                                ui.label(theme::bold(&name, 17.0));
                                ui.label(theme::mono(format!("{}×{} · {:.0} fps · {}", probe.width, probe.height, probe.fps, clock(probe.duration)), 12.5).color(theme::TEXT2));
                                ui.add_space(4.0);
                                if probe.hdr {
                                    theme::pill(ui, "HDR, converted to standard colour", theme::WARN, theme::tint(theme::WARN, 30), None);
                                }
                                if !probe.is_wide() {
                                    theme::pill(ui, "Not 16:9, the wide version is cropped", theme::WARN, theme::tint(theme::WARN, 30), None);
                                }
                                if probe.has_audio {
                                    theme::pill(ui, "Has sound", theme::TEXT2, theme::INPUT, None);
                                }
                            });
                        });
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        replace = theme::button(ui, "Open a different video", Kind::Secondary, 0.0).clicked();
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            next = theme::button_with_icon(ui, "Continue to Look", Some(Icon::ArrowRight), Kind::Primary, 0.0).clicked();
                        });
                    });
                });
            });
        });
        if replace {
            self.open_dialog();
        }
        if next {
            self.step = Step::Look;
        }
    }

    // ------------------------------------------------------------------- Look

    fn drag_value(&self, rect: Rect, pos: egui::Pos2) -> f32 {
        ((pos.x - rect.left()) / rect.width()).clamp(0.02, 0.98)
    }

    /// The before and after picture, with a divider you can drag.
    fn draw_preview(&mut self, ui: &mut Ui, region: Rect) {
        let (Some(before), Some(after)) = (self.before_tex.clone(), self.after_tex.clone()) else { return };
        let aspect = before.size()[0] as f32 / before.size()[1].max(1) as f32;
        let (w, h) = if region.width() / region.height().max(1.0) > aspect { (region.height() * aspect, region.height()) } else { (region.width(), region.width() / aspect) };
        let rect = Rect::from_center_size(region.center(), vec2(w.max(10.0), h.max(10.0)));
        let response = ui.interact(rect, ui.scope_id().with("preview"), Sense::click_and_drag());
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let image = |tex: &egui::TextureHandle, at: Rect, uv: Rect, radius: egui::CornerRadius| {
            egui::Image::new(egui::load::SizedTexture::new(tex.id(), at.size())).uv(uv).corner_radius(radius).paint_at(ui, at);
        };
        let round = egui::CornerRadius::same(14);
        ui.painter().rect_stroke(rect.expand(1.0), 15, Stroke::new(1.0, theme::LINE), StrokeKind::Outside);
        let label = |ui: &Ui, at: Rect, text: &str, after: bool, right: bool| {
            let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::new(12.0, theme::bold_family()), if after { theme::ON_ACCENT } else { theme::TEXT });
            let size = galley.size() + vec2(20.0, 10.0);
            let pos = if right { pos2(at.right() - 14.0 - size.x, at.top() + 14.0) } else { pos2(at.left() + 14.0, at.top() + 14.0) };
            let r = Rect::from_min_size(pos, size);
            ui.painter().rect_filled(r, 7, if after { theme::ACCENT } else { Color32::from_black_alpha(190) });
            ui.painter().galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
        };
        match self.compare {
            Compare::Before => {
                image(&before, rect, full, round);
                label(ui, rect, "BEFORE", false, false);
            }
            Compare::After => {
                image(&after, rect, full, round);
                label(ui, rect, "AFTER", true, false);
            }
            Compare::Split => {
                if let Some(p) = response.interact_pointer_pos().filter(|_| response.dragged() || response.clicked()) {
                    self.split = self.drag_value(rect, p);
                }
                let x = rect.left() + rect.width() * self.split;
                image(&after, rect, full, round);
                let left = Rect::from_min_max(rect.min, pos2(x, rect.bottom()));
                image(&before, left, Rect::from_min_max(pos2(0.0, 0.0), pos2(self.split, 1.0)), egui::CornerRadius { nw: 14, sw: 14, ne: 0, se: 0 });
                let painter = ui.painter();
                painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(2.0, Color32::WHITE));
                painter.circle(pos2(x, rect.center().y), 18.0, theme::ACCENT, Stroke::new(3.0, Color32::WHITE));
                painter.line_segment([pos2(x - 5.0, rect.center().y), pos2(x - 8.0, rect.center().y)], Stroke::new(2.5, theme::ON_ACCENT));
                theme::paint_icon(painter, pos2(x, rect.center().y), 14.0, Icon::ArrowRight, theme::ON_ACCENT, 2.4);
                label(ui, rect, "BEFORE", false, false);
                label(ui, rect, "AFTER", true, true);
            }
            Compare::Side => {
                let half = Rect::from_min_max(rect.min, pos2(rect.center().x - 3.0, rect.bottom()));
                let other = half.translate(vec2(rect.width() / 2.0 + 3.0, 0.0));
                image(&before, half, full, egui::CornerRadius { nw: 14, sw: 14, ne: 0, se: 0 });
                image(&after, other, full, egui::CornerRadius { nw: 0, sw: 0, ne: 14, se: 14 });
                label(ui, half, "BEFORE", false, false);
                label(ui, other, "AFTER", true, false);
            }
        }
    }

    pub fn look_center(&mut self, ui: &mut Ui) {
        Frame::new().inner_margin(Margin { left: 24, right: 24, top: 16, bottom: 0 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::bold("Look", 18.0));
                if ui.available_width() > 600.0 {
                    ui.label(theme::soft("Drag the divider to compare"));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    theme::segmented(ui, &mut self.compare, &[(Compare::After, "After"), (Compare::Before, "Before"), (Compare::Side, "Side by side"), (Compare::Split, "Slide")], false);
                });
            });
            let strip_h = 150.0;
            let region_h = (ui.available_height() - strip_h - 14.0).max(120.0);
            let (region, _) = ui.allocate_exact_size(vec2(ui.available_width(), region_h), Sense::hover());
            self.draw_preview(ui, region);
            ui.add_space(12.0);
            self.looks_strip(ui);
        });
    }

    fn looks_strip(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::bold("Looks", 13.0));
            theme::pill(ui, "Auto-fix on", theme::OK, theme::tint(theme::OK, 30), None);
            ui.label(theme::muted("Exposure and white balance are already corrected"));
        });
        let gap = 10.0;
        let w = ((ui.available_width() - 6.0 - gap * 7.0) / 8.0).floor();
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, look) in crate::grade::LOOKS.iter().enumerate() {
                let (rect, response) = ui.allocate_exact_size(vec2(w, w * 9.0 / 16.0 + 40.0), Sense::click());
                let on = self.look == Some(i);
                let painter = ui.painter();
                painter.rect(rect, 11, if on { theme::accent_soft(26) } else { theme::CARD }, Stroke::new(if on { 1.5 } else { 1.0 }, if on { theme::ACCENT } else { theme::LINE_SOFT }), StrokeKind::Inside);
                let thumb = Rect::from_min_size(rect.min + vec2(4.0, 4.0), vec2(w - 8.0, (w - 8.0) * 9.0 / 16.0));
                match self.look_tex.get(i) {
                    Some(tex) => {
                        egui::Image::new(egui::load::SizedTexture::new(tex.id(), thumb.size())).corner_radius(7).paint_at(ui, thumb);
                    }
                    None => {
                        painter.rect_filled(thumb, 7, theme::INPUT);
                    }
                }
                ui.painter().text(pos2(rect.left() + 8.0, thumb.bottom() + 16.0), Align2::LEFT_CENTER, ellipsize(look.name, ((w - 16.0) / 7.4) as usize), FontId::new(12.5, theme::bold_family()), theme::TEXT);
                if response.on_hover_text(look.note).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    picked = Some(i);
                }
            }
        });
        if let Some(i) = picked {
            self.pick_look(i);
        }
    }

    pub fn timeline_panel(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("timeline").exact_size(168.0).frame(Frame::new().fill(Color32::from_rgb(0x13, 0x14, 0x16)).inner_margin(Margin::symmetric(24, 12))).show(ui, |ui| {
            let Some(duration) = self.source.as_ref().map(|s| s.probe.duration) else {
                ui.label(theme::muted("This is a sample picture. Open a video to scrub and play."));
                return;
            };
            ui.horizontal(|ui| {
                let playing = self.player.is_some();
                if theme::round_button(ui, if playing { Icon::Pause } else { Icon::Play }, if playing { "Pause" } else { "Play" }).clicked() {
                    self.toggle_play();
                }
                ui.label(theme::mono(format!("{} ", clock(self.t)), 14.0).color(theme::TEXT));
                ui.label(theme::mono(format!("/ {}", clock(duration)), 14.0).color(theme::TEXT3));
                theme::pill(ui, &format!("Exporting {} to {} · {}", clock_short(self.trim.0), clock_short(self.trim.1), clock_short(self.trim.1 - self.trim.0)), theme::ACCENT_TEXT, theme::accent_soft(36), None);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::button(ui, "Whole video", Kind::Ghost, 0.0).clicked() {
                        self.trim = (0.0, duration);
                    }
                    if theme::button(ui, "End here", Kind::Secondary, 0.0).on_hover_text("Export ends at this moment").clicked() {
                        self.trim.1 = self.t.max(self.trim.0 + 0.5).min(duration);
                    }
                    if theme::button(ui, "Start here", Kind::Secondary, 0.0).on_hover_text("Export begins at this moment").clicked() {
                        self.trim.0 = self.t.min(self.trim.1 - 0.5).max(0.0);
                    }
                });
            });
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ui.available_height().max(56.0)), Sense::click_and_drag());
            let x_of = |s: f64| rect.left() + 10.0 + (rect.width() - 20.0) * (s / duration.max(0.01)) as f32;
            let t_of = |x: f32| (((x - rect.left() - 10.0) / (rect.width() - 20.0)).clamp(0.0, 1.0) as f64) * duration;
            // Handles first, so dragging one never moves the playhead.
            let handle = |ui: &mut Ui, at: f32, id: &str| ui.interact(Rect::from_center_size(pos2(at, rect.center().y), vec2(18.0, rect.height())), ui.scope_id().with(id), Sense::drag());
            let (left, right) = (handle(ui, x_of(self.trim.0), "trim-in"), handle(ui, x_of(self.trim.1), "trim-out"));
            if let Some(p) = left.interact_pointer_pos().filter(|_| left.dragged()) {
                self.trim.0 = t_of(p.x).min(self.trim.1 - 0.5).max(0.0);
            } else if let Some(p) = right.interact_pointer_pos().filter(|_| right.dragged()) {
                self.trim.1 = t_of(p.x).max(self.trim.0 + 0.5).min(duration);
            } else if let Some(p) = response.interact_pointer_pos().filter(|_| response.dragged() || response.clicked()) {
                let t = t_of(p.x);
                self.player = None;
                self.t = t;
                self.request_frame(t);
            }
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 10, theme::CARD);
            let bars = ((rect.width() - 20.0) / 6.0) as usize;
            for i in 0..bars {
                let f = i as f32 / bars as f32;
                let level = self.activity.as_ref().map_or(0.35, |a| {
                    let from = (f * a.len() as f32) as usize;
                    let to = (((i + 1) as f32 / bars as f32) * a.len() as f32).ceil() as usize;
                    let slice = &a[from.min(a.len().saturating_sub(1))..to.clamp(from + 1, a.len().max(from + 1)).min(a.len())];
                    if slice.is_empty() { 0.3 } else { slice.iter().sum::<f32>() / slice.len() as f32 }
                });
                let x = rect.left() + 10.0 + f * (rect.width() - 20.0) + 2.0;
                let half = (4.0 + level.clamp(0.05, 1.0) * (rect.height() * 0.38)).min(rect.height() * 0.45);
                let inside = (x as f64) >= x_of(self.trim.0) as f64 && (x as f64) <= x_of(self.trim.1) as f64;
                painter.rect_filled(Rect::from_center_size(pos2(x, rect.center().y), vec2(3.0, half * 2.0)), 1.5, if inside { Color32::from_rgb(0x6F, 0x72, 0x80) } else { Color32::from_rgb(0x3A, 0x3B, 0x42) });
            }
            let dim = Color32::from_rgba_unmultiplied(15, 16, 18, 180);
            painter.rect_filled(Rect::from_min_max(rect.min, pos2(x_of(self.trim.0), rect.bottom())), 10, dim);
            painter.rect_filled(Rect::from_min_max(pos2(x_of(self.trim.1), rect.top()), rect.max), 10, dim);
            for x in [x_of(self.trim.0), x_of(self.trim.1)] {
                painter.rect_filled(Rect::from_center_size(pos2(x, rect.center().y), vec2(6.0, rect.height())), 3, theme::ACCENT);
            }
            let head = x_of(self.t);
            painter.rect_filled(Rect::from_min_max(pos2(head - 1.0, rect.top()), pos2(head + 1.0, rect.bottom())), 0, Color32::WHITE);
            let flag = painter.layout_no_wrap(clock_short(self.t), FontId::new(11.0, egui::FontFamily::Monospace), theme::WINDOW);
            let flag_rect = Rect::from_min_size(pos2(head + 6.0, rect.top() + 4.0), flag.size() + vec2(10.0, 3.0));
            painter.rect_filled(flag_rect, 5, Color32::WHITE);
            painter.galley(flag_rect.center() - flag.size() / 2.0, flag, theme::WINDOW);
        });
    }

    pub fn look_inspector(&mut self, ui: &mut Ui) {
        let have = self.source.is_some();
        egui::Panel::right("inspector").exact_size(372.0).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(20, 0))).show(ui, |ui| {
            let mut next = false;
            egui::Panel::bottom("inspector-footer").exact_size(84.0).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(0, 16))).show_separator_line(true).show(ui, |ui| {
                next = theme::button_with_icon(ui, "Continue to Export", Some(Icon::ArrowRight), Kind::Primary, ui.available_width()).clicked();
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.label(theme::bold("Adjust", 15.0));
                    if let Some(i) = self.look {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::muted(crate::grade::LOOKS[i].name)));
                    }
                });
                ui.add_space(2.0);
                theme::card(ui, |ui| {
                    theme::slider_row(ui, "Overall amount", &mut self.grade.strength, 0.0..=1.0, 0.0, false);
                    if let Some(learned) = &mut self.learned {
                        theme::slider_row(ui, "Omni look amount", &mut learned.amount, 0.0..=1.0, 0.0, false);
                    }
                });
                let mut touched = false;
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::bold("Light", 14.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::small_button(ui, "Reset").clicked() {
                                let g = &mut self.grade;
                                (g.exposure, g.contrast, g.highlights, g.shadows) = (0.0, 0.0, 0.0, 0.0);
                                touched = true;
                            }
                        });
                    });
                    let g = &mut self.grade;
                    touched |= theme::slider_row(ui, "Exposure", &mut g.exposure, -1.0..=1.0, 0.0, true);
                    touched |= theme::slider_row(ui, "Contrast", &mut g.contrast, -0.5..=0.6, 0.0, true);
                    touched |= theme::slider_row(ui, "Highlights", &mut g.highlights, -1.0..=1.0, 0.0, true);
                    touched |= theme::slider_row(ui, "Shadows", &mut g.shadows, -1.0..=1.0, 0.0, true);
                });
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::bold("Colour", 14.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::small_button(ui, "Reset").clicked() {
                                let g = &mut self.grade;
                                (g.temperature, g.tint, g.saturation, g.vibrance, g.teal_orange, g.fade) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
                                touched = true;
                            }
                        });
                    });
                    let g = &mut self.grade;
                    touched |= theme::slider_row(ui, "Warmth", &mut g.temperature, -1.0..=1.0, 0.0, true);
                    touched |= theme::slider_row(ui, "Tint", &mut g.tint, -1.0..=1.0, 0.0, true);
                    touched |= theme::slider_row(ui, "Saturation", &mut g.saturation, -0.6..=0.6, 0.0, true);
                    touched |= theme::slider_row(ui, "Vibrance", &mut g.vibrance, 0.0..=0.8, 0.0, true);
                    if self.more_colour_open {
                        touched |= theme::slider_row(ui, "Teal and orange", &mut g.teal_orange, 0.0..=1.0, 0.0, false);
                        touched |= theme::slider_row(ui, "Fade", &mut g.fade, 0.0..=0.2, 0.0, false);
                    }
                    if theme::small_button(ui, if self.more_colour_open { "Fewer colour controls" } else { "More colour controls" }).clicked() {
                        self.more_colour_open = !self.more_colour_open;
                    }
                });
                if touched {
                    self.look = None;
                }
                let finish_text = format!("Sharpen {} · Sound level {}", if self.finish.sharpen > 0.01 { "on" } else { "off" }, if self.finish.loudness { "on" } else { "off" });
                theme::card(ui, |ui| {
                    let header = ui.horizontal(|ui| {
                        ui.label(theme::bold("Finish", 14.0));
                        ui.label(theme::muted(finish_text));
                    }).response.interact(Sense::click());
                    if header.clicked() {
                        self.finish_open = !self.finish_open;
                    }
                    if self.finish_open {
                        theme::slider_row(ui, "Sharpen (slower export)", &mut self.finish.sharpen, 0.0..=1.0, 0.0, false);
                        theme::slider_row(ui, "Noise cleanup", &mut self.finish.denoise, 0.0..=1.0, 0.0, false);
                        theme::toggle_row(ui, &mut self.finish.loudness, "Even out the sound (YouTube level)");
                    }
                });
                self.ai_assist_card(ui, have);
                ui.add_space(16.0);
            });
            if next {
                self.step = Step::Export;
            }
        });
    }

    fn ai_assist_card(&mut self, ui: &mut Ui, have_video: bool) {
        theme::card_with(ui, theme::accent_soft(14), theme::accent_soft(70), |ui| {
            ui.horizontal(|ui| {
                theme::icon(ui, Icon::Sparkle, 16.0, theme::ACCENT_TEXT);
                ui.label(theme::bold("AI assist", 14.0));
                ui.label(theme::muted("uses your own keys"));
            });
            ui.label(theme::muted("What kind of look do you want?"));
            ui.add(egui::TextEdit::multiline(&mut self.style_notes).desired_rows(2).desired_width(f32::INFINITY).margin(Margin::symmetric(12, 9)));
            let busy = self.ai_busy.is_some();
            let width = (ui.available_width() - 8.0) / 2.0;
            ui.horizontal(|ui| {
                if ai_button(ui, width, "Gemini Omni", "Copies a look · about $1", self.has_gemini && have_video && !busy).clicked() {
                    self.confirm_omni = true;
                }
                if ai_button(ui, width, "OpenAI suggest", "Sets the sliders for you", self.has_openai && !busy).clicked() {
                    self.run_openai();
                }
            });
            if !self.has_openai && !self.has_gemini {
                if theme::small_button(ui, "Add an AI key in Settings").clicked() {
                    self.show_settings = true;
                }
            }
            ui.horizontal(|ui| {
                ui.label(theme::muted("Model"));
                egui::ComboBox::from_id_salt("openai_model").selected_text(ai::OPENAI_MODELS[self.openai_model].1).width(170.0).show_ui(ui, |ui| {
                    for (i, (_, name)) in ai::OPENAI_MODELS.iter().enumerate() {
                        ui.selectable_value(&mut self.openai_model, i, *name);
                    }
                });
            });
            if !self.ai_result.is_empty() {
                ui.add(egui::Label::new(RichText::new(&self.ai_result).italics().size(13.0).color(theme::TEXT2)).wrap());
            }
        });
    }

    // ----------------------------------------------------------------- Export

    /// The 9:16 picture as it will be framed, for any of the four ways of framing it.
    fn draw_phone(&mut self, ui: &mut Ui, rect: Rect) {
        let Some(tex) = self.after_tex.clone() else { return };
        let aspect = tex.size()[0] as f32 / tex.size()[1].max(1) as f32;
        let response = ui.interact(rect, ui.scope_id().with("phone"), Sense::click_and_drag());
        let painter = ui.painter_at(rect.expand(8.0));
        painter.rect_filled(rect.expand(6.0), 36, theme::CARD);
        painter.rect_stroke(rect.expand(7.0), 37, Stroke::new(1.0, Color32::from_rgb(0x34, 0x35, 0x3C)), StrokeKind::Outside);
        let screen_radius = egui::CornerRadius::same(30);
        let draw = |ui: &Ui, tex: &egui::TextureHandle, at: Rect, uv: Rect, tint: Color32, radius: egui::CornerRadius| {
            egui::Image::new(egui::load::SizedTexture::new(tex.id(), at.size())).uv(uv).tint(tint).corner_radius(radius).paint_at(ui, at);
        };
        if self.framing == VerticalFraming::Blur {
            let b = self.blur.clamped();
            let cover = ((9.0 / 16.0) / aspect).min(1.0);
            draw(ui, &tex, rect, Rect::from_min_max(pos2(0.5 - cover / 2.0, 0.0), pos2(0.5 + cover / 2.0, 1.0)), Color32::from_gray(70), screen_radius);
            let (fw, fh) = (rect.width() * b.zoom, rect.width() * b.zoom / aspect);
            if response.dragged() {
                let d = response.drag_delta();
                if fw > rect.width() + 0.5 {
                    self.blur.x = (b.x - d.x / (fw - rect.width())).clamp(0.0, 1.0);
                }
                if fh < rect.height() - 0.5 {
                    self.blur.y = (b.y + d.y / (rect.height() - fh)).clamp(0.0, 1.0);
                }
            }
            let b = self.blur.clamped();
            let top = rect.top() + (rect.height() - fh.min(rect.height())) * b.y;
            let shown_h = fh.min(rect.height());
            let band = Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), shown_h));
            let u_span = (1.0 / b.zoom).min(1.0);
            let u0 = (1.0 - u_span) * b.x;
            draw(ui, &tex, band, Rect::from_min_max(pos2(u0, 0.0), pos2(u0 + u_span, 1.0)), Color32::WHITE, egui::CornerRadius::ZERO);
            cross(ui, band.center());
        } else {
            let share = self.source.as_ref().map_or(1.0, |s| s.probe.vertical_share() as f32).min(1.0);
            let pos = match self.framing {
                VerticalFraming::Manual => {
                    if response.dragged() {
                        self.manual_x = (self.manual_x - response.drag_delta().x / (rect.width() * 1.6)).clamp(0.0, 1.0);
                    }
                    self.manual_x
                }
                VerticalFraming::Follow => self.action.as_ref().map_or(0.5, |p| media::action_at(p, self.t)),
                _ => 0.5,
            };
            let u0 = (1.0 - share) * pos;
            draw(ui, &tex, rect, Rect::from_min_max(pos2(u0, 0.0), pos2(u0 + share, 1.0)), Color32::WHITE, screen_radius);
        }
    }

    pub fn export_center(&mut self, ui: &mut Ui) {
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 16, bottom: 0 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::bold("Export", 18.0));
                ui.label(theme::soft("This is what will be saved. Both previews use your look."));
            });
            let strip = 64.0;
            let region_h = (ui.available_height() - strip - 18.0).max(300.0);
            let (region, _) = ui.allocate_exact_size(vec2(ui.available_width(), region_h), Sense::hover());
            if self.wants_wide || self.wants_vertical {
                let phone_h = (region.height() - 70.0).clamp(300.0, 444.0);
                let phone = vec2(phone_h * 9.0 / 16.0, phone_h);
                let wide_w = (region.width() - phone.x - 60.0).clamp(300.0, 560.0);
                let wide = vec2(wide_w, wide_w * 9.0 / 16.0);
                let total = (if self.wants_wide { wide.x } else { 0.0 }) + (if self.wants_vertical { phone.x } else { 0.0 }) + if self.wants_wide && self.wants_vertical { 48.0 } else { 0.0 };
                let mut x = region.center().x - total / 2.0;
                let mid = region.center().y - 16.0;
                if self.wants_wide {
                    let r = Rect::from_min_size(pos2(x, mid - wide.y / 2.0), wide);
                    if let Some(tex) = self.after_tex.clone() {
                        egui::Image::new(egui::load::SizedTexture::new(tex.id(), r.size())).corner_radius(14).paint_at(ui, r);
                    }
                    ui.painter().rect_stroke(r.expand(1.0), 15, Stroke::new(1.0, theme::LINE), StrokeKind::Outside);
                    let (name, spec) = ("16:9 video", self.wide_spec());
                    ui.painter().text(pos2(r.left(), r.bottom() + 18.0), Align2::LEFT_CENTER, name, FontId::new(14.0, theme::bold_family()), theme::TEXT);
                    ui.painter().text(pos2(r.left() + 90.0, r.bottom() + 18.0), Align2::LEFT_CENTER, spec, FontId::new(12.0, egui::FontFamily::Monospace), theme::TEXT3);
                    x += wide.x + 48.0;
                }
                if self.wants_vertical {
                    let r = Rect::from_min_size(pos2(x, mid - phone.y / 2.0), phone);
                    self.draw_phone(ui, r);
                    ui.painter().text(pos2(r.left(), r.bottom() + 24.0), Align2::LEFT_CENTER, "9:16 video", FontId::new(14.0, theme::bold_family()), theme::TEXT);
                    let how = match self.framing {
                        VerticalFraming::Follow => "follows the action",
                        VerticalFraming::Centre => "centred",
                        VerticalFraming::Manual => "your framing",
                        VerticalFraming::Blur => "blurred background",
                    };
                    ui.painter().text(pos2(r.left(), r.bottom() + 44.0), Align2::LEFT_CENTER, format!("1080×1920 · {how}"), FontId::new(12.0, egui::FontFamily::Monospace), theme::TEXT3);
                }
            } else {
                ui.painter().text(region.center(), Align2::CENTER_CENTER, "Tick a video format on the right.", FontId::new(15.0, egui::FontFamily::Proportional), theme::TEXT3);
            }
            ui.add_space(8.0);
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::icon(ui, Icon::Bolt, 18.0, theme::OK);
                    ui.label(theme::bold(self.estimate_text(), 14.0));
                    ui.label(theme::soft(self.estimate_detail()));
                });
            });
        });
    }

    fn wide_spec(&self) -> String {
        let size = match (self.wide_height, &self.source) {
            (0, Some(s)) => format!("{}×{}", if s.probe.is_wide() { s.probe.width } else { media::even(s.probe.height as f64 * 16.0 / 9.0) }, s.probe.height),
            (h, _) => format!("{}×{}", media::even(h as f64 * 16.0 / 9.0), h),
        };
        format!("{size} · MP4 · with sound")
    }

    fn range_seconds(&self) -> f64 {
        (self.trim.1 - self.trim.0).max(0.0)
    }

    fn estimate_text(&self) -> String {
        let jobs = u8::from(self.wants_wide) + u8::from(self.wants_vertical);
        let on_card = self.gpu_grade && matches!(self.gpu_status, Some(Ok(_)));
        let hardware = self.encoder_choice.resolve(&self.encoders) != Encoder::Cpu;
        let speed = match (on_card, hardware) {
            (true, true) => 5.0,
            (true, false) => 2.6,
            (false, true) => 2.4,
            (false, false) => 1.8,
        };
        let seconds = self.range_seconds() / speed * f64::from(jobs.max(1));
        if seconds < 90.0 { format!("About {:.0} seconds", seconds.max(5.0)) } else { format!("About {:.0} minutes", (seconds / 60.0).ceil()) }
    }

    fn estimate_detail(&self) -> String {
        let on_card = self.gpu_grade && matches!(self.gpu_status, Some(Ok(_)));
        let enc = self.encoder_choice.resolve(&self.encoders);
        format!("for {} of video, with {} and colour on {}", clock_short(self.range_seconds()), if enc == Encoder::Cpu { "the CPU encoder".to_string() } else { format!("the {} encoder", enc.short()) }, if on_card { "the graphics card" } else { "the processor" })
    }

    pub fn export_inspector(&mut self, ui: &mut Ui) {
        let have = self.source.is_some();
        egui::Panel::right("inspector").exact_size(372.0).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(20, 0))).show(ui, |ui| {
            let mut start = false;
            let mut publish = false;
            let running = self.export.is_some();
            egui::Panel::bottom("inspector-footer").exact_size(if self.made.is_empty() { 84.0 } else { 140.0 }).frame(Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(0, 16))).show_separator_line(true).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                let jobs = u8::from(self.wants_wide) + u8::from(self.wants_vertical);
                let label = match (jobs, self.wants_thumb) {
                    (0, _) => "Export".to_string(),
                    (1, true) => "Export 1 video and a thumbnail".to_string(),
                    (1, false) => "Export 1 video".to_string(),
                    (_, true) => "Export 2 videos and a thumbnail".to_string(),
                    (_, false) => "Export 2 videos".to_string(),
                };
                ui.add_enabled_ui(have && !running, |ui| {
                    start = theme::button(ui, &label, Kind::Primary, ui.available_width()).clicked();
                });
                if !self.made.is_empty() {
                    publish = theme::button_with_icon(ui, "Continue to Publish", Some(Icon::ArrowRight), Kind::Secondary, ui.available_width()).clicked();
                }
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(16.0);
                ui.add_enabled_ui(!running, |ui| {
                    self.export_cards(ui);
                });
                if !self.made.is_empty() {
                    theme::card_with(ui, theme::tint(theme::OK, 18), theme::tint(theme::OK, 70), |ui| {
                        ui.label(theme::bold("Saved", 14.0));
                        for file in &self.made {
                            ui.label(RichText::new(file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())).size(12.5).color(theme::TEXT2));
                        }
                        if theme::small_button(ui, "Show in folder").clicked() {
                            if let Some(parent) = self.made[0].parent() {
                                show_in_folder(parent);
                            }
                        }
                    });
                }
                ui.add_space(16.0);
            });
            if start {
                self.start_export();
            }
            if publish {
                self.step = Step::Publish;
            }
        });
    }

    fn export_cards(&mut self, ui: &mut Ui) {
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::checkbox(ui, &mut self.wants_wide, "");
                ui.label(theme::bold("16:9 video", 14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::muted("For YouTube")));
            });
            if self.wants_wide {
                ui.horizontal(|ui| {
                    ui.label(theme::soft("Size"));
                    egui::ComboBox::from_id_salt("wide_height").selected_text(match self.wide_height {
                        0 => "Same as the original".to_string(),
                        h => format!("{h}p"),
                    }).width(210.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.wide_height, 0, "Same as the original");
                        ui.selectable_value(&mut self.wide_height, 1080, "1080p");
                        ui.selectable_value(&mut self.wide_height, 1440, "1440p");
                        ui.selectable_value(&mut self.wide_height, 2160, "2160p (4K)");
                    });
                });
            }
        });
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::checkbox(ui, &mut self.wants_vertical, "");
                ui.label(theme::bold("9:16 video", 14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::muted("Shorts, Reels, TikTok")));
            });
            if self.wants_vertical {
                theme::segmented(ui, &mut self.framing, &[(VerticalFraming::Follow, "Follow"), (VerticalFraming::Centre, "Centre"), (VerticalFraming::Manual, "Choose"), (VerticalFraming::Blur, "Blurred")], true);
                let help = match self.framing {
                    VerticalFraming::Follow => "A calm virtual camera that pans toward movement and detail.",
                    VerticalFraming::Centre => "Always the middle of the picture.",
                    VerticalFraming::Manual => "You choose where. Drag the phone preview left and right.",
                    VerticalFraming::Blur => "The whole picture over a blurred copy of itself. Drag the phone preview to move it.",
                };
                ui.label(theme::muted(help));
                match self.framing {
                    VerticalFraming::Blur => {
                        theme::slider_row(ui, "Zoom", &mut self.blur.zoom, 1.0..=2.0, 1.0, false);
                        theme::slider_row(ui, "Left / right", &mut self.blur.x, 0.0..=1.0, 0.0, false);
                        theme::slider_row(ui, "Up / down", &mut self.blur.y, 0.0..=1.0, 0.0, false);
                        ui.horizontal(|ui| {
                            if theme::small_button(ui, "Centre").clicked() {
                                self.blur.x = 0.5;
                                self.blur.y = 0.5;
                            }
                            if theme::small_button(ui, "Reset zoom").clicked() {
                                self.blur = crate::media::BlurLayout::default();
                            }
                        });
                    }
                    VerticalFraming::Manual => {
                        theme::slider_row(ui, "Left / right", &mut self.manual_x, 0.0..=1.0, 0.0, false);
                    }
                    VerticalFraming::Follow => {
                        if self.action_busy {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(theme::soft("Finding the action…"));
                            });
                        } else if self.action.is_none() {
                            if theme::small_button(ui, "Find the action now").clicked() {
                                self.follow_action();
                            }
                        } else {
                            ui.label(theme::muted("Action found. The preview shows the crop."));
                        }
                    }
                    VerticalFraming::Centre => {}
                }
            }
        });
        theme::card(ui, |ui| {
            ui.label(theme::bold("Extras", 14.0));
            theme::checkbox(ui, &mut self.wants_thumb, "Thumbnail from this frame");
            theme::checkbox(ui, &mut self.wants_lut, "Colour file (.cube) for other editors");
        });
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::bold("Speed", 14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let text = self.estimate_text();
                    let _ = text;
                    let speed = if self.gpu_grade && matches!(self.gpu_status, Some(Ok(_))) && self.encoder_choice.resolve(&self.encoders) != Encoder::Cpu { "About 5× real time" } else { "Slower without the graphics card" };
                    theme::pill(ui, speed, theme::OK, theme::tint(theme::OK, 30), None);
                });
            });
            self.encoder_picker(ui, true);
        });
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::icon(ui, Icon::Folder, 18.0, theme::TEXT3);
                let shown = self.out_dir.clone().or_else(|| self.source.as_ref().and_then(|s| s.path.parent().map(PathBuf::from)));
                let text = shown.map_or_else(|| "next to the video".to_string(), |d| d.display().to_string());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::small_button(ui, "Change").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.out_dir = Some(dir);
                        }
                    }
                    ui.add(egui::Label::new(theme::mono(text, 12.0).color(theme::TEXT2)).truncate());
                });
            });
        });
    }

    /// Which encoder writes the finished video, and whether the card grades the colour. Shared by Export and Make.
    pub fn encoder_picker(&mut self, ui: &mut Ui, show_grade: bool) {
        let using = self.encoder_choice.resolve(&self.encoders);
        let automatic = format!("Automatic ({})", using.short());
        ui.horizontal(|ui| {
            ui.label(theme::soft("Encoder"));
            let shown = match self.encoder_choice {
                EncoderChoice::Auto => automatic.clone(),
                _ => using.name().to_string(),
            };
            egui::ComboBox::from_id_salt("encoder").selected_text(shown).width(220.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.encoder_choice, EncoderChoice::Auto, automatic);
                ui.selectable_value(&mut self.encoder_choice, EncoderChoice::Cpu, Encoder::Cpu.name());
                for (choice, encoder) in [(EncoderChoice::Nvidia, Encoder::Nvidia), (EncoderChoice::Amd, Encoder::Amd), (EncoderChoice::Intel, Encoder::Intel)] {
                    if self.encoders.contains(&encoder) {
                        ui.selectable_value(&mut self.encoder_choice, choice, encoder.name());
                    }
                }
            });
        });
        ui.label(theme::muted(if self.encoders.is_empty() { "No graphics-card encoder found here, so the CPU is used." } else { "Graphics-card encoding is much faster. Files come out slightly larger." }));
        if show_grade {
            let ready = matches!(self.gpu_status, Some(Ok(_)));
            ui.add_enabled_ui(ready, |ui| {
                theme::toggle_row(ui, &mut self.gpu_grade, "Grade colour on the graphics card");
            });
            let status = match &self.gpu_status {
                None => "Checking the graphics card…".to_string(),
                Some(Ok(name)) => format!("Colour grading on: {name}"),
                Some(Err(why)) => format!("Not available here, so FFmpeg grades the colour: {why}"),
            };
            ui.label(theme::muted(status));
        }
    }

    pub fn pack_busy_text(&self) -> Option<String> {
        self.pack1.status().or_else(|| self.mon.pack_busy())
    }
}

// ---------------------------------------------------------------- local bits

#[allow(non_upper_case_globals)]
const CornerRadiusTop: egui::CornerRadius = egui::CornerRadius { nw: 18, ne: 18, sw: 0, se: 0 };

fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_string() } else { format!("{}…", text.chars().take(max.saturating_sub(1)).collect::<String>()) }
}

pub fn clock_short(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:02.0}", (t / 60.0) as u32, t % 60.0)
}

fn cross(ui: &Ui, centre: egui::Pos2) {
    let painter = ui.painter();
    for (w, c) in [(3.5, Color32::from_black_alpha(150)), (1.5, Color32::WHITE)] {
        let s = Stroke::new(w, c);
        painter.circle_stroke(centre, 8.0, s);
        for (a, b) in [((0.0, -14.0), (0.0, -8.0)), ((0.0, 8.0), (0.0, 14.0)), ((-14.0, 0.0), (-8.0, 0.0)), ((8.0, 0.0), (14.0, 0.0))] {
            painter.line_segment([centre + vec2(a.0, a.1), centre + vec2(b.0, b.1)], s);
        }
    }
}

/// A two-line button for the AI actions.
fn ai_button(ui: &mut Ui, width: f32, title: &str, subtitle: &str, enabled: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 52.0), if enabled { Sense::click() } else { Sense::hover() });
    let painter = ui.painter();
    painter.rect(rect, 10, if response.hovered() && enabled { theme::RAISED } else { theme::CARD }, Stroke::new(1.0, theme::LINE_STRONG), StrokeKind::Inside);
    painter.text(pos2(rect.left() + 12.0, rect.top() + 18.0), Align2::LEFT_CENTER, title, FontId::new(13.0, theme::bold_family()), if enabled { theme::TEXT } else { theme::TEXT3 });
    painter.text(pos2(rect.left() + 12.0, rect.top() + 36.0), Align2::LEFT_CENTER, subtitle, FontId::new(12.0, egui::FontFamily::Proportional), theme::TEXT3);
    if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// One of the three "ready to go" cards on the Start screen.
#[allow(clippy::too_many_arguments)]
fn ready_card(ui: &mut Ui, width: f32, title: &str, ok: Option<bool>, state: &str, detail: &str, action: Option<&str>, clicked: &mut bool) {
    let colour = match ok {
        Some(true) => theme::OK,
        Some(false) => theme::WARN,
        None => theme::TEXT3,
    };
    ui.allocate_ui_with_layout(vec2(width, 130.0), Layout::top_down(Align::LEFT), |ui| {
        Frame::new().fill(Color32::from_rgb(0x1A, 0x1B, 0x1E)).stroke(Stroke::new(1.0, theme::LINE_SOFT)).corner_radius(14).inner_margin(Margin::same(16)).show(ui, |ui| {
            ui.set_width(width - 34.0);
            ui.set_min_height(100.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 13.0, theme::tint(colour, 40));
                theme::paint_icon(ui.painter(), rect.center(), 14.0, if ok == Some(true) { Icon::Check } else { Icon::Warn }, colour, 2.6);
                ui.label(theme::bold(title, 14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(state).font(FontId::new(12.0, theme::bold_family())).color(colour)));
            });
            ui.add(egui::Label::new(theme::soft(detail)).wrap());
            if let Some(label) = action {
                if theme::small_button(ui, label).clicked() {
                    *clicked = true;
                }
            }
        });
    });
}
