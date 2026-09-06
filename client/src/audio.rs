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

/// 副系統(S/PDIF を混ぜる)の設定。
///
/// ★**AudioStats に置いてはいけない。** AudioStats は AudioPlayer と一緒に
///   出力デバイスやソースを切り替えるたびに作り直される。「混ぜたい」という
///   意思はアプリ側の設定であって再生器の状態ではないので、再生器より長生きする
///   場所に置く。ここを間違えると、再生器が作り直された瞬間に S/PDIF が黙って
///   落ち、パネルを開くまで戻らない(実際そうなった)。
#[derive(Debug)]
pub struct AuxCtl {
    pub on: AtomicBool,
    pub gain_bits: AtomicU32,
}

impl Default for AuxCtl {
    fn default() -> Self {
        Self { on: AtomicBool::new(false), gain_bits: AtomicU32::new(1.0f32.to_bits()) }
    }
}

impl AuxCtl {
    pub fn set(&self, on: bool, gain: f32) {
        self.on.store(on, Ordering::Relaxed);
        self.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }
}

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

    // --- 副系統(S/PDIF を主系統へ混ぜる)の統計 ---
    pub aux_buffered: AtomicU64,
    pub aux_underruns: AtomicU64,
    pub aux_packets: AtomicU64,
    /// 副系統の実サンプルレート(S/PDIF は 44.1kHz のこともある)
    pub aux_rate: AtomicU64,
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
            aux_buffered: AtomicU64::new(0),
            aux_underruns: AtomicU64::new(0),
            aux_packets: AtomicU64::new(0),
            aux_rate: AtomicU64::new(0),
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
fn track_ratio(ratio: f64, base: f64, have: f64, target: f64) -> f64 {
    let err = ((have - target) / target.max(1.0)).clamp(-1.0, 1.0);
    let want = (base * (1.0 + err * 0.001)).clamp(base * 0.999, base * 1.001);
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
    /// 出力1フレームあたり進む入力フレーム数。base_ratio からのずれが追従量
    ratio: f64,
    /// 入力レート / 出力レート。
    ///
    /// ★**ここを 1.0 決め打ちにしてはいけない。** 追従は ±0.1% しか動かないので、
    ///   レートが本当に違う入力(S/PDIF は 44.1kHz のことがある)では
    ///   追いつかず、バッファが片方向に溢れるか枯れる。アナログ2系統は
    ///   PCM1808 の 48kHz 固定なので今まで表面化しなかっただけ。
    base_ratio: f64,
    /// a,b を読み込んだか(枯渇後は読み直す)
    primed: bool,
    /// このコールバックで枯渇したか(回数を数えるための一時フラグ)
    dry: bool,
}

impl Ring {
    /// 送られてきたレートに合わせて基準比を決める。
    /// 変化したときだけ動かす(毎パケット書き換えると追従が乱れる)。
    fn set_source_rate(&mut self, src_hz: u32, dev_hz: u32) {
        if src_hz == 0 || dev_hz == 0 {
            return;
        }
        // ★**素直に信じない。** gateware の rate_hz は「1秒間に数えたフレーム数」
        //   なので、ロックした直後の1秒は途中までの値(12000 など)が乗る。
        //   そのまま基準比にすると 1/4 の速さで鳴って、次の1秒まで直らない。
        //   実在するレートの範囲から外れた値は無視する。
        if !(32_000..=192_000).contains(&src_hz) {
            return;
        }
        let base = src_hz as f64 / dev_hz as f64;
        if (base - self.base_ratio).abs() > 1e-4 {
            self.base_ratio = base;
            self.ratio = base;
        }
    }

    /// ASRC を1フレーム進めて出力を返す。枯渇したら None(呼び側は無音を出す)。
    /// 枯渇したら `dry` を立てるので、呼び側はコールバックあたり1回だけ数える。
    fn next_frame(&mut self) -> Option<(f32, f32)> {
        if !self.primed {
            match (self.pop_frame(), self.pop_frame()) {
                (Some(a), Some(b)) => {
                    self.a = a; self.b = b; self.frac = 0.0; self.primed = true;
                }
                _ => { self.started = false; self.dry = true; return None; }
            }
        }
        let t = self.frac as f32;
        let out = (self.a.0 + (self.b.0 - self.a.0) * t,
                   self.a.1 + (self.b.1 - self.a.1) * t);
        self.frac += self.ratio;
        while self.frac >= 1.0 {
            match self.pop_frame() {
                Some(nx) => { self.a = self.b; self.b = nx; self.frac -= 1.0; }
                None => {
                    self.started = false;
                    self.primed = false;
                    self.dry = true;
                    break;
                }
            }
        }
        Some(out)
    }

