//! The upload-ready pack: three title options, a description with chapters,
//! hashtags, and a thumbnail with bold text.
//!
//! The AI writes the words and chooses the thumbnail frame. Everything that
//! must be exact is computed here instead: chapter times come from where each
//! moment really sits in the finished video, and the thumbnail is drawn from
//! your own footage with the same colour grade as the video.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use crate::ai::AiPack;
use crate::grade::{Grade, LUT_SIZE, LearnedLook, Lut};
use crate::media::{self, Tools};

/// YouTube only makes chapters from a list that starts at 0:00, has at least
/// three entries, and keeps each at least ten seconds long.
pub const MIN_CHAPTER_GAP: f64 = 10.0;
pub const MAX_CHAPTERS: usize = 15;
pub const MAX_TITLE: usize = 100;

/// A moment in the finished video that could be a chapter.
#[derive(Clone, Debug)]
pub struct Segment {
    pub at: f64,
    pub note: String,
}

/// A frame the thumbnail could be made from.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub clip: PathBuf,
    pub t: f64,
    pub label: String,
    pub grade: Grade,
    #[serde(default)]
    pub learned: Option<LearnedLook>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chapter {
    pub at: f64,
    pub title: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Pack {
    pub titles: Vec<String>,
    pub title_pick: usize,
    pub intro: String,
    pub chapters: Vec<Chapter>,
    pub hashtags: Vec<String>,
    pub thumb_text: String,
    pub thumb_pick: usize,
    /// Where the 9:16 Shorts thumbnail is cut from a wide picture: 0 is the left edge, 1 the right. None is the middle.
    #[serde(default)]
    pub thumb_x: Option<f32>,
    /// What the creator wants the AI to show off in the thumbnails, in their own words.
    #[serde(default)]
    pub thumb_note: String,
    /// Set when the wide / tall thumbnail was designed by the AI: the key its picture is saved under.
    #[serde(default)]
    pub ai_wide: Option<String>,
    #[serde(default)]
    pub ai_tall: Option<String>,
    /// How the words look on each thumbnail. None means the default for that shape.
    #[serde(default)]
    pub style_wide: Option<TextStyle>,
    #[serde(default)]
    pub style_tall: Option<TextStyle>,
    pub reason: String,
    pub candidates: Vec<Candidate>,
    /// The music credit, when the video uses music that asks for one.
    pub credit: String,
    #[serde(default)]
    pub format: PackFormat,
    /// The one search phrase the title and description are built around.
    #[serde(default)]
    pub primary_keyword: String,
    #[serde(default)]
    pub secondary_keywords: Vec<String>,
    /// Hidden tags. YouTube says they matter little, mostly for misspellings.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Which kind of YouTube page the pack is for: the rules for a Short are different.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum PackFormat {
    #[default]
    Standard,
    Short,
}

impl PackFormat {
    pub fn name(self) -> &'static str {
        match self {
            PackFormat::Standard => "Standard video (16:9)",
            PackFormat::Short => "Short (9:16)",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            PackFormat::Standard => "a standard widescreen YouTube video",
            PackFormat::Short => "a vertical YouTube Short",
        }
    }
}

/// Hidden tags kept to a sensible size. Only distinct, short ones, and well under the 500 characters
/// YouTube Studio allows (commas count).
pub const MAX_TAGS: usize = 12;
pub const MAX_TAG_CHARS: usize = 450;

pub fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut total = 0usize;
    for t in tags {
        let tag: String = t.trim().trim_start_matches('#').split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
        if tag.is_empty() || out.iter().any(|o| o.eq_ignore_ascii_case(&tag)) {
            continue;
        }
        let cost = tag.chars().count() + usize::from(!out.is_empty());
        if total + cost > MAX_TAG_CHARS {
            break;
        }
        total += cost;
        out.push(tag);
        if out.len() == MAX_TAGS {
            break;
        }
    }
    out
}

pub fn stamp(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn tidy(text: &str, limit: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = one_line.trim_matches(|c: char| c == '"' || c == '\'' || c == '\u{201c}' || c == '\u{201d}').trim();
    let trimmed = trimmed.strip_prefix("Title:").map_or(trimmed, str::trim);
    trimmed.chars().take(limit).collect::<String>().trim_end().to_string()
}

pub fn clean_titles(titles: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in titles {
        let t = tidy(t, MAX_TITLE);
        if !t.is_empty() && out.iter().all(|o| !o.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
    }
    out.truncate(3);
    out
}

pub fn clean_hashtags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let tag: String = t.chars().filter(|c| c.is_alphanumeric()).take(30).collect();
        if !tag.is_empty() && out.iter().all(|o| !o.eq_ignore_ascii_case(&tag)) {
            out.push(tag);
        }
    }
    out.truncate(5);
    out
}

/// Short bold text for a thumbnail: capitals, at most three words.
pub fn clean_thumb_text(text: &str) -> String {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric() || matches!(c, '!' | '?' | '\'' | '$' | '%' | '+')).collect::<String>())
        .filter(|w| !w.is_empty())
        .take(3)
        .collect();
    words.join(" ").to_uppercase().chars().take(26).collect::<String>().trim().to_string()
}

/// Splits long thumbnail text over two lines so each line can be big.
pub fn wrap_thumb_text(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if text.chars().count() <= 11 || words.len() < 2 {
        return words.join(" ");
    }
    // Break where the two lines are most even.
    let best = (1..words.len()).min_by_key(|&i| {
        let (a, b) = (words[..i].join(" ").chars().count() as i32, words[i..].join(" ").chars().count() as i32);
        (a - b).abs()
    });
    let i = best.unwrap_or(1);
    format!("{}\n{}", words[..i].join(" "), words[i..].join(" "))
}

/// Chapters from the moments of the video and the AI's short titles for them.
/// Follows YouTube's rules, so the list is dropped entirely if it can't qualify.
pub fn build_chapters(segments: &[Segment], titles: &HashMap<usize, String>) -> Vec<Chapter> {
    let mut chapters: Vec<Chapter> = Vec::new();
    for (i, segment) in segments.iter().enumerate() {
        let at = if i == 0 { 0.0 } else { segment.at };
        let said = titles.get(&i).map(|t| tidy(t, 50)).filter(|t| !t.is_empty());
        let title = said.or_else(|| Some(tidy(&segment.note, 50)).filter(|t| !t.is_empty())).or_else(|| (i == 0).then(|| "Intro".to_string()));
        let Some(title) = title else { continue };
        if chapters.last().is_some_and(|last| at - last.at < MIN_CHAPTER_GAP) {
            continue;
        }
        chapters.push(Chapter { at, title });
        if chapters.len() == MAX_CHAPTERS {
            break;
        }
    }
    if chapters.len() < 3 || chapters[0].at != 0.0 { Vec::new() } else { chapters }
}

