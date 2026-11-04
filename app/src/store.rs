//! Remembers your work between runs, so nothing paid for is paid for twice.
//!
//! Everything lives in one folder (`saved` inside the app's home folder):
//! your last session, one file per video with its grade and AI results, the
//! montage in progress, and a cache of AI answers keyed by exactly what was
//! asked. Writes go to a temporary file first, so a crash can never leave a
//! half-written save behind.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::media;

/// Bump this when the shape of a saved answer changes, so old ones are ignored.
pub const VERSION: &str = "v1";

pub fn dir() -> PathBuf {
    // VIDEO_ENHANCER_SAVED lets a test run keep its saves apart from the real ones.
    std::env::var_os("VIDEO_ENHANCER_SAVED").map_or_else(|| media::home().join("saved"), PathBuf::from)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A short fingerprint of a file: where it is, how big, and when it last changed.
/// If the file is edited or replaced, the fingerprint changes and saved results are not reused.
pub fn fingerprint(path: &Path) -> String {
    let (len, modified) = std::fs::metadata(path)
        .map(|m| (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())))
        .unwrap_or((0, 0));
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    hash(&[full.to_string_lossy().to_lowercase(), len.to_string(), modified.to_string()])
}

/// A cache key for any list of facts.
pub fn hash(parts: &[String]) -> String {
    let mut sha = Sha256::new();
    sha.update(VERSION.as_bytes());
    for part in parts {
        sha.update([0]);
        sha.update(part.as_bytes());
    }
    hex(&sha.finalize()[..10])
}

pub fn save_in<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
    let (part, target) = (dir.join(format!("{name}.part")), dir.join(format!("{name}.json")));
    std::fs::write(&part, text).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &target).map_err(|e| e.to_string())
}

pub fn load_in<T: DeserializeOwned>(dir: &Path, name: &str) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{name}.json"))).ok()?).ok()
}

pub fn save<T: Serialize>(name: &str, value: &T) {
    // Saving is a convenience. If the disk is full or locked, the app carries on.
    let _ = save_in(&dir(), name, value);
}

pub fn load<T: DeserializeOwned>(name: &str) -> Option<T> {
    load_in(&dir(), name)
}

/// Removes every saved AI answer (the cache), leaving sessions alone. Returns how many.
pub fn clear_ai_cache() -> usize {
    let Ok(entries) = std::fs::read_dir(dir()) else { return 0 };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("ai-") && std::fs::remove_file(e.path()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::{Grade, LearnedLook};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ve-store-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn saved_work_comes_back_exactly() {
        let dir = scratch("roundtrip");
        let look = LearnedLook { curves: [vec![0.0, 0.5, 1.0], vec![0.1, 0.4, 0.9], vec![0.0, 0.6, 1.0]], saturation: 1.2, amount: 0.8 };
        let grade = Grade { contrast: 0.25, temperature: -0.1, ..Grade::default() };
        save_in(&dir, "video-abc", &(grade, look.clone())).unwrap();
        let (g, l): (Grade, LearnedLook) = load_in(&dir, "video-abc").unwrap();
        assert!(g == grade && l == look);
        assert!(load_in::<Grade>(&dir, "missing").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_broken_save_is_ignored_not_fatal() {
        let dir = scratch("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("video-x.json"), "{ half a file").unwrap();
        assert!(load_in::<Grade>(&dir, "video-x").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saving_replaces_the_old_file_and_leaves_no_temporary_one() {
        let dir = scratch("replace");
        save_in(&dir, "a", &1u32).unwrap();
        save_in(&dir, "a", &2u32).unwrap();
        assert_eq!(load_in::<u32>(&dir, "a"), Some(2));
        assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cache_keys_change_with_anything_that_changes_the_answer() {
        let k = |parts: &[&str]| hash(&parts.iter().map(|p| p.to_string()).collect::<Vec<_>>());
        assert_eq!(k(&["a", "b"]), k(&["a", "b"]));
        assert_ne!(k(&["a", "b"]), k(&["a", "c"]));
        // Parts are separated, so shifting text between them is a different question.
        assert_ne!(k(&["ab", "c"]), k(&["a", "bc"]));
    }

    #[test]
    fn a_file_fingerprint_changes_when_the_file_does() {
        let dir = scratch("fingerprint");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("clip.mp4");
        std::fs::write(&file, b"one").unwrap();
        let before = fingerprint(&file);
        assert_eq!(before, fingerprint(&file));
        std::fs::write(&file, b"different length").unwrap();
        assert_ne!(before, fingerprint(&file));
        std::fs::remove_dir_all(&dir).ok();
    }
}
