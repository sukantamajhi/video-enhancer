//! The Publish step for a single video: write the titles, description and tags with the AI, draw the thumbnails
//! (plain, or designed by the AI) and save the upload pack. It is part of the engine, with no user interface in it.

use std::path::PathBuf;

use crate::engine::{Engine, Msg};
use crate::montage::{self, Style};
use crate::media::{self, Finish, GrabOptions};
use crate::pack::{self, Candidate, Pack, Segment, TextStyle, ThumbShape};
use crate::{ai, store, thumb_ai};

/// Which video an upload pack is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Target {
    #[default]
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

#[derive(Default)]
pub struct PublishState {
    pub pack: Option<Pack>,
    /// While the AI writes the words: what it is doing.
    pub busy: Option<String>,
    /// The two thumbnails as JPEG files, wide and tall.
    pub thumb: Option<Vec<u8>>,
    pub thumb_tall: Option<Vec<u8>>,
    pub thumb_busy: bool,
    pub tall_busy: bool,
    pub thumb_error: Option<String>,
    /// While the AI is designing the thumbnails: what it is doing.
    pub ai_busy: Option<String>,
    pub ai_pending: u8,
    /// What the AI thumbnails cost this time, in dollars, from OpenAI's own usage report.
    pub ai_cost: f64,
    pub ai_reused: bool,
    pub ai_warnings: Vec<String>,
    /// Goes up when the words of the pack were replaced (a new pack, or one restored), so a front end can refill its text boxes.
    pub pack_gen: u64,
    /// Goes up when either thumbnail picture changed.
    pub thumb_gen: u64,
    restored: bool,
}

impl PublishState {
    pub fn with(pack: Option<Pack>) -> Self {
        Self { pack, pack_gen: 1, thumb_gen: 1, ..Self::default() }
    }

    pub fn working(&self) -> bool {
        self.busy.is_some() || self.thumb_busy || self.tall_busy || self.ai_busy.is_some()
    }

    /// True while the plain picture for this shape is being drawn.
    pub fn thumb_busy_for(&self, shape: ThumbShape) -> bool {
        match shape {
            ThumbShape::Wide => self.thumb_busy,
            ThumbShape::Tall => self.tall_busy,
        }
    }

    /// What to show on a progress card, if anything is running.
    pub fn status(&self) -> Option<String> {
        self.busy.clone().or_else(|| self.ai_busy.clone())
    }
}

/// Everything a pack request needs.
struct Request {
    kind: String,
    duration: f64,
    segments: Vec<Segment>,
    candidates: Vec<Candidate>,
    credit: String,
}

impl Engine {
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

    /// The state for a target: the one on screen, or the one kept aside.
    fn pubs_for(&mut self, target: Target) -> &mut PublishState {
        if target == self.target { &mut self.publish } else { &mut self.stash }
    }