impl Pack {
    /// The look of the words for a shape.
    pub fn style(&self, shape: ThumbShape) -> TextStyle {
        match shape {
            ThumbShape::Wide => self.style_wide.clone(),
            ThumbShape::Tall => self.style_tall.clone(),
        }
        .unwrap_or_else(|| TextStyle::default_for(shape))
        .clamped()
    }

    pub fn set_style(&mut self, shape: ThumbShape, style: TextStyle) {
        match shape {
            ThumbShape::Wide => self.style_wide = Some(style.clamped()),
            ThumbShape::Tall => self.style_tall = Some(style.clamped()),
        }
    }

    /// Builds the pack from what the AI said, the real chapter moments and the frames on offer.
    pub fn assemble(ai: &AiPack, segments: &[Segment], candidates: Vec<Candidate>, credit: String, format: PackFormat) -> Result<Pack, String> {
        let titles = clean_titles(&ai.titles);
        if titles.is_empty() {
            return Err("The AI didn't write any titles. Try again.".into());
        }
        if candidates.is_empty() {
            return Err("There is no frame to make a thumbnail from.".into());
        }
        let said: HashMap<usize, String> = ai.chapter_titles.iter().cloned().collect();
        let mut hashtags = clean_hashtags(&ai.hashtags);
        if format == PackFormat::Short && !hashtags.iter().take(3).any(|t| t.eq_ignore_ascii_case("shorts")) {
            // The game or topic stays first; Shorts takes second place, inside the three YouTube shows above the title.
            hashtags.retain(|t| !t.eq_ignore_ascii_case("shorts"));
            hashtags.insert(1.min(hashtags.len()), "Shorts".into());
            hashtags.truncate(5);
        }
        Ok(Pack {
            titles,
            title_pick: 0,
            intro: tidy(&ai.intro.replace("\n\n", "\u{1}"), 900).replace('\u{1}', "\n\n"),
            // A Short has no chapters.
            chapters: if format == PackFormat::Short { Vec::new() } else { build_chapters(segments, &said) },
            hashtags,
            thumb_text: clean_thumb_text(&ai.thumb_text),
            thumb_pick: ai.thumb_candidate.min(candidates.len() - 1),
            thumb_x: None,
            thumb_note: String::new(),
            ai_wide: None,
            ai_tall: None,
            style_wide: None,
            style_tall: None,
            reason: tidy(&ai.reason, 240),
            candidates,
            credit,
            format,
            primary_keyword: tidy(&ai.primary_keyword, 60),
            secondary_keywords: ai.secondary_keywords.iter().map(|k| tidy(k, 60)).filter(|k| !k.is_empty()).take(3).collect(),
            tags: clean_tags(&ai.tags),
        })
    }

