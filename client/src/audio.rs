//! AUDIOパケットの再生(cpal)。
//!
//! 受信スレッドが `AudioPlayer::push` でサンプルをリングバッファへ積み、cpalの
//! 出力コールバックが取り出して鳴らす。ネットワークのジッタを吸収するため、
//! 再生開始前に `prebuffer_ms` 分だけ溜める。溜まりすぎ(遅延の蓄積)は古い
//! サンプルを捨てて `max_ms` 以内に保つ。
//!
//! ボードのサンプルレート(48kHz, XO由来)と PC のオーディオデバイスのレートは
//! 独立した水晶なので長時間ではわずかにずれる。**実測 12.5ppm**(2.3時間で
//! 滞留が 72ms → 175ms に育った)。何もしないと上限240msに達して古いサンプルを
//! まとめて捨てるので、数時間ごとにプチッと鳴る。
//!
//! そこで読み出し側で連続的に吸収する(ASRC)。滞留量が目標から離れた分だけ
//! 読み出しの歩幅を ±0.1% の範囲で変え、線形補間で出す。12.5ppm を打ち消すのに
//! 必要な歩幅は 0.00125% なので、可聴域から桁で下にある。

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

/// 滞留量から次の読み出し歩幅を決める。**ゆっくり動かすこと。**
///
/// 速く動かすと音程が揺れて聞こえる。滞留が目標から離れた割合に比例して歩幅を
/// 変える比例制御で、定常偏差は残るがそれでよい: 12.5ppm を打ち消すのに必要なのは
/// 滞留が目標より 1.25% 多い状態(80msなら+1ms)で、遅延としては無視できる。
///
/// 歩幅は ±0.1%(1000ppm)で頭打ちにする。打ち消したいのは十数ppmなので桁で余る。
fn track_ratio(ratio: f64, have: f64, target: f64) -> f64 {
    let err = ((have - target) / target.max(1.0)).clamp(-1.0, 1.0);
    let want = (1.0 + err * 0.001).clamp(0.999, 1.001);
    ratio + (want - ratio) * 0.05
}

struct Ring {
    /// s16 のインターリーブ列(L,R,L,R,...)
    buf: std::collections::VecDeque<i16>,
    /// プリバッファ達成後に true
    started: bool,
    // --- 非同期サンプルレート変換(ASRC)の状態 ---
    /// 補間の左右の入力フレーム
    a: (f32, f32),
    b: (f32, f32),
    /// a と b の間の位置 [0,1)
    frac: f64,
    /// 出力1フレームあたり進む入力フレーム数。1.0 からのずれが追従量
    ratio: f64,
    /// a,b を読み込んだか(枯渇後は読み直す)
    primed: bool,
}

impl Ring {
    fn pop_frame(&mut self) -> Option<(f32, f32)> {
        let l = self.buf.pop_front()?;
        let r = self.buf.pop_front()?;
        Some((l as f32 / 32768.0, r as f32 / 32768.0))
    }
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
            a: (0.0, 0.0),
            b: (0.0, 0.0),
            frac: 0.0,
            ratio: 1.0,
            primed: false,
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

                    // --- ASRC の追従。**ゆっくり動かす。** 速く動かすと音程が
                    //     揺れて聞こえる。滞留がプリバッファ量から離れた割合に
                    //     比例して歩幅を変える比例制御で、12.5ppm を打ち消すのに
                    //     必要なのは滞留が目標より 1.25% 多い状態(80msなら+1ms)。
                    let have = (r.buf.len() / 2) as f64;
                    r.ratio = track_ratio(r.ratio, have, prebuffer_frames as f64);

