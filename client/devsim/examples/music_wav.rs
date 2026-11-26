//! ダミーデバイスの曲(src/music.rs)を WAV に書き出して耳で確かめる。
//!
//!     cargo run --release -p retrocastx-devsim --example music_wav -- out.wav
//!
//! 系統 0(矩形波)・1(三角波)・3(細いパルス)を1周ずつ続けて書く。
use retrocastx_devsim::music::{Tune, Voice};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "music.wav".into());
    let rate = 48_000u32;
    let secs = 34;
    let mut pcm: Vec<i16> = Vec::new();
    for v in [Voice::Square, Voice::Triangle, Voice::Pulse] {
        let mut t = Tune::new(v);
        for _ in 0..rate * secs {
            pcm.push((t.next(rate) * 7000.0) as i16);
        }
    }
    let data = (pcm.len() * 2) as u32;
    let mut w: Vec<u8> = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // モノラル
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(&path, w).expect("書き出せない");
    println!("{path}");
}
