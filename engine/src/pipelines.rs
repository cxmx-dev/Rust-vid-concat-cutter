//! Port of Concat (Python CONCATENATOR-IV) FFmpeg jobs.
//!
//! Core actions (no Whisper / Auto CC — those stay optional later):
//! - **concatenate** — join clips (robust filter path; optional reverse video/audio)
//! - **add_audio** — loop visuals to audio length (red-button workflow)
//! - **speed_up** — 9.5s fit or N× factor (silent for 9.5s mode)
//! - **convert** — re-encode to mp4 / webm / mov / mkv / avi

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{ffmpeg_exe, ffprobe_exe, sanitize_path};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
}

fn run_ffmpeg(args: &[String]) -> Result<()> {
    let mut cmd = Command::new(ffmpeg_exe());
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .with_context(|| format!("spawn ffmpeg ({})", ffmpeg_exe().display()))?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr
            .lines()
            .rev()
            .take(16)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        bail!("ffmpeg failed ({})\n{tail}", out.status);
    }
}

/// Probe duration via ffprobe (works for video and audio).
pub fn probe_duration(path: &Path) -> Result<f64> {
    let path_s = sanitize_path(path);
    let mut cmd = Command::new(ffprobe_exe());
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
        &path_s,
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().context("ffprobe duration")?;
    if !out.status.success() {
        bail!("ffprobe failed for {}", path.display());
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let d: f64 = text
        .parse()
        .with_context(|| format!("parse duration '{text}' for {}", path.display()))?;
    if d.is_finite() && d > 0.0 {
        Ok(d)
    } else {
        bail!("invalid duration for {}", path.display());
    }
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    Ok(())
}

fn unique_out(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let base = dir.join(format!("{stem}{ext}"));
    if !base.exists() {
        return base;
    }
    for n in 2..10_000 {
        let c = dir.join(format!("{stem}_{n}{ext}"));
        if !c.exists() {
            return c;
        }
    }
    dir.join(format!("{stem}_{}{ext}", stamp()))
}

// ---------------------------------------------------------------------------
// Concat
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConcatOptions {
    pub reverse_order: bool,
    /// Reverse each clip's video (re-encode).
    pub reverse_video: bool,
    /// Reverse each clip's audio (re-encode).
    pub reverse_audio: bool,
    /// Prefer stream-copy when no reverse filters (falls back to re-encode).
    pub prefer_stream_copy: bool,
}

impl Default for ConcatOptions {
    fn default() -> Self {
        Self {
            reverse_order: false,
            reverse_video: false,
            reverse_audio: false,
            prefer_stream_copy: true,
        }
    }
}

/// Join videos → `output` (primary video+audio streams only; ignores cover-art mjpeg).
pub fn concatenate(inputs: &[PathBuf], output: &Path, opts: &ConcatOptions) -> Result<PathBuf> {
    if inputs.is_empty() {
        bail!("no inputs for concat");
    }
    ensure_parent(output)?;
    let mut files: Vec<PathBuf> = inputs.to_vec();
    if opts.reverse_order {
        files.reverse();
    }

    let need_filters = opts.reverse_video || opts.reverse_audio || !opts.prefer_stream_copy;

    if !need_filters && files.len() >= 1 {
        // Try demuxer stream-copy first (fast). If it fails, fall through to filter path.
        if try_concat_demuxer_copy(&files, output).is_ok() && output.is_file() {
            return Ok(output.to_path_buf());
        }
    }

    // Robust path: filter_complex concat of v:0 + a:0 (handles multi-stream mp4s).
    concat_filter_complex(&files, output, opts)
}

fn try_concat_demuxer_copy(files: &[PathBuf], output: &Path) -> Result<()> {
    let list = std::env::temp_dir().join(format!("vcc_concat_{}.txt", stamp()));
    let mut body = String::new();
    for f in files {
        let p = sanitize_path(f).replace('\'', "'\\''");
        body.push_str(&format!("file '{p}'\n"));
    }
    std::fs::write(&list, body)?;
    let args = vec![
        "-y".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        sanitize_path(&list),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c".into(),
        "copy".into(),
        "-movflags".into(),
        "+faststart".into(),
        sanitize_path(output),
    ];
    let r = run_ffmpeg(&args);
    let _ = std::fs::remove_file(&list);
    r
}

fn concat_filter_complex(files: &[PathBuf], output: &Path, opts: &ConcatOptions) -> Result<PathBuf> {
    let n = files.len();
    let mut args: Vec<String> = vec!["-y".into()];
    for f in files {
        args.push("-i".into());
        args.push(sanitize_path(f));
    }

    // Build per-input labels with optional reverse.
    let mut parts = String::new();
    let mut labels = String::new();
    for i in 0..n {
        let mut v = format!("[{i}:v:0]");
        let mut a = format!("[{i}:a:0]");
        if opts.reverse_video {
            parts.push_str(&format!("{v}reverse[v{i}];"));
            v = format!("[v{i}]");
        }
        if opts.reverse_audio {
            parts.push_str(&format!("{a}areverse[a{i}];"));
            a = format!("[a{i}]");
        } else {
            // Some clips may lack audio — generate silence if needed is heavy;
            // require audio stream; use anullsrc only when reverse_audio not set and stream missing
            // handled by concat failing with clear error.
        }
        labels.push_str(&format!("{v}{a}"));
    }
    parts.push_str(&format!("{labels}concat=n={n}:v=1:a=1[v][a]"));

    args.push("-filter_complex".into());
    args.push(parts);
    args.extend([
        "-map".into(),
        "[v]".into(),
        "-map".into(),
        "[a]".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "fast".into(),
        "-crf".into(),
        "23".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-movflags".into(),
        "+faststart".into(),
        sanitize_path(output),
    ]);
    run_ffmpeg(&args)?;
    if !output.is_file() {
        bail!("concat produced no output");
    }
    Ok(output.to_path_buf())
}

// ---------------------------------------------------------------------------
// Add Audio (red button) — loop visuals to audio length
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AddAudioOptions {
    pub reverse_order: bool,
    pub reverse_video: bool,
    pub reverse_audio: bool,
    /// Target fps for looped visual base.
    pub fps: u32,
}

impl Default for AddAudioOptions {
    fn default() -> Self {
        Self {
            reverse_order: false,
            reverse_video: false,
            reverse_audio: false,
            fps: 30,
        }
    }
}

/// Build silent-ish join of visuals, loop until audio ends, mux audio.
/// Output duration = audio duration (`-shortest` / `-t`).
pub fn add_audio(
    visuals: &[PathBuf],
    audio: &Path,
    output: &Path,
    opts: &AddAudioOptions,
) -> Result<PathBuf> {
    if visuals.is_empty() {
        bail!("no visual clips for add_audio");
    }
    if !audio.is_file() {
        bail!("audio not found: {}", audio.display());
    }
    ensure_parent(output)?;

    let audio_dur = probe_duration(audio)?;
    let ts = stamp();
    let temp_join = std::env::temp_dir().join(format!("vcc_silent_base_{ts}.mp4"));

    let concat_opts = ConcatOptions {
        reverse_order: opts.reverse_order,
        reverse_video: opts.reverse_video,
        reverse_audio: false, // audio comes from external track
        prefer_stream_copy: false,
    };
    concatenate(visuals, &temp_join, &concat_opts)?;

    let mut args: Vec<String> = vec![
        "-y".into(),
        "-stream_loop".into(),
        "-1".into(),
        "-i".into(),
        sanitize_path(&temp_join),
        "-i".into(),
        sanitize_path(audio),
    ];

    // reverse_video already applied per-clip during silent join
    let vf = format!("fps={}", opts.fps.max(10).min(60));

    if opts.reverse_audio {
        args.extend([
            "-filter_complex".into(),
            format!("[0:v]{vf}[v];[1:a]areverse[a]"),
        ]);
        args.extend(["-map".into(), "[v]".into(), "-map".into(), "[a]".into()]);
    } else {
        args.extend([
            "-map".into(),
            "0:v:0".into(),
            "-map".into(),
            "1:a:0".into(),
            "-vf".into(),
            vf,
        ]);
    }

    args.extend([
        "-c:v".into(),
        "libx264".into(),
        "-crf".into(),
        "23".into(),
        "-preset".into(),
        "fast".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-r".into(),
        opts.fps.max(10).min(60).to_string(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-t".into(),
        format!("{audio_dur:.3}"),
        "-shortest".into(),
        "-movflags".into(),
        "+faststart".into(),
        sanitize_path(output),
    ]);

    let r = run_ffmpeg(&args);
    let _ = std::fs::remove_file(&temp_join);
    r?;
    if !output.is_file() {
        bail!("add_audio produced no output");
    }
    Ok(output.to_path_buf())
}

// ---------------------------------------------------------------------------
// Speed up
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum SpeedMode {
    /// Force output duration to ~seconds (silent).
    FitSeconds(f64),
    /// Multiply playback speed (e.g. 1.8). Keeps audio when possible.
    Factor(f64),
}

/// Speed-change a single video → `output`.
pub fn speed_up(input: &Path, output: &Path, mode: SpeedMode) -> Result<PathBuf> {
    if !input.is_file() {
        bail!("input not found: {}", input.display());
    }
    ensure_parent(output)?;
    let dur = probe_duration(input)?;
    let (factor, keep_audio) = match mode {
        SpeedMode::FitSeconds(target) => {
            let t = target.max(0.1);
            (dur / t, false)
        }
        SpeedMode::Factor(f) => (f.max(0.05), true),
    };

    let mut args: Vec<String> = vec![
        "-y".into(),
        "-i".into(),
        sanitize_path(input),
    ];

    if keep_audio {
        // atempo only accepts 0.5–2.0; chain for larger factors
        let mut remaining = factor;
        let mut atempo_parts: Vec<String> = Vec::new();
        while remaining > 2.0001 {
            atempo_parts.push("atempo=2.0".into());
            remaining /= 2.0;
        }
        while remaining < 0.4999 {
            atempo_parts.push("atempo=0.5".into());
            remaining /= 0.5;
        }
        atempo_parts.push(format!("atempo={remaining:.6}"));
        let afilter = atempo_parts.join(",");
        let fc = format!("[0:v]setpts=PTS/{factor},fps=30[v];[0:a]{afilter}[a]");
        args.extend([
            "-filter_complex".into(),
            fc,
            "-map".into(),
            "[v]".into(),
            "-map".into(),
            "[a]".into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "128k".into(),
        ]);
    } else {
        args.extend([
            "-filter:v".into(),
            format!("setpts=PTS/{factor},fps=30"),
            "-an".into(),
        ]);
    }

    args.extend([
        "-c:v".into(),
        "libx264".into(),
        "-crf".into(),
        "23".into(),
        "-preset".into(),
        "fast".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-r".into(),
        "30".into(),
        "-movflags".into(),
        "+faststart".into(),
        sanitize_path(output),
    ]);
    run_ffmpeg(&args)?;
    Ok(output.to_path_buf())
}

// ---------------------------------------------------------------------------
// Convert
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertFormat {
    Mp4,
    Webm,
    Mov,
    Mkv,
    Avi,
}

impl ConvertFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4 (H.264)",
            Self::Webm => "WebM (VP9)",
            Self::Mov => "MOV (H.264)",
            Self::Mkv => "MKV (H.264)",
            Self::Avi => "AVI (H.264)",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 => ".mp4",
            Self::Webm => ".webm",
            Self::Mov => ".mov",
            Self::Mkv => ".mkv",
            Self::Avi => ".avi",
        }
    }

    pub fn all() -> &'static [ConvertFormat] {
        &[
            ConvertFormat::Mp4,
            ConvertFormat::Webm,
            ConvertFormat::Mov,
            ConvertFormat::Mkv,
            ConvertFormat::Avi,
        ]
    }
}

