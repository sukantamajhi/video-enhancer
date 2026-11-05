//! The "Upload pack" panel, shared by the single-video and montage screens:
//! writes the titles, description and hashtags, picks a thumbnail frame and
//! draws it, and saves everything next to the finished video.

use std::path::PathBuf;

use eframe::egui::{self, Frame, Margin, RichText, Sense, Stroke, vec2};

use crate::grade::Grade;
use crate::media::{self, Finish, GrabOptions};
use crate::montage::{self, Cut, Style};
use crate::pack::{self, Candidate, Level, Pack, PackFormat, Segment, ThumbShape};
use crate::theme::{self, Icon, Kind};
use crate::thumb_ai;
use crate::{App, Msg, Step, ai, store};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Single,
    Montage,
}

pub enum Event {
    /// The pack, its wide and tall thumbnail pictures, and whether a saved answer was reused.
    Made(Target, Result<(Pack, Vec<u8>, Vec<u8>, bool), String>),
    Thumb(Target, ThumbShape, Result<Vec<u8>, String>),
    /// One AI-designed thumbnail is done, or failed.
    AiThumb(Target, ThumbShape, Result<thumb_ai::Made, String>),
    /// What the AI job is doing now, for the progress card.
    AiNote(Target, String),
}

/// What the montage screen can say about the finished video, for chapters and thumbnails.
pub struct MontageFacts {
    pub clips: Vec<crate::montage::Clip>,
    pub cuts: Vec<Cut>,
    pub grades: Vec<Grade>,
    pub style: Style,
    pub title: String,
    pub credit: String,
    pub out_dir: Option<PathBuf>,
    /// False while the chapter times are estimates from the plan rather than the rendered video.
    pub rendered: bool,
}

#[derive(Default)]
pub struct PackState {
    pub pack: Option<Pack>,
    pub busy: Option<String>,
    pub thumb: Option<Vec<u8>>,
    pub tex: Option<egui::TextureHandle>,
    pub thumb_busy: bool,
    pub thumb_error: Option<String>,
    pub auto_tried: bool,
    /// The 9:16 thumbnail for a Short, with its own picture and texture.
    pub thumb_tall: Option<Vec<u8>>,
    pub tex_tall: Option<egui::TextureHandle>,
    pub tall_busy: bool,
    /// Which of the two the preview shows.
    pub view_tall: bool,
    /// The crop slider moved and the picture hasn't been redrawn yet.
    pub tall_dirty: bool,
    /// While the AI is designing the thumbnails: what it is doing.
    pub ai_busy: Option<String>,
    pub ai_pending: u8,
    /// What the AI thumbnails cost this time, in dollars, from OpenAI's own usage report.
    pub ai_cost: f64,
    pub ai_reused: bool,
    pub ai_warnings: Vec<String>,
}

impl PackState {
    pub fn with(pack: Option<Pack>) -> Self {
        Self { pack, ..Self::default() }
    }

    pub fn working(&self) -> bool {
        self.busy.is_some() || self.thumb_busy || self.tall_busy || self.ai_busy.is_some()
    }

    /// What to show on the progress card, if anything is running.
    pub fn status(&self) -> Option<String> {
        self.busy.clone().or_else(|| self.ai_busy.clone())
    }
}

/// Everything a pack request needs, whichever screen it came from.
struct Request {
    kind: String,
    duration: f64,
    segments: Vec<Segment>,
    candidates: Vec<Candidate>,
    credit: String,
}

enum Action {
    Write,
    Redraw,
    RedrawTall,
    /// Draw just these again (a pack that came back from last time, missing some pictures).
    Restore(Vec<ThumbShape>),
    Save,
}

impl App {
    fn pack_state(&mut self, target: Target) -> &mut PackState {
        match target {
            Target::Single => &mut self.pack1,
            Target::Montage => self.mon.pack_state(),
        }
    }

