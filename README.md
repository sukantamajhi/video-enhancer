# Video Enhancer

A desktop app (Rust + egui) that colour-grades a video for YouTube and exports it as both **16:9** and **9:16**.

## Run it

```
cd app
cargo run --release
```

The first time, press **Download video tools** (FFmpeg, about 110 MB, checked against its published SHA-256 before use). If FFmpeg is already on your PATH it is used instead.

## How the window works

A narrow rail on the left walks you through four steps; a green tick appears on each one you have finished.

| | Video | Montage |
|---|---|---|
| 1 | **Source**: drop or open one video. Recent videos come back with their look and AI results. | **Clips**: add clips, choose a style, length, sound, look and music. |
| 2 | **Look**: before/after preview, a strip of the eight looks drawn from your own frame, sliders, trim timeline, AI assist. | **Plan**: the moments the AI chose, as a bar and a list you can untick. |
| 3 | **Export**: 16:9 and 9:16 previews, framing, extras, speed, folder. | **Make**: shapes, framing, speed, folder. |
| 4 | **Publish**: titles, description, chapters, tags, search check, thumbnail. | The same Publish screen. |

The **Video / Montage** switch and the **Settings** button (API keys, saved AI answers) are in the top bar. Running jobs show as a card at the bottom left with a Cancel button. Dropping several files on the Video screen starts a montage.

Colours, type and shapes live in `app/src/theme.rs`. Text uses the system fonts (Segoe UI, Cascadia Mono). Put `Manrope-Medium.ttf`, `Manrope-ExtraBold.ttf` and `JetBrainsMono-Medium.ttf` in `app/assets/fonts` (next to where you run the app) and they are used instead.

## What it does

- **Before/after preview**: slide, side by side, or either alone. Drop a file on the window or press Open video.
- **Auto-fix** measures exposure, black/white points and colour cast across the whole selected range, not one frame.
- **8 looks** (YouTube Pop, Clean Natural, Warm Vlog, Teal & Orange, Moody Film, Gaming Vivid, Bright Tutorial, Food & Travel) plus fine-tune sliders.
- **Gemini Omni 1.1** (`gemini-omni-1.1-flash`): Omni accepts uploaded clips of 10 seconds or less and redraws them, so the app sends a 10-second montage taken from across your video, then *learns the colour change* and applies it to your real footage. Faces and details stay untouched, and a 2-minute video costs one sample, not the whole length. It costs real money (the app asks first) and isn't available for uploaded video in the EU, UK or Switzerland.
- **OpenAI** (`gpt-6-luna`, `gpt-6.1-sol`, `gpt-6-astra`): sends 6 small frames and applies the slider values it suggests.
- **Export**: 16:9 (original size, 1080p, 1440p or 4K), 9:16 1080x1920, a thumbnail, and a `.cube` colour file. For 9:16 you can follow the action with a smooth virtual camera, centre, choose the position, or keep the whole picture on a blurred background. Preview and export use the same colour table.
- Sound is levelled to YouTube's loudness. Cancelling never leaves a half-written file, and nothing you made earlier is overwritten.

API keys are stored in the system credential store (or read from `GEMINI_API_KEY` / `OPENAI_API_KEY`).

**Whole picture on a blurred background (9:16):** the main picture is zoomed in a little by default and centred over a blurred copy of itself. Use the **zoom** slider to make it larger, and drag the small 9:16 preview (or use the sliders in the montage screen) to move it. The crosshair marks the middle of the main picture, **Centre** puts it back, and the preview matches the exported video. The choice is remembered per video.

## Montage (several clips into one video)

Switch to **Montage** at the top, then add up to 12 clips (for example five 2-minute clips).

