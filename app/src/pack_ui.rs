//! The "Upload pack" panel, shared by the single-video and montage screens:
//! writes the titles, description and hashtags, picks a thumbnail frame and
//! draws it, and saves everything next to the finished video.

use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText, vec2};

use crate::grade::Grade;
use crate::media::{self, Finish, GrabOptions};
use crate::montage::{self, Cut, Style};
use crate::pack::{self, Candidate, Pack, Segment};
use crate::{App, Msg, ai, store};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Single,
    Montage,
}

pub enum Event {
    /// The pack, its thumbnail picture, and whether a saved answer was reused.
    Made(Target, Result<(Pack, Vec<u8>, bool), String>),
    Thumb(Target, Result<Vec<u8>, String>),
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
}

impl PackState {
    pub fn with(pack: Option<Pack>) -> Self {
        Self { pack, ..Self::default() }
    }

    pub fn working(&self) -> bool {
        self.busy.is_some() || self.thumb_busy
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
            Event::Made(target, Ok((pack, jpeg, reused))) => {
                let st = self.pack_state(target);
                (st.busy, st.pack, st.thumb, st.tex, st.thumb_error, st.auto_tried) = (None, Some(pack), Some(jpeg), None, None, true);
                self.note(if reused { "Upload pack ready. Reused your saved AI answer, so no tokens were used." } else { "Upload pack ready. Edit anything, then save it." });
            }
            Event::Made(target, Err(e)) => {
                self.pack_state(target).busy = None;
                self.fail(e);
            }
            Event::Thumb(target, result) => {
                let st = self.pack_state(target);
                st.thumb_busy = false;
                match result {
                    Ok(bytes) => (st.thumb, st.tex, st.thumb_error) = (Some(bytes), None, None),
                    Err(e) => st.thumb_error = Some(e),
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
        self.pack_state(target).busy = Some("Writing your upload pack…".into());
        std::thread::spawn(move || {
            let work = std::env::temp_dir().join(format!("video-enhancer-pack-{}-{target:?}", std::process::id()));
            let result = (|| -> Result<(Pack, Vec<u8>, bool), String> {
                let options = GrabOptions { width: 512, finish: Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
                let mut frames = Vec::new();
                for c in &request.candidates {
                    let probe = media::probe(&tools, &c.clip)?;
                    frames.push((c.label.clone(), media::grab_frame(&tools, &probe, &c.clip, c.t, &options)?));
                }
                // The same footage, moments and wording always make the same question, so its answer can be reused.
                let mut facts = vec!["pack".to_string(), model.to_string(), request.kind.clone(), format!("{:.1}", request.duration), notes.trim().to_string()];
                facts.extend(request.segments.iter().map(|s| format!("{:.1}|{}", s.at, s.note)));
                facts.extend(request.candidates.iter().map(|c| format!("{}|{:.2}|{}", store::fingerprint(&c.clip), c.t, c.label)));
                let cache = format!("ai-pack-{}", store::hash(&facts));
                let (answer, reused) = match reuse.then(|| store::load::<ai::AiPack>(&cache)).flatten() {
                    Some(saved) => (saved, true),
                    None => {
                        let fresh = ai::openai_pack(&key, model, &ai::PackRequest { kind: &request.kind, duration: request.duration, notes: &notes, segments: &request.segments, frames: &frames })?;
                        store::save(&cache, &fresh);
                        (fresh, false)
                    }
                };
                let pack = Pack::assemble(&answer, &request.segments, request.candidates.clone(), request.credit.clone())?;
                let jpeg = pack::render_thumbnail(&tools, &pack.candidates[pack.thumb_pick], &pack.thumb_text, &work)?;
                Ok((pack, jpeg, reused))
            })();
            std::fs::remove_dir_all(&work).ok();
            mail.send(Msg::Pack(Event::Made(target, result)));
        });
    }

    fn redraw_thumbnail(&mut self, target: Target) {
        let Some(tools) = self.tools.clone() else { return };
        let st = self.pack_state(target);
        let Some(pack) = &st.pack else { return };
        let (candidate, text) = (pack.candidates.get(pack.thumb_pick).cloned(), pack.thumb_text.clone());
        let Some(candidate) = candidate else { return };
        st.thumb_busy = true;
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let work = std::env::temp_dir().join(format!("video-enhancer-thumb-{}-{target:?}", std::process::id()));
            let result = pack::render_thumbnail(&tools, &candidate, &text, &work);
            std::fs::remove_dir_all(&work).ok();
            mail.send(Msg::Pack(Event::Thumb(target, result)));
        });
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
        let (Some(pack), thumb) = (&st.pack, st.thumb.clone()) else { return };
        let text_file = media::free_name(&dir, &stem, "_upload_pack", "txt");
        let mut saved = vec![text_file.clone()];
        let mut problem = std::fs::write(&text_file, pack.as_text()).err().map(|e| e.to_string());
        if let (None, Some(bytes)) = (&problem, thumb) {
            let thumb_file = media::free_name(&dir, &stem, "_thumbnail", "jpg");
            problem = std::fs::write(&thumb_file, bytes).err().map(|e| e.to_string());
            saved.push(thumb_file);
        }
        match problem {
            None => self.note(format!("Saved {} to {}", saved.iter().filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned())).collect::<Vec<_>>().join(" and "), dir.display())),
            Some(e) => self.fail(format!("Couldn't save the pack: {e}")),
        }
    }

    pub fn pack_ui(&mut self, ui: &mut egui::Ui, target: Target) {
        let ready = match target {
            Target::Single => self.source.is_some(),
            Target::Montage => self.mon.pack_facts().is_some(),
        };
        let estimate = target == Target::Montage && self.mon.pack_facts().is_some_and(|f| !f.rendered);
        let has_key = self.has_openai;
        let ctx = ui.ctx().clone();
        let mut action: Option<Action> = None;
        let mut copy: Option<String> = None;

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
            // A pack restored from last time has its words but not its picture yet.
            if st.pack.is_some() && st.thumb.is_none() && !st.thumb_busy && !st.auto_tried {
                st.auto_tried = true;
                action = Some(Action::Redraw);
            }
            (st.busy.clone(), st.pack.is_some())
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Upload pack").strong().size(16.0));
            let button = egui::Button::new(if have { "Write it again" } else { "Write my upload pack" });
            if ui.add_enabled(ready && has_key && busy.is_none(), button).on_hover_text("Three titles, a description with chapters, hashtags and a thumbnail. Sends about 8 small pictures to OpenAI.").clicked() {
                action = Some(Action::Write);
            }
            if let Some(text) = &busy {
                ui.spinner();
                ui.label(text);
            }
        });
        if !has_key {
            ui.label(RichText::new("Needs your OpenAI key (AI helpers section).").weak().small());
        }
        if estimate && ready {
            ui.label(RichText::new("Make the montage first so the chapter times match the finished video exactly. Until then they are estimates.").weak().small());
        }

        let width = ui.available_width().clamp(200.0, 560.0);
        let st = self.pack_state(target);
        let thumb_busy = st.thumb_busy;
        let tex = st.tex.clone();
        let thumb_error = st.thumb_error.clone();
        if let Some(pack) = &mut st.pack {
            ui.add_space(4.0);
            ui.label(RichText::new("Title (pick one, edit freely)").weak().small());
            for i in 0..pack.titles.len() {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut pack.title_pick, i, "");
                    ui.add(egui::TextEdit::singleline(&mut pack.titles[i]).desired_width(width - 90.0));
                    ui.label(RichText::new(format!("{}/{}", pack.titles[i].chars().count(), pack::MAX_TITLE)).weak().small());
                    if ui.small_button("Copy").clicked() {
                        copy = Some(pack.titles[i].clone());
                    }
                });
            }
            ui.label(RichText::new("Description intro").weak().small());
            ui.add(egui::TextEdit::multiline(&mut pack.intro).desired_rows(3).desired_width(width));
            let mut shown = pack.description();
            ui.label(RichText::new(format!("Full description ({} chapters)", pack.chapters.len())).weak().small());
            ui.add(egui::TextEdit::multiline(&mut shown).desired_rows(8).desired_width(width).font(egui::TextStyle::Monospace).interactive(false));
            ui.horizontal(|ui| {
                if ui.small_button("Copy description").clicked() {
                    copy = Some(pack.description());
                }
                if pack.chapters.is_empty() {
                    ui.label(RichText::new("Too short for chapters (YouTube needs 3, each 10 s or more).").weak().small());
                }
            });

            ui.add_space(4.0);
            ui.label(RichText::new("Thumbnail").weak().small());
            if let Some(tex) = &tex {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(tex.id(), vec2(width.min(420.0), width.min(420.0) * 9.0 / 16.0))));
            } else if thumb_busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Drawing the thumbnail…");
                });
            }
            if let Some(e) = &thumb_error {
                ui.label(RichText::new(format!("The thumbnail couldn't be drawn: {e}")).color(Color32::from_rgb(255, 120, 110)).small());
            }
            ui.horizontal(|ui| {
                ui.label("Text");
                ui.add(egui::TextEdit::singleline(&mut pack.thumb_text).desired_width(170.0));
                if ui.add_enabled(!thumb_busy, egui::Button::new("Redraw")).clicked() {
                    action = Some(Action::Redraw);
                }
            });
            let count = pack.candidates.len().max(1);
            ui.horizontal(|ui| {
                if ui.add_enabled(!thumb_busy, egui::Button::new("Previous frame")).clicked() {
                    pack.thumb_pick = (pack.thumb_pick + count - 1) % count;
                    action = Some(Action::Redraw);
                }
                if ui.add_enabled(!thumb_busy, egui::Button::new("Next frame")).clicked() {
                    pack.thumb_pick = (pack.thumb_pick + 1) % count;
                    action = Some(Action::Redraw);
                }
                ui.label(RichText::new(format!("frame {} of {count}", pack.thumb_pick + 1)).weak().small());
            });
            if !pack.reason.is_empty() {
                ui.label(RichText::new(format!("Why this frame: {}", pack.reason)).weak().small());
            }
            ui.add_space(4.0);
            if ui.button("Save pack and thumbnail to the folder").clicked() {
                action = Some(Action::Save);
            }
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
                self.redraw_thumbnail(target);
            }
            Some(Action::Save) => self.save_pack(target),
            None => {}
        }
    }
}
