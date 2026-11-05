//! Copyright-claim-free music for the montage: search, download, and find the beat.
//!
//! Every track comes from NCS (NoCopyrightSounds, ncs.io), the library made for
//! creators: its usage policy allows its music in monetised YouTube and Twitch
//! videos without copyright strikes, as long as the artist and song are credited
//! in the description. The app uses the same public pages and download links the
//! website itself uses, picks the track you choose, and saves NCS's own credit
//! text next to the montage. Nothing is taken from YouTube.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::media::{self, Tools};

const USER_AGENT: &str = "VideoEnhancer/0.1 (desktop app)";
const SITE: &str = "https://ncs.io";
const POLICY: &str = "https://ncs.io/usage-policy";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub title: String,
    pub creator: String,
    /// The download link on ncs.io.
    pub url: String,
    /// The track's page on ncs.io.
    pub landing: String,
    /// Always "ncs".
    pub license: String,
    pub license_url: String,
    /// NCS's own credit text, filled in when the track is downloaded.
    pub attribution: String,
    /// Unknown until the file is downloaded, so 0 here.
    pub seconds: f64,
    /// The genre NCS files it under.
    pub source: String,
    /// Instrumental versions sit better under the game's own sound.
    pub instrumental: bool,
}

impl Track {
    #[allow(dead_code)]
    pub fn label(&self) -> String {
        format!("{} by {}  ({})", self.title, self.creator, self.source)
    }

    pub fn license_name(&self) -> String {
        "NCS, free for YouTube and Twitch with credit".into()
    }

    /// The text to paste in the video description.
    pub fn credit(&self) -> String {
        if !self.attribution.trim().is_empty() {
            return self.attribution.trim().to_string();
        }
        format!("Song: {} - {}\nMusic provided by NoCopyrightSounds\nFree Download/Stream: {}", self.creator, self.title, self.landing)
    }

    /// The id NCS gives the track, which is the end of its download link.
    fn id(&self) -> &str {
        let last = self.url.rsplit('/').next().unwrap_or("");
        last.strip_prefix("i_").unwrap_or(last)
    }
}

fn agent(seconds: u64) -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).max_response_header_size(1024 * 1024).timeout_global(Some(Duration::from_secs(seconds))).user_agent(USER_AGENT).build().into()
}

