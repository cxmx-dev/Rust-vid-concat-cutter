//! vid-concat-cutter — single GUI for **Cut** and **Concat** modes.
//!
//! GUI: eframe/egui, minimalist dark + teal.
//! Media: local `engine/ffmpeg.exe` + `engine/ffprobe.exe` (any format they support).
//! - **Cut**: non-destructive timeline (split / DEL / cut-paste / drag reorder) + export
//! - **Concat**: port of Concat (Python) jobs — join, add audio loop, speed, convert
//! Never edit or delete the `backup/` folder (AGENTS.md).

use eframe::egui;
use engine::{
    add_audio, concatenate, convert_video, default_audio_output, default_concat_output,
    default_speed_output, latest_mp4_in, list_audio_in_dir, list_videos_in_dir, speed_up,
    AddAudioOptions, Clip, ConcatOptions, ConvertFormat, SpeedMode, VIDEO_EXTENSIONS, VideoProject,
    extract_frame, ffmpeg_command, ffmpeg_exe, load_project, sanitize_path,
};
use image::{imageops, RgbaImage};
use rodio::Source;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum AppMode {
    #[default]
    Cut,
    Concat,
}

/// Background Concat-mode job result.
enum ConcatJobResult {
    Ok(String),
    Err(String),
}

struct ConcatPanel {
    files: Vec<String>,
    selected_index: Option<usize>,
    audio_path: Option<String>,
    reverse_order: bool,
    reverse_video: bool,
    reverse_audio: bool,
    speed_95: bool,
    speed_factor: f64,
    convert_fmt: ConvertFormat,
    status: String,
    busy: bool,
    job_rx: Option<Receiver<ConcatJobResult>>,
}

impl Default for ConcatPanel {
    fn default() -> Self {
        Self {
            files: vec![],
            selected_index: None,
            audio_path: None,
            reverse_order: false,
            reverse_video: false,
            reverse_audio: false,
            speed_95: true,
            speed_factor: 1.8,
            convert_fmt: ConvertFormat::Mp4,
            status: "Load in/ + audio/ · CONCAT | Add Audio | Speed | Convert".to_string(),
            busy: false,
            job_rx: None,
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_position(egui::Pos2::new((2560.0 - 1280.0) / 2.0, (1440.0 - 800.0) / 2.0))
            .with_title("Vid Concat + Cutter (engine/ffmpeg)"),
        ..Default::default()
    };
    eframe::run_native(
        "vid-concat-cutter",
        options,
        Box::new(|cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.window_fill = egui::Color32::from_rgb(18, 18, 18);
            visuals.panel_fill = egui::Color32::from_rgb(26, 26, 26);
            visuals.selection.bg_fill = egui::Color32::from_rgb(0, 200, 180); // teal
            visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(40, 40, 40);
            visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(60, 60, 60);
            cc.egui_ctx.set_visuals(visuals);
            Ok(Box::new(MyApp::default()))
        }),
    )
}

struct MyApp {
    mode: AppMode,
    concat: ConcatPanel,
    project: Option<VideoProject>,
    selected: HashSet<usize>,
    clipboard: Option<Vec<Clip>>,
    dragged: Option<usize>,
    undo_stack: Vec<VideoProject>,
    in_dir: PathBuf,
    out_dir: PathBuf,
    auto_loaded: bool,
    play_head: f64,
    is_playing: bool,
    ffwd: bool,
    rewind: bool,
    clip_visuals: Vec<Vec<egui::Color32>>,
    clip_thumbnails: Vec<egui::TextureHandle>,
    needs_thumbnail_update: bool,
    thumbnail_sender: Option<std::sync::mpsc::Sender<Vec<RgbaImage>>>,
    thumbnail_receiver: Option<std::sync::mpsc::Receiver<Vec<RgbaImage>>>,
    current_preview_tex: Option<egui::TextureHandle>,
    last_preview_time: f64,
    video_bytes: Option<Vec<u8>>,
    audio_needs_restart: bool,
    audio_stream: Option<rodio::OutputStream>,
    audio_sink: Option<rodio::Sink>,
    audio_wav: Option<PathBuf>,
}

impl Default for MyApp {
    fn default() -> Self {
        let in_dir = PathBuf::from("in");
        let out_dir = PathBuf::from("out");
        let _ = std::fs::create_dir_all(&out_dir);
        let _ = std::fs::create_dir_all(&in_dir);
        let _ = std::fs::create_dir_all("audio");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<RgbaImage>>();
        Self {
            mode: AppMode::Cut,
            concat: ConcatPanel::default(),
            project: None,
            selected: HashSet::new(),
            clipboard: None,
            dragged: None,
            undo_stack: vec![],
            in_dir,
            out_dir,
            auto_loaded: false,
            play_head: 0.0,
            is_playing: false,
            ffwd: false,
            rewind: false,
            clip_visuals: vec![],
            clip_thumbnails: vec![],
            needs_thumbnail_update: false,
            current_preview_tex: None,
            last_preview_time: -1.0,
            video_bytes: None,
            audio_needs_restart: false,
            audio_stream: None,
            audio_sink: None,
            audio_wav: None,
            thumbnail_sender: Some(tx),
            thumbnail_receiver: Some(rx),
        }
    }
}

impl MyApp {
    fn poll_concat_job(&mut self, ctx: &egui::Context) {
        let done = if let Some(rx) = &self.concat.job_rx {
            match rx.try_recv() {
                Ok(ConcatJobResult::Ok(msg)) => {
                    self.concat.status = msg;
                    self.concat.busy = false;
                    true
                }
                Ok(ConcatJobResult::Err(e)) => {
                    self.concat.status = format!("Error: {e}");
                    self.concat.busy = false;
                    true
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint();
                    false
                }
                Err(TryRecvError::Disconnected) => {
                    self.concat.status = "Job channel closed".into();
                    self.concat.busy = false;
                    true
                }
            }
        } else {
            false
        };
        if done {
            self.concat.job_rx = None;
        }
    }

