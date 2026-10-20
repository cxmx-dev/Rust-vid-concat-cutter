fn main() {
    println!("ffmpeg={}", engine::ffmpeg_exe().display());
    println!("ready={}", engine::ffmpeg_ready());
    let p = std::path::Path::new("in/sept.mp4");
    assert!(p.is_file());
    let proj = engine::load_project(p).expect("load_project");
    println!("source={}", proj.source_path.display());
    println!("duration={:.3}s", proj.duration);
    println!("clips={}", proj.clips.len());
    for tfrac in [0.0_f64, 0.25, 0.5, 0.75] {
        let t = proj.duration * tfrac;
        let f = engine::extract_frame(p, t, proj.duration, 320).expect("frame");
        println!("frame t={:.2}s -> {}x{}", t, f.width(), f.height());
    }
    // write one real frame to out for inspection
    let mid = proj.duration * 0.5;
    let f = engine::extract_frame(p, mid, proj.duration, 640).expect("mid");
    f.save("out/sept-smoke-mid.png").expect("save png");
    println!("saved out/sept-smoke-mid.png");
    println!("SMOKE_OK sept.mp4");
}
