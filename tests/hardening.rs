use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn decoder_worker_returns_validated_pixels() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_brickle"))
        .arg("--image-decoder")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    brickle::ipc::write_frame(&mut input, b"<svg xmlns='http://www.w3.org/2000/svg' width='2' height='3'><rect width='2' height='3' fill='red'/></svg>").unwrap();
    input.flush().unwrap();
    drop(input);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "decoder worker must run without a display");
    let frame = brickle::ipc::read_frame(&mut &out.stdout[..]).unwrap().unwrap();
    assert_eq!(&frame[..16], &[2, 0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]);
    assert_eq!(frame.len(), 16 + 2 * 3 * 4);
}

#[test]
fn strict_mode_rejects_disabled_sandbox() {
    let out = Command::new(env!("CARGO_BIN_EXE_brickle"))
        .args(["--strict-sandbox", "--no-sandbox", "dom", "about:home"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("strict sandbox requires sandboxing"));
}

#[test]
fn selected_strict_layers_work_without_optional_layers() {
    let out = Command::new(env!("CARGO_BIN_EXE_brickle"))
        .args([
            "--strict-sandbox=seccomp,rlimits",
            "--no-filters",
            "dom",
            "about:home",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("rlimits: on"));
}

#[test]
fn slow_data_image_is_discarded_and_next_image_renders() {
    use std::time::{Duration, Instant};
    let dir = std::env::temp_dir().join(format!("brickle-image-budget-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let page = dir.join("page.html");
    let png = dir.join("out.png");
    let uri = |b: &[u8]| b.iter().map(|v| format!("%{v:02X}")).collect::<String>();
    let red = b"<svg xmlns='http://www.w3.org/2000/svg' width='20' height='20'><rect width='20' height='20' fill='red'/></svg>";
    std::fs::write(&page, format!("<title>Image budget</title><img width='20' height='20' src='data:image/svg+xml,{}'><img src='data:image/svg+xml,{}'>", uri(include_bytes!("data/slow-svg.svg")), uri(red))).unwrap();
    let start = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_brickle"))
        .args([
            "--no-filters",
            "render",
            page.to_str().unwrap(),
            "-w",
            "200",
            "-o",
            png.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(8) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("image budget did not bound page work");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    assert!(start.elapsed() < Duration::from_secs(6));
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&png).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => panic!("unexpected PNG format"),
    };
    assert!(
        buf[..info.buffer_size()]
            .chunks(channels)
            .any(|p| p[0] > 200 && p[1] < 40 && p[2] < 40),
        "next image must still paint red"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