    fn spawn_concat_job<F>(&mut self, status: String, work: F)
    where
        F: FnOnce() -> Result<String, String> + Send + 'static,
    {
        if self.concat.busy {
            self.concat.status = "Busy — wait for current job".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.concat.job_rx = Some(rx);
        self.concat.busy = true;
        self.concat.status = status;
        std::thread::spawn(move || {
            let msg = match work() {
                Ok(s) => ConcatJobResult::Ok(s),
                Err(e) => ConcatJobResult::Err(e),
            };
            let _ = tx.send(msg);
        });
    }

    fn concat_paths(&self) -> Vec<PathBuf> {
        self.concat
            .files
            .iter()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .collect()
    }

    fn refresh_in_list(&mut self) {
        let vids = list_videos_in_dir(&self.in_dir);
        self.concat.files = vids
            .into_iter()
            .map(|p| p.display().to_string())
            .collect();
        self.concat.selected_index = None;
        self.concat.status = format!("Loaded {} from in/", self.concat.files.len());
    }

    fn refresh_audio_auto(&mut self) {
        let auds = list_audio_in_dir(Path::new("audio"));
        if let Some(a) = auds.first() {
            self.concat.audio_path = Some(a.display().to_string());
            self.concat.status = format!("Audio: {}", a.display());
        } else {
            self.concat.status = "No audio in audio/ — Select Audio or drop a file".into();
        }
    }

    fn job_concatenate(&mut self) {
        let paths = self.concat_paths();
        if paths.is_empty() {
            self.concat.status = "No videos — Refresh from in/ or Select Videos".into();
            return;
        }
        let out_dir = self.out_dir.clone();
        let opts = ConcatOptions {
            reverse_order: self.concat.reverse_order,
            reverse_video: self.concat.reverse_video,
            reverse_audio: self.concat.reverse_audio,
            prefer_stream_copy: !(self.concat.reverse_video || self.concat.reverse_audio),
        };
        let n = paths.len();
        self.spawn_concat_job(format!("CONCAT {n} clip(s)…"), move || {
            let out = default_concat_output(&out_dir);
            concatenate(&paths, &out, &opts).map_err(|e| e.to_string())?;
            Ok(format!("CONCAT done → {}", out.display()))
        });
    }

    fn job_add_audio(&mut self) {
        let paths = self.concat_paths();
        if paths.is_empty() {
            self.concat.status = "No videos for Add Audio".into();
            return;
        }
        let audio = match &self.concat.audio_path {
            Some(a) if Path::new(a).is_file() => PathBuf::from(a),
            _ => {
                self.concat.status = "Pick audio (audio/ or Select Audio)".into();
                return;
            }
        };
        let out_dir = self.out_dir.clone();
        let opts = AddAudioOptions {
            reverse_order: self.concat.reverse_order,
            reverse_video: self.concat.reverse_video,
            reverse_audio: self.concat.reverse_audio,
            fps: 30,
        };
        let rev_a = self.concat.reverse_audio;
        self.spawn_concat_job(
            format!("Add Audio (loop visuals → {})…", audio.display()),
            move || {
                let out = default_audio_output(&out_dir, rev_a);
                add_audio(&paths, &audio, &out, &opts).map_err(|e| e.to_string())?;
                Ok(format!("Add Audio done → {}", out.display()))
            },
        );
    }

    fn job_speed(&mut self) {
        let out_dir = self.out_dir.clone();
        let mode = if self.concat.speed_95 {
            SpeedMode::FitSeconds(9.5)
        } else {
            SpeedMode::Factor(self.concat.speed_factor.max(0.1))
        };
        // Prefer selected list item; else latest out/ for 9.5s mode (Concat behavior)
        let input = if let Some(idx) = self.concat.selected_index {
            self.concat
                .files
                .get(idx)
                .map(PathBuf::from)
                .filter(|p| p.is_file())
        } else if self.concat.speed_95 {
            latest_mp4_in(&out_dir)
        } else {
            self.concat
                .files
                .first()
                .map(PathBuf::from)
                .filter(|p| p.is_file())
                .or_else(|| latest_mp4_in(&out_dir))
        };
        let Some(input) = input else {
            self.concat.status =
                "Speed: select a clip or put an mp4 in out/ (9.5s uses latest out/)".into();
            return;
        };
        let label = if self.concat.speed_95 {
            "9.5s".to_string()
        } else {
            format!("{:.1}x", self.concat.speed_factor)
        };
        self.spawn_concat_job(format!("Speed {label} ← {}…", input.display()), move || {
            let out = default_speed_output(&out_dir, &input);
            speed_up(&input, &out, mode).map_err(|e| e.to_string())?;
            Ok(format!("Speed done → {}", out.display()))
        });
    }

    fn job_convert(&mut self) {
        let input = if let Some(idx) = self.concat.selected_index {
            self.concat.files.get(idx).map(PathBuf::from)
        } else {
            self.concat.files.first().map(PathBuf::from)
        };
        let Some(input) = input.filter(|p| p.is_file()) else {
            self.concat.status = "Convert: select a video in the list".into();
            return;
        };
        let fmt = self.concat.convert_fmt;
        let out_dir = self.out_dir.clone();
        self.spawn_concat_job(
            format!("Convert → {} …", fmt.label()),
            move || {
                let out = convert_video(&input, &out_dir, fmt).map_err(|e| e.to_string())?;
                Ok(format!("Convert done → {}", out.display()))
            },
        );
    }

    fn ui_mode_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mode:").strong());
            let cut_sel = self.mode == AppMode::Cut;
            let concat_sel = self.mode == AppMode::Concat;
            if ui
                .selectable_label(cut_sel, "✂ Cut")
                .on_hover_text("Interactive timeline cutter")
                .clicked()
            {
                self.mode = AppMode::Cut;
            }
            if ui
                .selectable_label(concat_sel, "⧉ Concat")
                .on_hover_text("Join / Add Audio / Speed / Convert (Concat port)")
                .clicked()
            {
                if self.mode != AppMode::Concat {
                    self.is_playing = false;
                    self.ffwd = false;
                    self.rewind = false;
                    if let Some(sink) = &self.audio_sink {
                        sink.pause();
                    }
                    // Auto-load in/ + audio/ when entering Concat
                    if self.concat.files.is_empty() {
                        self.refresh_in_list();
                    }
                    if self.concat.audio_path.is_none() {
                        self.refresh_audio_auto();
                    }
                }
                self.mode = AppMode::Concat;
            }
            ui.separator();
            ui.label(egui::RichText::new("Vid Concat + Cutter").strong());
            ui.label(
                egui::RichText::new(format!("|  {}", ffmpeg_exe().display()))
                    .small()
                    .color(egui::Color32::GRAY),
            );
            if self.concat.busy {
                ui.label(
                    egui::RichText::new("WORKING…")
                        .strong()
                        .color(egui::Color32::YELLOW),
                );
            }
        });
    }

    fn ui_concat(&mut self, ctx: &egui::Context) {
        self.poll_concat_job(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Concat — tools ported from Concat (Python)");
            ui.label(
                "CONCAT (join) · Add Audio (loop visuals to track) · Speed (9.5s / Nx) · Convert. \
                 Whisper Auto CC not ported (optional later).",
            );
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                let can = !self.concat.busy;
                if ui.add_enabled(can, egui::Button::new("Refresh from in/")).clicked() {
                    self.refresh_in_list();
                }
                if ui.add_enabled(can, egui::Button::new("Select Videos…")).clicked() {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter("Video", VIDEO_EXTENSIONS)
                        .set_directory(&self.in_dir)
                        .pick_files()
                    {
                        self.concat.files = paths
                            .into_iter()
                            .map(|p| p.display().to_string())
                            .collect();
                        self.concat.selected_index = None;
                        self.concat.status =
                            format!("{} file(s) selected", self.concat.files.len());
                    }
                }
                if ui.add_enabled(can, egui::Button::new("Select Audio…")).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Audio", &["mp3", "wav", "aac", "m4a", "flac", "ogg", "opus"])
                        .set_directory("audio")
                        .pick_file()
                    {
                        self.concat.audio_path = Some(path.display().to_string());
                        self.concat.status = format!("Audio: {}", path.display());
                    }
                }
                if ui.add_enabled(can, egui::Button::new("Auto audio/")).clicked() {
                    self.refresh_audio_auto();
                }
                if ui.button("Open in/").clicked() {
                    let _ = std::fs::create_dir_all(&self.in_dir);
                    let _ = std::process::Command::new("explorer")
                        .arg(&self.in_dir)
                        .spawn();
                }
                if ui.button("Open out/").clicked() {
                    let _ = std::fs::create_dir_all(&self.out_dir);
                    let _ = std::process::Command::new("explorer")
                        .arg(&self.out_dir)
                        .spawn();
                }
                if ui.add_enabled(can, egui::Button::new("Clear")).clicked() {
                    self.concat.files.clear();
                    self.concat.selected_index = None;
                    self.concat.status = "List cleared".into();
                }
            });

            ui.label(
                egui::RichText::new(format!(
                    "Audio: {}",
                    self.concat
                        .audio_path
                        .as_deref()
                        .unwrap_or("(none — needed for Add Audio)")
                ))
                .small()
                .color(egui::Color32::from_rgb(180, 220, 180)),
            );

            ui.add_space(4.0);
            ui.label(egui::RichText::new("Clip list (click = select for Speed/Convert)").small());
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    if self.concat.files.is_empty() {
                        ui.label("Empty — Refresh from in/ or Select Videos.");
                    } else {
                        for i in 0..self.concat.files.len() {
                            let label = self.concat.files[i].clone();
                            let selected = self.concat.selected_index == Some(i);
                            let text = format!("{}. {}", i + 1, label);
                            if ui.selectable_label(selected, text).clicked() {
                                self.concat.selected_index = Some(i);
                            }
                        }
                    }
                });

            ui.horizontal(|ui| {
                let can = !self.concat.busy;
                if ui.add_enabled(can, egui::Button::new("Move Up")).clicked() {
                    if let Some(idx) = self.concat.selected_index {
                        if idx > 0 {
                            self.concat.files.swap(idx, idx - 1);
                            self.concat.selected_index = Some(idx - 1);
                        }
                    }
                }
                if ui.add_enabled(can, egui::Button::new("Move Down")).clicked() {
                    if let Some(idx) = self.concat.selected_index {
                        if idx + 1 < self.concat.files.len() {
                            self.concat.files.swap(idx, idx + 1);
                            self.concat.selected_index = Some(idx + 1);
                        }
                    }
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.concat.reverse_order, "Reverse Order");
                ui.checkbox(&mut self.concat.reverse_video, "Reverse Video");
                ui.checkbox(&mut self.concat.reverse_audio, "Reverse Audio");
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.concat.speed_95, "9.5 sec Speed mode");
                if !self.concat.speed_95 {
                    ui.add(
                        egui::DragValue::new(&mut self.concat.speed_factor)
                            .speed(0.05)
                            .range(0.1..=16.0)
                            .prefix("factor "),
                    );
                }
                ui.label("Convert:");
                egui::ComboBox::from_id_salt("convert_fmt")
                    .selected_text(self.concat.convert_fmt.label())
                    .show_ui(ui, |ui| {
                        for f in ConvertFormat::all() {
                            ui.selectable_value(&mut self.concat.convert_fmt, *f, f.label());
                        }
                    });
            });

            ui.add_space(8.0);
            let can = !self.concat.busy;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        can,
                        egui::Button::new(
                            egui::RichText::new("CONCAT")
                                .strong()
                                .color(egui::Color32::BLACK),
                        )
                        .fill(egui::Color32::from_rgb(230, 200, 40)),
                    )
                    .on_hover_text("Join list → out/combined_*.mp4")
                    .clicked()
                {
                    self.job_concatenate();
                }
                if ui
                    .add_enabled(
                        can,
                        egui::Button::new(
                            egui::RichText::new("Add Audio")
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(200, 40, 40)),
                    )
                    .on_hover_text("Loop visuals to audio length → plus-audio-combined_*.mp4")
                    .clicked()
                {
                    self.job_add_audio();
                }
                if ui
                    .add_enabled(can, egui::Button::new("Speed"))
                    .on_hover_text("9.5s fit (silent) or Nx speed")
                    .clicked()
                {
                    self.job_speed();
                }
                if ui
                    .add_enabled(
                        can,
                        egui::Button::new(
                            egui::RichText::new("CONVERT")
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(180, 40, 180)),
                    )
                    .on_hover_text("Re-encode selected clip to chosen format")
                    .clicked()
                {
                    self.job_convert();
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.label(
                egui::RichText::new(&self.concat.status)
                    .color(egui::Color32::from_rgb(0, 200, 180)),
            );
            ui.label(
                egui::RichText::new(
                    "engine/ffmpeg · robust concat (primary v/a streams) · Add Audio loops to track duration",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
        });
    }

    fn recompute_clip_visuals(&mut self) {
        // Stable per-clip "thumbnails" for the timeline strips.
        // Sample 6 fixed points inside each clip from the cached video bytes.
        // This gives each segment a consistent visual fingerprint instead of flickering noise.
        // Pure visualization (H.264 not decoded). Double-down on patent-free pure-Rust mode.
        self.clip_visuals = Self::compute_visuals(
            &self.project.as_ref().map(|p| p.clips.clone()).unwrap_or_default(),
            &self.video_bytes,
            self.project.as_ref().map(|p| p.duration).unwrap_or(0.0),
        );
    }

    fn compute_visuals(clips: &[Clip], bytes: &Option<Vec<u8>>, duration: f64) -> Vec<Vec<egui::Color32>> {
        // Pure helper (no &mut self) so it can be called while &mut proj borrow is live inside CentralPanel.
        let mut out = vec![];
        if let Some(b) = bytes {
            if !b.is_empty() && !clips.is_empty() {
                let total = duration.max(1.0);
                for clip in clips {
                    let mut cols = vec![];
                    let c_dur = (clip.end - clip.start).max(0.001);
                    for s in 0..6 {
                        let f = s as f64 / 5.0;
                        let t = clip.start + f * c_dur;
                        let tnorm = (t / total).clamp(0.0, 0.999);
                        let idx = ((tnorm * b.len() as f64) as usize) % b.len();
                        let r = b[idx % b.len()];
                        let g = b[(idx + 1) % b.len()];
                        let bb = b[(idx + 2) % b.len()];
                        cols.push(egui::Color32::from_rgb(r, g, bb));
                    }
                    out.push(cols);
                }
            }
        }
        out
    }

    fn assembled_duration(clips: &[Clip]) -> f64 {
        clips.iter().map(|c| (c.end - c.start).max(0.0)).sum()
    }

    fn source_time_for_playhead(clips: &[Clip], play_head: f64) -> f64 {
        let mut pos = play_head.max(0.0);
        for c in clips {
            let cd = (c.end - c.start).max(0.0001);
            if pos <= cd {
                return c.start + pos;
            }
            pos -= cd;
        }
        clips.last().map(|c| c.end).unwrap_or(0.0)
    }

    /// Extract real audio track to a temp WAV (local ffmpeg) for rodio playback + seek.
    fn extract_audio_to_wav(&mut self, video_path: &std::path::Path) -> anyhow::Result<()> {
        let wav = std::env::temp_dir().join(format!(
            "vc_audio_{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        let mut cmd = ffmpeg_command();
        cmd.input(sanitize_path(video_path))
            .args(["-vn", "-acodec", "pcm_s16le", "-ar", "44100", "-ac", "2", "-y"])
            .output(sanitize_path(&wav));
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("ffmpeg audio extract spawn: {}", e))?;
        let _ = child.wait();
        if wav.exists() {
            self.audio_wav = Some(wav);
        }
        Ok(())
    }

    fn restart_audio_from_current(&mut self) {
        if let Some(p) = &self.project {
            if let (Some(sink), Some(wav)) = (&self.audio_sink, &self.audio_wav) {
                sink.stop();
                let src_t = Self::source_time_for_playhead(&p.clips, self.play_head);
                if let Ok(file) = std::fs::File::open(wav) {
                    if let Ok(decoder) = rodio::Decoder::new(file) {
                        let source =
                            decoder.skip_duration(std::time::Duration::from_secs_f64(src_t));
                        sink.append(source);
                        if self.is_playing {
                            sink.play();
                        }
                    }
                }
            }
        }
    }

    /// Non-destructive EDL (JSON + TXT) to out/.
    fn export_edl(&self, also_print_note: bool) {
        if let Some(p) = &self.project {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let assembled = Self::assembled_duration(&p.clips);

            let json_path = self.out_dir.join(format!("edit-list-{}.json", ts));
            let mut clips_json = String::new();
            for (i, c) in p.clips.iter().enumerate() {
                if i > 0 {
                    clips_json.push(',');
                }
                clips_json.push_str(&format!(
                    "\n    {{\"start\": {:.3}, \"end\": {:.3}}}",
                    c.start, c.end
                ));
            }
            let json_content = format!(
                "{{\n  \"source\": \"{}\",\n  \"source_duration\": {:.3},\n  \"assembled_duration\": {:.3},\n  \"clips\": [{} \n  ],\n  \"exported_at\": {},\n  \"engine\": \"ffmpeg-local\",\n  \"note\": \"Non-destructive edit list. Use Export Video for a remuxed .mp4 of kept ranges.\"\n}}",
                p.source_path.display(),
                p.duration,
                assembled,
                clips_json,
                ts
            );
            let _ = std::fs::write(&json_path, &json_content);

            let txt_path = self.out_dir.join(format!("edit-list-{}.txt", ts));
            let mut txt = format!(
                "Source: {}\nSource duration: {:.2}s\nAssembled (edited) duration: {:.2}s\nEngine: local ffmpeg\n\nKept clip list (EDL):\n",
                p.source_path.display(),
                p.duration,
                assembled
            );
            for (i, c) in p.clips.iter().enumerate() {
                txt.push_str(&format!(
                    "Clip {}: {:.2}s - {:.2}s (dur {:.2}s)\n",
                    i,
                    c.start,
                    c.end,
                    c.end - c.start
                ));
            }
            let _ = std::fs::write(&txt_path, &txt);

            if also_print_note {
                println!("\n=== EDL EXPORT ===");
                println!("JSON: {}", json_path.display());
                println!("TXT : {}", txt_path.display());
            }
        }
    }

    /// Remux kept clips to a real edited .mp4 in out/ (stream copy via local ffmpeg).
    fn export_edited_video_only(&mut self) {
        if let Some(p) = &self.project {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let video_name = p
                .source_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let video_out = self
                .out_dir
                .join(format!("{}-edited-{}.mp4", video_name, ts));

            let temp_dir = std::env::temp_dir();
            let mut segment_paths: Vec<std::path::PathBuf> = vec![];
            for (i, c) in p.clips.iter().enumerate() {
                let seg_path = temp_dir.join(format!("vc_seg_{}_{}.mp4", ts, i));
                let mut cmd = ffmpeg_command();
                cmd.overwrite()
                    .input(sanitize_path(&p.source_path))
                    .args(["-ss", &format!("{:.3}", c.start)])
                    .args(["-to", &format!("{:.3}", c.end)])
                    .args(["-c", "copy"])
                    .output(sanitize_path(&seg_path));
                if let Ok(mut child) = cmd.spawn() {
                    let _ = child.wait();
                    if seg_path.exists() {
                        segment_paths.push(seg_path);
                    }
                }
            }

            if segment_paths.is_empty() {
                eprintln!("No segments created for export (ffmpeg failed?)");
                return;
            }

            let list_path = temp_dir.join(format!("vc_concat_{}.txt", ts));
            let mut list_content = String::new();
            for seg in &segment_paths {
                let pstr = sanitize_path(seg).replace('\'', "'\\''");
                list_content.push_str(&format!("file '{}'\n", pstr));
            }
            let _ = std::fs::write(&list_path, list_content);

            let mut cmd = ffmpeg_command();
            cmd.overwrite()
                .args(["-f", "concat", "-safe", "0"])
                .input(sanitize_path(&list_path))
                .args(["-c", "copy"])
                .output(sanitize_path(&video_out));
            if let Ok(mut child) = cmd.spawn() {
                let _ = child.wait();
            }
            for pth in segment_paths {
                let _ = std::fs::remove_file(pth);
            }
            let _ = std::fs::remove_file(&list_path);

            if video_out.exists() {
                println!("Exported edited video to: {}", video_out.display());
            } else {
                eprintln!("Export finished but output missing: {}", video_out.display());
            }
        }
    }

    fn trigger_thumbnail_regen(&mut self, clips: &[Clip], source_path: &std::path::Path, duration: f64) {
        if let Some(tx) = &self.thumbnail_sender {
            let clips = clips.to_vec();
            let path = source_path.to_path_buf();
            let dur = duration;
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut thumbs = vec![];
                for clip in &clips {
                    let num_f = 3; // reduced to 3 for <1s perf on edits
                    let fw = 64u32;
                    let fh = 36u32;
                    let mut comp = RgbaImage::new(fw * num_f as u32, fh);
                    let cdur = (clip.end - clip.start).max(0.001);
                    for k in 0..num_f {
                        let f = if num_f > 1 { k as f64 / (num_f - 1) as f64 } else { 0.0 };
                        let t = clip.start + f * cdur;
                        if let Ok(rgba) = extract_frame(&path, t, dur, fw) {
                            let small = imageops::resize(&rgba, fw, fh, imageops::FilterType::Triangle);
                            imageops::replace(&mut comp, &small, k as i64 * fw as i64, 0);
                        }
                    }
                    thumbs.push(comp);
                }
                let _ = tx.send(thumbs);
            });
        }
        self.needs_thumbnail_update = false;
    }
}

