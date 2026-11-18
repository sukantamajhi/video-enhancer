//! AI-designed thumbnails. A real frame from the video, in the video's own colour,
//! goes to OpenAI's image-editing model together with the words and the game, and
//! comes back as a finished thumbnail. It is done twice: once wide (16:9) for a
//! normal video and once tall (9:16) for a Short, each designed for its own shape.
//!
//! Things this is careful about:
//! - The picture must stay true to the video. The prompt forbids inventing
//!   anything, because a misleading thumbnail is against YouTube's rules.
//! - AI image models can misspell lettering, so the result is read back by a vision
//!   model and compared with the words asked for. A wrong spelling is retried once.
//! - It costs real money, so nothing is sent without the creator's say-so, the real
//!   cost is worked out from OpenAI's own usage report, and every result is saved so
//!   the same request is never paid for twice.

use std::path::PathBuf;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use crate::ai;
use crate::pack::{PackFormat, TextStyle, ThumbShape, text_layout};
use crate::store;

pub const API: &str = "https://api.openai.com";

/// Where the requests go. In a debug build, `VE_OPENAI_BASE` points them at a local stand-in server, for testing.
pub fn api_base() -> String {
    #[cfg(debug_assertions)]
    if let Ok(base) = std::env::var("VE_OPENAI_BASE") {
        return base;
    }
    API.to_string()
}

/// Bump when the prompt changes, so saved results made with an older one aren't reused.
pub const PROMPT_VERSION: &str = "thumb-1";

pub const IMAGE_MODELS: [(&str, &str); 2] = [("gpt-image-2.5-sunburst", "Sunburst: most precise"), ("gpt-image-2.5-flare", "Flare: faster")];
pub const QUALITIES: [(&str, &str); 3] = [("low", "Draft: cheapest"), ("medium", "Good"), ("high", "Best: costs more")];

/// OpenAI's price per million tokens for these models: image in, text in, image out.
const PRICE_IMAGE_IN: f64 = 8.0;
const PRICE_TEXT_IN: f64 = 5.0;
const PRICE_IMAGE_OUT: f64 = 30.0;

/// The size asked of the AI. Both edges are multiples of 16 as the API requires.
/// Wide is exactly 16:9. Tall is 4 pixels wider than 9:16, trimmed afterwards.
pub fn ai_size(shape: ThumbShape) -> (u32, u32) {
    match shape {
        ThumbShape::Wide => (1536, 864),
        ThumbShape::Tall => (1088, 1920),
    }
}

// ------------------------------------------------------------------------ prompt

/// What the creator told us about the video.
#[derive(Clone, Debug)]
pub struct Brief {
    pub text: String,
    pub topic: String,
    pub direction: String,
    pub format: PackFormat,
    /// Where, how big and in what colours the creator wants the words.
    pub style: TextStyle,
}