                    // ★**枯渇は「回数」で数える。** 以前はサンプルフレームごとに
                    //   +1 していたので、1回の枯渇でコールバックの残り全部
                    //   (512フレームなら最大512)が加算され、数として読めなかった
                    //   (実機で underruns 416 と出て「416回途切れた」と読めてしまう
                    //    が、実際には1〜2回だった可能性がある)。
                    let mut dry = false;
                    for f in out.chunks_mut(channels) {
                        if !r.primed {
                            match (r.pop_frame(), r.pop_frame()) {
                                (Some(a), Some(b)) => {
                                    r.a = a; r.b = b; r.frac = 0.0; r.primed = true;
                                }
                                _ => {
                                    f.fill(0.0);
                                    r.started = false;
                                    stats_cb.playing.store(false, Ordering::Relaxed);
                                    if !dry {
                                        dry = true;
                                        stats_cb.underruns.fetch_add(1, Ordering::Relaxed);
                                    }
                                    continue;
                                }
                            }
                        }
                        // a と b の間を線形補間して1フレーム出す
                        let t = r.frac as f32;
                        let lf = ((r.a.0 + (r.b.0 - r.a.0) * t) * gain).clamp(-1.0, 1.0);
                        let rf = ((r.a.1 + (r.b.1 - r.a.1) * t) * gain).clamp(-1.0, 1.0);
                        for (i, sm) in f.iter_mut().enumerate() {
                            // ステレオ以上は L,R を先頭2chへ、残りは0
                            *sm = match i { 0 => lf, 1 => rf, _ => 0.0 };
                        }
                        // 歩幅ぶん進め、跨いだら次の入力フレームを取り込む
                        r.frac += r.ratio;
                        while r.frac >= 1.0 {
                            match r.pop_frame() {
                                Some(n) => { r.a = r.b; r.b = n; r.frac -= 1.0; }
                                None => {
                                    r.started = false;
                                    r.primed = false;
                                    stats_cb.playing.store(false, Ordering::Relaxed);
                                    if !dry {
                                        dry = true;
                                        stats_cb.underruns.fetch_add(1, Ordering::Relaxed);
                                    }
                                    break;
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


#[cfg(test)]
mod tests {
    use super::*;

    /// ボードとPCの水晶のずれを ASRC が吸収すること。
    ///
    /// ★実機で 2.3時間に滞留が 72ms → 175ms に育った(12.5ppm)。何もしないと
    ///   上限240msに達して古いサンプルをまとめて捨てるので、数時間ごとにプチッと鳴る。
    ///   ここではその状況を早送りで再現し、**追従があれば発散しない**ことを見る。
    fn simulate(drift_ppm: f64, track: bool) -> (f64, f64) {
        let rate = 48000.0f64;
        let target = rate * 0.080;             // プリバッファ 80ms
        let max = rate * 0.240;                // 上限 240ms
        let block = 512.0;                     // コールバック1回のフレーム数
        let mut have = target;
        let mut ratio = 1.0f64;
        let mut peak: f64 = have;
        // 4時間ぶん回す。12.5ppm なら 80ms から上限240msまで 3.6時間かかる
        // (実測の 2.3時間で +103ms = 44.8ms/時 と一致する)
        let blocks = (rate * 3600.0 * 4.0 / block) as usize;
        for _ in 0..blocks {
            if track {
                ratio = track_ratio(ratio, have, target);
            }
            // 生産は drift ぶん速い/遅い、消費は歩幅ぶん
            have += block * ((1.0 + drift_ppm * 1e-6) - ratio);
            if have > max {
                have = max;                    // 実装と同じく捨てる
            }
            if have < 0.0 {
                have = 0.0;
            }
            peak = peak.max(have);
        }
        (have, peak / rate * 1000.0)
    }

    #[test]
    fn asrc_absorbs_clock_drift() {
        let rate = 48000.0f64;
        let target_ms = 80.0;
        // 追従なし: 実機と同じ 12.5ppm で上限240msまで育つ
        let (_, peak_off) = simulate(12.5, false);
        assert!(peak_off >= 239.0,
                "追従なしで上限に達しない = 試験になっていない: 最大 {peak_off:.0}ms");
        // 追従あり: 目標付近に収まる
        let (end_on, peak_on) = simulate(12.5, true);
        let end_ms = end_on / rate * 1000.0;
        assert!((end_ms - target_ms).abs() < 8.0,
                "4時間後の滞留が目標から離れた: {end_ms:.1}ms(目標 {target_ms}ms)");
        assert!(peak_on < 120.0, "途中で膨らみすぎ: 最大 {peak_on:.0}ms");
        // 逆向き(PCが速い)でも枯渇しない
        let (end_neg, _) = simulate(-12.5, true);
        let neg_ms = end_neg / rate * 1000.0;
        assert!((neg_ms - target_ms).abs() < 8.0,
                "逆向きのずれで滞留がずれた: {neg_ms:.1}ms");
    }

    /// 歩幅の変化量が可聴域から桁で下にあること(音程が揺れて聞こえないこと)
    #[test]
    fn asrc_correction_is_inaudible() {
        let mut ratio = 1.0f64;
        for _ in 0..10000 {
            ratio = track_ratio(ratio, 48000.0 * 0.240, 48000.0 * 0.080);
        }
        assert!(ratio <= 1.001 + 1e-9, "歩幅が上限を超えた: {ratio}");
        // 滞留が上限まで振れても補正は 0.1% = 約1.7セント
        let cents = 1200.0 * (ratio.ln() / 2.0f64.ln());
        assert!(cents.abs() < 3.0, "音程が {cents:.2} セント動く(3セント未満に)");
    }
}