pub fn convert_video(input: &Path, out_dir: &Path, format: ConvertFormat) -> Result<PathBuf> {
    if !input.is_file() {
        bail!("input not found: {}", input.display());
    }
    std::fs::create_dir_all(out_dir)?;
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("converted");
    let out = unique_out(out_dir, &format!("converted_{stem}"), format.ext());

    let mut args: Vec<String> = vec![
        "-y".into(),
        "-i".into(),
        sanitize_path(input),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0?".into(),
    ];

    match format {
        ConvertFormat::Webm => {
            args.extend([
                "-c:v".into(),
                "libvpx-vp9".into(),
                "-crf".into(),
                "32".into(),
                "-b:v".into(),
                "0".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-c:a".into(),
                "libopus".into(),
                "-b:a".into(),
                "128k".into(),
            ]);
        }
        _ => {
            args.extend([
                "-c:v".into(),
                "libx264".into(),
                "-crf".into(),
                "23".into(),
                "-preset".into(),
                "fast".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                "192k".into(),
                "-movflags".into(),
                "+faststart".into(),
            ]);
        }
    }
    args.push(sanitize_path(&out));
    run_ffmpeg(&args)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Discovery helpers (list folders)
// ---------------------------------------------------------------------------

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "aac", "m4a", "flac", "ogg", "opus", "wma", "aiff", "aif", "mka",
];

