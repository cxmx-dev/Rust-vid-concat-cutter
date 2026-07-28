//! Media engine for vid-concat-cutter.
//!
//! **Any format ffmpeg supports** (H.264, HEVC, VP9, AV1, MP4, MKV, MOV, WebM, …).
//!
//! Binaries (prefer first match) — **project-local only**, no shared machine path:
//! 1. Env `VCC_FFMPEG` / `VCC_FFPROBE` / `VCC_FFMPEG_DIR` (optional overrides)
//! 2. Project `engine/` folder (cwd/engine, or next to exe / parents)
//! 3. Process working directory (project root when you `cargo run`)
//! 4. Same folder as this process executable (and parents up to project root)
//!
//! Place `ffmpeg.exe`, `ffprobe.exe`, and optionally `ffplay.exe` in **`engine/`**.
//! Never edit or delete the `backup/` folder (AGENTS.md).

use anyhow::{anyhow, Context, Result};
use ffmpeg_sidecar::command::FfmpegCommand;
use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::process::Command;

mod pipelines;
pub use pipelines::*;

/// Common video extensions accepted by Open / auto-load / Concat list.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "webm", "m4v", "mpg", "mpeg", "wmv", "flv", "ts", "m2ts",
    "mts", "3gp", "ogv", "vob", "f4v", "asf", "rm", "rmvb", "divx", "xvid", "hevc", "h265",
    "av1", "vp9", "mxf", "nut", "y4m", "webpm",
];

/// One non-destructive time range on the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub start: f64,
    pub end: f64,
}

/// Loaded source + ordered clip list (the edit).
#[derive(Debug, Clone)]
pub struct VideoProject {
    pub source_path: PathBuf,
    pub duration: f64,
    pub clips: Vec<Clip>,
}

/// True if path looks like a video by extension (case-insensitive).
pub fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| {
            let lower = s.to_ascii_lowercase();
            VIDEO_EXTENSIONS.iter().any(|ext| *ext == lower)
        })
        .unwrap_or(false)
}

fn push_unique(dirs: &mut Vec<PathBuf>, p: PathBuf) {
    if p.is_dir() && !dirs.iter().any(|d| d == &p) {
        dirs.push(p);
    }
}

fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(dir) = std::env::var("VCC_FFMPEG_DIR") {
        push_unique(&mut dirs, PathBuf::from(dir));
    }

    // Project / process working directory (repo root when you `cargo run`)
    if let Ok(cwd) = std::env::current_dir() {
        // Prefer vendored kit under engine/
        push_unique(&mut dirs, cwd.join("engine"));
        push_unique(&mut dirs, cwd);
    }

    // Next to the running binary (target/debug or a released build)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            push_unique(&mut dirs, parent.join("engine"));
            push_unique(&mut dirs, parent.to_path_buf());
            // cargo run: target/debug → walk up to project root
            if let Some(grand) = parent.parent() {
                push_unique(&mut dirs, grand.join("engine"));
                push_unique(&mut dirs, grand.to_path_buf());
                if let Some(root) = grand.parent() {
                    push_unique(&mut dirs, root.join("engine"));
                    push_unique(&mut dirs, root.to_path_buf());
                }
            }
        }
    }

    dirs
}

/// Resolve `ffmpeg.exe` (project-local only).
pub fn ffmpeg_exe() -> PathBuf {
    if let Ok(p) = std::env::var("VCC_FFMPEG") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return pb;
        }
    }
    for dir in candidate_dirs() {
        let c = dir.join("ffmpeg.exe");
        if c.is_file() {
            return c;
        }
        let c2 = dir.join("ffmpeg");
        if c2.is_file() {
            return c2;
        }
    }
    PathBuf::from("ffmpeg")
}

/// Resolve `ffprobe.exe` (project-local only).
pub fn ffprobe_exe() -> PathBuf {
    if let Ok(p) = std::env::var("VCC_FFPROBE") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return pb;
        }
    }
    for dir in candidate_dirs() {
        let c = dir.join("ffprobe.exe");
        if c.is_file() {
            return c;
        }
        let c2 = dir.join("ffprobe");
        if c2.is_file() {
            return c2;
        }
    }
    let ff = ffmpeg_exe();
    if let Some(parent) = ff.parent() {
        let sibling = parent.join("ffprobe.exe");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("ffprobe")
}

/// FfmpegCommand pointed at the resolved local binary.
pub fn ffmpeg_command() -> FfmpegCommand {
    FfmpegCommand::new_with_path(ffmpeg_exe())
}