    /// The description, ready to paste: intro, chapters, music credit, hashtags.
    pub fn description(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.intro.trim().is_empty() {
            parts.push(self.intro.trim().to_string());
        }
        if !self.chapters.is_empty() {
            let lines: Vec<String> = self.chapters.iter().map(|c| format!("{} {}", stamp(c.at), c.title)).collect();
            parts.push(format!("Chapters:\n{}", lines.join("\n")));
        }
        if !self.credit.trim().is_empty() {
            let credit = self.credit.trim();
            // NCS's own credit is several lines and starts with "Song:", so it needs no label.
            parts.push(if credit.contains('\n') { credit.to_string() } else { format!("Music: {credit}") });
        }
        if !self.hashtags.is_empty() {
            parts.push(self.hashtags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "));
        }
        parts.join("\n\n")
    }

    /// Everything as one text file.
    pub fn as_text(&self) -> String {
        let titles: Vec<String> = self.titles.iter().enumerate().map(|(i, t)| format!("{}. {t}", i + 1)).collect();
        let focus = if self.primary_keyword.is_empty() { String::new() } else { format!("FOCUS KEYWORD\n{}\n\n", self.primary_keyword) };
        let tags = if self.tags.is_empty() { String::new() } else { format!("\n\nTAGS (optional, in YouTube Studio under Show more)\n{}", self.tags.join(", ")) };
        format!("{focus}TITLE OPTIONS\n{}\n\nDESCRIPTION (paste this under the video)\n{}\n\nTHUMBNAIL TEXT\n{}{tags}\n", titles.join("\n"), self.description(), self.thumb_text)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Good,
    Warn,
    Bad,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub level: Level,
    pub text: String,
}

/// Lower case, letters and digits only, single spaces: so "Valorant: Clutch!" matches "valorant clutch".
fn norm(text: &str) -> String {
    text.chars().flat_map(|c| if c.is_alphanumeric() { c.to_lowercase().collect::<Vec<_>>() } else { vec![' '] }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Pack {
    /// Checks the title and description as they are now (including your edits) against what
    /// YouTube says about search, and against the widely used length rules of thumb.
    pub fn seo_checks(&self) -> Vec<Check> {
        use Level::*;
        let mut out: Vec<Check> = Vec::new();
        let mut add = |level: Level, text: String| out.push(Check { level, text });
        let title = self.titles.get(self.title_pick).map_or("", String::as_str);
        let (n, good) = (title.chars().count(), if self.format == PackFormat::Short { 40 } else { 60 });
        if n == 0 {
            add(Bad, "There is no title.".into());
        } else if n > MAX_TITLE {
            add(Bad, format!("The title is {n} characters, over YouTube's limit of {MAX_TITLE}."));
        } else if n <= good {
            add(Good, format!("The title is {n} characters, short enough to show in full."));
        } else {
            add(Warn, format!("The title is {n} characters and may be cut off in search results. Aim for {good} or fewer."));
        }

        let description = self.description();
        let keyword = norm(&self.primary_keyword);
        if keyword.is_empty() {
            add(Warn, "No focus keyword was chosen, so the keyword checks are skipped.".into());
        } else {
            let shown = &self.primary_keyword;
            let t = norm(title);
            match t.find(&keyword) {
                None => add(Warn, format!("The focus keyword \"{shown}\" is not in the title.")),
                Some(at) if at <= t.len() / 2 => add(Good, format!("The title has the focus keyword \"{shown}\", near the start.")),
                Some(_) => add(Warn, format!("The focus keyword \"{shown}\" is in the title but late. Earlier is better.")),
            }
            let opening: String = description.chars().take(150).collect();
            if norm(&opening).contains(&keyword) {
                add(Good, "The focus keyword is in the first 150 characters of the description, which is what viewers see first.".into());
            } else {
                add(Warn, "The focus keyword is not in the first 150 characters of the description, which is what viewers see first.".into());
            }
            let count = norm(&description).matches(&keyword).count();
            match count {
                0 => add(Warn, "The focus keyword never appears in the description.".into()),
                1..=4 => add(Good, format!("The focus keyword appears {count} time(s) in the description, which reads naturally.")),
                _ => add(Warn, format!("The focus keyword appears {count} times in the description. That can read as keyword stuffing.")),
            }
        }

        match self.hashtags.len() {
            3..=5 => add(Good, format!("{} hashtags. YouTube shows the first three above the title.", self.hashtags.len())),
            0 => add(Warn, "There are no hashtags.".into()),
            1 | 2 => add(Warn, format!("Only {} hashtag(s). Three to five work well.", self.hashtags.len())),
            more => add(Warn, format!("{more} hashtags is a lot. YouTube shows only the first three above the title, and over-tagging looks like spam.")),
        }
        if self.format == PackFormat::Standard {
            if self.chapters.len() >= 3 {
                add(Good, format!("{} chapters, which YouTube turns into a timeline.", self.chapters.len()));
            } else {
                add(Warn, "No chapters: YouTube needs at least three, ten seconds apart, so this video is too short for them.".into());
            }
        }
        let tag_chars: usize = self.tags.iter().map(|t| t.chars().count() + 1).sum();
        if self.tags.is_empty() {
            add(Warn, "No tags. They matter little, but they help with misspellings of the game's name.".into());
        } else if tag_chars <= MAX_TAG_CHARS + MAX_TAGS {
            add(Good, format!("{} tags. They matter little for discovery, so this is plenty.", self.tags.len()));
        } else {
            add(Warn, "The tags are very long. Fewer, better tags are safer than many.".into());
        }
        let thumb_words = self.thumb_text.split_whitespace().count();
        if (1..=3).contains(&thumb_words) && self.thumb_text.chars().count() <= 18 {
            add(Good, "The thumbnail text is short enough to read when small.".into());
        } else {
            add(Warn, "The thumbnail text is long: aim for 1 to 3 words and 18 characters or fewer.".into());
        }
        if description.chars().count() > 5000 {
            add(Bad, "The description is over YouTube's limit of 5000 characters.".into());
        }
        out
    }
}

// ----------------------------------------------------------------- thumbnail

/// A heavy, bold font for the thumbnail, if this computer has one.
fn bold_font() -> Option<PathBuf> {
    let windows = std::env::var_os("WINDIR").map(|w| Path::new(&w).join("Fonts"));
    let mut found: Vec<PathBuf> = Vec::new();
    for name in ["impact.ttf", "ariblk.ttf", "arialbd.ttf"] {
        found.extend(windows.as_ref().map(|d| d.join(name)));
    }
    found.extend(["/System/Library/Fonts/Supplemental/Impact.ttf", "/System/Library/Fonts/Supplemental/Arial Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"].map(PathBuf::from));
    found.into_iter().find(|p| p.is_file())
}

/// The two thumbnails a pack makes: the wide one for a normal video, the tall one for a Short.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThumbShape {
    Wide,
    Tall,
}

impl ThumbShape {
    pub fn size(self) -> (u32, u32) {
        match self {
            ThumbShape::Wide => (1280, 720),
            ThumbShape::Tall => (1080, 1920),
        }
    }
}

/// The font size that fits the wrapped text across the picture.
fn font_size_for(wrapped: &str, shape: ThumbShape) -> u32 {
    let lines = wrapped.lines().count().max(1) as f64;
    let longest = wrapped.lines().map(|l| l.chars().count()).max().unwrap_or(1).max(1) as f64;
    // Impact letters are about half as wide as the font is tall.
    let (across, tallest) = match shape {
        ThumbShape::Wide => (1160.0, 230.0),
        ThumbShape::Tall => (940.0, 300.0),
    };
    ((across / (0.52 * longest)).min(tallest / lines.sqrt())).max(40.0) as u32
}

/// How the words on a thumbnail look and where they sit. Each shape keeps its own, because a Short
/// needs its words in a different place from a wide thumbnail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    /// Where the middle of the words sits, as a share of the picture: 0 is the left / top edge, 1 the right / bottom.
    pub x: f32,
    pub y: f32,
    /// 1 is the size that just fits the picture. Smaller and larger are allowed, but never wider than the picture.
    pub size: f32,
    pub fill: [u8; 3],
    pub outline: [u8; 3],
    /// 1 is the usual outline, 0 none, 2 twice as thick.
    pub outline_width: f32,
    pub shadow: bool,
    /// How dark the soft shade behind the words is, 0 to 1.
    pub shade: f32,
    pub font: String,
    /// 0 lets the app decide, 1 forces one line, 2 forces two.
    pub lines: u8,
}

impl TextStyle {
    pub fn default_for(shape: ThumbShape) -> Self {
        Self {
            x: 0.5,
            y: if shape == ThumbShape::Wide { 0.84 } else { 0.60 },
            size: 1.0,
            fill: [0xFF, 0xE2, 0x00],
            outline: [0, 0, 0],
            outline_width: 1.0,
            shadow: true,
            shade: 0.7,
            font: "Impact".into(),
            lines: 0,
        }
    }

    /// Keeps every value in the range the drawing can use, so a saved or typed value can't break a thumbnail.
    pub fn clamped(mut self) -> Self {
        self.x = self.x.clamp(0.0, 1.0);
        self.y = self.y.clamp(0.0, 1.0);
        self.size = self.size.clamp(0.4, 1.8);
        self.outline_width = self.outline_width.clamp(0.0, 2.5);
        self.shade = self.shade.clamp(0.0, 1.0);
        self.lines = self.lines.min(2);
        self
    }
}

/// Fonts that make heavy thumbnail lettering, as (name shown, file name). Only the ones this computer has are offered.
const FONT_FILES: [(&str, &str); 4] = [("Impact", "impact.ttf"), ("Arial Black", "ariblk.ttf"), ("Arial Bold", "arialbd.ttf"), ("Segoe UI Black", "seguibl.ttf")];

pub fn available_fonts() -> &'static [String] {
    static FOUND: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        let dir = std::env::var_os("WINDIR").map(|w| Path::new(&w).join("Fonts"));
        let mut found: Vec<String> = FONT_FILES.iter().filter(|(_, file)| dir.as_ref().is_some_and(|d| d.join(file).is_file())).map(|(name, _)| name.to_string()).collect();
        if found.is_empty() && bold_font().is_some() {
            found.push("Impact".into());
        }
        found
    })
}

fn font_file(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("WINDIR").map(|w| Path::new(&w).join("Fonts"));
    FONT_FILES.iter().find(|(n, _)| *n == name).and_then(|(_, file)| dir.map(|d| d.join(file))).filter(|p| p.is_file()).or_else(bold_font)
}