pub fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| {
            let lower = s.to_ascii_lowercase();
            AUDIO_EXTENSIONS.iter().any(|ext| *ext == lower)
        })
        .unwrap_or(false)
}

pub fn list_videos_in_dir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() && crate::is_video_path(&p) {
            out.push(p);
        }
    }
    out.sort();
    out
}

pub fn list_audio_in_dir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() && is_audio_path(&p) {
            out.push(p);
        }
    }
    out.sort();
    out
}

pub fn default_concat_output(out_dir: &Path) -> PathBuf {
    unique_out(out_dir, &format!("combined_{}", stamp()), ".mp4")
}

pub fn default_audio_output(out_dir: &Path, reverse_audio: bool) -> PathBuf {
    let tag = if reverse_audio { "rev-audio" } else { "fwd" };
    unique_out(out_dir, &format!("plus-audio-combined_{tag}_{}", stamp()), ".mp4")
}

pub fn default_speed_output(out_dir: &Path, input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("clip");
    unique_out(out_dir, &format!("fast_{stem}"), ".mp4")
}

pub fn latest_mp4_in(dir: &Path) -> Option<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()).map(|s| s.eq_ignore_ascii_case("mp4")) != Some(true)
        {
            continue;
        }
        let mt = e.metadata().and_then(|m| m.modified()).ok();
        if let Some(mt) = mt {
            if best.as_ref().map(|(t, _)| mt > *t).unwrap_or(true) {
                best = Some((mt, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_ext() {
        assert!(is_audio_path(Path::new("a.MP3")));
        assert!(!is_audio_path(Path::new("a.mp4")));
    }

    #[test]
    fn convert_labels() {
        assert_eq!(ConvertFormat::Mp4.ext(), ".mp4");
    }
}
