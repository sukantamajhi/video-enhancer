//! Free-licensed music for the montage: search, download, and find the beat.
//!
//! Tracks come from ccMixter (a community catalogue of Creative Commons music
//! made to be reused in videos) and, as a bonus, Openverse. Only licences that
//! allow commercial use and editing are kept (CC0, public domain, CC BY,
//! CC BY-SA), so a monetised YouTube video is fine as long as the credit line
//! is used where the licence asks for it.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

use crate::media::{self, Tools};

const USER_AGENT: &str = "VideoEnhancer/0.1 (desktop app)";

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub title: String,
    pub creator: String,
    pub url: String,
    pub landing: String,
    pub license: String,
    pub license_url: String,
    pub attribution: String,
    pub seconds: f64,
    pub source: String,
    /// Instrumental tracks sit better under speech.
    pub instrumental: bool,
}

impl Track {
    pub fn label(&self) -> String {
        format!("{} by {}  ({}, {}:{:02.0})", self.title, self.creator, self.license_name(), (self.seconds / 60.0) as u32, self.seconds % 60.0)
    }

    pub fn license_name(&self) -> String {
        match self.license.as_str() {
            "cc0" => "CC0".into(),
            "pdm" => "Public domain".into(),
            other => format!("CC {}", other.to_uppercase()),
        }
    }

    /// The text to paste in the video description.
    pub fn credit(&self) -> String {
        if !self.attribution.trim().is_empty() {
            return self.attribution.trim().to_string();
        }
        format!("\"{}\" by {} ({}) is licensed under {}. {}", self.title, self.creator, self.landing, self.license_name(), self.license_url)
    }
}

/// Licences that allow use in a monetised video, with edits and mixing.
fn allowed(license: &str) -> bool {
    matches!(license.to_lowercase().as_str(), "cc0" | "pdm" | "by" | "by-sa")
}

pub fn parse_tracks(reply: &Value) -> Vec<Track> {
    reply["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let license = r["license"].as_str()?.to_lowercase();
            let url = r["url"].as_str().filter(|u| u.starts_with("https://"))?;
            allowed(&license).then(|| Track {
                title: r["title"].as_str().unwrap_or("Untitled").trim().to_string(),
                creator: r["creator"].as_str().unwrap_or("Unknown artist").trim().to_string(),
                url: url.to_string(),
                landing: r["foreign_landing_url"].as_str().unwrap_or("").to_string(),
                license_url: r["license_url"].as_str().unwrap_or("").to_string(),
                attribution: r["attribution"].as_str().unwrap_or("").to_string(),
                // Openverse reports the length in milliseconds.
                seconds: r["duration"].as_f64().unwrap_or(0.0) / 1000.0,
                source: r["source"].as_str().unwrap_or("").to_string(),
                instrumental: false,
                license,
            })
        })
        .collect()
}

fn agent(seconds: u64) -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).max_response_header_size(1024 * 1024).timeout_global(Some(Duration::from_secs(seconds))).user_agent(USER_AGENT).build().into()
}

/// Which of the allowed licences a Creative Commons link is, if any.
fn license_of(url: &str) -> Option<&'static str> {
    let url = url.to_lowercase();
    if url.contains("/licenses/by-sa/") {
        Some("by-sa")
    } else if url.contains("/licenses/by/") {
        Some("by")
    } else if url.contains("publicdomain/zero") {
        Some("cc0")
    } else if url.contains("publicdomain/mark") {
        Some("pdm")
    } else {
        None
    }
}

/// "3:24" or "1:02:03" as seconds.
fn parse_length(text: &str) -> f64 {
    text.split(':').filter_map(|p| p.trim().parse::<f64>().ok()).fold(0.0, |total, part| total * 60.0 + part)
}

pub fn parse_ccmixter(reply: &Value) -> Vec<Track> {
    reply
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let license_url = r["license_url"].as_str()?;
            let license = license_of(license_url)?;
            let file = r["files"].as_array()?.iter().find(|f| f["file_format_info"]["default-ext"] == "mp3")?;
            let url = file["download_url"].as_str().filter(|u| u.starts_with("https://"))?;
            let tags = r["upload_tags"].as_str().unwrap_or("");
            Some(Track {
                title: r["upload_name"].as_str().unwrap_or("Untitled").trim().to_string(),
                creator: r["user_real_name"].as_str().or_else(|| r["user_name"].as_str()).unwrap_or("Unknown artist").trim().to_string(),
                url: url.to_string(),
                landing: r["file_page_url"].as_str().unwrap_or("").to_string(),
                license: license.to_string(),
                license_url: license_url.to_string(),
                attribution: String::new(),
                seconds: parse_length(file["file_format_info"]["ps"].as_str().unwrap_or("")),
                source: "ccMixter".into(),
                instrumental: tags.contains(",instrumental,"),
            })
        })
        .collect()
}