fn get_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut response = agent.get(url).call().map_err(|e| format!("NCS couldn't be reached: {e}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("NCS said {status}."));
    }
    let bytes = response.body_mut().with_config().limit(4 * 1024 * 1024).read_to_vec().map_err(|e| format!("NCS's reply was cut off: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// ------------------------------------------------------------------ reading pages

fn decode(text: &str) -> String {
    text.replace("&amp;", "&").replace("&quot;", "\"").replace("&#039;", "'").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").trim().to_string()
}

/// The value of `name="..."` inside one HTML tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(decode(&tag[start..end]))
}

/// Where the tag that starts `text` ends. A quoted value may itself contain `>` (the artist link does).
fn tag_close(text: &str) -> Option<usize> {
    let mut quoted = false;
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '>' if !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

/// The tracks on a library page. Each row has a play link carrying the details, and a link to the track's page.
pub fn parse_library(html: &str, instrumental: bool) -> Vec<Track> {
    let mut tracks = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("class=\"player-play\"") {
        let tag_start = rest[..at].rfind('<').unwrap_or(at);
        let after = &rest[at..];
        let Some(tag_end) = tag_close(after) else { break };
        let tag = &rest[tag_start..at + tag_end];
        let row_end = after.find("</tr>").unwrap_or(after.len());
        let row = &after[tag_end..row_end];
        let slug = row.find("href=\"/").map(|i| &row[i + 7..]).and_then(|s| s.split('"').next()).filter(|s| !s.is_empty() && !s.starts_with("artist/"));
        if let (Some(tid), Some(title), Some(slug)) = (attr(tag, "data-tid"), attr(tag, "data-track"), slug) {
            let versions = attr(tag, "data-versions").unwrap_or_default();
            let has_instrumental = versions.contains("Instrumental");
            let use_instrumental = instrumental && has_instrumental;
            tracks.push(Track {
                title,
                creator: attr(tag, "data-artistraw").unwrap_or_else(|| "Unknown artist".into()),
                url: format!("{SITE}/track/download/{}{tid}", if use_instrumental { "i_" } else { "" }),
                landing: format!("{SITE}/{slug}"),
                license: "ncs".into(),
                license_url: POLICY.into(),
                attribution: String::new(),
                seconds: 0.0,
                source: attr(tag, "data-genre").unwrap_or_default(),
                instrumental: use_instrumental,
            });
        }
        rest = &after[row_end..];
    }
    tracks
}

/// The genre and mood choices the library offers: (name, id) pairs.
pub fn parse_choices(html: &str, field: &str) -> Vec<(String, String)> {
    let Some(start) = html.find(&format!("name=\"{field}\"")) else { return Vec::new() };
    let block = &html[start..];
    let block = &block[..block.find("</select>").unwrap_or(block.len())];
    block
        .split("<option value=\"")
        .skip(1)
        .filter_map(|o| {
            let (id, rest) = o.split_once('"')?;
            let name = rest.split_once('>')?.1.split('<').next()?;
            (!id.is_empty()).then(|| (decode(name), id.to_string()))
        })
        .collect()
}

/// Mood words the AI or you might use, and the NCS moods they mean.
const MOODS: &[(&[&str], &[&str])] = &[
    (&["upbeat", "energetic", "energy", "fast", "hype", "action", "intense", "driving", "pumped", "high"], &["energetic", "exciting"]),
    (&["exciting", "fun", "party", "dance"], &["exciting", "happy"]),
    (&["epic", "cinematic", "heroic", "triumphant", "dramatic"], &["epic", "majestic", "dramatic"]),
    (&["powerful", "heavy", "aggressive", "angry"], &["powerful", "heavy", "angry"]),
    (&["happy", "cheerful", "bright", "playful", "positive", "uplifting", "travel"], &["happy", "positive", "hopeful"]),
    (&["chill", "relaxed", "calm", "mellow", "lofi", "laid", "peaceful", "ambient"], &["relaxed", "peaceful", "laid back"]),
    (&["dark", "moody", "mysterious", "tense", "scary", "suspense"], &["dark", "mysterious", "tense"]),
    (&["funny", "comedy", "goofy", "quirky", "silly"], &["funny", "quirky"]),
    (&["sad", "emotional", "melancholy", "sentimental"], &["sad", "sentimental"]),
    (&["dreamy", "atmospheric", "floating"], &["dreamy", "floating"]),
];

/// What to look for on the library pages, given a mood description: (genre ids, mood ids) pairs, best first.
pub fn plan_searches(query: &str, genres: &[(String, String)], moods: &[(String, String)]) -> Vec<(Option<String>, Option<String>)> {
    let q = format!(" {} ", query.to_lowercase().replace('&', " and ").replace(['-', ',', '.'], " "));
    let words: Vec<&str> = q.split_whitespace().collect();
    let ids_of = |list: &[(String, String)], names: &[&str]| -> Vec<String> {
        list.iter().filter(|(n, _)| names.iter().any(|w| n.eq_ignore_ascii_case(w))).map(|(_, id)| id.clone()).collect()
    };
    let mut mood_ids: Vec<String> = Vec::new();
    for (triggers, names) in MOODS {
        if triggers.iter().any(|t| words.contains(t)) {
            mood_ids.extend(ids_of(moods, names));
        }
    }
    let mut genre_ids: Vec<String> = Vec::new();
    for (name, id) in genres {
        let n = name.to_lowercase().replace('&', "and").replace('-', " ");
        let alias = match n.as_str() {
            "drum and bass" => q.contains(" dnb ") || q.contains(" drum and bass "),
            "hip hop" | "alternative hip hop" => false,
            _ => q.contains(&format!(" {n} ")),
        };
        if alias {
            genre_ids.push(id.clone());
        }
    }
    mood_ids.dedup();
    if mood_ids.is_empty() && genre_ids.is_empty() {
        // Gameplay is rarely quiet: energetic is the safe default.
        mood_ids = ids_of(moods, &["energetic", "exciting"]);
    }
    let mut plan = Vec::new();
    for g in genre_ids.iter().take(2) {
        for m in mood_ids.iter().take(2) {
            plan.push((Some(g.clone()), Some(m.clone())));
        }
        plan.push((Some(g.clone()), None));
    }
    for m in mood_ids.iter().take(3) {
        plan.push((None, Some(m.clone())));
    }
    plan.push((None, None));
    plan.dedup();
    plan
}

/// The library page for a genre and mood. Instrumental versions are asked for first.
fn library_url(genre: &Option<String>, mood: &Option<String>, instrumental: bool) -> String {
    format!("{SITE}/music-search?q=&genre={}&mood={}&version={}", genre.as_deref().unwrap_or(""), mood.as_deref().unwrap_or(""), if instrumental { "instrumental" } else { "" })
}

/// Looks for NCS music matching the words. Instrumentals come first.
pub fn search(query: &str, _min_seconds: f64) -> Result<Vec<Track>, String> {
    let agent = agent(40);
    let front = get_text(&agent, &format!("{SITE}/music"))?;
    let (genres, moods) = (parse_choices(&front, "genre"), parse_choices(&front, "mood"));
    let plan = plan_searches(query, &genres, &moods);
    let mut tracks: Vec<Track> = Vec::new();
    let mut last_error = None;
    'outer: for instrumental in [true, false] {
        for (genre, mood) in plan.iter().take(5) {
            std::thread::sleep(Duration::from_millis(350));
            match get_text(&agent, &library_url(genre, mood, instrumental)) {
                Ok(html) => {
                    for t in parse_library(&html, instrumental) {
                        if tracks.iter().all(|x| x.id() != t.id()) {
                            tracks.push(t);
                        }
                    }
                }
                Err(e) => last_error = Some(e),
            }
            if tracks.len() >= 14 {
                break 'outer;
            }
        }
        if tracks.len() >= 8 {
            break;
        }
    }
    tracks.sort_by_key(|t| !t.instrumental);
    tracks.truncate(16);
    if tracks.is_empty() {
        return Err(format!("No NCS music was found. {} Try another mood, or choose your own music file.", last_error.unwrap_or_default()));
    }
    Ok(tracks)
}

/// NCS's own credit text for a track, from the same panel its website shows on Download.
pub fn parse_credit(info_html: &str) -> Option<String> {
    let start = info_html.find("id=\"panel-copy\">")? + "id=\"panel-copy\">".len();
    let block = &info_html[start..info_html[start..].find("</p>")? + start];
    let lines: Vec<String> = block
        .replace("<br />", "\n")
        .replace("<br/>", "\n")
        .replace("<br>", "\n")
        .lines()
        .map(|l| decode(l).replace(['<', '>'], "").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Downloads a track into `dir`. Returns where it was saved, and the track with NCS's credit text filled in.
pub fn download(track: &Track, dir: &Path) -> Result<(PathBuf, Track), String> {
    let agent = agent(180);
    let mut response = agent.get(&track.url).call().map_err(|e| format!("Couldn't download \"{}\": {e}", track.title))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("NCS didn't give \"{}\" (HTTP {status}).", track.title));
    }
    let bytes = response.body_mut().with_config().limit(120 * 1024 * 1024).read_to_vec().map_err(|e| format!("The song download stopped: {e}"))?;
    if bytes.len() < 10_000 {
        return Err(format!("\"{}\" didn't download properly.", track.title));
    }
    let path = dir.join("song.mp3");
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let mut used = track.clone();
    if let Ok(info) = get_text(&agent, &format!("{SITE}/track/info/{}", track.id())) {
        if let Some(credit) = parse_credit(&info) {
            used.attribution = credit;
        }
    }
    Ok((path, used))
}

/// The text file saved next to the montage.
pub fn credits_text(track: &Track) -> String {
    format!(
        "Music credit. Paste this into your video description (NCS asks for it):\n\n{}\n\nTrack: {}\nArtist: {}\nLicence: {}\nUsage policy: {}\nSource: {}\n",
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

    const PAGE: &str = r#"<tbody><tr><td><a class="player-play" data-url="https://s3/x.mp3" data-artist="<a href='/artist/1486/reece-rose'>Reece Rose</a>" data-artistraw="Reece Rose" data-track="Free &amp; Clear" data-cover="c.png" data-preview="30" data-tid="dc515b2d-711f-42f4-9bd0-6dc258819e2f" data-versions="Regular, Instrumental" data-genre="House"><i></i></a></td>
<td><a href="/RR_Free"><img alt=""></a></td><td><a href="/music-search?genre=10">House</a></td></tr>
<tr><td><a class="player-play" data-artistraw="Hester" data-track="KYOTO" data-tid="0e1" data-versions="Regular" data-genre="Progressive House"><i></i></a></td><td><a href="/kyoto"><img></a></td></tr></tbody>"#;

    #[test]
    fn library_rows_become_tracks_with_instrumentals_when_asked() {
        let tracks = parse_library(PAGE, true);
        assert_eq!(tracks.len(), 2);
        assert_eq!((tracks[0].title.as_str(), tracks[0].creator.as_str(), tracks[0].source.as_str()), ("Free & Clear", "Reece Rose", "House"));
        assert_eq!(tracks[0].url, "https://ncs.io/track/download/i_dc515b2d-711f-42f4-9bd0-6dc258819e2f");
        assert!(tracks[0].instrumental && tracks[0].id() == "dc515b2d-711f-42f4-9bd0-6dc258819e2f");
        assert_eq!(tracks[0].landing, "https://ncs.io/RR_Free");
        // No instrumental exists for the second, so the normal version is used.
        assert_eq!((tracks[1].url.as_str(), tracks[1].instrumental), ("https://ncs.io/track/download/0e1", false));
        let plain = parse_library(PAGE, false);
        assert!(plain[0].url.ends_with("/dc515b2d-711f-42f4-9bd0-6dc258819e2f") && !plain[0].instrumental);
    }

    fn choices() -> (Vec<(String, String)>, Vec<(String, String)>) {
        let page = r#"<select name="genre"><option value="">Search Genres</option><option value="6">EDM</option><option value="3">Drum &amp; Bass</option><option value="12">Dubstep</option></select>
<select name="mood"><option value="">Search Moods</option><option value="6">energetic</option><option value="37">exciting</option><option value="4">Epic</option><option value="17">relaxed</option></select>"#;
        (parse_choices(page, "genre"), parse_choices(page, "mood"))
    }

    #[test]
    fn the_choices_on_the_page_are_read() {
        let (genres, moods) = choices();
        assert_eq!(genres, [("EDM".to_string(), "6".to_string()), ("Drum & Bass".to_string(), "3".to_string()), ("Dubstep".to_string(), "12".to_string())]);
        assert_eq!(moods.len(), 4);
    }

    #[test]
    fn a_mood_description_picks_what_to_search() {
        let (genres, moods) = choices();
        let plan = plan_searches("upbeat gaming dubstep", &genres, &moods);
        assert_eq!(plan[0], (Some("12".into()), Some("6".into())));
        assert!(plan.contains(&(None, Some("37".into()))) && plan.last() == Some(&(None, None)));
        let calm = plan_searches("chill", &genres, &moods);
        assert_eq!(calm[0], (None, Some("17".into())));
        // Nothing recognised: energetic is the default for gameplay.
        let none = plan_searches("zzz", &genres, &moods);
        assert_eq!(none[0], (None, Some("6".into())));
        let dnb = plan_searches("dnb", &genres, &moods);
        assert_eq!(dnb[0].0, Some("3".into()));
    }

    #[test]
    fn ncss_own_credit_text_is_used_exactly() {
        let info = r#"<p id="panel-copy">Song: 2frers - EYES ON US<br />
Music provided by NoCopyrightSounds<br />
Free Download/Stream: &lt;http://ncs.io/EYESONUS&gt;<br />
Watch: http://ncs.lnk.to/EYESONUSAT/youtube</p>"#;
        let credit = parse_credit(info).unwrap();
        assert_eq!(credit, "Song: 2frers - EYES ON US\nMusic provided by NoCopyrightSounds\nFree Download/Stream: http://ncs.io/EYESONUS\nWatch: http://ncs.lnk.to/EYESONUSAT/youtube");
        let mut t = parse_library(PAGE, false).remove(0);
        // Until the real text is fetched, a correct credit is still built.
        assert!(t.credit().contains("Music provided by NoCopyrightSounds") && t.credit().contains("https://ncs.io/RR_Free"));
        t.attribution = credit.clone();
        let text = credits_text(&t);
        for needle in [credit.as_str(), "Free & Clear", "usage-policy"] {
            assert!(text.contains(needle), "missing {needle}: {text}");
        }
    }

    /// Talks to the real site and downloads one song: `cargo test live_ncs -- --ignored`.
    #[test]
    #[ignore]
    fn live_ncs_search_and_download_work() {
        for mood in ["upbeat energetic gaming", "chill", "zzzz-not-a-mood"] {
            let tracks = search(mood, 60.0).unwrap_or_else(|e| panic!("{mood}: {e}"));
            assert!(tracks.len() >= 5, "{mood}: only {}", tracks.len());
            assert!(tracks.iter().all(|t| t.url.starts_with("https://ncs.io/track/download/") && t.license == "ncs"));
            eprintln!("{mood}: {} tracks, first: {}", tracks.len(), tracks[0].label());
        }
        let tracks = search("upbeat", 60.0).unwrap();
        let dir = std::env::temp_dir().join("ve-ncs-test");
        std::fs::create_dir_all(&dir).unwrap();
        let (path, used) = download(&tracks[0], &dir).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 500_000, "only {size} bytes");
        assert!(used.attribution.contains("NoCopyrightSounds"), "{}", used.attribution);
        eprintln!("{} bytes\n{}", size, used.attribution);
        std::fs::remove_dir_all(&dir).ok();
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