fn one_line(text: &str, max: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

/// A plain name for a colour, so the instructions can say "bright yellow" as well as the hex code.
fn colour_name(c: [u8; 3]) -> &'static str {
    const NAMES: [(&str, [u8; 3]); 10] = [
        ("bright yellow", [0xFF, 0xE2, 0x00]),
        ("white", [0xFF, 0xFF, 0xFF]),
        ("orange", [0xFF, 0x6B, 0x3D]),
        ("cyan", [0x00, 0xE5, 0xFF]),
        ("lime green", [0x7C, 0xFF, 0x4F]),
        ("red", [0xFF, 0x3B, 0x30]),
        ("pink", [0xFF, 0x4F, 0xB8]),
        ("black", [0, 0, 0]),
        ("dark navy blue", [0x0B, 0x1B, 0x3A]),
        ("grey", [0x80, 0x80, 0x80]),
    ];
    let distance = |n: &[u8; 3]| (0..3).map(|i| (c[i] as i32 - n[i] as i32).pow(2)).sum::<i32>();
    NAMES.iter().min_by_key(|(_, n)| distance(n)).map_or("yellow", |(name, _)| name)
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Where the creator put the words, in plain language.
fn position_words(style: &TextStyle) -> String {
    let across = if style.x < 0.33 { "on the left" } else if style.x > 0.67 { "on the right" } else { "centred left to right" };
    let down = if style.y < 0.33 { "in the upper third" } else if style.y < 0.67 { "in the middle band" } else { "in the lower third" };
    format!("{down}, {across} (the middle of the words about {:.0}% across and {:.0}% down)", style.x * 100.0, style.y * 100.0)
}

/// The instructions that go with the frame. `misread` is what an earlier try spelled wrongly, if any.
/// The creator's own choices for the words (place, size, colours, lines) are passed on as they are.
pub fn prompt(shape: ThumbShape, brief: &Brief, misread: Option<&str>) -> String {
    let words = one_line(&brief.text, 40).to_uppercase().replace('"', "");
    let style = brief.style.clone().clamped();
    let layout = text_layout(&words, shape, &style);
    let lines = if layout.wrapped.lines().count() > 1 { "two lines" } else { "one line" };
    let share = (layout.width / shape.size().0 as f32 * 100.0).round();
    let composition = match shape {
        ThumbShape::Wide => "This is a 16:9 landscape YouTube thumbnail. Make the key moment large, off-centre on a third, and readable at the size of a small phone thumbnail.",
        ThumbShape::Tall => "This is a 9:16 portrait cover picture for a YouTube Short. Make the key moment large, and keep it readable at a small size. YouTube Shorts covers the top 10% and the bottom 25% of the picture with its title and buttons, so keep anything important out of those areas.",
    };
    let kind = match brief.format {
        PackFormat::Standard => "a normal YouTube video",
        PackFormat::Short => "a YouTube Short",
    };
    let topic = one_line(&brief.topic, 80);
    let topic_line = if topic.is_empty() { String::new() } else { format!("\nThe game or topic is: {topic}. This is background only. Do not write its name unless it is part of the lettering below.") };
    let direction = one_line(&brief.direction, 300);
    let direction_line = if direction.is_empty() { String::new() } else { format!("\nThe creator also asks: {direction}. Follow this only where it does not break the rules above.") };
    let retry_line = misread.map_or(String::new(), |found| format!("\nIMPORTANT: a previous attempt spelled the lettering as \"{found}\", which is wrong. The lettering must read exactly \"{words}\", letter for letter."));
    let outline = match style.outline_width {
        w if w < 0.05 => "no outline".to_string(),
        w if w < 0.6 => format!("a thin {} outline", colour_name(style.outline)),
        w if w <= 1.4 => format!("a thick {} outline ({})", colour_name(style.outline), hex(style.outline)),
        _ => format!("a very thick {} outline ({})", colour_name(style.outline), hex(style.outline)),
    };
    let shade = if style.shade > 0.3 { " A soft dark shade behind the words, fading out at its edges, may help them read." } else { "" };
    let shadow = if style.shadow { " with a soft drop shadow" } else { "" };
    let lettering = if words.is_empty() {
        "Do not add any text or lettering to the picture.".to_string()
    } else {
        format!(
            "Write exactly this lettering, once: \"{words}\". Copy every letter exactly as given, in capitals, on {lines}. Write no other words or letters anywhere. \
             Use heavy, clean sans-serif lettering in the style of {font}, about {share}% of the picture's width, with a {fill} fill ({fill_hex}), {outline}{shadow}, so it is easy to read on any background.{shade} \
             Keep every letter fully inside the picture with at least 4% of empty margin from each edge. These choices are the creator's, so follow them. Place the lettering {position}.",
            font = style.font,
            fill = colour_name(style.fill),
            fill_hex = hex(style.fill),
            position = position_words(&style),
        )
    };
    format!(
        "Design a finished thumbnail for {kind} from the attached picture, which is a real frame of the creator's own gameplay recording. Return one picture.\n\n\
         KEEP IT TRUE. The game, the scene, the characters, the weapons, the map and the on-screen interface in the frame must stay recognisable and honest to what really happened. \
         Do not invent people, characters, items, events, logos, brand marks, watermarks, buttons, view counts, play buttons, progress bars or any fake interface. \
         Do not change any real person's face or look. A misleading thumbnail breaks YouTube's rules, so everything in it must be something the video really shows.\n\n\
         MAKE IT BETTER. Make the key moment of the frame the clear hero of the picture: stronger and punchier colour, higher contrast, crisp detail, a subtle glow or rim light on the main subject, \
         and a gentle darkening or blur of distracting background so the subject stands out. Stay natural and true to the game's own look, not cartoonish. \
         At most one simple graphic accent, such as a soft outline or glow around the key moment, is fine; none is also fine.\n\n\
         THE LETTERING. {lettering}\n\n\
         COMPOSITION. {composition}{topic_line}{direction_line}{retry_line}"
    )
}

/// The request for the image-editing endpoint. Built without any network, so it can be checked.
pub fn edit_body(model: &str, quality: &str, shape: ThumbShape, prompt: &str, frame_jpeg: &[u8]) -> Value {
    let (w, h) = ai_size(shape);
    json!({
        "model": model,
        "images": [{ "image_url": format!("data:image/jpeg;base64,{}", STANDARD.encode(frame_jpeg)) }],
        "prompt": prompt,
        "size": format!("{w}x{h}"),
        "quality": quality,
        "n": 1,
        "output_format": "jpeg",
        "output_compression": 95,
        "moderation": "auto",
    })
}

// ------------------------------------------------------------------------ results

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub image_in: u64,
    pub text_in: u64,
    pub out: u64,
}