    pub fn pack_event(&mut self, event: Event) {
        match event {
            Event::Made(target, Ok((pack, jpeg, tall, reused))) => {
                let st = self.pack_state(target);
                st.view_tall = pack.format == PackFormat::Short;
                (st.busy, st.pack, st.thumb, st.tex, st.thumb_error, st.auto_tried) = (None, Some(pack), Some(jpeg), None, None, true);
                (st.thumb_tall, st.tex_tall, st.tall_busy) = (Some(tall), None, false);
                (st.ai_busy, st.ai_pending, st.ai_cost, st.ai_reused) = (None, 0, 0.0, false);
                st.ai_warnings.clear();
                self.note(if reused { "Upload pack ready. Reused your saved AI answer, so no tokens were used." } else { "Upload pack ready. Edit anything, then save it." });
            }
            Event::Made(target, Err(e)) => {
                self.pack_state(target).busy = None;
                self.fail(e);
            }
            Event::AiNote(target, text) => {
                let st = self.pack_state(target);
                if st.ai_pending > 0 {
                    st.ai_busy = Some(text);
                }
            }
            Event::AiThumb(target, shape, result) => {
                let st = self.pack_state(target);
                st.ai_pending = st.ai_pending.saturating_sub(1);
                let mut failure = None;
                match result {
                    Ok(made) => {
                        match shape {
                            ThumbShape::Wide => (st.thumb, st.tex) = (Some(made.jpeg), None),
                            ThumbShape::Tall => (st.thumb_tall, st.tex_tall) = (Some(made.jpeg), None),
                        }
                        if let Some(p) = &mut st.pack {
                            match shape {
                                ThumbShape::Wide => p.ai_wide = Some(made.name),
                                ThumbShape::Tall => p.ai_tall = Some(made.name),
                            }
                        }
                        st.ai_cost += made.cost.unwrap_or(0.0);
                        st.ai_reused |= made.reused;
                        st.ai_warnings.extend(made.warning);
                    }
                    Err(e) => failure = Some(e),
                }
                let done = st.ai_pending == 0;
                if done {
                    st.ai_busy = None;
                }
                let (cost, reused, warned) = (st.ai_cost, st.ai_reused, !st.ai_warnings.is_empty());
                if let Some(e) = failure {
                    self.pack_state(target).thumb_error = Some(e.clone());
                    self.fail(e);
                } else if done {
                    let spent = if cost > 0.0 { format!(" It cost about ${cost:.2}, worked out from OpenAI's own usage report.") } else if reused { " Reused your saved pictures, so nothing was charged.".to_string() } else { String::new() };
                    let check = if warned { " Check the warning under the picture." } else { "" };
                    self.note(format!("The AI designed both thumbnails.{spent}{check}"));
                }
            }
            Event::Thumb(target, shape, result) => {
                let st = self.pack_state(target);
                match (shape, result) {
                    (ThumbShape::Wide, Ok(bytes)) => (st.thumb_busy, st.thumb, st.tex, st.thumb_error) = (false, Some(bytes), None, None),
                    (ThumbShape::Tall, Ok(bytes)) => (st.tall_busy, st.thumb_tall, st.tex_tall, st.thumb_error) = (false, Some(bytes), None, None),
                    (ThumbShape::Wide, Err(e)) => (st.thumb_busy, st.thumb_error) = (false, Some(e)),
                    (ThumbShape::Tall, Err(e)) => (st.tall_busy, st.thumb_error) = (false, Some(e)),
                }
            }
        }
    }

    /// What goes into a pack for a single video: eight moments spread across the exported part.
    fn single_request(&self) -> Option<Request> {
        let source = self.source.as_ref()?;
        let (a, b) = self.trim;
        let times = media::spread_times(a, b, 8);
        Some(Request {
            kind: "a single gameplay recording".into(),
            duration: b - a,
            segments: times.iter().map(|t| Segment { at: t - a, note: String::new() }).collect(),
            candidates: times.iter().map(|&t| Candidate { clip: source.path.clone(), t, label: format!("at {}", pack::stamp(t - a)), grade: self.grade, learned: self.learned.clone() }).collect(),
            credit: String::new(),
        })
    }

    /// For a montage: every kept moment is a chapter candidate, and the eight best are thumbnail candidates.
    fn montage_request(&self) -> Option<Request> {
        let facts = self.mon.pack_facts()?;
        let starts = montage::segment_starts(&facts.cuts, facts.style);
        let segments: Vec<Segment> = facts.cuts.iter().zip(&starts).map(|(c, at)| Segment { at: *at, note: c.why.clone() }).collect();
        let mut best: Vec<usize> = (0..facts.cuts.len()).collect();
        best.sort_by(|&x, &y| facts.cuts[y].score.total_cmp(&facts.cuts[x].score));
        best.truncate(8);
        best.sort_unstable();
        let candidates = best
            .into_iter()
            .map(|i| {
                let cut = &facts.cuts[i];
                Candidate { clip: facts.clips[cut.clip].path.clone(), t: cut.start + cut.len() / 2.0, label: format!("moment {i}: {}", cut.why), grade: facts.grades.get(cut.clip).copied().unwrap_or_default(), learned: None }
            })
            .collect();
        let duration = starts.last().zip(facts.cuts.last()).map_or(0.0, |(s, c)| s + c.len());
        let kind = match facts.style {
            Style::Highlights => "a highlight reel made from several clips",
            Style::BeatSync => "a fast beat-synced montage made from several clips",
            Style::Story => "a story-style edit of several clips",
        };
        Some(Request { kind: kind.into(), duration, segments, candidates, credit: facts.credit })
    }

