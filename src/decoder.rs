//! One disposable sandboxed process per image. The browser only handles
//! bounded byte buffers and validated pixels, never image parsers.
use crate::{
    image::{Bitmap, Decoded, Frames, MAX_ANIMATION_BYTES, MAX_FRAMES, MAX_PIXELS, MAX_SIDE, STORE_SIDE},
    net::Cancel,
};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

pub const MAX_INPUT: usize = 8 * 1024 * 1024;
const MAX_OUTPUT: usize =
    16 + STORE_SIDE as usize * STORE_SIDE as usize * 4 + 4 + MAX_FRAMES * 4 + MAX_ANIMATION_BYTES;
const WALL_LIMIT: Duration = Duration::from_secs(3);

/// The worker's answer: natural and stored size, the pixels, then for an
/// animation the frame count, each frame's delay and the other frames.
pub fn unpack(bytes: &[u8]) -> Result<Decoded, String> {
    if bytes.len() < 16 {
        return Err("short image result".into());
    }
    let n = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
    let (nw, nh, w, h) = (n(0), n(4), n(8), n(12));
    if nw == 0
        || nh == 0
        || nw > MAX_SIDE
        || nh > MAX_SIDE
        || nw as u64 * nh as u64 > MAX_PIXELS
        || w == 0
        || h == 0
        || w > STORE_SIDE
        || h > STORE_SIDE
        || bytes.len() < 16 + w as usize * h as usize * 4
    {
        return Err("invalid image result dimensions".into());
    }
    let size = w as usize * h as usize * 4;
    let bitmap = Arc::new(Bitmap {
        w,
        h,
        rgba: bytes[16..16 + size].to_vec(),
    });
    let rest = &bytes[16 + size..];
    let frames = if rest.is_empty() {
        None
    } else {
        let bad = || "invalid animation in image result".to_string();
        let count = u32::from_le_bytes(rest.get(..4).ok_or_else(bad)?.try_into().unwrap()) as usize;
        if !(2..=MAX_FRAMES).contains(&count)
            || (count - 1) * size > MAX_ANIMATION_BYTES
            || rest.len() != 4 + count * 4 + (count - 1) * size
        {
            return Err(bad());
        }
        let delays: Vec<u32> = rest[4..4 + count * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c).min(60_000))
            .collect();
        let mut bitmaps = vec![bitmap.clone()];
        for px in rest[4 + count * 4..].chunks_exact(size) {
            bitmaps.push(Arc::new(Bitmap {
                w,
                h,
                rgba: px.to_vec(),
            }));
        }
        Some(Arc::new(Frames { bitmaps, delays }))
    };
    Ok(Decoded {
        natural: (nw, nh),
        bitmap,
        frames,
        svg: None,
    })
}

fn pack(d: Decoded) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + d.bitmap.rgba.len());
    for n in [d.natural.0, d.natural.1, d.bitmap.w, d.bitmap.h] {
        bytes.extend_from_slice(&n.to_le_bytes());
    }
    bytes.extend_from_slice(&d.bitmap.rgba);
    if let Some(f) = d.frames.filter(|f| f.bitmaps.len() >= 2) {
        bytes.extend_from_slice(&(f.bitmaps.len() as u32).to_le_bytes());
        for d in &f.delays {
            bytes.extend_from_slice(&d.to_le_bytes());
        }
        for b in &f.bitmaps[1..] {
            bytes.extend_from_slice(&b.rgba);
        }
    }
    bytes
}

fn bounded_frame(r: &mut impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut header = [0; 4];
    r.read_exact(&mut header).map_err(|e| e.to_string())?;
    let len = u32::from_le_bytes(header) as usize;
    if len > limit {
        return Err("image frame too large".into());
    }
    let mut out = Vec::new();
    r.take(len as u64)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() != len {
        return Err("truncated image frame".into());
    }
    Ok(out)
}

pub fn serve() -> Result<(), String> {
    let report = crate::sandbox::enter_decoder()?;
    crate::sandbox::validate_child(&report)?;
    let input = bounded_frame(&mut std::io::stdin().lock(), MAX_INPUT + 8)?;
    if input.len() < 8 {
        return Err("image input too short".into());
    }
    let size = |i: usize| u32::from_le_bytes(input[i..i + 4].try_into().unwrap_or_default());
    let want = (size(0) > 0 && size(4) > 0).then(|| (size(0), size(4)));
    let decoded = crate::image::decode_at(&input[8..], want)?;
    crate::ipc::write_frame(&mut std::io::stdout().lock(), &pack(decoded)).map_err(|e| e.to_string())
}

