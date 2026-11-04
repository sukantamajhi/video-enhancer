//! The Montage screen: add clips, let the AI plan, review the cut list, pick
//! music, then render 16:9 and 9:16 versions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, RichText, vec2};

use crate::grade::{Grade, LOOKS, LUT_SIZE, Lut};
use crate::media::{self, Probe};
use crate::montage::{self, Clip, Cut, Job, Music, Out, Sound, Style, Vertical};
use crate::music::{self, Track};
use crate::{App, Msg, ai};

const MAX_CLIPS: usize = 12;

pub enum Event {
    Probed(PathBuf, Result<Probe, String>),
    Note(String),
    Plan(Result<PlanData, String>),
    Tracks(Result<Vec<Track>, String>),
    Progress(f32, String),
    Done(Result<Vec<PathBuf>, String>),
}

pub struct PlanData {
    pub cuts: Vec<Cut>,
    pub grades: Vec<Grade>,
    pub title: String,
    pub music_query: String,
    pub tempo: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MusicSource {
    Online,
    File,
}

pub struct State {
    clips: Vec<Clip>,
    probing: usize,
    style: Style,
    sound: Sound,
    vertical: Vertical,
    target: f64,
    look: Option<usize>,
    source: MusicSource,
    file: Option<PathBuf>,
    tracks: Vec<Track>,
    track: usize,
    searching: bool,
    music_query: String,
    plan: Option<PlanData>,
    keep: Vec<bool>,
    title: String,
    busy: Option<String>,
    make_wide: bool,
    make_vertical: bool,
    out_dir: Option<PathBuf>,
    render: Option<(f32, String, Arc<AtomicBool>)>,
    made: Vec<PathBuf>,
}

impl State {
    pub fn new() -> Self {
        Self {
            clips: Vec::new(),
            probing: 0,
            style: Style::Highlights,
            sound: Sound::MusicAndOriginal,
            vertical: Vertical::Follow,
            target: 60.0,
            look: Some(0),
            source: MusicSource::Online,
            file: None,
            tracks: Vec::new(),
            track: 0,
            searching: false,
            music_query: String::new(),
            plan: None,
            keep: Vec::new(),
            title: String::new(),
            busy: None,
            make_wide: true,
            make_vertical: true,
            out_dir: None,
            render: None,
            made: Vec::new(),
        }
    }

    /// True while something is running that needs the window to keep refreshing.
    pub fn working(&self) -> bool {
        self.busy.is_some() || self.render.is_some() || self.searching || self.probing > 0
    }

