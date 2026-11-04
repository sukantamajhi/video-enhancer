//! The two AI helpers. Gemini Omni redraws a short sample in a better colour
//! and the app copies that look onto the real footage. OpenAI looks at frames
//! and suggests slider values. Neither one ever touches the full-length video.

use std::path::Path;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use crate::grade::{self, Grade, LearnedLook};
use crate::media::{self, Frame, GrabOptions, Tools};
use crate::montage::{Cut, Style};

pub const GEMINI_MODEL: &str = "gemini-omni-1.1-flash";
pub const OPENAI_MODELS: [(&str, &str); 3] = [
    ("gpt-6-luna", "Luna: cheapest"),
    ("gpt-6.1-sol", "Sol: balanced"),
    ("gpt-6-astra", "Astra: most capable"),
];
/// Gemini Omni refuses uploaded clips longer than this.
pub const OMNI_MAX_SECONDS: f64 = 10.0;

const SERVICE: &str = "video-enhancer";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    Gemini,
    OpenAi,
}

impl Provider {
    fn account(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini-api-key",
            Provider::OpenAi => "openai-api-key",
        }
    }

    fn env(self) -> &'static str {
        match self {
            Provider::Gemini => "GEMINI_API_KEY",
            Provider::OpenAi => "OPENAI_API_KEY",
        }
    }
}

/// Keys live in the system's secure store (Windows Credential Manager, macOS
/// Keychain), never in a plain file. An environment variable also works.
pub fn load_key(p: Provider) -> Option<String> {
    let stored = keyring::Entry::new(SERVICE, p.account()).ok().and_then(|e| e.get_password().ok());
    stored.or_else(|| std::env::var(p.env()).ok()).map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

pub fn save_key(p: Provider, key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, p.account()).map_err(|e| e.to_string())?;
    if key.trim().is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(key.trim()).map_err(|e| format!("Couldn't save the key: {e}"))
}

fn agent(seconds: u64) -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(seconds))).build().into()
}

