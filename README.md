 Rust-vid-concat+cutter

## What This Is

Desktop **video concat + interactive cutter** in **one GUI** (Rust + egui). Shares one project tree and one local FFmpeg kit under **`engine/`**.

**Kind:** local-tool (`hub.kind`) ΓÇö Windows desktop only, not a browser demo.

Concat-mode tools are a **Rust port of core jobs from the standalone Python Concat app** (join, add audio / loop visuals, speed, convert). Whisper Auto CC / burn captions are **not** ported yet.

## Run

From project root (so `engine/`, `in/`, `out/`, `audio/` resolve):

```text
cargo run
```

Top bar modes:

| Mode | What you get |
|------|----------------|
| **Γ£é Cut** | Interactive timeline cutter ΓÇö split, reorder, preview, EDL + remuxed export |
| **Γºë Concat** | Ported Concat jobs ΓÇö see table below |

Optional:

```text
cargo run -p vid-concat-cutter --example smoke_pure
cargo run -p vid-concat-cutter --example smoke_concat_port
```

## Concat mode (ported from Concat)

| Control | Role | Typical output |
|---------|------|----------------|
| **CONCAT** | Join list (primary v/a streams; stream-copy when possible, filter fallback) | `out/combined_*.mp4` |
| **Add Audio** | Loop visuals to match track duration (red-button workflow) | `out/plus-audio-combined_*.mp4` |
| **Speed** | 9.5s fit (silent) or N├ù factor | `out/fast_*.mp4` |
| **CONVERT** | Re-encode selected clip ΓåÆ mp4 / webm / mov / mkv / avi | `out/converted_*.*` |

Options: reverse order / reverse video / reverse audio; 9.5 sec speed mode; convert format picker. Refresh from `in/`; auto-pick first file in `audio/`.

**Not ported yet:** Whisper Auto CC, burn captions / Load CC, stop-motion + pulse-invert FX, still-image slideshow pipeline, list auto-sort by sequence numbers.

## Media stack

- Place **`ffmpeg.exe`**, **`ffprobe.exe`**, and **`ffplay.exe`** in **`engine/`**.
- Optional overrides: `VCC_FFMPEG`, `VCC_FFPROBE`, `VCC_FFMPEG_DIR`.
- Any format those binaries open is supported.
- Do not commit the binaries (gitignored).
- This-machine paths: see **`USER-NOTES.md`** (gitignored).

## Layout

```text
Rust-vid-concat+cutter/
  Cargo.toml           # workspace (engine + cutter)
  engine/              # ffmpeg kit + media crate + pipelines.rs
  cutter/              # single GUI (Cut + Concat)
  cutter/examples/     # smoke_pure, smoke_concat_port
  concat/              # legacy iced package (unused by cargo run)
  in/  out/  audio/
  hub.kind
```

## What Does What

| Piece | Role |
|-------|------|
| `cargo run` ΓåÆ `cutter/` | One egui window ΓÇö **Cut** / **Concat** |
| **Cut** | Clip sequencer, filmstrips, transport, EDL + remuxed `*-edited-*.mp4` |
| **Concat** | CONCAT / Add Audio / Speed / Convert (background threads) |
| `engine/pipelines.rs` | FFmpeg jobs ported from Python Concat |
| `engine/` crate (rest) | Probe, frame extract, path sanitize, binary resolve |
| `engine/*.exe` | Local full-build FFmpeg kit |
| `in/` ┬╖ `out/` ┬╖ `audio/` | Working media dirs |

## Status

**Working**

- One window via `cargo run` (User-verified)
- **Cut** playtest: auto-load from `in/`, engine ffmpeg resolve, Export Video ΓåÆ `out/*-edited-*.mp4`
- Concat mode: CONCAT, Add Audio (loop to track), Speed, Convert (wired; full GUI pass pending)
- Robust concat prefers primary video/audio streams (handles multi-stream / cover-art mp4s)
- Hub: auto-registered `start.ps1` / `_project.ps1` (local-tool; no git origin yet)
- Privacy: Repos scan OK; empty unused `vid-concat-cutter-Rust` slot removed

**Open**

- Whisper Auto CC / burn captions (Python Concat)
- Stop-motion / pulse-invert / still-image duration pipeline
- Cutter inspector DragValues commit-to-list
- Concat-mode GUI playtest (CONCAT / Add Audio / Speed / Convert)
- Optional git remote when User wants publish

## Version History

2026-07-27  
ΓÇó `update .mds`: User `cargo run` Cut playtest OK (auto-load + Export Video); hub privacy OK; start-all auto-register (no origin); empty `vid-concat-cutter-Rust` removed.

2026-07-27  
ΓÇó `update .mds`: document Python Concat port ΓÇö `engine/pipelines.rs` + Concat UI (CONCAT, Add Audio, Speed, Convert).  
ΓÇó FFmpeg kit previously refreshed to gyan.dev git full `2026-07-27-git-a757b708ae` (project `engine/` + shared kit).  
ΓÇó Manual join+audio smoke with two `in/` clips + `audio/Marblespin.mp3` earlier this session.

2026-07-27  
ΓÇó `update .mds`: README / SYNC / USER-NOTES / AGENTS for unified GUI.  
ΓÇó Product truth: one egui app, Cut + Concat modes, `cargo run` from project root.

2026-07-27  
ΓÇó Single GUI: Cut + Concat modes in one egui app (`cargo run`).

2026-07-27  
ΓÇó Combined former `video-cutter` + `vid-concatenator-Rust` into this hub folder.  
ΓÇó Media kit under `engine/`; consolidated docs.