1. **Style** (dropdown): *Highlight reel*, *Beat-synced to music*, or *Story / vlog edit* (keeps your order, fades through black, adds a title).
2. **Length**: you set it (30 s up to 15 min). It exports **16:9 and 9:16**.
3. **Sound** (dropdown): music with the original sound underneath (the music dips when people speak), original sound only, or music only.
4. **Music**: *NCS music* searches NoCopyrightSounds (ncs.io), the library made for creators: its usage policy allows its music in monetised YouTube and Twitch videos without copyright strikes, provided the song is credited in the description. The AI picks a mood from your footage and the app finds matching NCS tracks (instrumentals first, since they sit better under the game's sound). It uses the same public pages and download links the NCS website uses, and nothing is taken from YouTube. NCS's own credit text is saved in `..._music_credits.txt` next to the montage and added to your upload pack's description. You can also choose *Use my own file*.
5. **Plan with AI**: OpenAI sees about 10 small pictures per clip, plus where each clip is busiest (movement and loudness, measured locally), and proposes the moments to keep. Your video files are never uploaded. You see the cut list and can untick moments.
6. **Make montage**: FFmpeg cuts your real footage together, grades each clip separately so different cameras and lighting match, and renders both shapes. A beat-synced edit finds the song's tempo and lands every cut on a beat.

Notes:

- NCS's rules apply: credit the song in the description (the app writes the text for you). Check the usage policy at https://ncs.io/usage-policy for anything beyond YouTube and Twitch, such as other platforms or paid brand work, which NCS handles separately.
- Searches take a few seconds; a failed one can simply be retried with **Search again**.
- The story title needs a system font (Arial on Windows); without one the title is skipped.
- Pasting a YouTube link to rip music is deliberately not supported: it breaks YouTube's terms and can't make a track safe to use.

## Upload pack (titles, description, hashtags, thumbnail)

Press **Write my upload pack** under Export (single video) or at the bottom of the Montage screen.

Every pack makes **two thumbnails**: a 1280x720 one for a normal video, and a 1080x1920 (9:16) one for a Short. Switch between them above the picture. The tall one is cut out of the wide frame, with a slider to choose where, and its text sits in the middle band, clear of the Shorts buttons and title. **Save pack and thumbnails** writes `..._thumbnail.jpg` and `..._thumbnail_shorts.jpg`.

- **Written for YouTube search.** Type the **game or topic** (its exact name is the strongest keyword for a gaming video) and choose **Standard (16:9)** or **Short (9:16)**, since Shorts follow different rules (shorter titles, no chapters, `#Shorts`). You get **three title options** (each from a different angle, editable, with a character count), a **description intro**, **3-5 hashtags**, and hidden **tags**. Sends about 8 small pictures to OpenAI, never your video files.
- **The rules the AI is given** (`pack_instructions` in `app/src/ai.rs`, sent as the system instructions) follow YouTube's own help pages: pick 1-2 main keywords and feature them prominently in the title and the description; use the first lines of the description to describe the video; tags play only a minor part; only the first three hashtags show above the title (hashtags can't contain spaces, and more than 60 are all ignored); and misleading titles, thumbnails, descriptions, unrelated hashtags and excessive tags are against YouTube's spam policy. It is told never to guess a game, score or fact it can't see, and to use only the game name you typed or one clearly visible on screen. Two figures are widely used **rules of thumb, not YouTube limits**: titles of about 60 characters or fewer show in full, and the first ~150 characters of the description are what viewers see first. The only hard limits used are 100 characters for a title and 5000 for a description.
- **Search check.** Under the description, the app checks the pack itself (not the AI): title length, the focus keyword in the title (and near the start), the keyword in the first 150 characters of the description, the keyword not repeated so often it reads as stuffing, hashtag count, chapters, tags and thumbnail text. It updates as you edit, so a title you rewrite is checked straight away.
- **Chapters are computed by the app, not the AI.** Each chapter time is where that moment really sits in the finished video (for a montage, after crossfades and beat-snapping). The AI only supplies short titles. The list follows YouTube's rules (starts at 0:00, at least 3 chapters, 10 seconds apart, at most 15) and is left out entirely if the video is too short to qualify.
- The description also carries the **music credit line** when the song asks for one, and the hashtags.
- **Thumbnail:** the AI picks the clearest frame and 1-3 words of text. The app draws a 1280x720 JPEG from your footage in the same colour grade as the video, with bold outlined text (Impact, when the computer has it). Change the text, or step through the other candidate frames, and press **Redraw**.
- **Save pack and thumbnail to the folder** writes `..._upload_pack.txt` and `..._thumbnail.jpg`. The AI answer is cached like the others, so asking again with nothing changed costs nothing.

## Faster exports (graphics card)

Two separate things use the graphics card, and both are on by default when your computer supports them (**Single video** export panel):

- **Encoder** (NVIDIA NVENC, AMD AMF, Intel Quick Sync): the menu lists only encoders that really work on your computer. If one ever fails, the same video is made on the CPU and the message says so. Graphics-card files come out somewhat larger for the same quality (SSIM against the CPU encode is within 0.0001).
- **Grade colour on the graphics card:** FFmpeg's colour-table filter is the slowest part of an export, so the graphics card applies the colour grade instead. The export is split in three programs running side by side: FFmpeg decodes, crops and scales; the card grades every frame; a second FFmpeg adds the original sound and encodes. The card uses the same colour table and the same arithmetic as the preview. At start-up it is checked against the processor on a test picture (it must agree to within one brightness level) and if it isn't accurate, or isn't there, FFmpeg grades as before.

Measured end to end on a 16-thread PC with an NVIDIA RTX 3060, on 60 s of 1080p60 test footage (real gameplay will differ):

| | colour by FFmpeg | colour on the graphics card |
|---|---|---|
| 16:9, CPU encoder | 1.8x real time | 2.6x |
| 16:9, NVIDIA encoder | 2.4x | **5.3x** |
| 9:16, NVIDIA encoder | 2.2x | **4.6x** |

Before any of these changes it was about 1.2x. At 5x, a 2-hour 1080p60 session takes roughly 25 minutes to export, not "minutes".

What limits it now is not the graphics card (grading takes under 1 ms a frame) but the cost of moving every raw frame between the three programs: about 11 GB/s of memory copying on this PC. Going further would mean keeping frames on the graphics card the whole way, from decoder to encoder, which is not built. 4K takes four times the data. Two cases still use FFmpeg for colour: **montages** (their graph joins many clips) and any picture size whose width isn't a multiple of 8.

Other notes: sharpening is **off by default** (it adds roughly 40% to FFmpeg's work); and the colour grade now always runs after cropping, scaling and blurring, because scaling an already graded picture shifted its colours.

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
