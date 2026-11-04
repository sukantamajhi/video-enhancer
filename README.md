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

## Montage (several clips into one video)

Switch to **Montage** at the top, then add up to 12 clips (for example five 2-minute clips).

1. **Style** (dropdown): *Highlight reel*, *Beat-synced to music*, or *Story / vlog edit* (keeps your order, fades through black, adds a title).
2. **Length**: you set it (30 s up to 15 min). It exports **16:9 and 9:16**.
3. **Sound** (dropdown): music with the original sound underneath (the music dips when people speak), original sound only, or music only.
4. **Music**: *Find free music for me* searches ccMixter, a Creative Commons catalogue of music made for reuse in videos (no key needed), using a mood the AI picks from your footage; instrumentals come first because they sit better under speech, or *Use my own file*. Only CC0, public-domain, CC BY and CC BY-SA tracks are used (nothing marked NC or ND), and a credits text file (`..._music_credits.txt`) with a ready-to-paste credit line is saved next to the montage.
5. **Plan with AI**: OpenAI sees about 10 small pictures per clip, plus where each clip is busiest (movement and loudness, measured locally), and proposes the moments to keep. Your video files are never uploaded. You see the cut list and can untick moments.
6. **Make montage**: FFmpeg cuts your real footage together, grades each clip separately so different cameras and lighting match, and renders both shapes. A beat-synced edit finds the song's tempo and lands every cut on a beat.

Notes:

- ccMixter tracks are mostly CC BY (credit needed). Always paste the saved credit line into your video description. Openverse is tried as an extra source, but it is behind a bot challenge that often turns automated requests away, so it is optional.
- ccMixter is a small community server: searches take 5-15 seconds, and a failed search can simply be retried with **Search again**.
- The story title needs a system font (Arial on Windows); without one the title is skipped.
- Pasting a YouTube link to rip music is deliberately not supported: it breaks YouTube's terms and can't make a track safe to use.

## Your work is remembered (so AI results are never paid for twice)

The app saves as you go to `%LOCALAPPDATA%\VideoEnhancer\saved`:

- **Per video**: the grade, the look, a Gemini Omni look, an OpenAI suggestion, the export range and the 9:16 framing. Reopen the same file (even after a restart) and it all comes back. The app also reopens your last video on start.
- **The montage in progress**: clips, choices, the AI's cut list (including moments you unticked), the song list and your pick. It is restored when you start the app, and **Make montage** works at once.
- **A cache of AI answers**, keyed by exactly what was asked (the footage, range, style, length, model and your wording). Ask the same thing again and the saved answer is used: nothing is sent, no tokens or money are spent, and the status bar says so. Change anything, or edit the video file, and a new question is asked.
- **Reuse saved AI answers** (tick box) turns the cache off when you want fresh ideas; **Forget saved answers** clears it.

API keys are not in this folder; they stay in the system credential store. Saved files are plain JSON and safe to delete.

## Tests

`cargo test` runs the colour, AI-reply and parsing tests. If FFmpeg is installed it also runs the end-to-end tests that export a **2-minute** video in every shape and build montages in every style, sound mode and shape; without FFmpeg those tests skip themselves.

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