    fn start_pack(&mut self, target: Target) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else {
            self.fail("Add your OpenAI key first (in the AI helpers section).");
            return;
        };
        let request = match target {
            Target::Single => self.single_request(),
            Target::Montage => self.montage_request(),
        };
        let Some(request) = request else {
            self.fail(if target == Target::Single { "Open a video first." } else { "Plan the montage first." });
            return;
        };
        let (model, notes, reuse, mail) = (ai::OPENAI_MODELS[self.openai_model].0, self.style_notes.clone(), self.reuse_ai, self.mail.clone());
        let (topic, format) = (self.pack_topic.trim().to_string(), self.pack_format);
        self.pack_state(target).busy = Some("Writing your upload pack…".into());
        std::thread::spawn(move || {
            let work = std::env::temp_dir().join(format!("video-enhancer-pack-{}-{target:?}", std::process::id()));
            let result = (|| -> Result<(Pack, Vec<u8>, Vec<u8>, bool), String> {
                let options = GrabOptions { width: 512, finish: Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
                let mut frames = Vec::new();
                for c in &request.candidates {
                    let probe = media::probe(&tools, &c.clip)?;
                    frames.push((c.label.clone(), media::grab_frame(&tools, &probe, &c.clip, c.t, &options)?));
                }
                // The same footage, moments and wording always make the same question, so its answer can be reused.
                let mut facts = vec!["pack".to_string(), ai::PACK_PROMPT_VERSION.to_string(), model.to_string(), request.kind.clone(), format!("{:.1}", request.duration), notes.trim().to_string(), topic.clone(), format.name().to_string()];
                facts.extend(request.segments.iter().map(|s| format!("{:.1}|{}", s.at, s.note)));
                facts.extend(request.candidates.iter().map(|c| format!("{}|{:.2}|{}", store::fingerprint(&c.clip), c.t, c.label)));
                let cache = format!("ai-pack-{}", store::hash(&facts));
                let (answer, reused) = match reuse.then(|| store::load::<ai::AiPack>(&cache)).flatten() {
                    Some(saved) => (saved, true),
                    None => {
                        let fresh = ai::openai_pack(&key, model, &ai::PackRequest { kind: &request.kind, duration: request.duration, notes: &notes, topic: &topic, format, segments: &request.segments, frames: &frames })?;
                        store::save(&cache, &fresh);
                        (fresh, false)
                    }
                };
                let pack = Pack::assemble(&answer, &request.segments, request.candidates.clone(), request.credit.clone(), format)?;
                let picked = &pack.candidates[pack.thumb_pick];
                let jpeg = pack::render_thumbnail(&tools, picked, &pack.thumb_text, &work, ThumbShape::Wide, 0.5)?;
                let tall = pack::render_thumbnail(&tools, picked, &pack.thumb_text, &work, ThumbShape::Tall, 0.5)?;
                Ok((pack, jpeg, tall, reused))
            })();
            std::fs::remove_dir_all(&work).ok();
            mail.send(Msg::Pack(Event::Made(target, result)));
        });
    }