/// A word the music catalogue can use as a tag: "Chill Vibes" becomes "chill_vibes".
fn as_tag(word: &str) -> String {
    word.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect::<String>().trim_matches('_').to_string()
}

fn ccmixter(query: &str) -> Result<Vec<Track>, String> {
    let agent = agent(40);
    let mut tags: Vec<String> = query.split_whitespace().map(as_tag).filter(|t| !t.is_empty()).map(|t| format!("{t},instrumental")).collect();
    // Whatever the mood, these always have plenty of songs.
    tags.extend(["instrumental", "electronic,instrumental", "ambient"].map(String::from));
    let mut tracks: Vec<Track> = Vec::new();
    let mut last_error = None;
    for tag in tags.iter().take(7) {
        // A small community server: go gently, and try again if it drops a request.
        let mut reply_text = None;
        for attempt in 0..3u64 {
            std::thread::sleep(Duration::from_millis(400 + 900 * attempt));
            match agent.get("https://ccmixter.org/api/query").query("f", "json").query("tags", tag).query("sort", "rank").query("limit", "15").call() {
                Ok(mut response) if response.status().as_u16() == 200 => match response.body_mut().read_to_string() {
                    // The server sometimes answers a busy moment with nothing at all.
                    Ok(text) if text.trim().is_empty() => last_error = Some("ccMixter sent an empty reply.".to_string()),
                    Ok(text) => {
                        reply_text = Some(text);
                        break;
                    }
                    Err(e) => last_error = Some(format!("ccMixter's reply was cut off: {e}")),
                },
                Ok(response) => last_error = Some(format!("ccMixter said {}.", response.status().as_u16())),
                Err(e) => last_error = Some(format!("ccMixter couldn't be reached: {e}")),
            }
        }
        let Some(text) = reply_text else { continue };
        let reply: Value = serde_json::from_str(&text).map_err(|e| format!("ccMixter sent back something unreadable: {e}"))?;
        for track in parse_ccmixter(&reply) {
            if tracks.iter().all(|t| t.url != track.url) {
                tracks.push(track);
            }
        }
        if tracks.len() >= 12 {
            break;
        }
    }
    if tracks.is_empty() { Err(last_error.unwrap_or_else(|| "ccMixter had nothing for that mood.".into())) } else { Ok(tracks) }
}

/// One polite request. Openverse sometimes turns away automated callers, so
/// this is a bonus and its failure never stops the search.
fn openverse(query: &str) -> Result<Vec<Track>, String> {
    let mut response = agent(40)
        .get("https://api.openverse.org/v1/audio/")
        .query("q", query)
        .query("category", "music")
        .query("license_type", "commercial,modification")
        .query("page_size", "30")
        .call()
        .map_err(|e| format!("couldn't be reached: {e}"))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("turned the request away ({status})"));
    }
    let reply: Value = serde_json::from_str(&text).map_err(|e| format!("sent back something unreadable: {e}"))?;
    Ok(parse_tracks(&reply))
}

/// Looks for music matching the words. Instrumentals come first, since they
/// sit better under speech, then songs long enough not to repeat.
pub fn search(query: &str, min_seconds: f64) -> Result<Vec<Track>, String> {
    let mut problems: Vec<String> = Vec::new();
    let mut tracks = match ccmixter(query) {
        Ok(t) => t,
        Err(e) => {
            problems.push(e);
            Vec::new()
        }
    };
    if tracks.len() < 8 {
        match openverse(query) {
            Ok(more) => tracks.extend(more.into_iter().filter(|m| tracks.iter().all(|t| t.url != m.url)).collect::<Vec<_>>()),
            Err(e) => problems.push(format!("Openverse {e}.")),
        }
    }
    tracks.sort_by_key(|t| (u8::from(t.seconds < min_seconds.min(90.0)), u8::from(!t.instrumental)));
    if tracks.is_empty() {
        return Err(format!("No freely usable music was found. {} Try another mood, or choose your own music file.", problems.join(" ")));
    }
    Ok(tracks)
}

