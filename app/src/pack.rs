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
    pub reason: String,
    pub candidates: Vec<Candidate>,
    /// The music credit, when the video uses music that asks for one.
    pub credit: String,
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
    /// Builds the pack from what the AI said, the real chapter moments and the frames on offer.
    pub fn assemble(ai: &AiPack, segments: &[Segment], candidates: Vec<Candidate>, credit: String) -> Result<Pack, String> {
        let titles = clean_titles(&ai.titles);
        if titles.is_empty() {
            return Err("The AI didn't write any titles. Try again.".into());
        }
        if candidates.is_empty() {
            return Err("There is no frame to make a thumbnail from.".into());
        }
        let said: HashMap<usize, String> = ai.chapter_titles.iter().cloned().collect();
        Ok(Pack {
            titles,
            title_pick: 0,
            intro: tidy(&ai.intro.replace("\n\n", "\u{1}"), 900).replace('\u{1}', "\n\n"),
            chapters: build_chapters(segments, &said),
            hashtags: clean_hashtags(&ai.hashtags),
            thumb_text: clean_thumb_text(&ai.thumb_text),
            thumb_pick: ai.thumb_candidate.min(candidates.len() - 1),
            reason: tidy(&ai.reason, 240),
            candidates,
            credit,
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
            parts.push(format!("Music: {}", self.credit.trim()));
        }
        if !self.hashtags.is_empty() {
            parts.push(self.hashtags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "));
        }
        parts.join("\n\n")
    }

    /// Everything as one text file.
    pub fn as_text(&self) -> String {
        let titles: Vec<String> = self.titles.iter().enumerate().map(|(i, t)| format!("{}. {t}", i + 1)).collect();
        format!("TITLE OPTIONS\n{}\n\nDESCRIPTION (paste this under the video)\n{}\n\nTHUMBNAIL TEXT\n{}\n", titles.join("\n"), self.description(), self.thumb_text)
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

/// The font size that fits the wrapped text across a 1280-wide picture.
pub fn thumb_font_size(wrapped: &str) -> u32 {
    let lines = wrapped.lines().count().max(1) as f64;
    let longest = wrapped.lines().map(|l| l.chars().count()).max().unwrap_or(1).max(1) as f64;
    // Impact letters are about half as wide as the font is tall.
    ((1160.0 / (0.52 * longest)).min(230.0 / lines.sqrt())).max(40.0) as u32
}

/// Draws the thumbnail: 1280x720, the chosen frame in the video's own colour
/// with extra punch, and big bold text with a dark outline. Returns the JPEG.
pub fn render_thumbnail(tools: &Tools, candidate: &Candidate, text: &str, workdir: &Path) -> Result<Vec<u8>, String> {
    std::fs::create_dir_all(workdir).map_err(|e| e.to_string())?;
    let probe = media::probe(tools, &candidate.clip)?;
    Lut::build(LUT_SIZE, &candidate.grade, candidate.learned.as_ref()).write_cube(&workdir.join("thumb.cube")).map_err(|e| e.to_string())?;
    let text = clean_thumb_text(text);
    let wrapped = wrap_thumb_text(&text);
    let font = bold_font().filter(|_| !wrapped.is_empty());

    let mut chain = media::base_filters(&probe);
    chain.push("scale=1280:720:force_original_aspect_ratio=increase:flags=lanczos,crop=1280:720".into());
    chain.push("format=gbrp,lut3d=file=thumb.cube:interp=tetrahedral".into());
    // A little more punch than the video itself, the way thumbnails need to read at small sizes.
    chain.push("format=yuv444p,eq=contrast=1.08:saturation=1.22".into());
    if let Some(font) = font {
        std::fs::copy(font, workdir.join("font.ttf")).map_err(|e| e.to_string())?;
        std::fs::write(workdir.join("thumb.txt"), &wrapped).map_err(|e| e.to_string())?;
        let size = thumb_font_size(&wrapped);
        chain.push("drawbox=x=0:y=ih*0.52:w=iw:h=ih*0.48:color=black@0.32:t=fill".into());
        chain.push(format!(
            "drawtext=fontfile=font.ttf:textfile=thumb.txt:fontsize={size}:fontcolor=0xFFE200:borderw={}:bordercolor=black:shadowcolor=black@0.7:shadowx=7:shadowy=7:line_spacing=-6:x=(w-text_w)/2:y=h-text_h-64",
            (size / 13).max(4)
        ));
    }
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
        assert!(thumb_font_size("INSANE\nCLUTCH") > thumb_font_size("A VERY LONG LINE OF TEXT HERE"));
    }

    #[test]
    fn the_description_is_ready_to_paste() {
        let segments = [seg(0.0, "start"), seg(20.0, "middle"), seg(3700.0, "late")];
        let pack = Pack::assemble(&ai(&["One", "Two"]), &segments, vec![candidate(Path::new("a.mp4"), 1.0)], "\"Song\" by Ann (CC BY)".into()).unwrap();
        let text = pack.description();
        assert!(text.starts_with("A wild session."), "{text}");
        assert!(text.contains("Chapters:\n0:00 The start\n0:20 middle\n1:01:40 Big clutch"), "{text}");
        assert!(text.contains("Music: \"Song\" by Ann (CC BY)") && text.trim_end().ends_with("#Gaming #clutchplay"), "{text}");
        assert_eq!((pack.thumb_pick, pack.thumb_text.as_str(), pack.titles.len()), (0, "INSANE CLUTCH!!! WOW", 2), "an out-of-range frame choice is brought back in range");
        assert!(Pack::assemble(&ai(&[]), &segments, vec![candidate(Path::new("a.mp4"), 1.0)], String::new()).is_err());
        assert!(Pack::assemble(&ai(&["One"]), &segments, vec![], String::new()).is_err());
        assert!(pack.as_text().contains("1. One") && pack.as_text().contains("2. Two"));
    }

    /// The thumbnail is a real 1280x720 JPEG under 2 MB, in the video's colour, and the text is on it.
    #[test]
    fn the_thumbnail_is_a_correct_picture_with_text_on_it() {
        let Some(tools) = Tools::find() else { return };
        let dir = scratch("thumbnail");
        let clip = dir.join("clip.mp4");
        make_video(&tools, &clip, "1920x1080", 6);
        let plain = render_thumbnail(&tools, &candidate(&clip, 2.0), "", &dir.join("a")).unwrap();
        let texted = render_thumbnail(&tools, &candidate(&clip, 2.0), "insane clutch", &dir.join("b")).unwrap();
        let warm = render_thumbnail(&tools, &Candidate { grade: Grade { temperature: 0.8, ..Grade::default() }, ..candidate(&clip, 2.0) }, "insane clutch", &dir.join("c")).unwrap();
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
}