impl Usage {
    /// What the request cost, in US dollars, at OpenAI's published rates for these models.
    pub fn cost(self) -> f64 {
        (self.image_in as f64 * PRICE_IMAGE_IN + self.text_in as f64 * PRICE_TEXT_IN + self.out as f64 * PRICE_IMAGE_OUT) / 1_000_000.0
    }
}

/// The picture and what it cost, from the API's reply.
pub fn parse_reply(reply: &Value) -> Result<(Vec<u8>, Option<Usage>), String> {
    let b64 = reply["data"][0]["b64_json"].as_str().ok_or("OpenAI's reply had no picture in it.")?;
    let bytes = STANDARD.decode(b64).map_err(|e| format!("The returned picture was damaged: {e}"))?;
    let u = &reply["usage"];
    let usage = u["output_tokens"].as_u64().map(|out| Usage { image_in: u["input_tokens_details"]["image_tokens"].as_u64().unwrap_or(0), text_in: u["input_tokens_details"]["text_tokens"].as_u64().unwrap_or(0), out });
    Ok((bytes, usage))
}

/// Cuts and scales whatever came back to exactly the thumbnail size, and returns a JPEG under YouTube's 2 MB limit.
pub fn finish(bytes: &[u8], shape: ThumbShape) -> Result<Vec<u8>, String> {
    let picture = image::load_from_memory(bytes).map_err(|e| format!("The returned picture couldn't be read: {e}"))?.to_rgb8();
    let (tw, th) = shape.size();
    let (w, h) = picture.dimensions();
    // Cover the target, then cut the overhang evenly from both sides.
    let scale = (tw as f64 / w as f64).max(th as f64 / h as f64);
    let (sw, sh) = (((w as f64 * scale).round() as u32).max(tw), ((h as f64 * scale).round() as u32).max(th));
    let scaled = if (sw, sh) == (w, h) { picture } else { image::imageops::resize(&picture, sw, sh, image::imageops::FilterType::Lanczos3) };
    let cropped = image::imageops::crop_imm(&scaled, (sw - tw) / 2, (sh - th) / 2, tw, th).to_image();
    let mut quality = 93u8;
    loop {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode(cropped.as_raw(), tw, th, image::ExtendedColorType::Rgb8).map_err(|e| e.to_string())?;
        if out.len() <= 1_900_000 || quality <= 70 {
            return Ok(out);
        }
        quality -= 6;
    }
}

/// Turns an API error into advice the creator can act on.
pub fn explain(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("verif") {
        format!("{error}\nOpenAI asks for Organization Verification before its image models can be used. Do it once at https://platform.openai.com/settings/organization/general, then try again.")
    } else if lower.contains("moderation") || lower.contains("safety system") || lower.contains("content policy") || lower.contains("image_generation_user_error") {
        format!("{error}\nOpenAI's safety filter declined this picture (it can be strict about violent game scenes). Try another frame, or use the free Classic thumbnail.")
    } else if lower.contains("billing") || lower.contains("quota") || lower.contains("insufficient") {
        format!("{error}\nYour OpenAI account may be out of credit.")
    } else {
        error.to_string()
    }
}

// ---------------------------------------------------------------- checking the words

fn normal(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_uppercase).collect()
}

/// True when the lettering found in the picture is the lettering that was asked for.
pub fn same_words(wanted: &str, found: &str) -> bool {
    normal(wanted) == normal(found)
}

fn reading_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "words": { "type": "string", "description": "The large lettering exactly as written, letter by letter. Empty if there is none." } },
        "required": ["words"],
        "additionalProperties": false,
    })
}

