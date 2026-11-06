//! One disposable sandboxed process per image. The browser only handles
//! bounded byte buffers and validated pixels, never image parsers.
use crate::{
    image::{Bitmap, Decoded, MAX_PIXELS, MAX_SIDE, STORE_SIDE},
    net::Cancel,
};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

pub const MAX_INPUT: usize = 8 * 1024 * 1024;
const MAX_OUTPUT: usize = 16 + STORE_SIDE as usize * STORE_SIDE as usize * 4;
const WALL_LIMIT: Duration = Duration::from_secs(3);

pub fn unpack(bytes: &[u8]) -> Result<Decoded, String> {
    if bytes.len() < 16 {
        return Err("short image result".into());
    }
    let n = |i| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
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
        || bytes.len() != 16 + w as usize * h as usize * 4
    {
        return Err("invalid image result dimensions".into());
    }
    Ok(Decoded {
        natural: (nw, nh),
        bitmap: Arc::new(Bitmap {
            w,
            h,
            rgba: bytes[16..].to_vec(),
        }),
    })
}

fn pack(d: Decoded) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + d.bitmap.rgba.len());
    for n in [d.natural.0, d.natural.1, d.bitmap.w, d.bitmap.h] {
        bytes.extend_from_slice(&n.to_le_bytes());
    }
    bytes.extend_from_slice(&d.bitmap.rgba);
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
    let bytes = bounded_frame(&mut std::io::stdin().lock(), MAX_INPUT)?;
    let decoded = crate::image::decode(&bytes)?;
    crate::ipc::write_frame(&mut std::io::stdout().lock(), &pack(decoded)).map_err(|e| e.to_string())
}

pub fn decode_worker(bytes: Vec<u8>, cancel: &Cancel, deadline: Instant) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    cmd.arg("--image-decoder");
    if let Some(layers) = crate::sandbox::requirements() {
        cmd.arg(format!("--require-sandbox={layers}"));
    }
    exchange(cmd, bytes, cancel, deadline.min(Instant::now() + WALL_LIMIT))
}

fn exchange(mut cmd: Command, bytes: Vec<u8>, cancel: &Cancel, deadline: Instant) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_INPUT {
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