/// Sanitize paths for ffmpeg on Windows (strip `\\?\`, forward slashes).
pub fn sanitize_path(p: impl AsRef<Path>) -> String {
    let mut s = p.as_ref().to_string_lossy().to_string();
    if s.starts_with(r"\\?\") {
        s = s.replacen(r"\\?\", "", 1);
    }
    s.replace('\\', "/")
}

/// True when the resolved ffmpeg binary runs `-version`.
pub fn ffmpeg_ready() -> bool {
    Command::new(ffmpeg_exe())
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn media_ready() -> bool {
    ffmpeg_ready()
}

/// Probe duration in seconds via ffprobe (any format ffprobe understands).
pub fn get_media_duration(path: &Path) -> Result<f64> {
    let path_s = sanitize_path(path);
    let out = Command::new(ffprobe_exe())
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            &path_s,
        ])
        .output()
        .with_context(|| format!("ffprobe spawn failed ({})", ffprobe_exe().display()))?;

    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Ok(d) = text.parse::<f64>() {
            if d.is_finite() && d > 0.0 {
                return Ok(d);
            }
        }
    }

    let mut cmd = ffmpeg_command();
    cmd.input(&path_s).args(["-t", "0.01", "-f", "null", "-"]);
    let mut child = cmd.spawn().context("ffmpeg duration probe spawn")?;
    let mut stderr_blob = String::new();
    if let Ok(iter) = child.iter() {
        for ev in iter {
            if let ffmpeg_sidecar::event::FfmpegEvent::Log(_, line) = ev {
                stderr_blob.push_str(&line);
                stderr_blob.push('\n');
            }
        }
    }
    let _ = child.wait();
    if let Some(d) = parse_duration_from_ffmpeg_log(&stderr_blob) {
        return Ok(d);
    }

    Err(anyhow!(
        "could not probe duration for {} (put ffmpeg.exe + ffprobe.exe in engine/)",
        path.display()
    ))
}

fn parse_duration_from_ffmpeg_log(log: &str) -> Option<f64> {
    for line in log.lines() {
        if let Some(idx) = line.find("Duration:") {
            let rest = &line[idx + "Duration:".len()..];
            let token = rest.split(',').next()?.trim();
            let parts: Vec<&str> = token.split(':').collect();
            if parts.len() == 3 {
                let h: f64 = parts[0].trim().parse().ok()?;
                let m: f64 = parts[1].trim().parse().ok()?;
                let s: f64 = parts[2].trim().parse().ok()?;
                let total = h * 3600.0 + m * 60.0 + s;
                if total > 0.0 {
                    return Some(total);
                }
            }
        }
    }
    None
}

/// Open any video ffmpeg/ffprobe can read as a project with one full-length clip.
pub fn load_project(path: &Path) -> Result<VideoProject> {
    if !path.is_file() {
        return Err(anyhow!("file not found: {}", path.display()));
    }
    if !ffmpeg_ready() {
        return Err(anyhow!(
            "ffmpeg not ready at {} — place ffmpeg.exe + ffprobe.exe in engine/",
            ffmpeg_exe().display()
        ));
    }
    let duration = get_media_duration(path)?;
    Ok(VideoProject {
        source_path: path.to_path_buf(),
        duration,
        clips: vec![Clip {
            start: 0.0,
            end: duration,
        }],
    })
}

/// Extract one real decoded frame at `time` seconds as RGBA.
pub fn extract_frame(
    path: &Path,
    time: f64,
    _duration: f64,
    target_width: u32,
) -> Result<RgbaImage> {
    if !path.is_file() {
        return Err(anyhow!("file not found: {}", path.display()));
    }
    let t = time.max(0.0);
    let w = target_width.max(16);
    let tmp = std::env::temp_dir().join(format!(
        "vcc_frame_{}_{}.jpg",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));

    let mut cmd = ffmpeg_command();
    cmd.overwrite()
        .args(["-ss", &format!("{:.3}", t)])
        .input(sanitize_path(path))
        .args(["-frames:v", "1"])
        .args([
            "-vf",
            &format!("scale={w}:-1:force_original_aspect_ratio=decrease"),
        ])
        .args(["-update", "1"])
        .output(sanitize_path(&tmp));

    let mut child = cmd
        .spawn()
        .with_context(|| format!("ffmpeg extract_frame spawn ({})", ffmpeg_exe().display()))?;
    let _ = child.wait();

    if !tmp.exists() {
        return Err(anyhow!(
            "ffmpeg did not write frame at t={t:.3}s for {}",
            path.display()
        ));
    }

    let img = image::open(&tmp)
        .with_context(|| format!("open extracted frame {}", tmp.display()))?
        .to_rgba8();
    let _ = std::fs::remove_file(&tmp);
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ext_mp4() {
        assert!(is_video_path(Path::new("a.MP4")));
        assert!(is_video_path(Path::new("b.mkv")));
        assert!(!is_video_path(Path::new("c.txt")));
    }

    #[test]
    fn sanitize_strips_unc() {
        let s = sanitize_path(r"\\?\C:\path\to\y.mp4");
        assert!(!s.starts_with(r"\\?\"));
    }

    #[test]
    fn no_hardcoded_shared_toolchain_path() {
        // Production resolve paths must not hardcode a machine shared folder.
        // (Avoid putting the forbidden substring in this test source.)
        let src = include_str!("lib.rs");
        let needle = format!("{} - {}", "engine", "ffmpeg");
        // Only fail if the constant-style path appears outside this test module.
        let prod = src.split("mod tests").next().unwrap_or(src);
        assert!(
            !prod.contains(&needle),
            "production lib must not hardcode shared toolchain path"
        );
        assert!(!prod.contains("SHARED_FFMPEG_DIR"));
    }
}