/// Decode `bytes` in a sandboxed worker; an SVG is drawn at `want` pixels
/// if given.
pub fn decode_worker(
    bytes: Vec<u8>,
    want: Option<(u32, u32)>,
    cancel: &Cancel,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    cmd.arg("--image-decoder");
    if let Some(layers) = crate::sandbox::requirements() {
        cmd.arg(format!("--require-sandbox={layers}"));
    }
    if bytes.len() > MAX_INPUT {
        return Err("image input too large".into());
    }
    let (w, h) = want.unwrap_or((0, 0));
    let mut input = Vec::with_capacity(bytes.len() + 8);
    input.extend_from_slice(&w.to_le_bytes());
    input.extend_from_slice(&h.to_le_bytes());
    input.extend_from_slice(&bytes);
    exchange(cmd, input, cancel, deadline.min(Instant::now() + WALL_LIMIT))
}

fn exchange(mut cmd: Command, bytes: Vec<u8>, cancel: &Cancel, deadline: Instant) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_INPUT + 8 {
        return Err("image input too large".into());
    }
    if cancel.is_cancelled() || Instant::now() >= deadline {
        return Err("image decode stopped".into());
    }
    let mut child = cmd
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut input = child.stdin.take().ok_or("decoder input missing")?;
    let mut output = child.stdout.take().ok_or("decoder output missing")?;
    let writer = std::thread::spawn(move || crate::ipc::write_frame(&mut input, &bytes));
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let _ = tx.send(bounded_frame(&mut output, MAX_OUTPUT));
    });
    let result = loop {
        if cancel.is_cancelled() {
            break Err("image decode stopped".into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err("image decode timed out".into());
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(20))) {
            Ok(r) => {
                break r.and_then(|b| {
                    unpack(&b)?;
                    Ok(b)
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err("image decoder exited".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    // Always reap, including timeout, malformed output and cancellation.
    let _ = child.kill();
    let _ = child.wait();
    let _ = writer.join();
    let _ = reader.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn animations_cross_from_the_worker() {
        let b = |v: u8| {
            Arc::new(Bitmap {
                w: 1,
                h: 2,
                rgba: vec![v; 8],
            })
        };
        let d = Decoded {
            natural: (1, 2),
            bitmap: b(1),
            frames: Some(Arc::new(Frames {
                bitmaps: vec![b(1), b(2)],
                delays: vec![100, 250],
            })),
            svg: None,
        };
        let bytes = pack(d);
        let back = unpack(&bytes).unwrap();
        let f = back.frames.unwrap();
        assert_eq!(f.delays, vec![100, 250]);
        assert_eq!(f.bitmaps[1].rgba, vec![2; 8]);
        assert!(unpack(&bytes[..bytes.len() - 1]).is_err(), "truncated");
        let mut one = bytes.clone();
        one[16 + 8..16 + 12].copy_from_slice(&1u32.to_le_bytes());
        assert!(unpack(&one).is_err(), "an animation of one frame");
    }
    #[test]
    fn output_is_bounded_and_validated() {
        for b in [vec![], vec![0; 16], vec![255; 16]] {
            assert!(unpack(&b).is_err());
        }
        let mut header = ((MAX_OUTPUT + 1) as u32).to_le_bytes().to_vec();
        assert!(bounded_frame(&mut &header[..], MAX_OUTPUT)
            .unwrap_err()
            .contains("too large"));
        header = vec![1, 0, 0, 0];
        assert!(bounded_frame(&mut &header[..], MAX_OUTPUT).is_err());
    }
    #[test]
    fn stalled_worker_is_killed_and_next_image_can_run() {
        let mut cmd = Command::new("python3");
        cmd.args(["-I", "-c", "import time; time.sleep(0.4)"]);
        let start = Instant::now();
        assert!(exchange(
            cmd,
            vec![0; 100_000],
            &Cancel::new(),
            start + Duration::from_millis(100)
        )
        .unwrap_err()
        .contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
        let mut cmd = Command::new("python3");
        cmd.args(["-I", "-c", "import sys,struct; b=struct.pack('<IIII',1,1,1,1)+bytes([255,0,0,255]);sys.stdout.buffer.write(struct.pack('<I',len(b))+b)"]);
        assert!(exchange(
            cmd,
            vec![],
            &Cancel::new(),
            Instant::now() + Duration::from_secs(2)
        )
        .is_ok());
    }
}
