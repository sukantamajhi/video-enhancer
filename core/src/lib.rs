//! Everything the video enhancer does that has nothing to do with how it looks:
//! grading, FFmpeg, the graphics card, the AI helpers, music, thumbnails and saving.
//! Both front ends (egui and gpui) are built on this.

pub mod ai;
pub mod engine;
pub mod gpu;
pub mod grade;
pub mod media;
pub mod montage;
pub mod montage_engine;
pub mod music;
pub mod pack;
pub mod publish;
pub mod sample;
pub mod settings;
pub mod store;
pub mod thumb_ai;