/// Downloads a track into `dir` and returns where it was saved.
pub fn download(track: &Track, dir: &Path) -> Result<PathBuf, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_response_header_size(1024 * 1024).timeout_global(Some(Duration::from_secs(180))).user_agent(USER_AGENT).build().into();
    let mut response = agent.get(&track.url).call().map_err(|e| format!("Couldn't download \"{}\": {e}", track.title))?;
    let status = response.status().as_u16();
    if status != 200 {
        // Show what the file server itself said, so the reason isn't a mystery.
        let said: String = response.body_mut().with_config().limit(4096).read_to_string().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect();
        return Err(format!("\"{}\" was refused by {} (HTTP {status}){}.", track.title, track.url.split('/').nth(2).unwrap_or("the music server"), if said.is_empty() { String::new() } else { format!(": {said}") }));
    }
    let bytes = response.body_mut().with_config().limit(80 * 1024 * 1024).read_to_vec().map_err(|e| format!("The song download stopped: {e}"))?;
    if bytes.len() < 10_000 {
        return Err(format!("\"{}\" didn't download properly.", track.title));
    }
    let path = dir.join("song.mp3");
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The text file saved next to the montage.
pub fn credits_text(track: &Track) -> String {
    format!(
        "Music credit. Paste this into your video description:\n\n{}\n\nTrack: {}\nArtist: {}\nLicence: {} {}\nSource: {}\n",
        track.credit(),
        track.title,
        track.creator,
        track.license_name(),
        track.license_url,
        track.landing
    )
}

// ------------------------------------------------------------------- the beat

#[derive(Clone, Copy, Debug)]
pub struct Beats {
    pub bpm: f64,
    pub interval: f64,
    /// Seconds into the song where the first strong beat falls.
    pub first: f64,
}

const RATE: usize = 11025;
const HOP: usize = 220;

