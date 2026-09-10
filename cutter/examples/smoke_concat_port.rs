use engine::*;
use std::path::PathBuf;

fn main() {
    let root = std::env::current_dir().unwrap();
    let mut vids: Vec<PathBuf> = std::fs::read_dir(root.join("in")).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_video_path(p))
        .collect();
    vids.sort();
    assert!(vids.len() >= 2, "need 2 vids");
    let audio = list_audio_in_dir(&root.join("audio"));
    assert!(!audio.is_empty(), "need audio");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();

    println!("ffmpeg={}", ffmpeg_exe().display());
    let join = default_concat_output(&out);
    println!("CONCAT…");
    concatenate(&vids[..2], &join, &ConcatOptions::default()).expect("concat");
    println!("ok {}", join.display());

    let aa = default_audio_output(&out, false);
    println!("ADD AUDIO…");
    add_audio(&vids[..2], &audio[0], &aa, &AddAudioOptions::default()).expect("add_audio");
    println!("ok {}", aa.display());

    let sp = default_speed_output(&out, &join);
    println!("SPEED 9.5…");
    speed_up(&join, &sp, SpeedMode::FitSeconds(9.5)).expect("speed");
    println!("ok {}", sp.display());

    let cv = convert_video(&join, &out, ConvertFormat::Mp4).expect("convert");
    println!("CONVERT ok {}", cv.display());
    println!("SMOKE_ALL_OK");
}