/// Asks a vision model to read the big lettering back from the finished thumbnail.
pub fn read_words(base: &str, key: &str, text_model: &str, jpeg: &[u8]) -> Result<String, String> {
    let body = json!({
        "model": text_model,
        "instructions": "You read lettering in pictures. Report the large headline lettering exactly as it is written, letter by letter, in reading order, without correcting spelling and without adding anything. Ignore small interface text from the game. If there is no large lettering, return an empty string.",
        "input": [{ "role": "user", "content": [
            { "type": "input_text", "text": "What does the large lettering in this picture say?" },
            { "type": "input_image", "image_url": format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg)), "detail": "high" },
        ] }],
        "text": { "format": { "type": "json_schema", "name": "thumbnail_words", "strict": true, "schema": reading_schema() } },
    });
    let reply = ai::post_json(&format!("{base}/v1/responses"), &[("authorization", format!("Bearer {key}"))], &body, 120)?;
    let parsed: Value = serde_json::from_str(&ai::openai_text(&reply)?).map_err(|e| format!("The reading wasn't valid: {e}"))?;
    Ok(parsed["words"].as_str().unwrap_or("").to_string())
}

// -------------------------------------------------------------------- the whole job

/// One thumbnail to design.
#[derive(Clone, Debug)]
pub struct Job {
    pub base: String,
    pub key: String,
    pub model: String,
    pub quality: String,
    /// The text model that reads the lettering back.
    pub text_model: String,
    pub shape: ThumbShape,
    pub brief: Brief,
    /// The video's own frame at the AI size, as a JPEG.
    pub frame: Vec<u8>,
    pub reuse: bool,
    pub cache_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Made {
    pub jpeg: Vec<u8>,
    /// The saved-picture name, to find it again after a restart.
    pub name: String,
    /// What it cost, when OpenAI said. Nothing when a saved picture was reused.
    pub cost: Option<f64>,
    pub reused: bool,
    /// Something the creator should look at, such as lettering that still isn't right.
    pub warning: Option<String>,
}

/// The name a result is saved under: the same frame, words, wishes, model and quality always give the same name.
pub fn cache_name(job: &Job) -> String {
    let shape = if job.shape == ThumbShape::Wide { "wide" } else { "tall" };
    let facts = [PROMPT_VERSION.to_string(), job.model.clone(), job.quality.clone(), shape.to_string(), job.brief.text.clone(), job.brief.topic.clone(), job.brief.direction.clone(), job.brief.format.name().to_string(), serde_json::to_string(&job.brief.style).unwrap_or_default(), store::hash_bytes(&job.frame)];
    format!("ai-thumb-{}", store::hash(&facts))
}

struct Attempt {
    jpeg: Vec<u8>,
    cost: Option<f64>,
    found: Option<String>,
}

fn attempt(job: &Job, misread: Option<&str>) -> Result<Attempt, String> {
    let body = edit_body(&job.model, &job.quality, job.shape, &prompt(job.shape, &job.brief, misread), &job.frame);
    let reply = ai::post_json(&format!("{}/v1/images/edits", job.base), &[("authorization", format!("Bearer {}", job.key))], &body, 360).map_err(|e| explain(&e))?;
    let (raw, usage) = parse_reply(&reply)?;
    let jpeg = finish(&raw, job.shape)?;
    let wanted = job.brief.text.trim();
    let found = if wanted.is_empty() { None } else { read_words(&job.base, &job.key, &job.text_model, &jpeg).ok() };
    Ok(Attempt { jpeg, cost: usage.map(Usage::cost), found })
}

fn add(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0.0) + y.unwrap_or(0.0)),
    }
}