/// Sends a JSON request and returns the parsed reply, turning API errors into readable text.
fn post_json(url: &str, headers: &[(&str, String)], body: &Value, seconds: u64) -> Result<Value, String> {
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let mut request = agent(seconds).post(url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let mut response = request.send(&bytes[..]).map_err(|e| format!("Couldn't reach the service: {e}"))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .with_config()
        .limit(400 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("The reply was cut off: {e}"))?;
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        return if json.is_null() { Err("The service sent back something unreadable.".into()) } else { Ok(json) };
    }
    let message = json["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| text.chars().take(300).collect());
    Err(match status {
        400 | 403 if message.to_lowercase().contains("region") || message.to_lowercase().contains("location") => {
            format!("{message} (Gemini Omni can't edit uploaded videos in the EU, UK or Switzerland.)")
        }
        401 | 403 => format!("The key was refused ({status}). Check it in the AI settings. {message}"),
        429 => format!("Too many requests or no credit left ({status}). {message}"),
        _ => format!("The service said {status}: {message}"),
    })
}

pub fn encode_jpeg(frame: &Frame, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode(&frame.rgb, frame.width as u32, frame.height as u32, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

// ---------------------------------------------------------------- Gemini Omni

pub const DEFAULT_STYLE: &str = "a bright, punchy, natural YouTube look with clean whites and good skin tones";

pub fn omni_prompt(style: &str) -> String {
    format!(
        "Colour grade this footage for a YouTube video: {style}. Change ONLY colour, exposure and contrast. \
         Keep the framing, composition, people, faces, objects, text, camera movement and timing exactly as they are."
    )
}

/// Pulls the video out of the reply: inline data, or a link to download.
fn omni_video(reply: &Value, key: &str) -> Result<Vec<u8>, String> {
    let steps = reply["steps"].as_array().ok_or("Gemini Omni's reply had no result in it.")?;
    for step in steps.iter().filter(|s| s["type"] == "model_output") {
        for part in step["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "video") {
            if let Some(data) = part["data"].as_str() {
                return STANDARD.decode(data).map_err(|e| format!("The returned video was damaged: {e}"));
            }
            if let Some(uri) = part["uri"].as_str() {
                let url = if uri.contains('?') { format!("{uri}&key={key}") } else { format!("{uri}?key={key}") };
                let mut response = agent(300).get(&url).call().map_err(|e| format!("Couldn't download the graded clip: {e}"))?;
                if !response.status().is_success() {
                    return Err(format!("Couldn't download the graded clip ({}).", response.status().as_u16()));
                }
                return response.body_mut().with_config().limit(400 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string());
            }
        }
    }
    let note = reply["status"].as_str().unwrap_or("unknown");
    Err(format!("Gemini Omni didn't return a video (status: {note}). Its safety rules may have blocked this footage."))
}

/// Sends a clip of 10 seconds or less to Gemini Omni and returns the graded video file's bytes.
pub fn omni_grade(key: &str, clip: &Path, prompt: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(clip).map_err(|e| format!("Couldn't read the sample clip: {e}"))?;
    let body = json!({
        "model": GEMINI_MODEL,
        "input": [{
            "type": "user_input",
            "content": [
                { "type": "video", "mime_type": "video/mp4", "data": STANDARD.encode(&bytes) },
                { "type": "text", "text": prompt },
            ],
        }],
        "response_format": { "type": "video" },
    });
    let reply = post_json("https://generativelanguage.googleapis.com/v1beta/interactions", &[("x-goog-api-key", key.to_string())], &body, 600)?;
    omni_video(&reply, key)
}

/// Compares a sample clip with Omni's graded version and learns the colour change.
pub fn learn_from_omni(tools: &Tools, original: &Path, graded: &Path) -> Result<LearnedLook, String> {
    let options = GrabOptions { width: 480, finish: media::Finish { sharpen: 0.0, denoise: 0.0, loudness: false }, crop_wide: false };
    let (po, pg) = (media::probe(tools, original)?, media::probe(tools, graded)?);
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for fraction in [0.1, 0.3, 0.5, 0.7, 0.9] {
        // Omni's reply may be a little shorter or longer, so each side uses its own clock.
        before.extend(media::grab_frame(tools, &po, original, po.duration * fraction, &options)?.rgb);
        after.extend(media::grab_frame(tools, &pg, graded, pg.duration * fraction, &options)?.rgb);
    }
    Ok(grade::learn_look(&before, &after))
}

// --------------------------------------------------------------------- OpenAI

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub look: String,
    pub reasoning: String,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub teal_orange: f32,
    pub fade: f32,
}

impl Suggestion {
    /// Lays the suggested changes over the automatic correction.
    pub fn apply_over(&self, base: &Grade) -> Grade {
        let mut g = *base;
        g.exposure = (base.exposure + self.exposure).clamp(-1.0, 1.0);
        g.contrast = self.contrast.clamp(-0.5, 0.6);
        g.highlights = self.highlights.clamp(-1.0, 1.0);
        g.shadows = self.shadows.clamp(-1.0, 1.0);
        g.temperature = (base.temperature + self.temperature).clamp(-1.0, 1.0);
        g.tint = (base.tint + self.tint).clamp(-1.0, 1.0);
        g.saturation = self.saturation.clamp(-0.6, 0.6);
        g.vibrance = self.vibrance.clamp(0.0, 0.8);
        g.teal_orange = self.teal_orange.clamp(0.0, 1.0);
        g.fade = self.fade.clamp(0.0, 0.2);
        g
    }
}

fn suggestion_schema() -> Value {
    let number = |what: &str| json!({ "type": "number", "description": what });
    json!({
        "type": "object",
        "properties": {
            "look": { "type": "string", "description": "Name of the style, 2-4 words" },
            "reasoning": { "type": "string", "description": "One or two plain sentences on what was wrong with the footage and what you changed" },
            "exposure": number("-0.5 darker .. 0.5 brighter, in stops. Extra change on top of the automatic fix"),
            "contrast": number("-0.2 flatter .. 0.4 punchier"),
            "highlights": number("-0.4 recover bright areas .. 0.2"),
            "shadows": number("-0.2 deeper .. 0.4 lift dark areas"),
            "temperature": number("-0.3 cooler .. 0.3 warmer. Extra change on top of the automatic fix"),
            "tint": number("-0.2 magenta .. 0.2 green. Extra change on top of the automatic fix"),
            "saturation": number("-0.3 .. 0.3 overall colour strength"),
            "vibrance": number("0 .. 0.6 boosts only dull colours, protects skin"),
            "teal_orange": number("0 .. 0.6 teal shadows and warm highlights"),
            "fade": number("0 .. 0.1 lifted blacks for a film look"),
        },
        "required": ["look", "reasoning", "exposure", "contrast", "highlights", "shadows", "temperature", "tint", "saturation", "vibrance", "teal_orange", "fade"],
        "additionalProperties": false,
    })
}

/// Pulls the answer text out of a Responses API reply.
fn openai_text(reply: &Value) -> Result<String, String> {
    if let Some(error) = reply["error"]["message"].as_str() {
        return Err(error.to_string());
    }
    for item in reply["output"].as_array().into_iter().flatten().filter(|i| i["type"] == "message") {
        for part in item["content"].as_array().into_iter().flatten() {
            match part["type"].as_str() {
                Some("output_text") => {
                    if let Some(text) = part["text"].as_str() {
                        return Ok(text.to_string());
                    }
                }
                Some("refusal") => return Err(format!("The model declined: {}", part["refusal"].as_str().unwrap_or("no reason given"))),
                _ => {}
            }
        }
    }
    if reply["status"] == "incomplete" {
        return Err("The model ran out of room before it answered. Try again.".into());
    }
    Err("The model's reply had no answer in it.".into())
}

/// Shows frames from across the video to an OpenAI model and gets slider values back.
pub fn openai_suggest(key: &str, model: &str, frames: &[Frame], goal: &str) -> Result<Suggestion, String> {
    let mut content = vec![json!({
        "type": "input_text",
        "text": format!(
            "These {} pictures were taken evenly across one video, in order. They have not been colour graded yet. \
             You are a professional colourist. Goal: {goal}. Suggest the grade that makes the whole video look its best \
             without wrecking skin tones or clipping highlights. Be moderate: small numbers are fine.",
            frames.len()
        ),
    })];
    for frame in frames {
        let jpeg = encode_jpeg(frame, 82)?;
        content.push(json!({ "type": "input_image", "image_url": format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg)), "detail": "auto" }));
    }
    let body = json!({
        "model": model,
        "input": [{ "role": "user", "content": content }],
        "text": { "format": { "type": "json_schema", "name": "colour_grade", "strict": true, "schema": suggestion_schema() } },
    });
    let reply = post_json("https://api.openai.com/v1/responses", &[("authorization", format!("Bearer {key}"))], &body, 180)?;
    let parsed: Value = serde_json::from_str(&openai_text(&reply)?).map_err(|e| format!("The suggestion wasn't valid: {e}"))?;
    let n = |field: &str| parsed[field].as_f64().map(|v| v as f32).ok_or_else(|| format!("The suggestion was missing '{field}'."));
    Ok(Suggestion {
        look: parsed["look"].as_str().unwrap_or("AI look").to_string(),
        reasoning: parsed["reasoning"].as_str().unwrap_or("").to_string(),
        exposure: n("exposure")?,
        contrast: n("contrast")?,
        highlights: n("highlights")?,
        shadows: n("shadows")?,
        temperature: n("temperature")?,
        tint: n("tint")?,
        saturation: n("saturation")?,
        vibrance: n("vibrance")?,
        teal_orange: n("teal_orange")?,
        fade: n("fade")?,
    })
}