    fn total(&self) -> f64 {
        self.clips.iter().map(|c| c.probe.duration).sum()
    }
}

fn clock(t: f64) -> String {
    format!("{}:{:02.0}", (t / 60.0) as u32, t % 60.0)
}

fn safe_name(title: &str) -> String {
    let name: String = title.trim().chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').collect::<String>().split_whitespace().collect::<Vec<_>>().join("_");
    if name.is_empty() { "montage".into() } else { name.chars().take(40).collect() }
}

/// A font for the story title, copied next to the render so FFmpeg can find it.
fn title_font() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("WINDIR").map(|w| Path::new(&w).join("Fonts").join("arial.ttf")),
        Some("/System/Library/Fonts/Supplemental/Arial.ttf".into()),
        Some("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into()),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

impl App {
    pub fn add_clips(&mut self, paths: Vec<PathBuf>) {
        let Some(tools) = self.tools.clone() else {
            self.fail("Install the video tools first (Single video screen, button on the left).");
            return;
        };
        for path in paths {
            if self.mon.clips.len() + self.mon.probing >= MAX_CLIPS {
                self.fail(format!("A montage can use up to {MAX_CLIPS} clips."));
                break;
            }
            if self.mon.clips.iter().any(|c| c.path == path) {
                continue;
            }
            self.mon.probing += 1;
            let (tools, mail) = (tools.clone(), self.mail.clone());
            std::thread::spawn(move || {
                let probe = media::probe(&tools, &path);
                mail.send(Msg::Montage(Event::Probed(path, probe)));
            });
        }
    }

    pub fn montage_event(&mut self, event: Event) {
        match event {
            Event::Probed(path, result) => {
                self.mon.probing = self.mon.probing.saturating_sub(1);
                match result {
                    Ok(probe) if probe.duration >= 2.0 => {
                        self.mon.clips.push(Clip { path, probe });
                        self.mon.plan = None;
                        self.mon.tracks.clear();
                    }
                    Ok(_) => self.fail(format!("{} is shorter than 2 seconds.", path.display())),
                    Err(e) => self.fail(format!("{}: {e}", path.display())),
                }
            }
            Event::Note(text) => self.note(text),
            Event::Plan(Ok(plan)) => {
                self.mon.busy = None;
                self.mon.keep = vec![true; plan.cuts.len()];
                self.mon.title = plan.title.clone();
                self.mon.music_query = plan.music_query.clone();
                let total: f64 = plan.cuts.iter().map(Cut::len).sum();
                self.note(format!("The AI chose {} moments ({} before fades). Untick any you don't want, then make the montage.", plan.cuts.len(), clock(total)));
                self.mon.plan = Some(plan);
            }
            Event::Plan(Err(e)) => {
                self.mon.busy = None;
                self.mon.searching = false;
                self.fail(e);
            }
            Event::Tracks(result) => {
                self.mon.searching = false;
                match result {
                    Ok(tracks) => {
                        self.mon.tracks = tracks;
                        self.mon.track = 0;
                        self.note("Found free music that fits. The first song is selected; pick another if you like.");
                    }
                    Err(e) => self.fail(e),
                }
            }
            Event::Progress(p, label) => {
                if let Some(r) = &mut self.mon.render {
                    r.0 = p;
                    r.1 = label;
                }
            }
            Event::Done(result) => {
                self.mon.render = None;
                match result {
                    Ok(files) => {
                        self.note(format!("Finished. {} file(s) saved.", files.len()));
                        self.mon.made = files;
                    }
                    Err(e) if e == "Cancelled" => self.note("Cancelled. Nothing half-finished was left behind."),
                    Err(e) => self.fail(format!("Couldn't make the montage: {e}")),
                }
            }
        }
    }

    fn search_music(&mut self) {
        if self.mon.searching {
            return;
        }
        self.mon.searching = true;
        let (query, target, mail) = (self.mon.music_query.clone(), self.mon.target, self.mail.clone());
        std::thread::spawn(move || mail.send(Msg::Montage(Event::Tracks(music::search(&query, target)))));
    }

    fn plan_montage(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else {
            self.fail("Add your OpenAI key first (the box under 'AI helper' on this screen).");
            return;
        };
        if self.mon.clips.is_empty() {
            self.fail("Add at least one clip first.");
            return;
        }
        if self.mon.style.needs_music() && !self.mon.sound.uses_music() {
            self.fail("A beat-synced montage needs music. Choose a sound option that includes music.");
            return;
        }
        let m = &self.mon;
        let (clips, style, target, look, notes) = (m.clips.clone(), m.style, m.target, m.look, self.style_notes.clone());
        let wants_music = m.sound.uses_music() && m.source == MusicSource::Online;
        let model = ai::OPENAI_MODELS[self.openai_model].0;
        self.mon.busy = Some("Planning the montage…".into());
        self.mon.plan = None;
        self.mon.tracks.clear();
        self.mon.searching = wants_music;
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let note = |text: String| mail.send(Msg::Montage(Event::Note(text)));
            let result = (|| -> Result<PlanData, String> {
                let (mut briefs, mut energies, mut grades) = (Vec::new(), Vec::new(), Vec::new());
                for (i, clip) in clips.iter().enumerate() {
                    note(format!("Looking at clip {} of {}: {}…", i + 1, clips.len(), clip.name()));
                    let count = (clip.probe.duration / 12.0).clamp(6.0, 12.0) as usize;
                    let frames = montage::sample_frames(&tools, clip, count, 384)?;
                    let energy = montage::analyse_energy(&tools, clip)?;
                    grades.push(montage::clip_grade(&frames, look));
                    briefs.push(ai::ClipBrief { name: clip.name(), duration: clip.probe.duration, peaks: energy.peaks(5), frames });
                    energies.push(energy);
                }
                note(format!("Asking {model} to choose the best moments…"));
                let plan = ai::openai_plan(&key, model, &briefs, style, target, &notes)?;
                let durations: Vec<f64> = clips.iter().map(|c| c.probe.duration).collect();
                let cuts = montage::plan_cuts(plan.cuts, &durations, &energies, style, target);
                if cuts.is_empty() {
                    return Err("The AI's suggestions didn't fit your clips. Try again.".into());
                }
                Ok(PlanData { cuts, grades, title: plan.title, music_query: plan.music_query, tempo: plan.tempo })
            })();
            let query = result.as_ref().ok().map(|p| p.music_query.clone());
            mail.send(Msg::Montage(Event::Plan(result)));
            if let (true, Some(query)) = (wants_music, query) {
                note("Searching for free music…".into());
                mail.send(Msg::Montage(Event::Tracks(music::search(&query, target))));
            }
        });
    }