pub fn detect_beats(tools: &Tools, song: &Path) -> Result<Beats, String> {
    let output = media::hidden(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(song)
        .args(["-vn", "-t", "240", "-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    let samples: Vec<f32> = output.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    if samples.len() < RATE * 4 {
        return Err("The music is too short to find a beat in.".into());
    }
    Ok(estimate_beats(&samples))
}

/// Finds the tempo and where the beats fall, from the sound's pulses.
pub fn estimate_beats(samples: &[f32]) -> Beats {
    let hop = HOP as f64 / RATE as f64;
    let energy: Vec<f32> = samples.chunks(HOP).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt()).collect();
    let onset: Vec<f32> = energy.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect();
    // An onset between window i and i+1 happens at the start of window i+1.
    let at = |t: f64| ((t / hop).round() as usize).checked_sub(1).and_then(|i| onset.get(i)).copied().unwrap_or(0.0);
    let length = onset.len() as f64 * hop;

    // Try every tempo from fast to slow. A strict improvement is required, so
    // on a tie the faster tempo wins (a 120 beat is not mistaken for 60).
    let (mut best, mut best_score) = (Beats { bpm: 120.0, interval: 0.5, first: 0.0 }, f32::MIN);
    let mut by_tempo: Vec<(f64, Beats, f32)> = Vec::new();
    let mut bpm = 180.0;
    while bpm >= 70.0 {
        let mut tempo_best: Option<(Beats, f32)> = None;
        let interval = 60.0 / bpm;
        let phases = (interval / hop).ceil() as usize;
        for p in 0..phases {
            let phase = p as f64 * hop;
            let count = ((length - phase) / interval) as usize;
            if count < 8 {
                continue;
            }
            let sum: f32 = (0..count).map(|k| at(phase + k as f64 * interval)).sum();
            let score = sum / count as f32;
            if tempo_best.is_none_or(|(_, s)| score > s) {
                tempo_best = Some((Beats { bpm, interval, first: phase }, score));
            }
        }
        if let Some((beats, score)) = tempo_best {
            by_tempo.push((bpm, beats, score));
            if score > best_score * 1.0005 {
                best_score = score;
                best = beats;
            }
        }
        bpm -= 0.5;
    }
    // A grid on every second beat scores almost as well as the real one, so if
    // twice the tempo is nearly as strong, it is the true tempo.
    if best.bpm * 2.0 <= 180.0
        && let Some((_, doubled, score)) = by_tempo.iter().find(|(b, _, _)| (b - best.bpm * 2.0).abs() < 0.26)
        && *score >= best_score * 0.8
    {
        return *doubled;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(license: &str, url: &str) -> Value {
        json!({ "title": "Song", "creator": "Ann", "url": url, "license": license, "license_url": "https://creativecommons.org/licenses/by/4.0/", "foreign_landing_url": "https://example.org/s", "duration": 125000, "attribution": "", "source": "x" })
    }

    #[test]
    fn only_licences_that_allow_commercial_edits_are_kept() {
        let reply = json!({ "results": [
            item("by", "https://a/1.mp3"), item("by-sa", "https://a/2.mp3"), item("cc0", "https://a/3.mp3"), item("pdm", "https://a/4.mp3"),
            item("by-nc", "https://a/5.mp3"), item("by-nd", "https://a/6.mp3"), item("by-nc-sa", "https://a/7.mp3"), item("by-nc-nd", "https://a/8.mp3"),
            item("by", "http://insecure/9.mp3"),
        ] });
        let tracks = parse_tracks(&reply);
        assert_eq!(tracks.iter().map(|t| t.license.as_str()).collect::<Vec<_>>(), ["by", "by-sa", "cc0", "pdm"]);
        assert_eq!(tracks[0].seconds, 125.0);
    }

    fn mixter_item(license_url: &str, ext: &str, tags: &str) -> Value {
        json!({
            "upload_name": "I dunno", "user_name": "grapes", "user_real_name": "Grapes Beats", "file_page_url": "https://ccmixter.org/files/grapes/16626",
            "license_url": license_url, "upload_tags": tags,
            "files": [{ "file_format_info": { "default-ext": ext, "ps": "2:45" }, "download_url": "https://ccmixter.org/content/grapes/song.mp3" }],
        })
    }

    #[test]
    fn ccmixter_tracks_keep_only_commercial_licences_and_mp3_files() {
        let reply = json!([
            mixter_item("http://creativecommons.org/licenses/by/3.0/", "mp3", ",hip_hop,instrumental,"),
            mixter_item("http://creativecommons.org/licenses/by-sa/4.0/", "mp3", ",vocals,"),
            mixter_item("http://creativecommons.org/publicdomain/zero/1.0/", "mp3", ",ambient,"),
            mixter_item("http://creativecommons.org/licenses/by-nc/3.0/", "mp3", ",instrumental,"),
            mixter_item("http://creativecommons.org/licenses/by-nc-sa/3.0/", "mp3", ",instrumental,"),
            mixter_item("http://creativecommons.org/licenses/by-nd/3.0/", "mp3", ",instrumental,"),
            mixter_item("http://creativecommons.org/licenses/sampling+/1.0/", "mp3", ",instrumental,"),
            mixter_item("http://creativecommons.org/licenses/by/3.0/", "zip", ",instrumental,"),
        ]);
        let tracks = parse_ccmixter(&reply);
        assert_eq!(tracks.iter().map(|t| t.license.as_str()).collect::<Vec<_>>(), ["by", "by-sa", "cc0"]);
        assert_eq!((tracks[0].seconds, tracks[0].instrumental, tracks[1].instrumental), (165.0, true, false));
        assert_eq!(tracks[0].creator, "Grapes Beats");
        assert!(tracks[0].credit().contains("ccmixter.org/files/grapes/16626"));
    }

    /// Talks to the real service: `cargo test live_music_search -- --ignored`.
    #[test]
    #[ignore]
    fn live_music_search_finds_usable_tracks() {
        for mood in ["upbeat energetic travel", "cinematic", "zzzz-not-a-mood"] {
            let tracks = search(mood, 60.0).unwrap_or_else(|e| panic!("{mood}: {e}"));
            assert!(tracks.len() >= 3, "{mood}: only {}", tracks.len());
            assert!(tracks.iter().all(|t| allowed(&t.license) && t.url.starts_with("https://")));
            eprintln!("{mood}: {} tracks, first: {}", tracks.len(), tracks[0].label());
        }
    }

    #[test]
    fn tags_and_lengths_are_cleaned_up() {
        assert_eq!(as_tag(" Chill Vibes! "), "chill_vibes");
        assert_eq!((parse_length("3:24"), parse_length("1:02:03"), parse_length("")), (204.0, 3723.0, 0.0));
    }

    #[test]
    fn credit_text_names_the_artist_licence_and_link() {
        let t = parse_tracks(&json!({ "results": [item("by-sa", "https://a/1.mp3")] })).remove(0);
        let text = credits_text(&t);
        for needle in ["Song", "Ann", "CC BY-SA", "https://example.org/s", "creativecommons.org"] {
            assert!(text.contains(needle), "missing {needle}: {text}");
        }
    }

    /// Writes clicks at a known tempo and checks the detector finds it.
    #[test]
    fn beat_detector_finds_a_known_tempo_and_phase() {
        for (bpm, offset) in [(120.0, 0.3), (96.0, 0.12), (140.0, 0.0)] {
            let mut samples = vec![0f32; RATE * 40];
            let interval = 60.0 / bpm;
            let mut t = offset;
            while t < 39.0 {
                let start = (t * RATE as f64) as usize;
                for i in 0..400 {
                    samples[start + i] = 0.9 * (1.0 - i as f32 / 400.0) * ((i as f32) * 0.5).sin();
                }
                t += interval;
            }
            let found = estimate_beats(&samples);
            assert!((found.bpm - bpm).abs() < 1.5, "wanted {bpm}, found {}", found.bpm);
            // Within 25 ms of a real click, which is well inside what an edit can show.
            let drift = ((found.first - offset) / interval).rem_euclid(1.0) * interval;
            assert!(drift < 0.025 || interval - drift < 0.025, "phase off: {} vs {offset}", found.first);
        }
    }
}
