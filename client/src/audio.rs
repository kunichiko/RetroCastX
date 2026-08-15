//! AUDIOパケットの再生(cpal)。
//!
//! 受信スレッドが `AudioPlayer::push` でサンプルをリングバッファへ積み、cpalの
//! 出力コールバックが取り出して鳴らす。ネットワークのジッタを吸収するため、
//! 再生開始前に `prebuffer_ms` 分だけ溜める。溜まりすぎ(遅延の蓄積)は古い
//! サンプルを捨てて `max_ms` 以内に保つ。
//!
//! ボードのサンプルレート(48kHz, XO由来)と PC のオーディオデバイスのレートは
//! 独立した水晶なので長時間ではわずかにずれる。ここではレート変換せず、
//! バッファ長で吸収し、溢れ/枯渇の回数を統計として出す(A/V同期の作り込みは
//! タイムスタンプを使う将来課題)。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct AudioStats {
    /// 現在のバッファ長[フレーム]
    pub buffered: AtomicU64,
    /// 枯渇(無音で埋めた)回数
    pub underruns: AtomicU64,
    /// 溢れて捨てたフレーム数
    pub dropped: AtomicU64,
    /// 受信したAUDIOパケット数
    pub packets: AtomicU64,
    /// デバイスの実サンプルレート
    pub device_rate: AtomicU64,
    /// 再生中か(プリバッファ完了)
    pub playing: AtomicBool,
    /// 音量。f32をビットパターンで持つ(コールバックからロックなしで読むため)。
    /// 1.0=原音、0.0=無音。1.0超も許すが、クリップは出力時に飽和させる。
    pub gain_bits: AtomicU32,
}

impl Default for AudioStats {
    fn default() -> Self {
        Self {
            buffered: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            packets: AtomicU64::new(0),
            device_rate: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
        }
    }
}

struct Ring {
    /// s16 のインターリーブ列(L,R,L,R,...)
    buf: std::collections::VecDeque<i16>,
    /// プリバッファ達成後に true
    started: bool,
}

pub struct AudioPlayer {
    ring: Arc<Mutex<Ring>>,
    pub stats: Arc<AudioStats>,
    /// 実際に開いたデバイス名(UI表示用)
    pub device_name: String,
    /// 再生するsource(0=RGB端子音声, 1=LINE入力, 2=S/PDIF)
    pub source: u8,
    prebuffer_frames: usize,
    max_frames: usize,
    /// Drop時にストリームを止める。保持するだけでよい。
    _stream: Option<cpal::Stream>,
}