    fn pop_frame(&mut self) -> Option<(f32, f32)> {
        let l = self.buf.pop_front()?;
        let r = self.buf.pop_front()?;
        Some((l as f32 / 32768.0, r as f32 / 32768.0))
    }
}

pub struct AudioPlayer {
    ring: Arc<Mutex<Ring>>,
    /// 副系統(S/PDIF)。主系統に混ぜて出す
    aux_ring: Arc<Mutex<Ring>>,
    /// 副系統の設定。再生器より長生きする(作り直しても設定が消えないように)
    pub aux: Arc<AuxCtl>,
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
        aux: Arc<AuxCtl>,
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
            base_ratio: 1.0,
            primed: false,
            dry: false,
        }));
        // 副系統(S/PDIF を混ぜる)。使わなくても確保しておく——ON にした瞬間から
        // 積めるようにしておかないと、切り替えでデバイスを開き直すことになる。
        let aux_ring = Arc::new(Mutex::new(Ring {
            buf: std::collections::VecDeque::with_capacity(max_frames * 2 + 4096),
            started: false,
            a: (0.0, 0.0),
            b: (0.0, 0.0),
            frac: 0.0,
            ratio: 1.0,
            base_ratio: 1.0,
            primed: false,
            dry: false,
        }));
        let stats = Arc::new(AudioStats::default());
        stats.device_rate.store(device_rate as u64, Ordering::Relaxed);

        let ring_cb = ring.clone();
        let aux_cb = aux_ring.clone();
        let aux_ctl = aux.clone();
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
                    let mut m = ring_cb.lock().unwrap();
                    let mut x = aux_cb.lock().unwrap();
                    // 音量はコールバックの頭で1回読む(ブロック内で一定にする)
                    let gain = f32::from_bits(stats_cb.gain_bits.load(Ordering::Relaxed));
                    let aux_gain = f32::from_bits(aux_ctl.gain_bits.load(Ordering::Relaxed));
                    let aux_on = aux_ctl.on.load(Ordering::Relaxed);

                    // プリバッファに満たない間は無音(頭切れ/断続を防ぐ)。
                    // ★**系統ごとに独立に待つ。** 副系統(S/PDIF)は繋がっていない
                    //   ことが普通なので、そちらの充填を待つと主系統まで鳴らなくなる。
                    if !m.started && m.buf.len() >= prebuffer_frames * 2 {
                        m.started = true;
                        stats_cb.playing.store(true, Ordering::Relaxed);
                    }
                    if aux_on && !x.started && x.buf.len() >= prebuffer_frames * 2 {
                        x.started = true;
                    }
                    if !m.started && !(aux_on && x.started) {
                        out.fill(0.0);
                        stats_cb.buffered.store((m.buf.len() / 2) as u64, Ordering::Relaxed);
                        return;
                    }

                    // --- ASRC の追従。**ゆっくり動かす。** 速く動かすと音程が
                    //     揺れて聞こえる。滞留がプリバッファ量から離れた割合に
                    //     比例して歩幅を変える比例制御で、12.5ppm を打ち消すのに
                    //     必要なのは滞留が目標より 1.25% 多い状態(80msなら+1ms)。
                    let have = (m.buf.len() / 2) as f64;
                    m.ratio = track_ratio(m.ratio, m.base_ratio, have, prebuffer_frames as f64);
                    let have_x = (x.buf.len() / 2) as f64;
                    x.ratio = track_ratio(x.ratio, x.base_ratio, have_x, prebuffer_frames as f64);

                    // ★**枯渇は「回数」で数える。** 以前はサンプルフレームごとに
                    //   +1 していたので、1回の枯渇でコールバックの残り全部
                    //   (512フレームなら最大512)が加算され、数として読めなかった。
                    m.dry = false;
                    x.dry = false;
                    for f in out.chunks_mut(channels) {
                        let (mut lf, mut rf) = (0.0f32, 0.0f32);
                        if m.started {
                            if let Some((a, b)) = m.next_frame() {
                                lf += a * gain;
                                rf += b * gain;
                            }
                        }
                        if aux_on && x.started {
                            if let Some((a, b)) = x.next_frame() {
                                lf += a * aux_gain;
                                rf += b * aux_gain;
                            }
                        }
                        // ★**混ぜてから飽和させる。** 系統ごとにクリップすると、
                        //   片方が歪んでいるのか合計で溢れたのかが分からなくなる。
                        let (lf, rf) = (lf.clamp(-1.0, 1.0), rf.clamp(-1.0, 1.0));
                        for (i, sm) in f.iter_mut().enumerate() {
                            // ステレオ以上は L,R を先頭2chへ、残りは0
                            *sm = match i { 0 => lf, 1 => rf, _ => 0.0 };
                        }
                    }
                    if m.dry {
                        stats_cb.playing.store(false, Ordering::Relaxed);
                        stats_cb.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    if x.dry {
                        stats_cb.aux_underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    stats_cb.buffered.store((m.buf.len() / 2) as u64, Ordering::Relaxed);
                    stats_cb
                        .aux_buffered
                        .store((x.buf.len() / 2) as u64, Ordering::Relaxed);
                },
                err_fn,
                None,
            )
            .ok()?;
        stream.play().ok()?;

        Some(Self {
            ring,
            aux_ring,
            aux,
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
    pub fn push(&self, samples: &[u8], rate_hz: u32) {
        let mut r = self.ring.lock().unwrap();
        r.set_source_rate(rate_hz, self.stats.device_rate.load(Ordering::Relaxed) as u32);
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

    /// 副系統(S/PDIF)へ積む。主系統とは別のリング・別のレート・別の音量。
    pub fn push_aux(&self, samples: &[u8], rate_hz: u32) {
        let mut r = self.aux_ring.lock().unwrap();
        r.set_source_rate(rate_hz, self.stats.device_rate.load(Ordering::Relaxed) as u32);
        for c in samples.chunks_exact(2) {
            r.buf.push_back(i16::from_le_bytes([c[0], c[1]]));
        }
        // 溜まりすぎ(遅延の蓄積)は古い方を捨てて上限内に保つ
        let max = self.max_frames * 2;
        if r.buf.len() > max {
            let excess = r.buf.len() - max;
            // ★溢れを主系統と混ぜて数えない。どちらが詰まったのか分からなくなる
            r.buf.drain(..excess);
        }
        self.stats.aux_packets.fetch_add(1, Ordering::Relaxed);
        self.stats
            .aux_buffered
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
                ratio = track_ratio(ratio, 1.0, have, target);
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
            ratio = track_ratio(ratio, 1.0, 48000.0 * 0.240, 48000.0 * 0.080);
        }
        assert!(ratio <= 1.001 + 1e-9, "歩幅が上限を超えた: {ratio}");
        // 滞留が上限まで振れても補正は 0.1% = 約1.7セント
        let cents = 1200.0 * (ratio.ln() / 2.0f64.ln());
        assert!(cents.abs() < 3.0, "音程が {cents:.2} セント動く(3セント未満に)");
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    fn ring() -> Ring {
        Ring {
            buf: Default::default(), started: false,
            a: (0.0, 0.0), b: (0.0, 0.0), frac: 0.0,
            ratio: 1.0, base_ratio: 1.0, primed: false, dry: false,
        }
    }

    /// ★**44.1kHz の入力を 48kHz のデバイスへ出せること。**
    ///   追従は基準比の ±0.1% しか動かないので、基準比を入れずに 1.0 のままだと
    ///   1秒あたり 3900 フレームぶん食い違い、バッファが片方向に振り切れる。
    ///   S/PDIF を混ぜる以上ここは避けて通れない(アナログ2系統は 48kHz 固定
    ///   だったので今まで表面化していなかった)。
    #[test]
    fn base_ratio_follows_the_source_rate() {
        let mut r = ring();
        r.set_source_rate(44_100, 48_000);
        assert!((r.base_ratio - 44_100.0 / 48_000.0).abs() < 1e-9, "{}", r.base_ratio);
        // 追従はその周りだけを動く
        let tracked = track_ratio(r.ratio, r.base_ratio, 100.0, 100.0);
        assert!((tracked - r.base_ratio).abs() < r.base_ratio * 0.002,
                "基準比から離れた: {tracked}");
    }

    /// 同じレートが続く間は基準比を動かさない(毎パケット書き換えると追従が乱れる)
    #[test]
    fn base_ratio_is_stable_for_the_same_rate() {
        let mut r = ring();
        r.set_source_rate(48_000, 48_000);
        r.ratio = 1.0005;                 // 追従で動いた状態
        r.set_source_rate(48_000, 48_000);
        assert_eq!(r.ratio, 1.0005, "同じレートなのに追従値が捨てられた");
    }

    /// レートが変わったら入れ直す(S/PDIF は機器を差し替えると変わる)
    #[test]
    fn base_ratio_resets_when_the_rate_changes() {
        let mut r = ring();
        r.set_source_rate(48_000, 48_000);
        r.set_source_rate(44_100, 48_000);
        assert!((r.ratio - 44_100.0 / 48_000.0).abs() < 1e-9, "{}", r.ratio);
    }

    /// 0 は無視する(MODE/AUDIO が来る前のレート未確定を踏まない)
    #[test]
    fn zero_rate_is_ignored() {
        let mut r = ring();
        r.set_source_rate(0, 48_000);
        assert_eq!(r.base_ratio, 1.0);
    }
}