    fn render_montage(&mut self) {
        let Some(tools) = self.tools.clone() else { return };
        let m = &self.mon;
        let Some(plan) = &m.plan else { return };
        let cuts: Vec<Cut> = plan.cuts.iter().zip(&m.keep).filter(|(_, k)| **k).map(|(c, _)| c.clone()).collect();
        if cuts.is_empty() {
            self.fail("Every moment is unticked. Keep at least one.");
            return;
        }
        if !m.make_wide && !m.make_vertical {
            self.fail("Tick 16:9, 9:16, or both.");
            return;
        }
        // The chosen song first, then the next few, in case a server refuses one.
        let candidates: Vec<Track> = if m.source == MusicSource::Online { m.tracks.iter().cycle().skip(m.track).take(m.tracks.len().min(4)).cloned().collect() } else { Vec::new() };
        let file = (m.source == MusicSource::File).then(|| m.file.clone()).flatten();
        if m.sound.uses_music() && candidates.is_empty() && file.is_none() {
            self.fail("Choose music first: search for free music, pick your own file, or switch the sound to original only.");
            return;
        }
        let (clips, grades, style, sound, vertical, target) = (m.clips.clone(), plan.grades.clone(), m.style, m.sound, m.vertical, m.target);
        let (wide, tall, title) = (m.make_wide, m.make_vertical, m.title.trim().to_string());
        let out_dir = m.out_dir.clone().or_else(|| clips.first().and_then(|c| c.path.parent().map(PathBuf::from))).unwrap_or_else(std::env::temp_dir);
        let cancel = Arc::new(AtomicBool::new(false));
        self.mon.render = Some((0.0, "Getting ready…".into(), cancel.clone()));
        self.mon.made.clear();
        let mail = self.mail.clone();
        std::thread::spawn(move || {
            let work = std::env::temp_dir().join(format!("video-enhancer-montage-{}", std::process::id()));
            let result = (|| -> Result<Vec<PathBuf>, String> {
                let say = |p: f32, text: &str| mail.send(Msg::Montage(Event::Progress(p, text.to_string())));
                std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
                for (i, g) in grades.iter().enumerate() {
                    Lut::build(LUT_SIZE, g, None).write_cube(&work.join(format!("lut{i}.cube"))).map_err(|e| e.to_string())?;
                }
                let mut cuts = cuts;
                let durations: Vec<f64> = clips.iter().map(|c| c.probe.duration).collect();
                let mut used: Option<Track> = None;

                // Music, and for a beat-synced edit the beat grid.
                let mut song = None;
                if sound.uses_music() {
                    let path = match &file {
                        Some(f) => f.clone(),
                        None => {
                            let mut found = None;
                            let mut why = String::new();
                            for t in &candidates {
                                say(0.0, &format!("Downloading \"{}\"…", t.title));
                                match music::download(t, &work) {
                                    Ok(p) => {
                                        used = Some(t.clone());
                                        found = Some(p);
                                        break;
                                    }
                                    Err(e) => why = e,
                                }
                            }
                            found.ok_or_else(|| format!("None of the songs could be downloaded. Last problem: {why} Choose your own music file instead, or switch the sound to original only."))?
                        }
                    };
                    let mut start = 0.0;
                    if style == Style::BeatSync {
                        say(0.0, "Finding the beat…");
                        let beats = music::detect_beats(&tools, &path)?;
                        montage::quantise(&mut cuts, &durations, beats.interval, target);
                        start = beats.first;
                    }
                    song = Some(Music { path, start });
                }

                // Where the action is, for vertical videos that follow it.
                let mut actions: Vec<Option<Vec<(f64, f32)>>> = vec![None; clips.len()];
                if tall && vertical == Vertical::Follow {
                    for i in cuts.iter().map(|c| c.clip).collect::<std::collections::BTreeSet<_>>() {
                        say(0.0, &format!("Following the action in {}…", clips[i].name()));
                        actions[i] = media::analyse_action(&tools, &clips[i].probe, &clips[i].path).ok();
                    }
                }

                if style == Style::Story && !title.is_empty() {
                    if let Some(font) = title_font() {
                        std::fs::copy(font, work.join("font.ttf")).map_err(|e| e.to_string())?;
                        std::fs::write(work.join("title.txt"), &title).map_err(|e| e.to_string())?;
                    }
                }

                let stem = safe_name(&title);
                let shapes: Vec<(&str, Out, &str)> = [(wide, "16:9", Out::Wide, "_16x9"), (tall, "9:16", Out::Vertical(vertical), "_9x16")].into_iter().filter(|s| s.0).map(|s| (s.1, s.2, s.3)).collect();
                let mut made = Vec::new();
                for (i, (label, out, suffix)) in shapes.iter().enumerate() {
                    let output = media::free_name(&out_dir, &stem, suffix, "mp4");
                    let job = Job { clips: &clips, actions: &actions, cuts: &cuts, style, sound, music: song.as_ref(), out: *out, title: Some(&title), workdir: &work, output: &output };
                    let steps = shapes.len() as f32;
                    media_progress(&mail, i as f32, steps, label);
                    montage::render(&tools, &job, &cancel, |f| media_progress(&mail, i as f32 + f, steps, label))?;
                    made.push(output);
                }
                if let Some(t) = &used {
                    let credits = media::free_name(&out_dir, &stem, "_music_credits", "txt");
                    std::fs::write(&credits, music::credits_text(t)).map_err(|e| e.to_string())?;
                    made.push(credits);
                }
                Ok(made)
            })();
            std::fs::remove_dir_all(&work).ok();
            mail.send(Msg::Montage(Event::Done(result)));
        });
    }