/// How wide a letter is compared with the font size, on average. Impact is narrow; the others are wide.
fn letter_width(font: &str) -> f32 {
    if font == "Impact" { 0.52 } else { 0.70 }
}

/// The words as drawn: where the lines break, the font size in pixels, and the size of the block in pixels.
pub struct Layout {
    pub wrapped: String,
    pub size: u32,
    pub width: f32,
    pub height: f32,
}

pub fn text_layout(text: &str, shape: ThumbShape, style: &TextStyle) -> Layout {
    let style = style.clone().clamped();
    let text = clean_thumb_text(text);
    let words: Vec<&str> = text.split_whitespace().collect();
    let wrapped = match style.lines {
        1 => words.join(" "),
        2 if words.len() >= 2 => {
            let i = (1..words.len()).min_by_key(|&i| (words[..i].join(" ").chars().count() as i32 - words[i..].join(" ").chars().count() as i32).abs()).unwrap_or(1);
            format!("{}\n{}", words[..i].join(" "), words[i..].join(" "))
        }
        _ => wrap_thumb_text(&text),
    };
    let lines = wrapped.lines().count().max(1) as f32;
    let longest = wrapped.lines().map(|l| l.chars().count()).max().unwrap_or(1).max(1) as f32;
    let factor = letter_width(&style.font);
    let (w, _) = shape.size();
    let fit = font_size_for(&wrapped, shape) as f32 * 0.52 / factor;
    // Never wider than the picture, however large the slider.
    let widest = (w as f32 * 0.92) / (factor * longest);
    let size = (fit * style.size).min(widest).max(24.0);
    Layout { wrapped, size: size as u32, width: longest * size * factor, height: lines * size * 1.05 }
}

/// Where the middle of the words really sits: the chosen spot, nudged so the whole block stays inside the margin.
pub fn text_centre(layout: &Layout, shape: ThumbShape, style: &TextStyle) -> (f32, f32) {
    let style = style.clone().clamped();
    let (w, h) = (shape.size().0 as f32, shape.size().1 as f32);
    let m = w * 0.04;
    let cx = (style.x * w).clamp(m + layout.width / 2.0, (w - m - layout.width / 2.0).max(m + layout.width / 2.0));
    let cy = (style.y * h).clamp(m + layout.height / 2.0, (h - m - layout.height / 2.0).max(m + layout.height / 2.0));
    (cx, cy)
}

/// Draws a thumbnail with the default look of its shape. See `render_styled`.
pub fn render_thumbnail(tools: &Tools, candidate: &Candidate, text: &str, workdir: &Path, shape: ThumbShape, x: f32) -> Result<Vec<u8>, String> {
    render_styled(tools, candidate, text, workdir, shape, x, &TextStyle::default_for(shape))
}

/// Draws a thumbnail: the chosen frame in the video's own colour with extra punch, and the words
/// in the style given. Wide is 1280x720 for YouTube. Tall is 1080x1920 for a Short, cut out of the
/// frame at `x` (0 left, 1 right). Returns the JPEG.
pub fn render_styled(tools: &Tools, candidate: &Candidate, text: &str, workdir: &Path, shape: ThumbShape, x: f32, style: &TextStyle) -> Result<Vec<u8>, String> {
    std::fs::create_dir_all(workdir).map_err(|e| e.to_string())?;
    let probe = media::probe(tools, &candidate.clip)?;
    Lut::build(LUT_SIZE, &candidate.grade, candidate.learned.as_ref()).write_cube(&workdir.join("thumb.cube")).map_err(|e| e.to_string())?;
    let style = style.clone().clamped();
    let layout = text_layout(text, shape, &style);
    let font = font_file(&style.font).filter(|_| !layout.wrapped.is_empty());
    let (w, h) = shape.size();

    let mut chain = cover_chain(&probe, w, h, shape == ThumbShape::Tall, x);
    chain.push("format=gbrp,lut3d=file=thumb.cube:interp=tetrahedral".into());
    // A little more punch than the video itself, the way thumbnails need to read at small sizes.
    chain.push("format=yuv444p,eq=contrast=1.1:saturation=1.25,unsharp=5:5:0.7:5:5:0.0".into());
    // Darker corners pull the eye to the middle of the picture.
    chain.push("vignette=angle=0.5".into());
    if let Some(font) = font {
        std::fs::copy(font, workdir.join("font.ttf")).map_err(|e| e.to_string())?;
        std::fs::write(workdir.join("thumb.txt"), &layout.wrapped).map_err(|e| e.to_string())?;
        let (cx, cy) = text_centre(&layout, shape, &style);
        let m = w as f32 * 0.04;
        // A soft shade behind the words, fading in and out, in place of a hard-edged box. Only the brightness changes.
        if style.shade > 0.01 {
            let (half, feather) = (layout.height * 0.55, layout.height * 0.5);
            chain.push(format!("geq=lum='lum(X,Y)*(1-{:.3}*min(1,max(0,1-(abs(Y-{cy:.1})-{half:.1})/{feather:.1})))':cb='cb(X,Y)':cr='cr(X,Y)'", 0.7 * style.shade));
        }
        let hex = |c: [u8; 3]| format!("0x{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
        let border = (layout.size as f32 / 13.0 * style.outline_width).round() as u32;
        let shadow = if style.shadow { format!(":shadowcolor=black@0.7:shadowx={0}:shadowy={0}", (layout.size / 30).max(3)) } else { String::new() };
        chain.push(format!(
            "drawtext=fontfile=font.ttf:textfile=thumb.txt:fontsize={}:fontcolor={}:borderw={border}:bordercolor={}{shadow}:line_spacing=-6:x='max({m:.0},min(w-{m:.0}-text_w,{cx:.1}-text_w/2))':y='max({m:.0},min(h-{m:.0}-text_h,{cy:.1}-text_h/2))'",
            layout.size,
            hex(style.fill),
            hex(style.outline),
        ));
    }
    run_chain(tools, candidate, &chain, workdir)
}

/// The filters that cut the video's picture to exactly `w` x `h`. A tall cut is taken at `x` across a wide picture.
fn cover_chain(probe: &media::Probe, w: u32, h: u32, tall: bool, x: f32) -> Vec<String> {
    let mut chain = media::base_filters(probe);
    if tall {
        chain.push(format!("scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,crop={w}:{h}:(iw-{w})*{:.3}:(ih-{h})/2", x.clamp(0.0, 1.0)));
    } else {
        chain.push(format!("scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,crop={w}:{h}"));
    }
    chain
}

/// Runs the filters on the candidate's frame and returns a JPEG under YouTube's 2 MB limit.
fn run_chain(tools: &Tools, candidate: &Candidate, chain: &[String], workdir: &Path) -> Result<Vec<u8>, String> {
    let output = workdir.join("thumb.jpg");
    // JPEG is full range and brightness is judged by the same matrix everywhere, so no video colour tags here.
    let mut quality = 2;
    loop {
        let result = media::hidden(&tools.ffmpeg)
            .current_dir(workdir)
            .args(["-y", "-v", "error", "-ss", &format!("{:.3}", candidate.t.max(0.0)), "-i"])
            .arg(&candidate.clip)
            .args(["-frames:v", "1", "-vf", &chain.join(","), "-pix_fmt", "yuvj420p", "-q:v", &quality.to_string()])
            .arg(&output)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("Couldn't start the video tools: {e}"))?;
        if !result.status.success() {
            return Err(String::from_utf8_lossy(&result.stderr).lines().last().unwrap_or("The thumbnail couldn't be made.").to_string());
        }
        let bytes = std::fs::read(&output).map_err(|e| e.to_string())?;
        // YouTube refuses thumbnails over 2 MB.
        if bytes.len() <= 1_900_000 || quality >= 10 {
            return Ok(bytes);
        }
        quality += 3;
    }
}