    /// For a montage: every kept moment is a chapter candidate, and the eight best are thumbnail candidates.
    fn montage_request(&self) -> Option<Request> {
        let facts = self.montage_facts()?;
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

    /// Chooses which video the Publish step is about. The other one's pack is kept aside, untouched.
    pub fn set_target(&mut self, target: Target) {
        if target != self.target {
            std::mem::swap(&mut self.publish, &mut self.stash);
            self.target = target;
            // A pack that came back from last time has its words but not yet its pictures.
            self.restore_thumbnails();
        }
    }

    /// A pack for the montage the way the AI would write it, for tests.
    #[cfg(test)]
    pub(crate) fn montage_pack_for_test(&self) -> Pack {
        let request = self.montage_request().unwrap();
        let ai = ai::AiPack {
            titles: vec!["Test Reel Highlights".into(), "Second title".into()],
            intro: "A reel.".into(),
            chapter_titles: vec![],
            hashtags: vec!["#Valorant".into()],
            thumb_candidate: 1,
            thumb_text: "insane clutch".into(),
            reason: "clear action".into(),
            primary_keyword: "Valorant clutch".into(),
            secondary_keywords: vec![],
            tags: vec!["valorant".into()],
        };
        Pack::assemble(&ai, &request.segments, request.candidates, request.credit, pack::PackFormat::Standard).unwrap()
    }

    pub(crate) fn pack_event(&mut self, event: Event) {
        let target = match &event {
            Event::Made(t, _) | Event::Thumb(t, ..) | Event::AiThumb(t, ..) | Event::AiNote(t, _) => *t,
        };
        let st = self.pubs_for(target);
        match event {
            Event::Made(_, Ok((pack, jpeg, tall, reused))) => {
                (st.busy, st.pack, st.thumb, st.thumb_error) = (None, Some(pack), Some(jpeg), None);
                (st.thumb_tall, st.thumb_busy, st.tall_busy) = (Some(tall), false, false);
                (st.ai_busy, st.ai_pending, st.ai_cost, st.ai_reused) = (None, 0, 0.0, false);
                st.ai_warnings.clear();
                st.restored = true;
                st.pack_gen += 1;
                st.thumb_gen += 1;
                self.note(if reused { "Upload pack ready. Reused your saved AI answer, so no tokens were used." } else { "Upload pack ready. Edit anything, then save it." });
            }
            Event::Made(_, Err(e)) => {
                st.busy = None;
                self.fail(e);
            }
            Event::AiNote(_, text) => {
                if st.ai_pending > 0 {
                    st.ai_busy = Some(text);
                }
            }
            Event::AiThumb(_, shape, result) => {
                st.ai_pending = st.ai_pending.saturating_sub(1);
                let mut failure = None;
                match result {
                    Ok(made) => {
                        match shape {
                            ThumbShape::Wide => st.thumb = Some(made.jpeg),
                            ThumbShape::Tall => st.thumb_tall = Some(made.jpeg),
                        }
                        st.thumb_gen += 1;
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
                    st.thumb_error = Some(e.clone());
                    self.fail(e);
                } else if done {
                    let spent = if cost > 0.0 { format!(" It cost about ${cost:.2}, worked out from OpenAI's own usage report.") } else if reused { " Reused your saved pictures, so nothing was charged.".to_string() } else { String::new() };
                    let check = if warned { " Check the warning under the picture." } else { "" };
                    self.note(format!("The AI designed both thumbnails.{spent}{check}"));
                }
            }
            Event::Thumb(_, shape, result) => {
                match (shape, result) {
                    (ThumbShape::Wide, Ok(bytes)) => (st.thumb_busy, st.thumb, st.thumb_error) = (false, Some(bytes), None),
                    (ThumbShape::Tall, Ok(bytes)) => (st.tall_busy, st.thumb_tall, st.thumb_error) = (false, Some(bytes), None),
                    (ThumbShape::Wide, Err(e)) => (st.thumb_busy, st.thumb_error) = (false, Some(e)),
                    (ThumbShape::Tall, Err(e)) => (st.tall_busy, st.thumb_error) = (false, Some(e)),
                }
                st.thumb_gen += 1;
            }
        }
    }

    /// What either video's pack is doing right now, for the progress card.
    pub fn publish_busy_text(&self) -> Option<String> {
        self.publish.status().or_else(|| self.stash.status())
    }

    /// True while either video's pack is being written or its thumbnails drawn.
    pub fn publish_working(&self) -> bool {
        self.publish.working() || self.stash.working()
    }

    /// Says so in the status line after something was copied to the clipboard.
    pub fn note_copied(&mut self) {
        self.note("Copied.");
    }

    /// Asks the AI to write the titles, description, tags and thumbnail words from eight frames and the moments.
    pub fn start_pack(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else {
            self.fail("Add your OpenAI key first.");
            return;
        };
        let target = self.target;
        let request = match target {
            Target::Single => self.single_request(),
            Target::Montage => self.montage_request(),
        };
        let Some(request) = request else {
            self.fail(if target == Target::Single { "Open a video first." } else { "Plan the montage first." });
            return;
        };
        let s = &self.settings;
        let (model, notes, reuse, mail) = (ai::OPENAI_MODELS[s.openai_model].0, s.style_notes.clone(), s.reuse_ai, self.mail.clone());
        let (topic, format) = (s.pack_topic.trim().to_string(), s.pack_format);
        self.publish.busy = Some("Writing your upload pack…".into());
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
    pub fn redraw_thumbnails(&mut self, shapes: Vec<ThumbShape>) {
        let Some(tools) = self.tools.clone() else { return };
        let target = self.target;
        let st = &mut self.publish;
        let Some(pack) = &mut st.pack else { return };
        let (candidate, text, x) = (pack.candidates.get(pack.thumb_pick).cloned(), pack.thumb_text.clone(), pack.thumb_x.unwrap_or(0.5));
        let styles = (pack.style(ThumbShape::Wide), pack.style(ThumbShape::Tall));
        let Some(candidate) = candidate else { return };
        for shape in &shapes {
            match shape {
                ThumbShape::Wide => (pack.ai_wide, st.thumb_busy) = (None, true),
                ThumbShape::Tall => (pack.ai_tall, st.tall_busy) = (None, true),
            }
        }
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let work = std::env::temp_dir().join(format!("video-enhancer-thumb-{}-{target:?}", std::process::id()));
            for shape in shapes {
                let style = if shape == ThumbShape::Wide { &styles.0 } else { &styles.1 };
                let result = pack::render_styled(&tools, &candidate, &text, &work, shape, x, style);
                mail.send(Msg::Pack(Event::Thumb(target, shape, result)));
            }
            std::fs::remove_dir_all(&work).ok();
        });
    }

    /// Sends the chosen frame and the words to OpenAI to design both thumbnails, one request per shape, side by side.
    pub fn start_ai_thumbnails(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else {
            self.fail("Add your OpenAI key first.");
            return;
        };
        let s = &self.settings;
        let (model, quality) = (thumb_ai::IMAGE_MODELS[s.thumb_model.min(thumb_ai::IMAGE_MODELS.len() - 1)].0, thumb_ai::QUALITIES[s.thumb_quality.min(thumb_ai::QUALITIES.len() - 1)].0);
        let (text_model, topic, reuse, mail) = (ai::OPENAI_MODELS[s.openai_model].0, s.pack_topic.trim().to_string(), s.reuse_ai, self.mail.clone());
        let target = self.target;
        let st = &mut self.publish;
        if st.ai_busy.is_some() {
            return;
        }
        let Some(pack) = &st.pack else { return };
        let Some(candidate) = pack.candidates.get(pack.thumb_pick).cloned() else { return };
        let brief = thumb_ai::Brief { text: pack::clean_thumb_text(&pack.thumb_text), topic, direction: pack.thumb_note.clone(), format: pack.format, style: TextStyle::default_for(ThumbShape::Wide) };
        let styles = (pack.style(ThumbShape::Wide), pack.style(ThumbShape::Tall));
        let x = pack.thumb_x.unwrap_or(0.5);
        (st.ai_busy, st.ai_pending, st.ai_cost, st.ai_reused, st.thumb_error) = (Some("Getting the frames ready…".into()), 2, 0.0, false, None);
        st.ai_warnings.clear();
        for shape in [ThumbShape::Wide, ThumbShape::Tall] {
            let (tools, key, candidate, mut brief, mail) = (tools.clone(), key.clone(), candidate.clone(), brief.clone(), mail.clone());
            brief.style = if shape == ThumbShape::Wide { styles.0.clone() } else { styles.1.clone() };
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

    /// A pack that came back from last time has its words but not its pictures: AI-designed ones are read back from disk, free,
    /// and the others are drawn plainly. Called once after a video opens.
    pub(crate) fn restore_thumbnails(&mut self) {
        let st = &mut self.publish;
        let Some(pack) = &mut st.pack else { return };
        if st.restored {
            return;
        }
        st.restored = true;
        let mut missing = Vec::new();
        match pack.ai_wide.as_deref().and_then(store::load_bytes) {
            Some(bytes) => st.thumb = Some(bytes),
            None => (pack.ai_wide, missing) = (None, [missing, vec![ThumbShape::Wide]].concat()),
        }
        match pack.ai_tall.as_deref().and_then(store::load_bytes) {
            Some(bytes) => st.thumb_tall = Some(bytes),
            None => (pack.ai_tall, missing) = (None, [missing, vec![ThumbShape::Tall]].concat()),
        }
        st.thumb_gen += 1;
        if !missing.is_empty() {
            self.redraw_thumbnails(missing);
        }
    }

    /// Saves the upload pack as a text file, and both thumbnails, next to the video (or in the chosen folder).
    pub fn save_pack(&mut self) {
        let (dir, stem) = match self.target {
            Target::Single => {
                let Some(source) = &self.source else { return };
                let stem = source.path.file_stem().map_or_else(|| "video".to_string(), |s| s.to_string_lossy().into_owned());
                (self.settings.out_dir.clone().or_else(|| source.path.parent().map(PathBuf::from)), stem)
            }
            Target::Montage => {
                let Some(facts) = self.montage_facts() else { return };
                (facts.out_dir.or_else(|| facts.clips.first().and_then(|c| c.path.parent().map(PathBuf::from))), crate::montage_engine::safe_name(&facts.title))
            }
        };
        let dir = dir.unwrap_or_else(std::env::temp_dir);
        let st = &self.publish;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiPack;
    use crate::engine::tests::{isolate_saves, wait};
    use crate::media::Tools;
    use crate::media::tests::{make_video, scratch};

    fn pack_for(engine: &Engine) -> Pack {
        let path = engine.source.as_ref().unwrap().path.clone();
        let candidates: Vec<Candidate> = [1.0, 2.0, 3.0].iter().map(|&t| Candidate { clip: path.clone(), t, label: format!("at {t}"), grade: engine.grade, learned: None }).collect();
        let ai = AiPack {
            titles: vec!["Valorant Clutch Moments".into(), "Second title".into()],
            intro: "A wild round.".into(),
            chapter_titles: vec![],
            hashtags: vec!["#Valorant".into()],
            thumb_candidate: 1,
            thumb_text: "insane clutch".into(),
            reason: "clear action".into(),
            primary_keyword: "Valorant clutch".into(),
            secondary_keywords: vec![],
            tags: vec!["valorant".into()],
        };
        let segments = vec![Segment { at: 0.0, note: "start".into() }];
        Pack::assemble(&ai, &segments, candidates, String::new(), pack::PackFormat::Standard).unwrap()
    }

    #[test]
    fn thumbnails_are_drawn_restyled_saved_and_come_back_with_the_video() {
        isolate_saves();
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("publish");
        let video = dir.join("clip.mp4");
        make_video(&tools, &video, "1920x1080", 6);
        let mut engine = Engine::new(|| {});
        engine.settings.out_dir = Some(dir.clone());
        engine.open(video.clone());
        wait(&mut engine, 60, |e| e.source.is_some() && e.graded.is_some());

        // A pack the AI would have written. The plain thumbnails are drawn for both shapes.
        engine.publish.pack = Some(pack_for(&engine));
        engine.publish.pack_gen += 1;
        engine.redraw_thumbnails(vec![ThumbShape::Wide, ThumbShape::Tall]);
        assert!(engine.publish.thumb_busy && engine.publish.tall_busy && engine.publish.working());
        wait(&mut engine, 60, |e| e.publish.thumb.is_some() && e.publish.thumb_tall.is_some() && !e.publish.working());
        let size = |b: &[u8]| image::load_from_memory(b).map(|i| (i.width(), i.height())).unwrap();
        assert_eq!((size(engine.publish.thumb.as_ref().unwrap()), size(engine.publish.thumb_tall.as_ref().unwrap())), ((1280, 720), (1080, 1920)));

        // Changing the look of the words for one shape redraws that shape only.
        let (wide_before, tall_before) = (engine.publish.thumb.clone().unwrap(), engine.publish.thumb_tall.clone().unwrap());
        let pack = engine.publish.pack.as_mut().unwrap();
        let style = TextStyle { size: 0.5, y: 0.2, ..pack.style(ThumbShape::Tall) };
        pack.set_style(ThumbShape::Tall, style);
        engine.redraw_thumbnails(vec![ThumbShape::Tall]);
        wait(&mut engine, 60, |e| !e.publish.working());
        assert_eq!(engine.publish.thumb.clone().unwrap(), wide_before, "the wide picture was left alone");
        assert!(engine.publish.thumb_tall.clone().unwrap() != tall_before, "the tall picture follows its new style");

        // Saving writes the words and both pictures.
        engine.save_pack();
        assert!(!engine.status_is_error, "{}", engine.status);
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        for want in ["clip_upload_pack.txt", "clip_thumbnail.jpg", "clip_thumbnail_shorts.jpg"] {
            assert!(names.iter().any(|n| n == want), "{want} missing in {names:?}");
        }
        let text = std::fs::read_to_string(dir.join("clip_upload_pack.txt")).unwrap();
        assert!(text.contains("Valorant Clutch Moments"), "{text}");

        // The pack is kept with the video: open it again and the words and both pictures return.
        engine.autosave();
        let mut again = Engine::new(|| {});
        again.open(video);
        wait(&mut again, 60, |e| e.publish.pack.is_some() && e.publish.thumb.is_some() && e.publish.thumb_tall.is_some() && !e.publish.working());
        let back = again.publish.pack.as_ref().unwrap();
        assert_eq!((back.titles[0].as_str(), back.style(ThumbShape::Tall).size), ("Valorant Clutch Moments", 0.5));
        std::fs::remove_dir_all(&dir).ok();
    }
}