/// Designs one thumbnail. A saved result is used when there is one. Otherwise the AI designs it, the
/// lettering is read back and, if it is spelled wrong, one more try is made before saying so.
pub fn run(job: &Job, say: &dyn Fn(&str)) -> Result<Made, String> {
    let name = cache_name(job);
    if job.reuse
        && let Some(jpeg) = store::load_bytes_in(&job.cache_dir, &name)
    {
        return Ok(Made { jpeg, name, cost: None, reused: true, warning: None });
    }
    let label = if job.shape == ThumbShape::Wide { "16:9" } else { "9:16" };
    say(&format!("Designing the {label} thumbnail…"));
    let wanted = job.brief.text.trim().to_string();
    let first = attempt(job, None)?;
    let mut cost = first.cost;
    let mut warning = None;
    let mut best = first;
    if !wanted.is_empty() {
        match best.found.clone() {
            Some(found) if same_words(&wanted, &found) => {}
            Some(found) => {
                say(&format!("The {label} lettering came out as \"{found}\". Trying once more…"));
                let second = attempt(job, Some(&found))?;
                cost = add(cost, second.cost);
                match second.found.clone() {
                    Some(f) if same_words(&wanted, &f) => {}
                    Some(f) => warning = Some(format!("The AI wrote \"{f}\" instead of \"{wanted}\" on the {label} thumbnail, even after a second try. Edit the words, press Make with AI again, or use the free Classic thumbnail.")),
                    None => warning = Some(format!("The {label} lettering couldn't be double-checked. Look at the spelling before you use it.")),
                }
                best = second;
            }
            None => warning = Some(format!("The {label} lettering couldn't be double-checked. Look at the spelling before you use it.")),
        }
    }
    store::save_bytes_in(&job.cache_dir, &name, &best.jpeg)?;
    Ok(Made { jpeg: best.jpeg, name, cost, reused: false, warning })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn brief(text: &str) -> Brief {
        Brief { text: text.into(), topic: "Valorant".into(), direction: String::new(), format: PackFormat::Standard, style: TextStyle::default_for(ThumbShape::Wide) }
    }

    #[test]
    fn the_sizes_asked_of_the_ai_are_allowed_and_cut_to_the_right_shape() {
        for shape in [ThumbShape::Wide, ThumbShape::Tall] {
            let (w, h) = ai_size(shape);
            assert!(w % 16 == 0 && h % 16 == 0, "{w}x{h} must be multiples of 16");
            assert!((655_360..=8_294_400).contains(&(w * h)), "{w}x{h} is outside the allowed pixel count");
            assert!(h.max(w) <= 3840 && (h.max(w) as f64 / h.min(w) as f64) <= 3.0);
            let (tw, th) = shape.size();
            // Cutting down to the final size only ever trims a sliver or scales, never stretches.
            assert!(((w as f64 / h as f64) - (tw as f64 / th as f64)).abs() < 0.01);
        }
    }

    #[test]
    fn the_prompt_protects_the_truth_and_asks_for_the_exact_words() {
        let wide = prompt(ThumbShape::Wide, &brief("insane clutch"), None);
        let tall = prompt(ThumbShape::Tall, &Brief { style: TextStyle::default_for(ThumbShape::Tall), ..brief("insane clutch") }, None);
        for p in [&wide, &tall] {
            assert!(p.contains("\"INSANE CLUTCH\""), "{p}");
            assert!(p.contains("Do not invent") && p.contains("misleading thumbnail breaks YouTube's rules"));
            assert!(p.contains("Write no other words"));
            assert!(p.contains("Valorant") && p.contains("background only"));
        }
        assert!(wide.contains("16:9 landscape") && wide.contains("lower third"));
        assert!(tall.contains("9:16 portrait") && tall.contains("in the middle band") && tall.contains("bottom 25%"));
        // A short word stays on one line; a longer phrase is allowed two.
        assert!(prompt(ThumbShape::Wide, &brief("ace"), None).contains("on one line"));
        assert!(wide.contains("on two lines"));
    }

    #[test]
    fn the_creators_choices_for_the_words_reach_the_ai() {
        let mut b = brief("insane clutch");
        b.style = TextStyle { x: 0.1, y: 0.1, size: 0.6, fill: [0xFF, 0xFF, 0xFF], outline: [0x0B, 0x1B, 0x3A], outline_width: 2.0, shadow: false, shade: 0.0, font: "Arial Black".into(), lines: 1 };
        let p = prompt(ThumbShape::Wide, &b, None);
        for needle in ["in the upper third", "on the left", "white fill (#FFFFFF)", "very thick dark navy blue outline (#0B1B3A)", "Arial Black", "on one line", "about 10% across and 10% down"] {
            assert!(p.contains(needle), "missing '{needle}' in {p}");
        }
        assert!(!p.contains("drop shadow") && !p.contains("soft dark shade"), "{p}");
        // The size shown to the AI follows the size slider.
        let share = |size: f32| -> f32 {
            let mut b = brief("ace");
            b.style.size = size;
            let p = prompt(ThumbShape::Wide, &b, None);
            let at = p.find("about ").and_then(|i| p[i + 6..].split('%').next().map(str::to_string)).unwrap();
            at.parse().unwrap()
        };
        assert!(share(0.6) < share(1.0) && share(1.0) < share(1.5));
        // Changing the look makes a new request, so an old saved picture isn't reused for it.
        let job = |style: TextStyle| {
            let mut j = job("http://x", ThumbShape::Wide, "ace", "style");
            j.brief.style = style;
            cache_name(&j)
        };
        assert_ne!(job(TextStyle::default_for(ThumbShape::Wide)), job(TextStyle { x: 0.2, ..TextStyle::default_for(ThumbShape::Wide) }));
        assert_eq!(colour_name([0xFE, 0xE0, 0x05]), "bright yellow");
    }

    #[test]
    fn wishes_retries_and_missing_words_change_the_prompt() {
        let mut b = brief("ace");
        b.direction = "make   the explosion\nbigger".into();
        let p = prompt(ThumbShape::Wide, &b, Some("ACF"));
        assert!(p.contains("The creator also asks: make the explosion bigger."));
        assert!(p.contains("spelled the lettering as \"ACF\"") && p.contains("exactly \"ACE\""));
        let silent = prompt(ThumbShape::Tall, &brief("  "), None);
        assert!(silent.contains("Do not add any text"));
        // Quotes can't break out of the quoted lettering.
        assert!(prompt(ThumbShape::Wide, &brief("say \"hi\""), None).contains("\"SAY HI\""));
    }

    #[test]
    fn the_request_carries_everything_the_endpoint_needs() {
        let body = edit_body("gpt-image-2.5-sunburst", "medium", ThumbShape::Tall, "make it good", &[1, 2, 3]);
        assert_eq!((body["model"].as_str(), body["size"].as_str(), body["quality"].as_str()), (Some("gpt-image-2.5-sunburst"), Some("1088x1920"), Some("medium")));
        assert_eq!(body["images"][0]["image_url"].as_str(), Some("data:image/jpeg;base64,AQID"));
        assert_eq!((body["prompt"].as_str(), body["n"].as_u64(), body["output_format"].as_str()), (Some("make it good"), Some(1), Some("jpeg")));
        assert_eq!(edit_body("m", "low", ThumbShape::Wide, "p", &[0])["size"], "1536x864");
    }

    #[test]
    fn the_cost_comes_from_openais_own_usage_numbers() {
        let reply = json!({ "data": [{ "b64_json": STANDARD.encode(b"x") }], "usage": { "input_tokens": 1500, "input_tokens_details": { "image_tokens": 1000, "text_tokens": 500 }, "output_tokens": 4000, "total_tokens": 5500 } });
        let (bytes, usage) = parse_reply(&reply).unwrap();
        assert_eq!(bytes, b"x");
        let usage = usage.unwrap();
        assert_eq!(usage, Usage { image_in: 1000, text_in: 500, out: 4000 });
        // 1000 x $8 + 500 x $5 + 4000 x $30, per million.
        assert!((usage.cost() - 0.1305).abs() < 1e-9, "{}", usage.cost());
        assert!(parse_reply(&json!({ "data": [] })).is_err());
        assert!(parse_reply(&json!({ "data": [{ "b64_json": "!!!" }] })).is_err());
        assert_eq!(parse_reply(&json!({ "data": [{ "b64_json": "eA==" }] })).unwrap().1, None);
    }

    fn picture(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 90]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8).unwrap();
        out
    }

    #[test]
    fn whatever_comes_back_is_cut_to_the_exact_size() {
        let size = |bytes: &[u8]| image::load_from_memory(bytes).map(|i| (i.width(), i.height())).unwrap();
        assert_eq!(size(&finish(&picture(1536, 864), ThumbShape::Wide).unwrap()), (1280, 720));
        assert_eq!(size(&finish(&picture(1088, 1920), ThumbShape::Tall).unwrap()), (1080, 1920));
        // A square comes back when the model ignores the size: it is covered and trimmed, never stretched.
        assert_eq!(size(&finish(&picture(1024, 1024), ThumbShape::Wide).unwrap()), (1280, 720));
        assert_eq!(size(&finish(&picture(1024, 1024), ThumbShape::Tall).unwrap()), (1080, 1920));
        assert!(finish(b"not a picture", ThumbShape::Wide).is_err());
    }

    #[test]
    fn spelling_is_compared_by_letters_only() {
        assert!(same_words("insane clutch", "INSANE  CLUTCH!"));
        assert!(same_words("1V3", "1v3"));
        assert!(!same_words("insane clutch", "INSANE CLUCTH"));
        assert!(!same_words("ace", ""));
    }

    #[test]
    fn errors_come_with_advice() {
        assert!(explain("Your organization must be verified to use the model gpt-image-2.5.").contains("Organization Verification"));
        assert!(explain("Your request was rejected by the safety system.").contains("safety filter"));
        assert!(explain("You exceeded your current quota").contains("out of credit"));
        assert_eq!(explain("plain"), "plain");
    }

    // ---- a small local server that plays OpenAI, to check the real requests end to end

    struct Fake {
        base: String,
        seen: Arc<Mutex<Vec<(String, String, Value)>>>,
    }

    /// Answers each request in turn with the next (status, body), and records path, auth header and JSON body.
    fn fake_openai(replies: Vec<(u16, String)>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for (status, body) in replies {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 65536];
                let (head_end, length) = loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break (0, 0);
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buffer[..at]).to_lowercase();
                        let length = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                        break (at + 4, length);
                    }
                };
                while buffer.len() < head_end + length {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let auth = head.lines().find(|l| l.to_lowercase().starts_with("authorization:")).unwrap_or("").to_string();
                let json: Value = serde_json::from_slice(&buffer[head_end..]).unwrap_or(Value::Null);
                log.lock().unwrap().push((path, auth, json));
                let reply = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Fake { base, seen }
    }

    fn image_reply(w: u32, h: u32) -> (u16, String) {
        let body = json!({ "data": [{ "b64_json": STANDARD.encode(picture(w, h)) }], "usage": { "input_tokens": 1000, "input_tokens_details": { "image_tokens": 900, "text_tokens": 100 }, "output_tokens": 2000, "total_tokens": 3000 } });
        (200, body.to_string())
    }

    fn words_reply(words: &str) -> (u16, String) {
        let inner = json!({ "words": words }).to_string();
        (200, json!({ "output": [{ "type": "message", "content": [{ "type": "output_text", "text": inner }] }] }).to_string())
    }

    fn job(base: &str, shape: ThumbShape, text: &str, tag: &str) -> Job {
        let dir = std::env::temp_dir().join(format!("ve-thumbai-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        Job { base: base.into(), key: "sk-test".into(), model: "gpt-image-2.5-sunburst".into(), quality: "medium".into(), text_model: "gpt-6-luna".into(), shape, brief: brief(text), frame: picture(ai_size(shape).0, ai_size(shape).1), reuse: true, cache_dir: dir }
    }

    fn quiet(_: &str) {}

    #[test]
    fn a_good_first_try_is_sent_checked_saved_and_reused() {
        let fake = fake_openai(vec![image_reply(1536, 864), words_reply("INSANE CLUTCH")]);
        let j = job(&fake.base, ThumbShape::Wide, "insane clutch", "good");
        let made = run(&j, &quiet).unwrap();
        assert!(made.warning.is_none() && !made.reused);
        assert_eq!(image::load_from_memory(&made.jpeg).map(|i| (i.width(), i.height())).unwrap(), (1280, 720));
        // 900 x $8 + 100 x $5 + 2000 x $30, per million.
        assert!((made.cost.unwrap() - 0.0677).abs() < 1e-9, "{:?}", made.cost);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let (path, auth, body) = &seen[0];
        assert_eq!((path.as_str(), auth.to_lowercase().as_str()), ("/v1/images/edits", "authorization: bearer sk-test"));
        assert_eq!((body["model"].as_str(), body["size"].as_str(), body["quality"].as_str()), (Some("gpt-image-2.5-sunburst"), Some("1536x864"), Some("medium")));
        assert!(body["images"][0]["image_url"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
        assert!(body["prompt"].as_str().unwrap().contains("\"INSANE CLUTCH\""));
        // The second request is the reading-back, with the finished picture attached.
        assert_eq!(seen[1].0, "/v1/responses");
        assert_eq!(seen[1].2["input"][0]["content"][1]["type"], "input_image");
        drop(seen);
        // Asking again finds the saved picture and sends nothing.
        let again = run(&j, &quiet).unwrap();
        assert!(again.reused && again.cost.is_none() && again.jpeg == made.jpeg);
        assert_eq!(fake.seen.lock().unwrap().len(), 2);
        std::fs::remove_dir_all(&j.cache_dir).ok();
    }

    #[test]
    fn wrong_lettering_gets_one_more_try_that_names_the_mistake() {
        let fake = fake_openai(vec![image_reply(1088, 1920), words_reply("INSANE CLUCTH"), image_reply(1088, 1920), words_reply("INSANE CLUTCH")]);
        let j = job(&fake.base, ThumbShape::Tall, "insane clutch", "retry");
        let made = run(&j, &quiet).unwrap();
        assert!(made.warning.is_none());
        assert_eq!(image::load_from_memory(&made.jpeg).map(|i| (i.width(), i.height())).unwrap(), (1080, 1920));
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(!seen[0].2["prompt"].as_str().unwrap().contains("INSANE CLUCTH"));
        assert!(seen[2].2["prompt"].as_str().unwrap().contains("spelled the lettering as \"INSANE CLUCTH\""));
        // Both tries are paid for, and both are counted.
        assert!((made.cost.unwrap() - 2.0 * 0.0677).abs() < 1e-9);
        std::fs::remove_dir_all(&j.cache_dir).ok();
    }

    #[test]
    fn lettering_that_stays_wrong_is_kept_but_flagged() {
        let fake = fake_openai(vec![image_reply(1536, 864), words_reply("INSAEN"), image_reply(1536, 864), words_reply("INSANE CLUCH")]);
        let j = job(&fake.base, ThumbShape::Wide, "insane clutch", "wrong");
        let made = run(&j, &quiet).unwrap();
        let warning = made.warning.expect("a spelling warning");
        assert!(warning.contains("INSANE CLUCH") && warning.contains("insane clutch") && warning.contains("Classic"), "{warning}");
        assert_eq!(fake.seen.lock().unwrap().len(), 4, "only one retry");
        std::fs::remove_dir_all(&j.cache_dir).ok();
    }

    #[test]
    fn no_words_means_nothing_to_check_and_a_failed_check_is_said() {
        let fake = fake_openai(vec![image_reply(1536, 864)]);
        let j = job(&fake.base, ThumbShape::Wide, "", "silent");
        let made = run(&j, &quiet).unwrap();
        assert!(made.warning.is_none());
        assert_eq!(fake.seen.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(&j.cache_dir).ok();
        // The picture arrives but the reading-back fails: the creator is told to look at the spelling.
        let fake = fake_openai(vec![image_reply(1536, 864), (500, "{}".into())]);
        let j = job(&fake.base, ThumbShape::Wide, "ace", "unchecked");
        let made = run(&j, &quiet).unwrap();
        assert!(made.warning.unwrap().contains("couldn't be double-checked"));
        std::fs::remove_dir_all(&j.cache_dir).ok();
    }

    #[test]
    fn a_refusal_comes_back_with_advice_and_nothing_is_saved() {
        let body = json!({ "error": { "message": "Your organization must be verified to use the model `gpt-image-2.5-sunburst`.", "type": "invalid_request_error" } }).to_string();
        let fake = fake_openai(vec![(403, body)]);
        let j = job(&fake.base, ThumbShape::Wide, "ace", "refused");
        let error = run(&j, &quiet).unwrap_err();
        assert!(error.contains("Organization Verification"), "{error}");
        assert!(!j.cache_dir.exists() || std::fs::read_dir(&j.cache_dir).unwrap().count() == 0);
        std::fs::remove_dir_all(&j.cache_dir).ok();
    }

    /// Checks the request shape against the real endpoint WITHOUT any charge: the request is made invalid on purpose
    /// (a size that isn't a multiple of 16 and a made-up quality), so OpenAI refuses it before drawing anything.
    /// The refusal shows whether the body was understood. `cargo test live_endpoint -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_endpoint_understands_the_request_shape() {
        let Some(key) = ai::load_key(ai::Provider::OpenAi) else {
            eprintln!("No OpenAI key is saved, so nothing was checked.");
            return;
        };
        // Only the quality is wrong. If the model, the sizes or the body shape were not accepted, OpenAI would complain about those instead.
        for (model, _) in IMAGE_MODELS {
            for shape in [ThumbShape::Wide, ThumbShape::Tall] {
                let mut body = edit_body(model, "medium", shape, "check", &picture(64, 64));
                body["quality"] = json!("not-a-quality");
                let error = ai::post_json(&format!("{API}/v1/images/edits"), &[("authorization", format!("Bearer {key}"))], &body, 60).unwrap_err();
                eprintln!("{model} {shape:?}: {error}");
                let lower = error.to_lowercase();
                assert!(lower.contains("not-a-quality"), "something else was wrong first: {error}");
                assert!(!lower.contains("size") && !lower.contains("model") && !lower.contains("content-type") && !lower.contains("image"), "{error}");
            }
        }
    }

    #[test]
    fn the_saved_name_changes_with_anything_that_changes_the_picture() {
        let base = job("http://x", ThumbShape::Wide, "ace", "name");
        let name = cache_name(&base);
        assert!(name.starts_with("ai-thumb-"), "so Forget saved answers also removes it");
        assert_eq!(name, cache_name(&base.clone()));
        let mut changed = base.clone();
        changed.brief.text = "ACE!".into();
        assert_ne!(name, cache_name(&changed));
        let mut changed = base.clone();
        changed.quality = "high".into();
        assert_ne!(name, cache_name(&changed));
        let mut changed = base.clone();
        changed.frame = picture(1536, 864);
        changed.frame.push(0);
        assert_ne!(name, cache_name(&changed));
        let mut changed = base.clone();
        changed.shape = ThumbShape::Tall;
        assert_ne!(name, cache_name(&changed));
    }
}
