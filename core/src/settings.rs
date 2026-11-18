//! The choices that carry over from one run to the next, and the list of videos opened before.
//! Both front ends read and write the same file, so a session started in one opens in the other.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::media::EncoderChoice;
use crate::{ai, pack, store};

/// A video opened before, for the Start screen.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recent {
    pub path: PathBuf,
    pub duration: f64,
    pub height: u32,
    pub fps: f64,
}

/// Choices that carry over from one run to the next.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub last_video: Option<PathBuf>,
    pub montage: bool,
    pub openai_model: usize,
    pub style_notes: String,
    pub reuse_ai: bool,
    pub wants_wide: bool,
    pub wide_height: u32,
    pub wants_vertical: bool,
    pub wants_lut: bool,
    pub wants_thumb: bool,
    pub out_dir: Option<PathBuf>,
    pub encoder: EncoderChoice,
    pub gpu_grade: bool,
    pub pack_topic: String,
    pub pack_format: pack::PackFormat,
    pub recent: Vec<Recent>,
    pub thumb_model: usize,
    pub thumb_quality: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            last_video: None,
            montage: false,
            openai_model: 0,
            style_notes: ai::DEFAULT_STYLE.to_string(),
            reuse_ai: true,
            wants_wide: true,
            wide_height: 0,
            wants_vertical: true,
            wants_lut: false,
            wants_thumb: true,
            out_dir: None,
            encoder: EncoderChoice::Auto,
            gpu_grade: true,
            pack_topic: String::new(),
            pack_format: pack::PackFormat::Standard,
            recent: Vec::new(),
            thumb_model: 0,
            thumb_quality: 1,
        }
    }
}

impl Settings {
    /// The saved settings, or the defaults when nothing has been saved yet. Videos that are gone are dropped from the recent list.
    pub fn load() -> Self {
        let mut s: Settings = store::load("settings").unwrap_or_default();
        s.recent.retain(|r| r.path.is_file());
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_an_older_version_still_open_and_gaps_get_defaults() {
        let s: Settings = serde_json::from_str(r#"{"last_video":"C:/a.mp4","montage":true}"#).unwrap();
        assert!(s.montage && s.last_video.is_some());
        assert!(s.reuse_ai && s.wants_wide && s.thumb_quality == 1, "missing choices take their defaults");
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.last_video, s.last_video);
    }
}