impl eframe::App for MyApp {
    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {} // dummy for some eframe versions if required by trait
    #[allow(deprecated)]
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Auto-detect videos in 'in/' folder at start (Cut mode only).
        // Falls back to browse button for other files. Scans common video exts.
        if self.mode == AppMode::Cut && !self.auto_loaded && self.project.is_none() {
            if let Ok(entries) = std::fs::read_dir(&self.in_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_file() && engine::is_video_path(&path) {
                        match engine::load_project(&path) {
                            Ok(proj) => {
                                self.project = Some(proj);
                                self.selected.clear();
                                self.clipboard = None;
                                self.play_head = 0.0;
                                self.is_playing = false;
                                self.ffwd = false;
                                self.rewind = false;
                                self.current_preview_tex = None;
                                self.last_preview_time = -1.0;
                                self.video_bytes = std::fs::read(&path).ok();
                                self.recompute_clip_visuals();
                                self.audio_stream = None;
                                self.audio_sink = None;
                                self.audio_wav = None;
                                let _ = self.extract_audio_to_wav(&path);
                                self.needs_thumbnail_update = true;
                                self.auto_loaded = true;
                                println!(
                                    "Auto-loaded from in/: {} (ffmpeg={})",
                                    path.display(),
                                    engine::ffmpeg_exe().display()
                                );
                                break;
                            }
                            Err(e) => {
                                eprintln!("Auto-load failed for {}: {}", path.display(), e);
                            }
                        }
                    }
                }
            }
            self.auto_loaded = true; // mark done even if none found
        }

        // Cut-mode only: transport, audio, preview, filmstrips.
        if self.mode == AppMode::Cut && self.is_playing {
            if let Some(p) = &self.project {
                let mut rate = 1.0f64;
                if self.rewind {
                    rate = -3.0; // rewind speed (negative)
                } else if self.ffwd {
                    rate = 5.0; // fast forward speed
                }
                self.play_head += (ctx.input(|i| i.stable_dt) as f64) * rate;
                let assembled = Self::assembled_duration(&p.clips);
                if self.play_head > assembled {
                    self.play_head = assembled;
                    self.is_playing = false;
                    self.ffwd = false;
                    self.rewind = false;
                }
                if self.play_head < 0.0 {
                    self.play_head = 0.0;
                    self.is_playing = false;
                    self.ffwd = false;
                    self.rewind = false;
                }
            }
            ctx.request_repaint();
        }

        // Audio output (real track extracted via local ffmpeg → rodio).
        if self.mode == AppMode::Cut && self.audio_stream.is_none() {
            if let Ok((stream, handle)) = rodio::OutputStream::try_default() {
                if let Ok(sink) = rodio::Sink::try_new(&handle) {
                    sink.pause();
                    self.audio_sink = Some(sink);
                    self.audio_stream = Some(stream);
                }
            }
        }
        if self.mode == AppMode::Cut {
            if let Some(sink) = &self.audio_sink {
                if self.is_playing {
                    sink.play();
                } else {
                    sink.pause();
                }
            }
        }

        // Real decoded preview frame at playhead (local ffmpeg).
        if self.mode == AppMode::Cut {
        if let Some(p) = &self.project {
            let mut pos = self.play_head;
            let mut src_time = p.clips.last().map(|c| c.start).unwrap_or(0.0);
            for c in &p.clips {
                let cdur = (c.end - c.start).max(0.0001);
                if pos <= cdur {
                    src_time = c.start + pos;
                    break;
                }
                pos -= cdur;
            }
            // Better preview behavior for usability:
            // - tighter during FFWD/REWIND (fast movement)
            // - looser when just playing (less chaotic)
            // - very loose / stable when idle (no rapid noise)
            let throttle = if self.ffwd || self.rewind {
                0.025
            } else if self.is_playing {
                0.09
            } else {
                0.5
            };
            if (src_time - self.last_preview_time).abs() > throttle || self.current_preview_tex.is_none() {
                if let Ok(rgba) = extract_frame(&p.source_path, src_time, p.duration, 320) {
                    let cimg = egui::ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    );
                    self.current_preview_tex = Some(ctx.load_texture(
                        "video-preview",
                        cimg,
                        Default::default(),
                    ));
                    self.last_preview_time = src_time;
                }
            }

            // If flag (set by mutations in loads/central/keyboard/script), spawn background th here (safe, no active proj borrow from panels).
            // Poll any completed from threads. Makes edits fast.
            if self.needs_thumbnail_update {
                let (clips, src, dur) = if let Some(p) = &self.project {
                    (p.clips.clone(), p.source_path.clone(), p.duration)
                } else {
                    (vec![], std::path::PathBuf::new(), 0.0)
                };
                self.trigger_thumbnail_regen(&clips, &src, dur);
                self.needs_thumbnail_update = false;
            }
            if let Some(rx) = &self.thumbnail_receiver {
                while let Ok(thumbs) = rx.try_recv() {
                    self.clip_thumbnails.clear();
                    for comp in thumbs {
                        let cimg = egui::ColorImage::from_rgba_unmultiplied(
                            [comp.width() as usize, comp.height() as usize],
                            comp.as_raw(),
                        );
                        let tex = ctx.load_texture(
                            format!("clip_filmstrip_{}", self.clip_thumbnails.len()),
                            cimg,
                            Default::default(),
                        );
                        self.clip_thumbnails.push(tex);
                    }
                }
            }
        }
        } // end Cut-mode preview/thumbnails

        // Top bar: mode switch always; Cut tools when in Cut mode
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            self.ui_mode_bar(ui);
            if self.mode == AppMode::Cut {
                ui.horizontal(|ui| {
                    if ui.button("Open Video").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Video (any ffmpeg format)", VIDEO_EXTENSIONS)
                            .add_filter("All files", &["*"])
                            .pick_file()
                        {
                            match load_project(&path) {
                                Ok(proj) => {
                                    self.project = Some(proj);
                                    self.selected.clear();
                                    self.clipboard = None;
                                    self.play_head = 0.0;
                                    self.is_playing = false;
                                    self.ffwd = false;
                                    self.rewind = false;
                                    self.current_preview_tex = None;
                                    self.last_preview_time = -1.0;
                                    self.video_bytes = std::fs::read(&path).ok();
                                    self.recompute_clip_visuals();
                                    self.audio_stream = None;
                                    self.audio_sink = None;
                                    self.audio_wav = None;
                                    let _ = self.extract_audio_to_wav(&path);
                                    self.needs_thumbnail_update = true;
                                }
                                Err(e) => {
                                    eprintln!("Load failed: {}", e);
                                }
                            }
                        }
                    }
                    ui.label("  |  Cut mode  |  engine/ffmpeg  |  any format");
                    if ui.button("Export EDL").clicked() {
                        self.export_edl(true);
                    }
                    if ui.button("Export Video + EDL").clicked() {
                        self.export_edl(false);
                        self.export_edited_video_only();
                    }
                });
            }
        });

        if self.mode == AppMode::Concat {
            self.ui_concat(ctx);
            return;
        }

        // Left: metadata / inspector (Phase 5 polish)
        egui::SidePanel::left("left").resizable(true).show(ctx, |ui| {
            ui.heading("Metadata / Inspector");
            if let Some(p) = &self.project {
                ui.label(format!("Source: {}", p.source_path.display()));
                ui.label(format!("Total Duration: {:.1}s", p.duration));
                ui.label(format!("Clips: {}", p.clips.len()));
                ui.label(egui::RichText::new("Real frames & audio via engine/ffmpeg.exe (any format)").small().color(egui::Color32::from_rgb(100, 200, 100)));
                ui.separator();
                ui.heading("Selected");
                if !self.selected.is_empty() {
                    for &i in &self.selected {
                        if let Some(c) = p.clips.get(i) {
                            ui.label(format!("Clip {}: {:.1} - {:.1}s", i, c.start, c.end));
                            // Numeric edit (Phase 5)
                            let mut start = c.start;
                            let mut end = c.end;
                            if ui.add(egui::DragValue::new(&mut start).speed(0.01).prefix("start: ")).changed() {
                                // would clamp and update in real
                            }
                            if ui.add(egui::DragValue::new(&mut end).speed(0.01).prefix("end: ")).changed() {
                            }
                        }
                    }
                } else {
                    ui.label("No clips selected");
                }
            } else {
                ui.label("No video loaded. Use 'Open Video'.");
            }
            ui.separator();
            ui.label("DEL: delete | Ctrl+X: cut | Ctrl+V: paste (end) | Drag strips to reorder | Ctrl+Z undo | Ctrl+C copy | Ctrl+D duplicate | Arrows nav");

            // Playback controls (basic media player per user request) + preview that "shows the video" at play_head
            ui.separator();
            ui.heading("Playback (click blue timeline hud / ruler to live-preview; tabs or strips also seek)");
            ui.horizontal(|ui| {
                // Exact order per [Image #2] annotations: STOP > << REWIND > FFWD > PLAY
                // PLAY toggles play/pause. FFWD and REWIND are toggles that set fast/rewind rates.
                // Toggling FFWD/REWIND on will start/resume play at that rate. Mutually exclusive.
                if ui.button("⏹ Stop").clicked() {
                    self.is_playing = false;
                    self.play_head = 0.0;
                    self.ffwd = false;
                    self.rewind = false;
                    if let Some(sink) = &self.audio_sink {
                        sink.pause();
                        sink.stop();
                    }
                }
                let rew_text = if self.rewind { "⏪ REW" } else { "<< REW" };
                if ui.button(rew_text).clicked() {
                    self.rewind = !self.rewind;
                    if self.rewind {
                        self.ffwd = false;
                        self.is_playing = true;
                    }
                    self.last_preview_time = -999.0; // force live preview update
                    ctx.request_repaint();
                }
                let ff_text = if self.ffwd { "⏩ FFWD" } else { "FFWD" };
                if ui.button(ff_text).clicked() {
                    self.ffwd = !self.ffwd;
                    if self.ffwd {
                        self.rewind = false;
                        self.is_playing = true;
                    }
                    self.last_preview_time = -999.0; // force live preview update
                    ctx.request_repaint();
                }
                let play_text = if self.is_playing { "⏸ Pause" } else { "▶ Play" };
                if ui.button(play_text).clicked() {
                    self.is_playing = !self.is_playing;
                    if self.is_playing {
                        self.audio_needs_restart = true;
                    } else {
                        if let Some(sink) = &self.audio_sink {
                            sink.pause();
                        }
                        self.ffwd = false;
                        self.rewind = false;
                    }
                }
            });
            if let Some(p) = &self.project {
                let assembled = Self::assembled_duration(&p.clips);
                ui.label(format!("{:.1} / {:.1} s", self.play_head, assembled));
            }

            // Preview: macroblock visualization derived from raw file bytes at the current play position.
            // Stable/less noisy during slow play; forces update on blue-hud clicks and speed changes.
            // This is the permanent visualization for H.264 sources in pure-Rust patent-free mode.
            ui.separator();
            ui.label(egui::RichText::new("Preview (play position) — real decoded frame").small().color(egui::Color32::from_rgb(100, 200, 100)));
            ui.label(egui::RichText::new("engine/ffmpeg.exe + ffprobe.exe").small().color(egui::Color32::GRAY));
            let preview_size = egui::vec2(220.0, 124.0);
            if let Some(tex) = &self.current_preview_tex {
                ui.add(egui::Image::new(tex).fit_to_exact_size(preview_size));
            } else {
                ui.add_sized(preview_size, egui::Label::new(egui::RichText::new("REAL FRAME\n(play or scrub)").size(10.0)));
            }
        });

        // Right: Synced Script View (powerful EDL-style per Q+A clarification, stays in sync with visual strips)
        egui::SidePanel::right("script").resizable(true).show(ctx, |ui| {
            ui.heading("Timeline Script (Synced EDL-style view)");
            ui.label("Edit times here or in visual - they sync. Powerful list view for the edit script.");
            if let Some(p) = &mut self.project {
                let original = p.clone();
                let mut clips = p.clips.clone();
                let mut changed = false;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for i in 0..clips.len() {
                        ui.horizontal(|ui| {
                            ui.label(format!("Clip {}", i));
                            let mut c = clips[i].clone();
                            // Finer speed for micro-edits (e.g. precisely cut the last 4s of your 8s video).
                            let changed_start = ui.add(egui::DragValue::new(&mut c.start).speed(0.01).prefix("start ")).changed();
                            let changed_end = ui.add(egui::DragValue::new(&mut c.end).speed(0.01).prefix("end ")).changed();
                            if changed_start || changed_end {
                                changed = true;
                                clips[i].start = c.start;
                                clips[i].end = c.end;
                            }
                            if ui.button("Select").clicked() {
                                self.selected.clear();
                                self.selected.insert(i);
                            }
                        });
                    }
                });
                if changed {
                    self.undo_stack.push(original);
                    if self.undo_stack.len() > 20 { self.undo_stack.remove(0); }
                    p.clips = clips;
                    self.clip_visuals = Self::compute_visuals(&p.clips, &self.video_bytes, p.duration);
                    self.needs_thumbnail_update = true;
                }
            } else {
                ui.label("Load a video to see the synced script.");
            }
        });

        // Central: the interactive sequencer (core of Phases 1-3) - continuous timeline ruler with segments (per Q+A clarification)
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Interactive Timeline (Continuous Ruler with Framed Horizontal Segments / Clip Sequencer)");
            ui.label("Blue scale = live preview scrubber (click/drag). Lower strips = split / drag / tabs. Real filmstrip frames from local ffmpeg.");
            ui.add_space(32.0); // push ruler + time markers down (so green numbers fully visible, per screenshot annotation)

            if let Some(proj) = &mut self.project {
                let mut new_clips = proj.clips.clone();
                let strip_h = 72.0;
                let gap = 2.0; // small gap for continuous feel (between clips)
                let available_w = ui.available_width();
                let assembled_dur = Self::assembled_duration(&proj.clips).max(1.0);
                let ruler_y = ui.cursor().top();

                // Draw continuous ruler background and time scale FIRST (this is the "blue outlined timeline hud")
                let ruler_rect = egui::Rect::from_min_size(
                    egui::pos2(ui.cursor().left(), ruler_y),
                    egui::vec2(available_w, 20.0),
                );
                let painter = ui.painter();
                painter.rect_filled(ruler_rect, 2.0, egui::Color32::from_rgb(50, 50, 50));
                // time ticks - adaptive for short videos (e.g. your 8s video).
                // More divisions + 1 decimal place when assembled_dur is small so you can precisely target cuts like "last 4 seconds".
                // This makes the entire ruler visually match the real video length instead of being stretched for a 120s stub.
                let divisions = if assembled_dur <= 10.0 { 20 } else if assembled_dur <= 30.0 { 15 } else { 10 };
                for t in 0..=divisions {
                    let frac = t as f64 / divisions as f64;
                    let tx = ruler_rect.left() + (frac * available_w as f64) as f32;
                    painter.line_segment(
                        [egui::pos2(tx, ruler_rect.bottom()), egui::pos2(tx, ruler_rect.bottom() - 5.0)],
                        egui::Stroke::new(1.0, egui::Color32::LIGHT_GRAY),
                    );
                    // small dark background + larger green font for readable time markers
                    let label = if assembled_dur < 10.0 {
                        format!("{:.1}s", frac * assembled_dur)
                    } else {
                        format!("{:.0}s", frac * assembled_dur)
                    };
                    let text_pos = egui::pos2(tx - 14.0, ruler_rect.top());
                    painter.rect_filled(egui::Rect::from_min_size(text_pos, egui::vec2(32.0, 14.0)), 1.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 220));
                    painter.text(
                        text_pos,
                        egui::Align2::LEFT_TOP,
                        label,
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgb(0, 255, 0),
                    );
                }

                // === BLUE TIMELINE HUD (upper ruler scale) now interactive ===
                // Per [Image #2]: "make this blue outlined timeline hud be able to select a part of the video so it live previews where the user clicks"
                // Click (or drag) here scrubs the playhead and forces an immediate live preview update in the inspector.
                // Pure seek — does NOT split. Splits remain on the lower visual framed strips (the green hud).
                let ruler_response = ui.interact(ruler_rect, ui.id().with("timeline_hud"), egui::Sense::click_and_drag());
                if let Some(pos) = ruler_response.interact_pointer_pos() {
                    let frac = ((pos.x - ruler_rect.left()) / ruler_rect.width()).clamp(0.0, 1.0) as f64;
                    self.play_head = frac * assembled_dur;
                    self.last_preview_time = -999.0; // force immediate extract + tex update for live preview
                    self.audio_needs_restart = true;
                    ctx.request_repaint();
                }

                // === GREEN HUD SEPARATION ===
                // Pull the tab + colored clip segments bar (the "green annotated hud") DOWN so it does not
                // obscure the blue timeline hud / scale labels above it. Per annotations: "pull the green one down away from the blue one (timeline hud) so it is not obscured."
                // Tabs now sit clearly below the ruler scale; strips even lower.
                let ruler_bottom = ruler_y + 20.0;
                let hud_gap = 14.0; // breathing room between blue scale and green tab/segment hud
                let tab_h = 18.0;
                let tab_y = ruler_bottom + hud_gap;
                let strip_y = tab_y + tab_h + 4.0; // tabs above the visual strips, all well below blue hud

                // Playhead vertical line (red) now spans the separated huds
                let play_x = ruler_rect.left() + (self.play_head / assembled_dur * available_w as f64) as f32;
                let play_bottom = strip_y + strip_h + 4.0;
                painter.line_segment(
                    [egui::pos2(play_x, ruler_rect.top()), egui::pos2(play_x, play_bottom)],
                    egui::Stroke::new(2.5, egui::Color32::from_rgb(255, 60, 60)),
                );

                let mut current_x = ui.cursor().left();
                for i in 0..proj.clips.len() {
                    let clip = proj.clips[i].clone();
                    let clip_dur = clip.end - clip.start;
                    let strip_w = ((clip_dur / assembled_dur) * available_w as f64).max(40.0) as f32; // min for clickability
                    // Tab above each segmented split/frame (click the tab to select/DEL, left click hold/release to drag & drop/rearrange the clip)
                    let tab_rect = egui::Rect::from_min_size(
                        egui::pos2(current_x, tab_y),
                        egui::vec2(strip_w, tab_h),
                    );
                    let strip_rect = egui::Rect::from_min_size(
                        egui::pos2(current_x, strip_y),
                        egui::vec2(strip_w, strip_h),
                    );

                    let tab_response = ui.interact(tab_rect, ui.id().with("tab").with(i), egui::Sense::click_and_drag());
                    let tab_painter = ui.painter_at(tab_rect);
                    let tcol = if self.selected.contains(&i) { egui::Color32::from_rgb(0, 200, 180) } else { egui::Color32::from_rgb(70, 70, 70) };
                    tab_painter.rect_filled(tab_rect, 2.0, tcol);
                    tab_painter.text(tab_rect.center(), egui::Align2::CENTER_CENTER, format!("C{} {:.1}-{:.1}", i, clip.start, clip.end), egui::FontId::proportional(8.0), egui::Color32::WHITE);

                    if tab_response.clicked() {
                        let ctrl = ctx.input(|i| i.modifiers.ctrl);
                        if ctrl {
                            if self.selected.contains(&i) { self.selected.remove(&i); } else { self.selected.insert(i); }
                        } else {
                            self.selected.clear();
                            self.selected.insert(i);
                        }
                        // seek playhead to the *assembled* start time of this clip in the current sequence (so preview maps correctly to source bytes for "show video")
                        let mut acc = 0.0;
                        for j in 0..i {
                            acc += proj.clips[j].end - proj.clips[j].start;
                        }
                        self.play_head = acc;
                        self.last_preview_time = -999.0; // live preview at the clicked tab
                        self.audio_needs_restart = true;
                        ctx.request_repaint();
                    }
                    if tab_response.dragged() {
                        self.dragged = Some(i);
                    }
                    if tab_response.drag_stopped() {
                        if let Some(d) = self.dragged.take() {
                            if d != i {
                                self.undo_stack.push(proj.clone()); if self.undo_stack.len() > 20 { self.undo_stack.remove(0); }
                                let moving = new_clips.remove(d);
                                let insert_pos = if i > d { i } else { i };
                                new_clips.insert(insert_pos.min(new_clips.len()), moving);
                                proj.clips = new_clips.clone();
                                self.clip_visuals = Self::compute_visuals(&proj.clips, &self.video_bytes, proj.duration);
                                self.needs_thumbnail_update = true;
                                break;
                            }
                        }
                    }

                    let response = ui.interact(
                        strip_rect,
                        ui.id().with("strip").with(i),
                        egui::Sense::click_and_drag(),
                    );

                    let painter = ui.painter_at(strip_rect);

                    // Frame (framed look for segment on the ruler) - the visual "frames that get split"
                    let is_sel = self.selected.contains(&i);
                    let frame_col = if is_sel {
                        egui::Color32::from_rgb(0, 200, 180)
                    } else {
                        egui::Color32::from_rgb(80, 80, 80)
                    };
                    painter.rect_filled(strip_rect, 3.0, egui::Color32::from_rgb(35, 35, 35));
                    painter.rect_stroke(strip_rect, 3.0, egui::Stroke::new(2.0, frame_col), egui::StrokeKind::Middle);

                    // Thumbnails in the timeline strips: draw the precomputed thumbnail image for the clip
                    // (from extract at mid-point of clip) as the visual content.
                    // This makes the strips show "thumbnails" for each segment.
                    // The large preview shows live at play position.
                    let content_rect = strip_rect.shrink(6.0);
                    if let Some(tex) = self.clip_thumbnails.get(i) {
                        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                        painter.image(tex.id(), content_rect, uv, egui::Color32::WHITE);
                        // film perforations
                        for k in 0..4 {
                            let py = content_rect.top() + (k as f32 + 0.5) * (content_rect.height() / 4.0);
                            let hole = egui::Rect::from_center_size(egui::pos2(content_rect.left() + 5.0, py), egui::vec2(4.0, 3.0));
                            painter.rect_filled(hole, 0.0, egui::Color32::from_rgb(20, 20, 20));
                            let hole2 = egui::Rect::from_center_size(egui::pos2(content_rect.right() - 5.0, py), egui::vec2(4.0, 3.0));
                            painter.rect_filled(hole2, 0.0, egui::Color32::from_rgb(20, 20, 20));
                        }
                    } else {
                        painter.rect_filled(content_rect, 0.0, egui::Color32::from_rgb(60, 60, 80));
                    }

                    // Duration and time label
                    painter.text(
                        strip_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("{:.1}s", clip_dur),
                        egui::FontId::proportional(10.0),
                        egui::Color32::WHITE,
                    );

                    // Split on the visual frames (click the framed strip below the tab)
                    if response.clicked() {
                        let ctrl = ctx.input(|i| i.modifiers.ctrl);
                        if ctrl {
                            if self.selected.contains(&i) {
                                self.selected.remove(&i);
                            } else {
                                self.selected.insert(i);
                            }
                        } else {
                            self.selected.clear();
                            self.selected.insert(i);
                        }

                        // push undo
                        self.undo_stack.push(proj.clone()); if self.undo_stack.len() > 20 { self.undo_stack.remove(0); }

                        // Split on click position (exact user request) on the visual frames
                        if let Some(pos) = response.interact_pointer_pos() {
                            let frac = ((pos.x - strip_rect.left()) / strip_rect.width()).clamp(0.0, 1.0) as f64;
                            let split_t = clip.start + frac * (clip.end - clip.start);

                            // Seek playhead to the clicked time on the visual frame (standard scrub/seek)
                            let mut acc = 0.0;
                            for j in 0..i {
                                acc += proj.clips[j].end - proj.clips[j].start;
                            }
                            acc += frac * (clip.end - clip.start);
                            self.play_head = acc;
                            self.last_preview_time = -999.0; // live preview at the clicked position on the framed strip
                            ctx.request_repaint();

                            if split_t > clip.start + 0.05 && split_t < clip.end - 0.05 {
                                let mut left = clip.clone();
                                left.end = split_t;
                                let mut right = clip.clone();
                                right.start = split_t;
                                new_clips[i] = left;
                                new_clips.insert(i + 1, right);
                                proj.clips = new_clips.clone();
                                self.clip_visuals = Self::compute_visuals(&proj.clips, &self.video_bytes, proj.duration);
                                self.needs_thumbnail_update = true;
                                break;
                            }
                        }
                    }

                    // Drag also supported on the visual segment if wanted
                    if response.dragged() {
                        self.dragged = Some(i);
                    }
                    if response.drag_stopped() {
                        if let Some(d) = self.dragged.take() {
                            if d != i {
                                self.undo_stack.push(proj.clone()); if self.undo_stack.len() > 20 { self.undo_stack.remove(0); }
                                let moving = new_clips.remove(d);
                                let insert_pos = if i > d { i } else { i };
                                new_clips.insert(insert_pos.min(new_clips.len()), moving);
                                proj.clips = new_clips.clone();
                                self.clip_visuals = Self::compute_visuals(&proj.clips, &self.video_bytes, proj.duration);
                                self.needs_thumbnail_update = true;
                                break;
                            }
                        }
                    }

                    current_x += strip_w + gap;
                }

                if !new_clips.is_empty() && proj.clips.len() != new_clips.len() {
                    proj.clips = new_clips;
                }

                // Keyboard (DEL, Ctrl+X, Ctrl+V, Ctrl+C, Ctrl+D, Ctrl+Z, Arrows) - expanded per Q+A
                let input = ctx.input(|i| i.clone());
                if input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace) {
                    self.undo_stack.push(proj.clone()); if self.undo_stack.len() > 20 { self.undo_stack.remove(0); }
                    let mut idxs: Vec<_> = self.selected.iter().cloned().collect();
                    idxs.sort_unstable_by(|a, b| b.cmp(a));
                    for idx in idxs {
                        if idx < proj.clips.len() {
                            proj.clips.remove(idx);
                        }
                    }
                    self.selected.clear();
                    self.clip_visuals = Self::compute_visuals(&proj.clips, &self.video_bytes, proj.duration);
                    self.needs_thumbnail_update = true;
                }
                if input.modifiers.ctrl && input.key_pressed(egui::Key::X) {
                    if !self.selected.is_empty() {
                        self.undo_stack.push(proj.clone()); if self.undo_stack.len() > 20 { self.undo_stack.remove(0); }
                        let mut cut: Vec<Clip> = self
                            .selected
                            .iter()
                            .filter_map(|&i| proj.clips.get(i).cloned())
                            .collect();
                        cut.sort_by_key(|c| c.start as i32);
                        self.clipboard = Some(cut);
                        let mut idxs: Vec<_> = self.selected.iter().cloned().collect();
                        idxs.sort_unstable_by(|a, b| b.cmp(a));
                        for idx in idxs {
                            if idx < proj.clips.len() {
                                proj.clips.remove(idx);
                            }
                        }
                        self.selected.clear();
                        self.clip_visuals = Self::compute_visuals(&proj.clips, &self.video_bytes, proj.duration);
                        self.needs_thumbnail_update = true;
                    }
                }
                if input.modifiers.ctrl && input.key_pressed(egui::Key::V) {
                    if let Some(cut) = &self.clipboard {
                        self.undo_stack.push(proj.clone()); if self.undo_stack.len() > 20 { self.undo_stack.remove(0); }
                        proj.clips.extend_from_slice(cut);
                        self.clip_visuals = Self::compute_visuals(&proj.clips, &self.video_bytes, proj.duration);
                        self.needs_thumbnail_update = true;
                    }
                }
                if input.modifiers.ctrl && input.key_pressed(egui::Key::C) {
                    if !self.selected.is_empty() {
                        let mut clips = vec![];
                        for &i in &self.selected {
                            if let Some(c) = proj.clips.get(i) {
                                clips.push(c.clone());
                            }
                        }
                        self.clipboard = Some(clips);
                    }
                }
                if input.modifiers.ctrl && input.key_pressed(egui::Key::D) {
                    if !self.selected.is_empty() {
                        self.undo_stack.push(proj.clone()); if self.undo_stack.len() > 20 { self.undo_stack.remove(0); }
                        let mut to_add = vec![];
                        for &i in &self.selected {
                            if let Some(c) = proj.clips.get(i) {
                                to_add.push(c.clone());
                            }
                        }
                        let max_i = *self.selected.iter().max().unwrap_or(&0);
                        proj.clips.splice((max_i + 1)..(max_i + 1), to_add);
                        self.clip_visuals = Self::compute_visuals(&proj.clips, &self.video_bytes, proj.duration);
                        self.needs_thumbnail_update = true;
                    }
                }
                if input.modifiers.ctrl && input.key_pressed(egui::Key::Z) {
                    if !self.undo_stack.is_empty() {
                        if let Some(old) = self.undo_stack.pop() {
                            *proj = old;
                        }
                        self.selected.clear();
                        self.clip_visuals = Self::compute_visuals(&proj.clips, &self.video_bytes, proj.duration);
                        self.needs_thumbnail_update = true;
                    }
                }
                if input.key_pressed(egui::Key::ArrowRight) {
                    if let Some(&current) = self.selected.iter().max() {
                        let next = if current + 1 < proj.clips.len() { current + 1 } else { current };
                        self.selected.clear();
                        self.selected.insert(next);
                    }
                }
                if input.key_pressed(egui::Key::ArrowLeft) {
                    if let Some(&current) = self.selected.iter().min() {
                        let prev = if current > 0 { current - 1 } else { current };
                        self.selected.clear();
                        self.selected.insert(prev);
                    }
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("Open a video file to begin").size(20.0));
                        ui.label(egui::RichText::new("Any format local ffmpeg can open. Drop in in/ or Open Video. Export Video remuxes cuts to out/.").small());
                    });
                });
            }
        });

        if self.audio_needs_restart {
            self.restart_audio_from_current();
            self.audio_needs_restart = false;
        }

        // Bottom status
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            let left_text = if let Some(p) = &self.project {
                let assembled = Self::assembled_duration(&p.clips);
                let mut text = format!(
                    "Clips: {}  |  Assembled: {:.1}s  |  Selected: {}  |  local ffmpeg",
                    p.clips.len(),
                    assembled,
                    self.selected.len()
                );
                if self.clipboard.is_some() {
                    text.push_str("  |  Clipboard has clips (Ctrl+V to paste | Ctrl+C copy)");
                }
                text.push_str("  |  Ctrl+Z: undo | Ctrl+D: duplicate | Arrows: nav");
                text
            } else {
                "Ready — Cut mode · engine/ffmpeg · switch to Concat in the top bar.".to_string()
            };

            ui.horizontal(|ui| {
                ui.label(left_text);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.project.is_some() {
                        if ui.button("Export Video").clicked() {
                            self.export_edited_video_only();
                        }
                        if ui.button("Export EDL").clicked() {
                            self.export_edl(true);
                        }
                    }
                });
            });
        });
    }
}