// -------------------------------------------------------------------- montage

/// What the AI is told about one clip.
pub struct ClipBrief {
    pub name: String,
    pub duration: f64,
    pub frames: Vec<(f64, Frame)>,
    /// Seconds where the clip is busiest, measured on this computer.
    pub peaks: Vec<f64>,
}

pub struct AiPlan {
    pub title: String,
    pub music_query: String,
    pub tempo: String,
    pub cuts: Vec<Cut>,
}

fn plan_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string", "description": "A short, catchy title for the montage, 2-5 words" },
            "music_query": { "type": "string", "description": "One or two plain words for the music mood or genre, for example 'upbeat' or 'cinematic travel'" },
            "tempo": { "type": "string", "enum": ["slow", "medium", "fast"] },
            "cuts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "clip": { "type": "integer", "description": "Clip number as given, starting at 0" },
                        "start": { "type": "number", "description": "Start second inside that clip" },
                        "end": { "type": "number", "description": "End second inside that clip" },
                        "score": { "type": "number", "description": "0 to 1, how good this moment is" },
                        "why": { "type": "string", "description": "A few words on why it is worth keeping" },
                    },
                    "required": ["clip", "start", "end", "score", "why"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["title", "music_query", "tempo", "cuts"],
        "additionalProperties": false,
    })
}