    pub fn montage_screen(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_max_width(980.0);
            ui.heading("Montage");
            ui.label(RichText::new("Drop several clips here. The AI picks the best moments, and your real footage is cut together.").weak());
            ui.add_space(6.0);
            self.clips_section(ui);
            ui.add_space(8.0);
            self.settings_section(ui);
            ui.add_space(8.0);
            self.plan_section(ui);
            ui.add_space(8.0);
            self.make_section(ui);
        });
    }

    fn clips_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("1. Clips").strong().size(16.0));
            if ui.button("Add clips…").clicked()
                && let Some(paths) = rfd::FileDialog::new().add_filter("Video", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "wmv"]).pick_files()
            {
                self.add_clips(paths);
            }
            if self.mon.probing > 0 {
                ui.spinner();
            }
            if !self.mon.clips.is_empty() {
                ui.label(RichText::new(format!("{} clips, {} in total", self.mon.clips.len(), clock(self.mon.total()))).weak());
            }
        });
        let mut action: Option<(usize, i32)> = None;
        for (i, clip) in self.mon.clips.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{}.", i + 1));
                ui.label(RichText::new(clip.name()).strong());
                ui.label(RichText::new(format!("{}  {}x{}", clock(clip.probe.duration), clip.probe.width, clip.probe.height)).weak());
                if i > 0 && ui.small_button("Up").clicked() {
                    action = Some((i, -1));
                }
                if i + 1 < self.mon.clips.len() && ui.small_button("Down").clicked() {
                    action = Some((i, 1));
                }
                if ui.small_button("Remove").clicked() {
                    action = Some((i, 0));
                }
            });
        }
        if let Some((i, step)) = action {
            match step {
                0 => {
                    self.mon.clips.remove(i);
                }
                s => self.mon.clips.swap(i, (i as i32 + s) as usize),
            }
            self.mon.plan = None;
            self.mon.tracks.clear();
        }
        if self.mon.clips.is_empty() {
            ui.label(RichText::new("No clips yet.").weak());
        }
    }

    fn settings_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("2. Choices").strong().size(16.0));
        egui::Grid::new("montage_settings").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            ui.label("Style");
            ui.vertical(|ui| {
                egui::ComboBox::from_id_salt("m_style").selected_text(self.mon.style.name()).width(280.0).show_ui(ui, |ui| {
                    for s in Style::ALL {
                        ui.selectable_value(&mut self.mon.style, s, s.name());
                    }
                });
                ui.label(RichText::new(self.mon.style.blurb()).weak().small());
            });
            ui.end_row();

            ui.label("Length");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.mon.target).range(10.0..=900.0).speed(1.0).suffix(" seconds"));
                for (label, secs) in [("30s", 30.0), ("1 min", 60.0), ("2 min", 120.0), ("3 min", 180.0), ("5 min", 300.0)] {
                    if ui.small_button(label).clicked() {
                        self.mon.target = secs;
                    }
                }
            });
            ui.end_row();

            ui.label("Sound");
            egui::ComboBox::from_id_salt("m_sound").selected_text(self.mon.sound.name()).width(320.0).show_ui(ui, |ui| {
                for s in Sound::ALL {
                    ui.selectable_value(&mut self.mon.sound, s, s.name());
                }
            });
            ui.end_row();

            ui.label("Colour look");
            egui::ComboBox::from_id_salt("m_look").selected_text(self.mon.look.map_or("Natural (fix only)", |i| LOOKS[i].name)).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.mon.look, None, "Natural (fix only)");
                for (i, look) in LOOKS.iter().enumerate() {
                    ui.selectable_value(&mut self.mon.look, Some(i), look.name);
                }
            });
            ui.end_row();

            ui.label("9:16 framing");
            egui::ComboBox::from_id_salt("m_vertical").selected_text(match self.mon.vertical {
                Vertical::Follow => "Follow the action",
                Vertical::Centre => "Centre",
                Vertical::Blur => "Whole picture on a blurred background",
            }).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.mon.vertical, Vertical::Follow, "Follow the action");
                ui.selectable_value(&mut self.mon.vertical, Vertical::Centre, "Centre");
                ui.selectable_value(&mut self.mon.vertical, Vertical::Blur, "Whole picture on a blurred background");
            });
            ui.end_row();

            ui.label("Anything special?");
            ui.add(egui::TextEdit::multiline(&mut self.style_notes).desired_rows(2).desired_width(420.0));
            ui.end_row();
        });

        if self.mon.sound.uses_music() {
            ui.add_space(6.0);
            ui.label(RichText::new("Music").strong());
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.mon.source, MusicSource::Online, "Find free music for me");
                ui.radio_value(&mut self.mon.source, MusicSource::File, "Use my own file");
            });
            match self.mon.source {
                MusicSource::Online => {
                    ui.label(RichText::new("Searches openly licensed music (CC0, public domain, CC BY, CC BY-SA, so it is safe in a monetised video). The AI chooses the mood. A credit line for your description is saved next to the montage.").weak().small());
                    if self.mon.plan.is_some() {
                        let mut again = false;
                        ui.horizontal(|ui| {
                            ui.label("Mood");
                            ui.add(egui::TextEdit::singleline(&mut self.mon.music_query).desired_width(160.0));
                            again = ui.add_enabled(!self.mon.searching, egui::Button::new("Search again")).clicked();
                            if self.mon.searching {
                                ui.spinner();
                            }
                        });
                        if again {
                            self.search_music();
                        }
                        if !self.mon.tracks.is_empty() {
                            let label = self.mon.tracks.get(self.mon.track).map_or_else(String::new, Track::label);
                            egui::ComboBox::from_id_salt("m_track").selected_text(label).width(520.0).show_ui(ui, |ui| {
                                for (i, t) in self.mon.tracks.iter().enumerate() {
                                    ui.selectable_value(&mut self.mon.track, i, t.label());
                                }
                            });
                        }
                    }
                }
                MusicSource::File => {
                    ui.horizontal(|ui| {
                        if ui.button("Choose music file…").clicked()
                            && let Some(f) = rfd::FileDialog::new().add_filter("Audio", &["mp3", "wav", "m4a", "aac", "ogg", "flac"]).pick_file()
                        {
                            self.mon.file = Some(f);
                        }
                        ui.label(self.mon.file.as_ref().map_or_else(|| "No file chosen".to_string(), |f| f.display().to_string()));
                    });
                    ui.label(RichText::new("Make sure you have the right to use this song in your video.").weak().small());
                }
            }
        }
    }

    fn plan_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("3. AI plan").strong().size(16.0));
            let ready = !self.mon.clips.is_empty() && self.mon.busy.is_none() && self.mon.render.is_none();
            if ui.add_enabled(ready && self.has_openai, egui::Button::new("Plan with AI")).on_hover_text("Sends small pictures from each clip to OpenAI (about 10 per clip). Your video files are not uploaded.").clicked() {
                self.plan_montage();
            }
            egui::ComboBox::from_id_salt("m_model").selected_text(ai::OPENAI_MODELS[self.openai_model].1).show_ui(ui, |ui| {
                for (i, (_, name)) in ai::OPENAI_MODELS.iter().enumerate() {
                    ui.selectable_value(&mut self.openai_model, i, *name);
                }
            });
            if let Some(text) = &self.mon.busy {
                ui.spinner();
                ui.label(text);
            }
        });
        if !self.has_openai {
            ui.horizontal(|ui| {
                ui.label("OpenAI key");
                ui.add(egui::TextEdit::singleline(&mut self.openai_key).password(true).hint_text("paste your key").desired_width(220.0));
                if ui.button("Save").clicked() {
                    match ai::save_key(ai::Provider::OpenAi, &self.openai_key) {
                        Ok(()) => {
                            self.openai_key.clear();
                            self.has_openai = ai::load_key(ai::Provider::OpenAi).is_some();
                        }
                        Err(e) => self.fail(e),
                    }
                }
                ui.label(RichText::new("Stored in your computer's secure credential store.").weak().small());
            });
        }
        let Some(plan) = &self.mon.plan else { return };
        ui.label(RichText::new(format!("Tempo the AI suggests: {}", plan.tempo)).weak().small());
        if self.mon.style == Style::Story {
            ui.horizontal(|ui| {
                ui.label("Title shown at the start");
                ui.add(egui::TextEdit::singleline(&mut self.mon.title).desired_width(260.0));
            });
        }
        egui::ScrollArea::vertical().id_salt("plan_rows").max_height(260.0).show(ui, |ui| {
            egui::Grid::new("plan").striped(true).num_columns(5).spacing([12.0, 4.0]).show(ui, |ui| {
                for h in ["Keep", "Clip", "From to", "Score", "Why"] {
                    ui.label(RichText::new(h).weak().small());
                }
                ui.end_row();
                for (i, cut) in plan.cuts.iter().enumerate() {
                    ui.checkbox(&mut self.mon.keep[i], "");
                    ui.label(self.mon.clips.get(cut.clip).map_or_else(|| "?".to_string(), Clip::name));
                    ui.label(format!("{} to {}  ({:.1}s)", clock(cut.start), clock(cut.end), cut.len()));
                    ui.label(format!("{:.0}%", cut.score * 100.0));
                    ui.label(RichText::new(&cut.why).small());
                    ui.end_row();
                }
            });
        });
    }

    fn make_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("4. Make it").strong().size(16.0));
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.mon.make_wide, "16:9");
            ui.checkbox(&mut self.mon.make_vertical, "9:16");
            ui.label("Save to");
            let shown = self.mon.out_dir.clone().or_else(|| self.mon.clips.first().and_then(|c| c.path.parent().map(PathBuf::from)));
            ui.label(RichText::new(shown.map_or_else(|| "next to the first clip".to_string(), |d| d.display().to_string())).small());
            if ui.button("Choose folder…").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                self.mon.out_dir = Some(dir);
            }
        });
        if let Some((progress, label, cancel)) = &self.mon.render {
            ui.label(label);
            ui.add(egui::ProgressBar::new(*progress).show_percentage());
            if ui.button("Cancel").clicked() {
                cancel.store(true, Ordering::Relaxed);
            }
        } else {
            let ready = self.mon.plan.is_some() && self.mon.busy.is_none();
            if ui.add_enabled(ready, egui::Button::new(RichText::new("Make montage").strong().size(16.0)).fill(Color32::from_rgb(214, 92, 51)).min_size(vec2(220.0, 34.0))).clicked() {
                self.render_montage();
            }
            if !ready {
                ui.label(RichText::new("Plan with AI first.").weak().small());
            }
        }
        if !self.mon.made.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Made:").strong());
            for file in &self.mon.made {
                ui.label(RichText::new(file.display().to_string()).small());
            }
        }
    }
}

fn media_progress(mail: &crate::Mailbox, done: f32, steps: f32, label: &str) {
    mail.send(Msg::Montage(Event::Progress((done / steps).clamp(0.0, 1.0), format!("Making the {label} montage…"))));
}