/// The video's own frame at the size the AI is asked to design at, in the video's colour, with nothing
/// added: this is the picture sent to OpenAI. A tall one is cut at `x` across the wide picture.
pub fn base_frame(tools: &Tools, candidate: &Candidate, shape: ThumbShape, x: f32, workdir: &Path) -> Result<Vec<u8>, String> {
    std::fs::create_dir_all(workdir).map_err(|e| e.to_string())?;
    let probe = media::probe(tools, &candidate.clip)?;
    Lut::build(LUT_SIZE, &candidate.grade, candidate.learned.as_ref()).write_cube(&workdir.join("thumb.cube")).map_err(|e| e.to_string())?;
    let (w, h) = crate::thumb_ai::ai_size(shape);
    let mut chain = cover_chain(&probe, w, h, shape == ThumbShape::Tall, x);
    chain.push("format=gbrp,lut3d=file=thumb.cube:interp=tetrahedral".into());
    chain.push("format=yuv444p".into());
    run_chain(tools, candidate, &chain, workdir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::tests::{make_video, scratch};

    fn seg(at: f64, note: &str) -> Segment {
        Segment { at, note: note.into() }
    }

    fn ai(titles: &[&str]) -> AiPack {
        AiPack {
            titles: titles.iter().map(|t| t.to_string()).collect(),
            intro: "A wild session.".into(),
            chapter_titles: vec![(0, "The start".into()), (2, "Big clutch".into())],
            hashtags: vec!["#Gaming".into(), "clutch play".into(), "GAMING".into(), "".into()],
            thumb_candidate: 9,
            thumb_text: "insane clutch!!! wow".into(),
            reason: "clear action".into(),
            primary_keyword: "Valorant clutch".into(),
            secondary_keywords: vec!["1v5".into(), "ace".into(), "comeback".into(), "extra".into()],
            tags: vec!["valorant".into(), "valorent".into(), "Valorant".into()],
        }
    }

    fn candidate(path: &Path, t: f64) -> Candidate {
        Candidate { clip: path.to_path_buf(), t, label: "x".into(), grade: Grade::default(), learned: None }
    }

    #[test]
    fn chapters_follow_youtubes_rules() {
        let titles: HashMap<usize, String> = [(0, "Intro".to_string()), (2, "Clutch".to_string())].into();
        // Starts at 0:00, gaps under ten seconds are merged, and at least three remain.
        let segments = [seg(3.0, "warm up"), seg(8.0, "too close"), seg(14.0, "the clutch"), seg(30.0, "after"), seg(41.0, "end")];
        let chapters = build_chapters(&segments, &titles);
        let times: Vec<f64> = chapters.iter().map(|c| c.at).collect();
        assert_eq!(times, vec![0.0, 14.0, 30.0, 41.0], "{chapters:?}");
        assert_eq!(chapters[1].title, "Clutch");
        assert_eq!(chapters[2].title, "after", "falls back to the moment's own note");
        // Too short to make three chapters: none at all rather than an invalid list.
        assert!(build_chapters(&[seg(0.0, "a"), seg(5.0, "b"), seg(9.0, "c")], &HashMap::new()).is_empty());
        // No more than fifteen.
        let many: Vec<Segment> = (0..40).map(|i| seg(i as f64 * 12.0, "part")).collect();
        assert_eq!(build_chapters(&many, &HashMap::new()).len(), MAX_CHAPTERS);
    }

    #[test]
    fn titles_hashtags_and_thumbnail_text_are_cleaned() {
        let titles = clean_titles(&["  \"Title: I Won 1v5!\"  ".into(), "i won 1v5!".into(), "".into(), "B".into(), "C".into(), "D".into(), "x".repeat(300)]);
        assert_eq!(titles, vec!["I Won 1v5!".to_string(), "B".into(), "C".into()], "unique, tidy and at most three");
        assert!(clean_titles(&["y".repeat(300)])[0].chars().count() <= MAX_TITLE);
        assert_eq!(clean_hashtags(&["#Gaming".into(), "clutch play".into(), "GAMING".into(), "!!".into(), "a".into(), "b".into(), "c".into(), "d".into()]), vec!["Gaming", "clutchplay", "a", "b", "c"]);
        assert_eq!(clean_thumb_text("insane clutch!!! wow, really"), "INSANE CLUTCH!!! WOW");
        assert_eq!(wrap_thumb_text("INSANE CLUTCH"), "INSANE\nCLUTCH");
        assert_eq!(wrap_thumb_text("WOW"), "WOW");
        assert!(font_size_for("INSANE\nCLUTCH", ThumbShape::Wide) > font_size_for("A VERY LONG LINE OF TEXT HERE", ThumbShape::Wide));
    }

    #[test]
    fn the_description_is_ready_to_paste() {
        let segments = [seg(0.0, "start"), seg(20.0, "middle"), seg(3700.0, "late")];
        let pack = Pack::assemble(&ai(&["One", "Two"]), &segments, vec![candidate(Path::new("a.mp4"), 1.0)], "\"Song\" by Ann (CC BY)".into(), PackFormat::Standard).unwrap();
        let text = pack.description();
        assert!(text.starts_with("A wild session."), "{text}");
        assert!(text.contains("Chapters:\n0:00 The start\n0:20 middle\n1:01:40 Big clutch"), "{text}");
        assert!(text.contains("Music: \"Song\" by Ann (CC BY)") && text.trim_end().ends_with("#Gaming #clutchplay"), "{text}");
        assert_eq!((pack.thumb_pick, pack.thumb_text.as_str(), pack.titles.len()), (0, "INSANE CLUTCH!!! WOW", 2), "an out-of-range frame choice is brought back in range");
        assert!(Pack::assemble(&ai(&[]), &segments, vec![candidate(Path::new("a.mp4"), 1.0)], String::new(), PackFormat::Standard).is_err());
        assert!(Pack::assemble(&ai(&["One"]), &segments, vec![], String::new(), PackFormat::Standard).is_err());
        assert!(pack.as_text().contains("1. One") && pack.as_text().contains("2. Two"));
    }

    /// The thumbnail is a real 1280x720 JPEG under 2 MB, in the video's colour, and the text is on it.
    #[test]
    fn the_thumbnail_is_a_correct_picture_with_text_on_it() {
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("thumbnail");
        let clip = dir.join("clip.mp4");
        make_video(&tools, &clip, "1920x1080", 6);
        let plain = render_thumbnail(&tools, &candidate(&clip, 2.0), "", &dir.join("a"), ThumbShape::Wide, 0.5).unwrap();
        let texted = render_thumbnail(&tools, &candidate(&clip, 2.0), "insane clutch", &dir.join("b"), ThumbShape::Wide, 0.5).unwrap();
        let warm = render_thumbnail(&tools, &Candidate { grade: Grade { temperature: 0.8, ..Grade::default() }, ..candidate(&clip, 2.0) }, "insane clutch", &dir.join("c"), ThumbShape::Wide, 0.5).unwrap();
        for bytes in [&plain, &texted, &warm] {
            let picture = image::load_from_memory(bytes).expect("a valid JPEG");
            assert_eq!((picture.width(), picture.height()), (1280, 720));
            assert!(bytes.len() < 2_000_000);
        }
        let rgb = |b: &[u8]| image::load_from_memory(b).unwrap().to_rgb8();
        // Yellow letters sit on the lower part of the picture, so the text really is drawn.
        let (a, b) = (rgb(&plain), rgb(&texted));
        let lower = |img: &image::RgbImage| img.enumerate_pixels().filter(|(_, y, _)| *y > 450).map(|(_, _, p)| p[0] as u64 + p[1] as u64).sum::<u64>();
        assert!(bold_font().is_none() || lower(&b) != lower(&a), "the text made no difference");
        // The video's colour grade is applied: a warm grade is redder.
        let reds = |img: &image::RgbImage| img.pixels().map(|p| p[0] as i64 - p[2] as i64).sum::<i64>();
        assert!(reds(&rgb(&warm)) > reds(&b), "the warm grade did not reach the thumbnail");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_shorts_thumbnail_is_tall_cut_where_asked_and_has_text_in_the_middle_band() {
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("thumbnail-tall");
        let clip = dir.join("clip.mp4");
        make_video(&tools, &clip, "1920x1080", 6);
        let at = |x: f32, text: &str, name: &str| render_thumbnail(&tools, &candidate(&clip, 2.0), text, &dir.join(name), ThumbShape::Tall, x).unwrap();
        let (left, right, plain, texted) = (at(0.0, "", "l"), at(1.0, "", "r"), at(0.5, "", "p"), at(0.5, "insane clutch", "t"));
        let rgb = |b: &[u8]| image::load_from_memory(b).unwrap().to_rgb8();
        for bytes in [&left, &right, &texted] {
            let picture = image::load_from_memory(bytes).expect("a valid JPEG");
            assert_eq!((picture.width(), picture.height()), (1080, 1920));
            assert!(bytes.len() < 2_000_000);
        }
        // The crop really moves across the picture.
        let sum = |img: &image::RgbImage| img.pixels().map(|p| p[0] as u64 * 3 + p[1] as u64 * 2 + p[2] as u64).sum::<u64>();
        assert_ne!(sum(&rgb(&left)), sum(&rgb(&right)), "the crop position made no difference");
        // Yellow letters sit in the middle band (46% to 76% of the height), and nowhere near the bottom fifth.
        let band = |img: &image::RgbImage, from: u32, to: u32| img.enumerate_pixels().filter(|(_, y, _)| *y >= from && *y < to).map(|(_, _, p)| p[0] as u64 + p[1] as u64).sum::<u64>();
        let (a, b) = (rgb(&plain), rgb(&texted));
        assert!(bold_font().is_none() || band(&b, 900, 1400) != band(&a, 900, 1400), "no text in the middle band");
        assert_eq!(band(&b, 1720, 1920), band(&a, 1720, 1920), "text or shade spilled into the bottom of the Short");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_frames_sent_to_the_ai_are_the_right_size_in_the_videos_colour_with_nothing_drawn_on() {
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("base-frames");
        let clip = dir.join("clip.mp4");
        make_video(&tools, &clip, "1920x1080", 6);
        let size = |b: &[u8]| image::load_from_memory(b).map(|i| (i.width(), i.height())).unwrap();
        let rgb = |b: &[u8]| image::load_from_memory(b).unwrap().to_rgb8();
        let wide = base_frame(&tools, &candidate(&clip, 2.0), ThumbShape::Wide, 0.5, &dir.join("w")).unwrap();
        let tall = base_frame(&tools, &candidate(&clip, 2.0), ThumbShape::Tall, 0.5, &dir.join("t")).unwrap();
        assert_eq!((size(&wide), size(&tall)), ((1536, 864), (1088, 1920)));
        // The tall cut moves with x, and the video's grade is in the picture the AI gets.
        let left = base_frame(&tools, &candidate(&clip, 2.0), ThumbShape::Tall, 0.0, &dir.join("l")).unwrap();
        let right = base_frame(&tools, &candidate(&clip, 2.0), ThumbShape::Tall, 1.0, &dir.join("r")).unwrap();
        assert_ne!(rgb(&left).as_raw(), rgb(&right).as_raw(), "the cut position made no difference");
        let warm = base_frame(&tools, &Candidate { grade: Grade { temperature: 0.8, ..Grade::default() }, ..candidate(&clip, 2.0) }, ThumbShape::Wide, 0.5, &dir.join("g")).unwrap();
        let reds = |img: &image::RgbImage| img.pixels().map(|p| p[0] as i64 - p[2] as i64).sum::<i64>();
        assert!(reds(&rgb(&warm)) > reds(&rgb(&wide)), "the grade did not reach the picture sent to the AI");
        // Nothing is drawn on it: no yellow lettering band, unlike the plain thumbnail.
        let plain = render_thumbnail(&tools, &candidate(&clip, 2.0), "insane clutch", &dir.join("p"), ThumbShape::Wide, 0.5).unwrap();
        assert_ne!(rgb(&wide).as_raw()[..1000], rgb(&plain).as_raw()[..1000]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_style_of_the_words_really_changes_the_thumbnail() {
        let Some(tools) = Tools::find() else { return };
        if bold_font().is_none() {
            return;
        }
        let dir = scratch("text-style");
        let clip = dir.join("clip.mp4");
        make_video(&tools, &clip, "1920x1080", 6);
        let draw = |name: &str, shape: ThumbShape, text: &str, edit: &dyn Fn(&mut TextStyle)| {
            let mut style = TextStyle::default_for(shape);
            edit(&mut style);
            let bytes = render_styled(&tools, &candidate(&clip, 2.0), text, &dir.join(name), shape, 0.5, &style).unwrap();
            image::load_from_memory(&bytes).unwrap().to_rgb8()
        };
        let close = |p: &image::Rgb<u8>, c: [u8; 3]| (0..3).all(|i| (p[i] as i32 - c[i] as i32).abs() < 40);
        // The pixels that are the colour `c` now but weren't in the same picture without words: count, and where they are on average.
        let lettering = |img: &image::RgbImage, plain: &image::RgbImage, c: [u8; 3]| -> (u64, f64, f64) {
            let (mut n, mut sx, mut sy) = (0u64, 0f64, 0f64);
            for (x, y, p) in img.enumerate_pixels() {
                if close(p, c) && !close(plain.get_pixel(x, y), c) {
                    n += 1;
                    sx += x as f64;
                    sy += y as f64;
                }
            }
            (n, sx / n.max(1) as f64, sy / n.max(1) as f64)
        };
        let yellow = [0xFF, 0xE2, 0x00];
        let plain_wide = draw("plain", ThumbShape::Wide, "", &|_| {});
        let base = lettering(&draw("base", ThumbShape::Wide, "ace", &|_| {}), &plain_wide, yellow);
        assert!(base.0 > 500, "the default lettering should be there: {}", base.0);
        assert!(base.2 > 480.0, "the default sits in the lower part: {base:?}");
        // Position: moved to the top and to the left.
        let top_left = lettering(&draw("tl", ThumbShape::Wide, "ace", &|s| (s.x, s.y) = (0.15, 0.12)), &plain_wide, yellow);
        assert!(top_left.2 < base.2 - 250.0 && top_left.1 < base.1 - 300.0, "{top_left:?} vs {base:?}");
        // Size: smaller and larger.
        let small = lettering(&draw("small", ThumbShape::Wide, "ace", &|s| s.size = 0.5), &plain_wide, yellow).0;
        let large = lettering(&draw("large", ThumbShape::Wide, "ace", &|s| s.size = 1.4), &plain_wide, yellow).0;
        assert!(small * 2 < base.0 && large > base.0, "small {small}, base {}, large {large}", base.0);
        // Colour: the lettering follows the fill colour.
        let red = [0xFF, 0x20, 0x20];
        let recoloured = draw("red", ThumbShape::Wide, "ace", &|s| s.fill = red);
        assert!(lettering(&recoloured, &plain_wide, red).0 > 300 && lettering(&recoloured, &plain_wide, yellow).0 < base.0 / 20);
        // The outline can be switched off.
        let black = [0, 0, 0];
        let no_outline = lettering(&draw("noline", ThumbShape::Wide, "ace", &|s| (s.outline_width, s.shadow) = (0.0, false)), &plain_wide, black).0;
        let outline = lettering(&draw("line", ThumbShape::Wide, "ace", &|s| s.outline_width = 1.0), &plain_wide, black).0;
        assert!(outline > no_outline + 500, "{outline} vs {no_outline}");
        // A Short can have its words near the top, and a huge size still stays inside the picture.
        let plain_tall = draw("plain-tall", ThumbShape::Tall, "", &|_| {});
        let tall = draw("tall", ThumbShape::Tall, "insane clutch", &|s| (s.y, s.size) = (0.2, 1.8));
        let (n, cx, cy) = lettering(&tall, &plain_tall, yellow);
        assert!(n > 500 && cy < 700.0 && (300.0..780.0).contains(&cx), "{n} {cx} {cy}");
        for x in (0..24).chain(1056..1080) {
            assert!((0..1920).all(|y| !(close(tall.get_pixel(x, y), yellow) && !close(plain_tall.get_pixel(x, y), yellow))), "lettering touched the edge at column {x}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_layout_follows_the_style_and_stays_inside_the_picture() {
        let style = TextStyle::default_for(ThumbShape::Wide);
        let auto = text_layout("insane clutch", ThumbShape::Wide, &style);
        assert!(auto.wrapped.contains('\n'));
        let one = text_layout("insane clutch", ThumbShape::Wide, &TextStyle { lines: 1, ..style.clone() });
        assert!(!one.wrapped.contains('\n') && one.width > auto.width, "{} vs {}", one.width, auto.width);
        let forced = text_layout("big win", ThumbShape::Wide, &TextStyle { lines: 2, ..style.clone() });
        assert_eq!(forced.wrapped, "BIG\nWIN");
        assert_eq!(text_layout("ace", ThumbShape::Wide, &TextStyle { lines: 2, ..style.clone() }).wrapped, "ACE", "one word can't be split");
        let bigger = text_layout("ace", ThumbShape::Wide, &TextStyle { size: 1.4, ..style.clone() });
        let smaller = text_layout("ace", ThumbShape::Wide, &TextStyle { size: 0.6, ..style.clone() });
        assert!(smaller.size < bigger.size);
        // Never wider than the picture, and a wide font gets a smaller size for the same words.
        for size in [1.0, 1.8, 99.0] {
            let l = text_layout("insane clutch now", ThumbShape::Wide, &TextStyle { size, lines: 1, ..style.clone() });
            assert!(l.width <= 1280.0 * 0.93, "{} wide at size {size}", l.width);
        }
        let wide_font = text_layout("insane clutch", ThumbShape::Wide, &TextStyle { font: "Arial Black".into(), ..style.clone() });
        assert!(wide_font.size < auto.size);
        // The centre is nudged in so the block keeps its margin, whatever was asked.
        let corner = TextStyle { x: 0.0, y: 1.0, ..style.clone() };
        let l = text_layout("insane clutch", ThumbShape::Wide, &corner);
        let (cx, cy) = text_centre(&l, ThumbShape::Wide, &corner);
        assert!(cx - l.width / 2.0 >= 50.0 && cy + l.height / 2.0 <= 720.0 - 50.0, "{cx} {cy}");
        // Wild values are brought back into range.
        let wild = TextStyle { x: -3.0, y: 9.0, size: 50.0, outline_width: -1.0, shade: 4.0, lines: 9, ..style }.clamped();
        assert_eq!((wild.x, wild.y, wild.size, wild.outline_width, wild.shade, wild.lines), (0.0, 1.0, 1.8, 0.0, 1.0, 2));
    }

    #[test]
    fn a_pack_saved_before_styles_existed_still_opens_and_each_shape_keeps_its_own_look() {
        let mut p: Pack = serde_json::from_str(r#"{"titles":["A"],"title_pick":0,"intro":"","chapters":[],"hashtags":[],"thumb_text":"ACE","thumb_pick":0,"reason":"","candidates":[],"credit":""}"#).unwrap();
        assert_eq!(p.style(ThumbShape::Tall), TextStyle::default_for(ThumbShape::Tall));
        assert!(p.style(ThumbShape::Tall).y < p.style(ThumbShape::Wide).y, "a Short's words sit higher than a wide thumbnail's");
        p.set_style(ThumbShape::Wide, TextStyle { size: 1.3, ..p.style(ThumbShape::Wide) });
        let back: Pack = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.style(ThumbShape::Wide).size, 1.3);
        assert_eq!(back.style(ThumbShape::Tall).size, 1.0);
    }

    fn pack(title: &str, intro: &str) -> Pack {
        Pack {
            titles: vec![title.into()],
            intro: intro.into(),
            chapters: vec![Chapter { at: 0.0, title: "Start".into() }, Chapter { at: 20.0, title: "Fight".into() }, Chapter { at: 50.0, title: "End".into() }],
            hashtags: vec!["Valorant".into(), "clutch".into(), "fps".into()],
            tags: vec!["valorant".into(), "valorant clutch".into()],
            thumb_text: "INSANE CLUTCH".into(),
            primary_keyword: "Valorant clutch".into(),
            ..Pack::default()
        }
    }

    #[test]
    fn a_well_made_pack_passes_every_seo_check() {
        let p = pack("Valorant Clutch: Winning a 1v5 With 1 HP", "This Valorant clutch turns a lost round around in the last ten seconds. Watch how the 1v5 plays out.");
        let checks = p.seo_checks();
        assert!(checks.iter().all(|c| c.level == Level::Good), "{checks:#?}");
        assert!(checks.len() >= 7);
    }

    #[test]
    fn the_checks_catch_the_common_mistakes() {
        // A title that is too long and has no keyword; a description that never says it; too many hashtags.
        let long = "Absolutely Unbelievable Gameplay Moment That You Will Not Believe When You See What Happens Next".to_string();
        let mut p = pack(&long, "Something amazing happens here.");
        p.hashtags = (0..8).map(|i| format!("tag{i}")).collect();
        let text: Vec<String> = p.seo_checks().iter().filter(|c| c.level != Level::Good).map(|c| c.text.clone()).collect();
        let joined = text.join(" | ");
        for needle in ["may be cut off", "not in the title", "not in the first 150", "never appears", "8 hashtags is a lot"] {
            assert!(joined.contains(needle), "missing '{needle}' in: {joined}");
        }
        // Over YouTube's hard limit is a failure, not a warning.
        let mut over = pack(&"x".repeat(120), "ok");
        over.titles[0] = "x".repeat(120);
        assert!(over.seo_checks().iter().any(|c| c.level == Level::Bad && c.text.contains("limit of 100")));
        // The keyword buried late in the title, and stuffed in the description.
        let late = pack("An absolutely wild round to watch tonight with Valorant clutch", "Valorant clutch. Valorant clutch. Valorant clutch. Valorant clutch. Valorant clutch.");
        let joined: String = late.seo_checks().iter().filter(|c| c.level == Level::Warn).map(|c| c.text.clone()).collect::<Vec<_>>().join(" | ");
        assert!(joined.contains("late") && joined.contains("keyword stuffing"), "{joined}");
    }

    #[test]
    fn keyword_matching_ignores_case_and_punctuation() {
        assert_eq!(norm("Valorant: CLUTCH!"), "valorant clutch");
        let p = pack("VALORANT: Clutch (1v5)!", "valorant, clutch... a story");
        assert!(p.seo_checks().iter().any(|c| c.level == Level::Good && c.text.contains("near the start")));
    }

    #[test]
    fn a_short_gets_no_chapters_and_the_shorts_hashtag() {
        let segments = [seg(0.0, "a"), seg(15.0, "b"), seg(30.0, "c"), seg(45.0, "d")];
        let short = Pack::assemble(&ai(&["One"]), &segments, vec![candidate(Path::new("a.mp4"), 1.0)], String::new(), PackFormat::Short).unwrap();
        assert!(short.chapters.is_empty());
        assert_eq!(short.hashtags[1], "Shorts", "the topic stays first and Shorts is inside the three YouTube shows: {:?}", short.hashtags);
        assert!(short.hashtags.len() <= 5);
        assert!(!short.seo_checks().iter().any(|c| c.text.contains("chapter")), "a Short isn't checked for chapters");
        // A standard video keeps its chapters and gets no Shorts tag.
        let standard = Pack::assemble(&ai(&["One"]), &segments, vec![candidate(Path::new("a.mp4"), 1.0)], String::new(), PackFormat::Standard).unwrap();
        assert!(standard.chapters.len() >= 3 && !standard.hashtags.iter().any(|t| t == "Shorts"));
    }

    #[test]
    fn tags_are_distinct_short_and_capped() {
        let tags = clean_tags(&["#Valorant".into(), "valorant".into(), "  valorant   clutch ".into(), "".into(), "x".repeat(80)]);
        assert_eq!(tags[0], "Valorant");
        assert_eq!(tags[1], "valorant clutch");
        assert!(tags[2].chars().count() <= 40, "{tags:?}");
        let many: Vec<String> = (0..60).map(|i| format!("tag number {i}")).collect();
        let capped = clean_tags(&many);
        assert!(capped.len() <= MAX_TAGS && capped.iter().map(|t| t.chars().count() + 1).sum::<usize>() <= MAX_TAG_CHARS + 1);
    }

    #[test]
    fn the_saved_text_has_the_keyword_and_tags() {
        let p = Pack::assemble(&ai(&["Valorant Clutch In 1v5"]), &[seg(0.0, "a")], vec![candidate(Path::new("a.mp4"), 1.0)], String::new(), PackFormat::Standard).unwrap();
        let text = p.as_text();
        assert!(text.starts_with("FOCUS KEYWORD\nValorant clutch") && text.contains("TAGS (optional") && text.contains("valorant, valorent"), "{text}");
        assert_eq!(p.secondary_keywords.len(), 3, "no more than three secondary keywords");
        assert_eq!(p.tags, vec!["valorant", "valorent"], "tags are de-duplicated ignoring case");
    }
}