/// 利用可能な出力デバイス名の一覧(UIの選択肢用)。
pub fn output_devices() -> Vec<String> {
    let host = cpal::default_host();
    match host.output_devices() {
        Ok(it) => it
            .filter_map(|d| d.description().ok().map(|x| x.name().to_string()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 既定出力デバイスの名前(UIで「既定」を示すため)。
pub fn default_output_device_name() -> Option<String> {
    let d = cpal::default_host().default_output_device()?;
    d.description().ok().map(|x| x.name().to_string())
}

impl AudioPlayer {
    /// 出力デバイスを開いて再生を始める。デバイスが無い/開けない場合も
    /// Err にせず「再生なし」で動作継続できるよう Option を返す。
    /// device_name=None なら既定デバイス。
    pub fn new(
        source: u8,
        prebuffer_ms: u32,
        max_ms: u32,
        device_name: Option<&str>,
    ) -> Option<Self> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(want) => host
                .output_devices()
                .ok()?
                .find(|d| {
                    d.description().map(|x| x.name() == want).unwrap_or(false)
                })
                .or_else(|| host.default_output_device())?,
            None => host.default_output_device()?,
        };
        let dev_name = device
            .description()
            .map(|x| x.name().to_string())
            .unwrap_or_default();
        let cfg = device.default_output_config().ok()?;
        let device_rate = cfg.sample_rate();
        let channels = cfg.channels() as usize;

        let prebuffer_frames = (device_rate as u64 * prebuffer_ms as u64 / 1000) as usize;
        let max_frames = (device_rate as u64 * max_ms as u64 / 1000) as usize;

        let ring = Arc::new(Mutex::new(Ring {
            buf: std::collections::VecDeque::with_capacity(max_frames * 2 + 4096),
            started: false,
        }));
        let stats = Arc::new(AudioStats::default());
        stats.device_rate.store(device_rate as u64, Ordering::Relaxed);

        let ring_cb = ring.clone();
        let stats_cb = stats.clone();
        let err_fn = |e| eprintln!("audio stream error: {e}");

        // 出力は f32 に限定(macOS/CoreAudioの既定。他形式は必要になったら追加)
        if cfg.sample_format() != cpal::SampleFormat::F32 {
            eprintln!(
                "audio: 未対応のサンプル形式 {:?} なので再生しません",
                cfg.sample_format()
            );
            return None;
        }
        let stream = device
            .build_output_stream(
                cfg.config(),
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut r = ring_cb.lock().unwrap();
                    // プリバッファに満たない間は無音(頭切れ/断続を防ぐ)
                    if !r.started {
                        if r.buf.len() >= prebuffer_frames * 2 {
                            r.started = true;
                            stats_cb.playing.store(true, Ordering::Relaxed);
                        } else {
                            out.fill(0.0);
                            return;
                        }
                    }
                    // 音量はコールバックの頭で1回読む(ブロック内で一定にする)
                    let gain = f32::from_bits(stats_cb.gain_bits.load(Ordering::Relaxed));
                    // ★**枯渇は「回数」で数える。** 以前はサンプルフレームごとに
                    //   +1 していたので、1回の枯渇でコールバックの残り全部
                    //   (512フレームなら最大512)が加算され、数として読めなかった
                    //   (実機で underruns 416 と出て「416回途切れた」と読めてしまう
                    //    が、実際には1〜2回だった可能性がある)。
                    let mut dry = false;
                    for f in out.chunks_mut(channels) {
                        let l = r.buf.pop_front();
                        let rr = r.buf.pop_front();
                        match (l, rr) {
                            (Some(l), Some(rr)) => {
                                // 1.0超のゲインでも歪ませないよう ±1.0 で飽和させる
                                let lf = (l as f32 / 32768.0 * gain).clamp(-1.0, 1.0);
                                let rf = (rr as f32 / 32768.0 * gain).clamp(-1.0, 1.0);
                                for (i, s) in f.iter_mut().enumerate() {
                                    // ステレオ以上は L,R を先頭2chへ、残りは0
                                    *s = match i {
                                        0 => lf,
                                        1 => rf,
                                        _ => 0.0,
                                    };
                                }
                            }
                            _ => {
                                // 枯渇: 無音を出し、次回プリバッファし直す
                                f.fill(0.0);
                                r.started = false;
                                stats_cb.playing.store(false, Ordering::Relaxed);
                                if !dry {
                                    dry = true;
                                    stats_cb.underruns.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    stats_cb
                        .buffered
                        .store((r.buf.len() / 2) as u64, Ordering::Relaxed);
                },
                err_fn,
                None,
            )
            .ok()?;
        stream.play().ok()?;

        Some(Self {
            ring,
            stats,
            device_name: dev_name,
            source,
            prebuffer_frames,
            max_frames,
            _stream: Some(stream),
        })
    }

    /// 音量を設定する(1.0=原音)。コールバックは次のブロックから反映する。
    pub fn set_gain(stats: &AudioStats, gain: f32) {
        stats.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// AUDIOパケットのペイロード(s16le L/R interleaved)を積む。
    pub fn push(&self, samples: &[u8]) {
        let mut r = self.ring.lock().unwrap();
        for c in samples.chunks_exact(2) {
            r.buf.push_back(i16::from_le_bytes([c[0], c[1]]));
        }
        // 溜まりすぎ(遅延の蓄積)は古い方を捨てて上限内に保つ
        let max = self.max_frames * 2;
        if r.buf.len() > max {
            let excess = r.buf.len() - max;
            r.buf.drain(..excess);
            self.stats
                .dropped
                .fetch_add((excess / 2) as u64, Ordering::Relaxed);
        }
        self.stats.packets.fetch_add(1, Ordering::Relaxed);
        self.stats
            .buffered
            .store((r.buf.len() / 2) as u64, Ordering::Relaxed);
        let _ = self.prebuffer_frames;
    }
}
