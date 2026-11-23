//! 受信した主系統の音声を、そのまま WAV に書き出すデバッグ機能。
//!
//! `RCX_AUDIO_DUMP=パス` を付けて起動すると有効になる(付けなければ何もしない)。
//! 書き出すのは**ボードの ADC が取った値そのもの**で、Viewer の音量・
//! リサンプル・バッファ調整を通る前。音割れが ADC の手前(アナログの入力
//! レベル)で起きているのか、Viewer 側なのかを切り分けるのに使う
//! (PS2 の RCA 音声が PCM1808 の入力上限を超えていたのを、これで突き止めた)。
//!
//! - 形式は 16bit ステレオの WAV。レートは最初に届いたパケットのもの。
//! - ヘッダの長さは約1秒ごとに書き直すので、Viewer が落ちても読める。
//! - 取りこぼしたパケットは詰めて書く(無音で埋めない)。時刻は少しずれるが、
//!   値の頭打ちを調べる用途には影響しない。
//! - 主系統を切り替えると、その後は新しい系統の音を同じファイルに続けて書く。
//! - 窓を複数開いているときは、最初に音を書いた窓だけを録る(混ざらないように)。

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const HEADER_LEN: u32 = 44;

struct Dump {
    path: PathBuf,
    /// 録っている窓(受信スレッドの `Shared` のアドレス)。0 ならまだ決まっていない
    owner: usize,
    out: BufWriter<File>,
    /// None ならまだヘッダを書いていない
    rate: Option<u32>,
    data_bytes: u32,
    /// 前回ヘッダを書き直してから増えたバイト数
    since_fix: u32,
    warned_rate: bool,
}

fn header(rate: u32, data_bytes: u32) -> [u8; HEADER_LEN as usize] {
    let mut h = [0u8; HEADER_LEN as usize];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&2u16.to_le_bytes()); // ステレオ
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 4).to_le_bytes());
    h[32..34].copy_from_slice(&4u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    h
}

impl Dump {
    fn write(&mut self, samples: &[u8], rate_hz: u32) -> std::io::Result<()> {
        let rate = match self.rate {
            Some(r) => r,
            None => {
                self.out.write_all(&header(rate_hz, 0))?;
                self.rate = Some(rate_hz);
                rate_hz
            }
        };
        if rate != rate_hz && !self.warned_rate {
            self.warned_rate = true;
            eprintln!(
                "RCX_AUDIO_DUMP: 途中でレートが {rate} → {rate_hz} Hz に変わりました。\
                 WAV のヘッダは {rate} Hz のままです"
            );
        }
        // WAV は 4GB まで。超える分は捨てる
        let room = (u32::MAX - HEADER_LEN - self.data_bytes) as usize;
        let samples = &samples[..samples.len().min(room)];
        self.out.write_all(samples)?;
        self.data_bytes += samples.len() as u32;
        self.since_fix += samples.len() as u32;
        if self.since_fix >= rate * 4 {
            self.fix_header()?;
        }
        Ok(())
    }

    /// ヘッダの長さを今の値に直す(途中で落ちても読めるように)
    fn fix_header(&mut self) -> std::io::Result<()> {
        let Some(rate) = self.rate else { return Ok(()) };
        self.since_fix = 0;
        self.out.flush()?;
        let f = self.out.get_mut();
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(rate, self.data_bytes))?;
        f.seek(SeekFrom::End(0))?;
        Ok(())
    }
}

fn dump() -> Option<&'static Mutex<Dump>> {
    static D: OnceLock<Option<Mutex<Dump>>> = OnceLock::new();
    D.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os("RCX_AUDIO_DUMP")?);
        match File::create(&path) {
            Ok(f) => {
                eprintln!("RCX_AUDIO_DUMP: 受信した主系統の音声を {} に書き出します", path.display());
                Some(Mutex::new(Dump {
                    path,
                    owner: 0,
                    out: BufWriter::new(f),
                    rate: None,
                    data_bytes: 0,
                    since_fix: 0,
                    warned_rate: false,
                }))
            }
            Err(e) => {
                eprintln!("RCX_AUDIO_DUMP: {} を作れません: {e}", path.display());
                None
            }
        }
    })
    .as_ref()
}

/// 主系統の AUDIO パケット(s16le の L,R インターリーブ)を書き足す。
/// `RCX_AUDIO_DUMP` が無ければ何もしない。`owner` は窓ごとに違う値(受信側の
/// `Shared` のアドレス)で、最初に書いた窓以外は無視する。
pub fn push(owner: usize, samples: &[u8], rate_hz: u32) {
    let Some(d) = dump() else { return };
    let mut d = d.lock().unwrap();
    if d.owner == 0 {
        d.owner = owner;
    } else if d.owner != owner {
        return;
    }
    if let Err(e) = d.write(samples, rate_hz) {
        eprintln!("RCX_AUDIO_DUMP: {} に書けません: {e}", d.path.display());
    }
}

/// 終了時に呼ぶ。ヘッダを最終の長さに直す。
pub fn finish() {
    let Some(d) = dump() else { return };
    let mut d = d.lock().unwrap();
    let _ = d.fix_header();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_matches_written_length() {
        let dir = std::env::temp_dir().join(format!("rcx_dump_test_{}", std::process::id()));
        let f = File::create(&dir).unwrap();
        let mut d = Dump {
            path: dir.clone(),
            owner: 0,
            out: BufWriter::new(f),
            rate: None,
            data_bytes: 0,
            since_fix: 0,
            warned_rate: false,
        };
        // 1.5 秒分(1秒目でヘッダが書き直され、最後は fix_header で揃う)
        let pkt = vec![0x11u8; 48 * 4];
        for _ in 0..1500 {
            d.write(&pkt, 48000).unwrap();
        }
        d.fix_header().unwrap();
        drop(d);
        let b = std::fs::read(&dir).unwrap();
        std::fs::remove_file(&dir).unwrap();
        let data = 1500 * 48 * 4;
        assert_eq!(b.len(), HEADER_LEN as usize + data);
        assert_eq!(&b[..HEADER_LEN as usize], &header(48000, data as u32));
    }
}
