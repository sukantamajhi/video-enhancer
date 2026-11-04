# Video Enhancer

A desktop app (Rust + egui) that colour-grades a video for YouTube and exports it as both **16:9** and **9:16**.

## Run it

```
cd app
cargo run --release
```

The first time, press **Download video tools** (FFmpeg, about 110 MB, checked against its published SHA-256 before use). If FFmpeg is already on your PATH it is used instead.

## What it does

- **Before/after preview**: slide, side by side, or either alone. Drop a file on the window or press Open video.
- **Auto-fix** measures exposure, black/white points and colour cast across the whole selected range, not one frame.
- **8 looks** (YouTube Pop, Clean Natural, Warm Vlog, Teal & Orange, Moody Film, Gaming Vivid, Bright Tutorial, Food & Travel) plus fine-tune sliders.
- **Gemini Omni 1.1** (`gemini-omni-1.1-flash`): Omni accepts uploaded clips of 10 seconds or less and redraws them, so the app sends a 10-second montage taken from across your video, then *learns the colour change* and applies it to your real footage. Faces and details stay untouched, and a 2-minute video costs one sample, not the whole length. It costs real money (the app asks first) and isn't available for uploaded video in the EU, UK or Switzerland.
- **OpenAI** (`gpt-6-luna`, `gpt-6.1-sol`, `gpt-6-astra`): sends 6 small frames and applies the slider values it suggests.
- **Export**: 16:9 (original size, 1080p, 1440p or 4K), 9:16 1080x1920, a thumbnail, and a `.cube` colour file. For 9:16 you can follow the action with a smooth virtual camera, centre, choose the position, or keep the whole picture on a blurred background. Preview and export use the same colour table.
- Sound is levelled to YouTube's loudness. Cancelling never leaves a half-written file, and nothing you made earlier is overwritten.

API keys are stored in the system credential store (or read from `GEMINI_API_KEY` / `OPENAI_API_KEY`).

## Tests

`cargo test` runs the colour, AI-reply and parsing tests. If FFmpeg is installed it also runs the end-to-end test that exports a **2-minute** video in every shape; without FFmpeg those tests skip themselves.

## Ideas for what to add next

1. Auto-captions: burn-in subtitles for Shorts, in word-by-word style.
2. Scene-by-scene grading: detect cuts and grade each shot, so a sunny shot and an indoor shot each look right.
3. Face-aware 9:16 framing: track the speaker instead of general motion.
4. Skin-tone protection: a mask so strong looks never turn faces orange.
5. Batch mode and presets per channel, so every upload gets the same look.
6. Silence and filler-word trimming.
7. Thumbnail generator with an AI-chosen best frame and title text.
8. Background music ducking under speech, and audio noise removal.
9. Upload straight to YouTube with a title and description drafted from the video.
10. Square (1:1) and 4:5 exports for Instagram and LinkedIn.
11. Shake stabilisation and automatic lens correction.
12. A "match this reference" mode: grade your footage to look like a frame from another video.
