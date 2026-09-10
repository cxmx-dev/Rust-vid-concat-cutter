# Rust-vid-concat+cutter

Desktop **video concat + interactive cutter** in **one GUI** (Rust + egui). Shares one project tree and one local FFmpeg kit under **`engine/`**.

Windows desktop tool — not a browser demo.

Concat-mode tools are a **Rust port of core jobs from a standalone Python Concat app** (join, add audio / loop visuals, speed, convert). Whisper Auto CC / burn captions are **not** ported yet.

## Run

From project root (so `engine/`, `in/`, `out/`, `audio/` resolve):

```text
cargo run
```

Top bar modes:

| Mode | What you get |
|------|----------------|
| **✂ Cut** | Interactive timeline cutter — split, reorder, preview, EDL + remuxed export |
| **⧉ Concat** | Ported Concat jobs — see table below |

Optional:

```text
cargo run -p vid-concat-cutter --example smoke_pure
cargo run -p vid-concat-cutter --example smoke_concat_port
```

## Concat mode

| Control | Role | Typical output |
|---------|------|----------------|
| **CONCAT** | Join list (primary v/a streams; stream-copy when possible, filter fallback) | `out/combined_*.mp4` |
| **Add Audio** | Loop visuals to match track duration | `out/plus-audio-combined_*.mp4` |
| **Speed** | 9.5s fit (silent) or N× factor | `out/fast_*.mp4` |
| **CONVERT** | Re-encode selected clip → mp4 / webm / mov / mkv / avi | `out/converted_*.*` |

Options: reverse order / reverse video / reverse audio; 9.5 sec speed mode; convert format picker. Refresh from `in/`; auto-pick first file in `audio/`.

**Not ported yet:** Whisper Auto CC, burn captions, stop-motion + pulse-invert FX, still-image slideshow pipeline, list auto-sort by sequence numbers.

## Media stack

- Place **`ffmpeg.exe`**, **`ffprobe.exe`**, and **`ffplay.exe`** in **`engine/`**.
- Optional overrides: `VCC_FFMPEG`, `VCC_FFPROBE`, `VCC_FFMPEG_DIR`.
- Do not commit the binaries (gitignored).

## Layout

```text
Rust-vid-concat+cutter/
  Cargo.toml           # workspace (engine + cutter)
  engine/              # ffmpeg kit + media crate + pipelines.rs
  cutter/              # single GUI (Cut + Concat)
  cutter/examples/     # smoke_pure, smoke_concat_port
  in/  out/  audio/
```

## What Does What

| Piece | Role |
|-------|------|
| `cargo run` → `cutter/` | One egui window — **Cut** / **Concat** |
| **Cut** | Clip sequencer, filmstrips, transport, EDL + remuxed `*-edited-*.mp4` |
| **Concat** | CONCAT / Add Audio / Speed / Convert (background threads) |
| `engine/pipelines.rs` | FFmpeg jobs |
| `engine/` crate (rest) | Probe, frame extract, path sanitize, binary resolve |
| `in/` · `out/` · `audio/` | Working media dirs |