    /// Draws the plain (free) thumbnails again, for the shapes given. An AI picture for a shape is replaced.
    fn redraw_thumbnail(&mut self, target: Target, shapes: Vec<ThumbShape>) {
        let Some(tools) = self.tools.clone() else { return };
        let st = self.pack_state(target);
        let Some(pack) = &st.pack else { return };
        let (candidate, text, x) = (pack.candidates.get(pack.thumb_pick).cloned(), pack.thumb_text.clone(), pack.thumb_x.unwrap_or(0.5));
        let Some(candidate) = candidate else { return };
        if let Some(p) = &mut st.pack {
            for shape in &shapes {
                match shape {
                    ThumbShape::Wide => p.ai_wide = None,
                    ThumbShape::Tall => p.ai_tall = None,
                }
            }
        }
        for shape in &shapes {
            match shape {
                ThumbShape::Wide => st.thumb_busy = true,
                ThumbShape::Tall => st.tall_busy = true,
            }
        }
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let work = std::env::temp_dir().join(format!("video-enhancer-thumb-{}-{target:?}", std::process::id()));
            for shape in shapes {
                let result = pack::render_thumbnail(&tools, &candidate, &text, &work, shape, x);
                mail.send(Msg::Pack(Event::Thumb(target, shape, result)));
            }
            std::fs::remove_dir_all(&work).ok();
        });
    }

    /// Sends the chosen frame and the words to OpenAI to design both thumbnails, one request per shape, side by side.
    pub fn start_ai_thumbnails(&mut self, target: Target) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else {
            self.fail("Add your OpenAI key first (Settings, top right).");
            return;
        };
        let (model, quality) = (thumb_ai::IMAGE_MODELS[self.thumb_model.min(thumb_ai::IMAGE_MODELS.len() - 1)].0, thumb_ai::QUALITIES[self.thumb_quality.min(thumb_ai::QUALITIES.len() - 1)].0);
        let (text_model, topic, reuse, mail) = (ai::OPENAI_MODELS[self.openai_model].0, self.pack_topic.trim().to_string(), self.reuse_ai, self.mail.clone());
        let st = self.pack_state(target);
        if st.ai_busy.is_some() {
            return;
        }
        let Some(pack) = &st.pack else { return };
        let Some(candidate) = pack.candidates.get(pack.thumb_pick).cloned() else { return };
        let brief = thumb_ai::Brief { text: pack::clean_thumb_text(&pack.thumb_text), topic, direction: pack.thumb_note.clone(), format: pack.format };
        let x = pack.thumb_x.unwrap_or(0.5);
        (st.ai_busy, st.ai_pending, st.ai_cost, st.ai_reused, st.thumb_error) = (Some("Getting the frames ready…".into()), 2, 0.0, false, None);
        st.ai_warnings.clear();
        for shape in [ThumbShape::Wide, ThumbShape::Tall] {
            let (tools, key, candidate, brief, mail) = (tools.clone(), key.clone(), candidate.clone(), brief.clone(), mail.clone());
            let (model, quality, text_model) = (model.to_string(), quality.to_string(), text_model.to_string());
            std::thread::spawn(move || {
                let work = std::env::temp_dir().join(format!("video-enhancer-ai-thumb-{}-{target:?}-{shape:?}", std::process::id()));
                let result = (|| {
                    let frame = pack::base_frame(&tools, &candidate, shape, x, &work)?;
                    let job = thumb_ai::Job { base: thumb_ai::api_base(), key, model, quality, text_model, shape, brief, frame, reuse, cache_dir: store::dir() };
                    thumb_ai::run(&job, &|text| mail.send(Msg::Pack(Event::AiNote(target, text.to_string()))))
                })();
                std::fs::remove_dir_all(&work).ok();
                mail.send(Msg::Pack(Event::AiThumb(target, shape, result)));
            });
        }
    }

    fn save_pack(&mut self, target: Target) {
        let (dir, stem) = match target {
            Target::Single => {
                let Some(source) = &self.source else { return };
                let stem = source.path.file_stem().map_or_else(|| "video".to_string(), |s| s.to_string_lossy().into_owned());
                (self.out_dir.clone().or_else(|| source.path.parent().map(PathBuf::from)), stem)
            }
            Target::Montage => {
                let Some(facts) = self.mon.pack_facts() else { return };
                (facts.out_dir.or_else(|| facts.clips.first().and_then(|c| c.path.parent().map(PathBuf::from))), crate::montage_ui::safe_name(&facts.title))
            }
        };
        let dir = dir.unwrap_or_else(std::env::temp_dir);
        let st = self.pack_state(target);
        let (Some(pack), thumb, tall) = (&st.pack, st.thumb.clone(), st.thumb_tall.clone()) else { return };
        let text_file = media::free_name(&dir, &stem, "_upload_pack", "txt");
        let mut saved = vec![text_file.clone()];
        let mut problem = std::fs::write(&text_file, pack.as_text()).err().map(|e| e.to_string());
        if let (None, Some(bytes)) = (&problem, thumb) {
            let thumb_file = media::free_name(&dir, &stem, "_thumbnail", "jpg");
            problem = std::fs::write(&thumb_file, bytes).err().map(|e| e.to_string());
            saved.push(thumb_file);
        }
        if let (None, Some(bytes)) = (&problem, tall) {
            let tall_file = media::free_name(&dir, &stem, "_thumbnail_shorts", "jpg");
            problem = std::fs::write(&tall_file, bytes).err().map(|e| e.to_string());
            saved.push(tall_file);
        }
        match problem {
            None => self.note(format!("Saved {} to {}", saved.iter().filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned())).collect::<Vec<_>>().join(" and "), dir.display())),
            Some(e) => self.fail(format!("Couldn't save the pack: {e}")),
        }
    }

    /// The Publish step: the words on the left, the search check and thumbnail on the right.
    pub fn publish_screen(&mut self, ui: &mut egui::Ui, target: Target) {
        let ready = match target {
            Target::Single => self.source.is_some(),
            Target::Montage => self.mon.pack_facts().is_some(),
        };
        let estimate = target == Target::Montage && self.mon.pack_facts().is_some_and(|f| !f.rendered);
        let has_key = self.has_openai;
        let ctx = ui.ctx().clone();
        let mut action: Option<Action> = None;
        let mut copy: Option<String> = None;
        let mut open_settings = false;
        let mut go_back = false;

        let (busy, have) = {
            let st = self.pack_state(target);
            // Decode the thumbnail picture into a texture the first time it is needed.
            if st.tex.is_none()
                && let Some(bytes) = &st.thumb
                && let Ok(picture) = image::load_from_memory(bytes)
            {
                let rgba = picture.to_rgba8();
                st.tex = Some(ctx.load_texture(format!("pack-thumb-{target:?}"), egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], &rgba), egui::TextureOptions::LINEAR));
            }
            if st.tex_tall.is_none()
                && let Some(bytes) = &st.thumb_tall
                && let Ok(picture) = image::load_from_memory(bytes)
            {
                let rgba = picture.to_rgba8();
                st.tex_tall = Some(ctx.load_texture(format!("pack-thumb-tall-{target:?}"), egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], &rgba), egui::TextureOptions::LINEAR));
            }
            // A pack restored from last time has its words but not its pictures yet.
            if st.pack.is_some() && (st.thumb.is_none() || st.thumb_tall.is_none()) && !st.thumb_busy && !st.tall_busy && !st.auto_tried {
                st.auto_tried = true;
                // A pack for a Short opens on the tall thumbnail.
                st.view_tall = st.pack.as_ref().is_some_and(|p| p.format == PackFormat::Short);
                // Pictures the AI designed earlier come back from disk, free. The others are drawn plainly.
                let mut missing = Vec::new();
                if let Some(p) = &mut st.pack {
                    match p.ai_wide.as_deref().and_then(store::load_bytes) {
                        Some(bytes) => (st.thumb, st.tex) = (Some(bytes), None),
                        None => (p.ai_wide, missing) = (None, [missing, vec![ThumbShape::Wide]].concat()),
                    }
                    match p.ai_tall.as_deref().and_then(store::load_bytes) {
                        Some(bytes) => (st.thumb_tall, st.tex_tall) = (Some(bytes), None),
                        None => (p.ai_tall, missing) = (None, [missing, vec![ThumbShape::Tall]].concat()),
                    }
                }
                if !missing.is_empty() {
                    action = Some(Action::Restore(missing));
                }
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
                    let (mut model_i, mut quality_i) = (self.thumb_model, self.thumb_quality);
                    let st = self.pack_state(target);
                    let (ai_busy, ai_cost, ai_reused, ai_warnings) = (st.ai_busy.clone(), st.ai_cost, st.ai_reused, st.ai_warnings.clone());
                    let (thumb_busy, tex, thumb_error) = (st.thumb_busy || st.tall_busy || st.ai_busy.is_some(), st.tex.clone(), st.thumb_error.clone());
                    let (tex_tall, mut view_tall) = (st.tex_tall.clone(), st.view_tall);
                    let mut dirty = st.tall_dirty;
                    let released = !ui.input(|i| i.pointer.any_down());
                    let mut changed_x = false;
                    let mut ask_ai = false;
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
                        let width = ui.available_width();
                        let shown = if view_tall { &tex_tall } else { &tex };
                        if view_tall {
                            let (rect, _) = ui.allocate_exact_size(vec2(width, 420.0), Sense::hover());
                            ui.painter().rect_filled(rect, 10, theme::INPUT);
                            let r = egui::Rect::from_center_size(rect.center(), vec2(420.0 * 9.0 / 16.0, 420.0));
                            if let Some(tex) = shown {
                                egui::Image::new(egui::load::SizedTexture::new(tex.id(), r.size())).corner_radius(10).paint_at(ui, r);
                            } else if thumb_busy {
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Drawing the thumbnail…", egui::FontId::proportional(14.0), theme::TEXT2);
                            }
                        } else {
                            let (rect, _) = ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::hover());
                            ui.painter().rect_filled(rect, 10, theme::INPUT);
                            if let Some(tex) = shown {
                                egui::Image::new(egui::load::SizedTexture::new(tex.id(), rect.size())).corner_radius(10).paint_at(ui, rect);
                            } else if thumb_busy {
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Drawing the thumbnail…", egui::FontId::proportional(14.0), theme::TEXT2);
                            }
                        }
                        // A plain frame with words on it is only a draft. The AI design is the real thumbnail.
                        let designed = if view_tall { pack.ai_tall.is_some() } else { pack.ai_wide.is_some() };
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
                            ui.label(theme::muted("The Short thumbnail is cut out of the wide frame. Slide it to keep the action in. The text sits in the middle band, clear of the Shorts buttons."));
                        }
                        if let Some(e) = &thumb_error {
                            ui.label(RichText::new(format!("The thumbnail couldn't be drawn: {e}")).color(theme::BAD).size(12.5));
                        }
                        ui.horizontal(|ui| {
                            ui.label(theme::soft("Words on it"));
                            ui.add(egui::TextEdit::singleline(&mut pack.thumb_text).desired_width(ui.available_width() - 90.0).margin(Margin::symmetric(10, 8)));
                            ui.add_enabled_ui(!thumb_busy, |ui| {
                                if theme::small_button(ui, "Redraw (free)").clicked() {
                                    action = Some(Action::Redraw);
                                }
                            });
                        });
                        ui.label(theme::muted("Pick the frame"));
                        let count = pack.candidates.len();
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
                    let st = self.pack_state(target);
                    st.view_tall = view_tall;
                    (self.thumb_model, self.thumb_quality) = (model_i, quality_i);
                    if ask_ai {
                        self.confirm_ai_thumb = Some(target);
                    }
                    let st = self.pack_state(target);
                    dirty |= changed_x;
                    // Redraw once the slider is let go, not on every pixel of the drag.
                    if dirty && released && !st.tall_busy {
                        dirty = false;
                        action = Some(Action::RedrawTall);
                    }
                    st.tall_dirty = dirty;
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
                            ui.add(egui::TextEdit::singleline(&mut self.pack_topic).hint_text("for example Valorant").desired_width(220.0).margin(Margin::symmetric(10, 8)))
                                .on_hover_text("The game's exact name is the strongest search keyword for a gaming video, and it is used exactly as you type it. Leave it empty and the AI names a game only if it can clearly see which one.");
                        });
                        theme::segmented(ui, &mut self.pack_format, &[(PackFormat::Standard, "Standard video"), (PackFormat::Short, "YouTube Short")], false);
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

                    let st = self.pack_state(target);
                    let Some(pack) = &mut st.pack else { return };
                    let good_len = if pack.format == PackFormat::Short { 40 } else { 60 };
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
                                let colour = if n == 0 || n > pack::MAX_TITLE { theme::BAD } else if n <= good_len { theme::OK } else { theme::WARN };
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
            self.note("Copied.");
        }
        match action {
            Some(Action::Write) => self.start_pack(target),
            Some(Action::Redraw) => {
                if let Some(pack) = &mut self.pack_state(target).pack {
                    pack.thumb_text = pack::clean_thumb_text(&pack.thumb_text);
                }
                self.redraw_thumbnail(target, vec![ThumbShape::Wide, ThumbShape::Tall]);
            }
            Some(Action::RedrawTall) => self.redraw_thumbnail(target, vec![ThumbShape::Tall]),
            Some(Action::Restore(shapes)) => self.redraw_thumbnail(target, shapes),
            Some(Action::Save) => self.save_pack(target),
            None => {}
        }
    }
}