fn parse_plan(parsed: &Value) -> Result<AiPlan, String> {
    let cuts: Vec<Cut> = parsed["cuts"]
        .as_array()
        .ok_or("The plan had no cuts in it.")?
        .iter()
        .filter_map(|c| {
            Some(Cut {
                clip: c["clip"].as_u64()? as usize,
                start: c["start"].as_f64()?,
                end: c["end"].as_f64()?,
                score: c["score"].as_f64().unwrap_or(0.5) as f32,
                why: c["why"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();
    if cuts.is_empty() {
        return Err("The AI didn't pick any moments. Try again.".into());
    }
    Ok(AiPlan {
        title: parsed["title"].as_str().unwrap_or("").trim().to_string(),
        music_query: parsed["music_query"].as_str().unwrap_or("upbeat").trim().to_string(),
        tempo: parsed["tempo"].as_str().unwrap_or("medium").to_string(),
        cuts,
    })
}

/// Shows pictures from every clip to an OpenAI model and gets back the best moments to keep.
pub fn openai_plan(key: &str, model: &str, clips: &[ClipBrief], style: Style, target: f64, notes: &str) -> Result<AiPlan, String> {
    let (min, max) = style.cut_range();
    let how = match style {
        Style::Highlights => "a highlight reel: only the most exciting, beautiful or funny moments, mixed across all the clips",
        Style::BeatSync => "a fast beat-synced montage: short punchy moments with clear movement, building toward the best one",
        Style::Story => "a story: the clips in order, longer moments that show what happened, skipping dull stretches",
    };
    let mut content = vec![json!({
        "type": "input_text",
        "text": format!(
            "You are a video editor for YouTube. Below are {} clips, each shown as pictures taken at the listed seconds. \
             Make {how}. The finished video should be about {target:.0} seconds, so choose enough moments to add up to roughly {:.0} seconds in total. \
             Each moment must be between {min:.1} and {max:.1} seconds long and must lie inside its clip. Avoid repeating the same moment. \
             Prefer moments with clear subjects, good light and action; avoid blurry, empty or shaky stretches. {}",
            clips.len(),
            target * 1.1,
            if notes.trim().is_empty() { String::new() } else { format!("The creator wants: {}.", notes.trim()) },
        ),
    })];
    for (i, clip) in clips.iter().enumerate() {
        let times: Vec<String> = clip.frames.iter().map(|(t, _)| format!("{t:.0}")).collect();
        let peaks: Vec<String> = clip.peaks.iter().map(|t| format!("{t:.0}")).collect();
        content.push(json!({
            "type": "input_text",
            "text": format!("Clip {i}: \"{}\", {:.0} seconds long. Pictures at seconds: {}. Busiest moments (movement and sound), in seconds: {}.", clip.name, clip.duration, times.join(", "), peaks.join(", ")),
        }));
        for (_, frame) in &clip.frames {
            content.push(json!({ "type": "input_image", "image_url": format!("data:image/jpeg;base64,{}", STANDARD.encode(encode_jpeg(frame, 78)?)), "detail": "low" }));
        }
    }
    let body = json!({
        "model": model,
        "input": [{ "role": "user", "content": content }],
        "text": { "format": { "type": "json_schema", "name": "montage_plan", "strict": true, "schema": plan_schema() } },
    });
    let reply = post_json("https://api.openai.com/v1/responses", &[("authorization", format!("Bearer {key}"))], &body, 300)?;
    let parsed: Value = serde_json::from_str(&openai_text(&reply)?).map_err(|e| format!("The plan wasn't valid: {e}"))?;
    parse_plan(&parsed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn plan_schema_is_strict_and_complete() {
        let s = plan_schema();
        let item = &s["properties"]["cuts"]["items"];
        for object in [&s, item] {
            let mut props: Vec<&str> = object["properties"].as_object().unwrap().keys().map(String::as_str).collect();
            let mut required: Vec<&str> = object["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            props.sort();
            required.sort();
            assert_eq!(props, required);
            assert_eq!(object["additionalProperties"], false);
        }
    }

    #[test]
    fn plan_is_read_and_bad_cuts_are_skipped() {
        let plan = parse_plan(&json!({ "title": " Road Trip ", "music_query": "upbeat", "tempo": "fast", "cuts": [
            { "clip": 0, "start": 1.5, "end": 5.0, "score": 0.9, "why": "sunset" },
            { "clip": "x", "start": 1.0, "end": 2.0, "score": 0.1, "why": "broken" },
        ] })).unwrap();
        assert_eq!((plan.title.as_str(), plan.cuts.len(), plan.cuts[0].end), ("Road Trip", 1, 5.0));
        assert!(parse_plan(&json!({ "cuts": [] })).is_err());
    }

    use super::*;

    #[test]
    fn omni_inline_video_is_found_in_the_steps() {
        let reply = json!({
            "steps": [
                { "type": "user_input", "content": [{ "type": "text", "text": "x" }] },
                { "type": "thought", "content": [{ "text": "hm", "type": "thought" }] },
                { "type": "model_output", "content": [{ "type": "video", "mime_type": "video/mp4", "data": STANDARD.encode(b"movie") }] },
            ],
            "status": "completed",
        });
        assert_eq!(omni_video(&reply, "k").unwrap(), b"movie");
    }

    #[test]
    fn omni_reply_without_video_explains_itself() {
        let err = omni_video(&json!({ "steps": [{ "type": "model_output", "content": [{ "type": "text" }] }], "status": "failed" }), "k").unwrap_err();
        assert!(err.contains("failed"), "{err}");
    }

    #[test]
    fn openai_answer_and_refusal_are_read() {
        let ok = json!({ "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "{\"a\":1}" }] }] });
        assert_eq!(openai_text(&ok).unwrap(), "{\"a\":1}");
        let refused = json!({ "output": [{ "type": "message", "content": [{ "type": "refusal", "refusal": "no" }] }] });
        assert!(openai_text(&refused).unwrap_err().contains("declined"));
    }

    #[test]
    fn suggestion_schema_is_strict_and_complete() {
        let s = suggestion_schema();
        let props: Vec<&str> = s["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        let mut required: Vec<&str> = s["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let mut props_sorted = props.clone();
        props_sorted.sort();
        required.sort();
        assert_eq!(props_sorted, required, "strict mode needs every property listed as required");
    }

    #[test]
    fn suggestions_are_kept_in_safe_ranges() {
        let wild = Suggestion {
            look: "x".into(),
            reasoning: String::new(),
            exposure: 9.0,
            contrast: 9.0,
            highlights: -9.0,
            shadows: 9.0,
            temperature: 9.0,
            tint: -9.0,
            saturation: 9.0,
            vibrance: 9.0,
            teal_orange: 9.0,
            fade: 9.0,
        };
        let g = wild.apply_over(&Grade::default());
        assert!(g.exposure <= 1.0 && g.contrast <= 0.6 && g.highlights >= -1.0 && g.saturation <= 0.6 && g.fade <= 0.2 && g.vibrance <= 0.8);
    }
}
