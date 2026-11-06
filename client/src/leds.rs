//! ボードのシリアルLED(小亀基板のLED列と、メイン基板の状態LED)の設定。
//!
//! ★**効果はすべてここでキーフレーム表に直してから送る。** ゲートウェアは
//!   「保持 → 次のフレームへ線形フェード」をループ再生するだけで、レインボーも
//!   呼吸も知らない(`gateware/retrocastx_led.py` 冒頭)。新しい効果を足すのに
//!   ゲートウェアを更新しなくて済むようにするため。
//!
//! 送るのは専用のスレッドと専用のソケット。表は最大で150語ほどあり、ボードは
//! CONFIG の応答を1つしか溜めないので、まとめて投げると応答が上書きされて
//! 届いたかどうか確かめられない。1語ずつ応答を待って送る。受信スレッドの
//! ソケットを使わないのは、そちらで待つと映像の受信が止まるため。

use std::net::UdpSocket;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::protocol as proto;
use crate::receiver::Shared;

pub const KEY_KLED_COUNT: u16 = 0x0090;
pub const KEY_KLED_FRAMES: u16 = 0x0091;
pub const KEY_KLED_BANK: u16 = 0x0092;
pub const KEY_KLED_BRIGHT: u16 = 0x0093;
pub const KEY_STAT_BRIGHT: u16 = 0x0094;
pub const KEY_STAT_STATE: u16 = 0x0095;
pub const KEY_TABLE_BASE: u16 = 0x0400;
/// 1バンクの語数。フレーム数 × (1 + LED数) がこれに収まること
pub const BANK_WORDS: usize = 256;
pub const MAX_LEDS: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Off,
    Solid,
    Blink,
    Breathe,
    Rainbow,
}

impl Effect {
    pub const ALL: [Effect; 5] =
        [Effect::Rainbow, Effect::Solid, Effect::Breathe, Effect::Blink, Effect::Off];

    pub fn key(self) -> &'static str {
        match self {
            Effect::Off => "off",
            Effect::Solid => "solid",
            Effect::Blink => "blink",
            Effect::Breathe => "breathe",
            Effect::Rainbow => "rainbow",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Effect::Off => "消灯",
            Effect::Solid => "固定色",
            Effect::Blink => "点滅",
            Effect::Breathe => "呼吸",
            Effect::Rainbow => "レインボー",
        }
    }

    pub fn from_key(s: &str) -> Option<Effect> {
        Effect::ALL.into_iter().find(|e| e.key() == s)
    }

    /// 色を選ぶ意味があるか(レインボーと消灯は色を使わない)
    pub fn uses_color(self) -> bool {
        matches!(self, Effect::Solid | Effect::Blink | Effect::Breathe)
    }

    /// 周期を選ぶ意味があるか
    pub fn uses_period(self) -> bool {
        matches!(self, Effect::Blink | Effect::Breathe | Effect::Rainbow)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LedSettings {
    pub effect: Effect,
    pub color: [u8; 3],
    /// 1周の長さ[ms]
    pub period_ms: u32,
    /// レインボーで LED ごとに色相をずらす量(0=全部同じ色、100=列全体で1周)
    pub spread_pct: u32,
    /// 小亀基板の LED の個数
    pub count: u8,
    pub bright: u8,
    pub status_bright: u8,
}

impl Default for LedSettings {
    fn default() -> Self {
        Self {
            effect: Effect::Rainbow,
            color: [0x00, 0x80, 0xFF],
            period_ms: 4000,
            spread_pct: 50,
            count: 4,
            bright: 64,
            status_bright: 32,
        }
    }
}

impl LedSettings {
    /// 設定ファイルの1行。`効果,RRGGBB,周期ms,ずらし%,個数,明るさ,状態LEDの明るさ`
    pub fn format(&self) -> String {
        format!(
            "{},{:06x},{},{},{},{},{}",
            self.effect.key(), rgb(self.color), self.period_ms, self.spread_pct,
            self.count, self.bright, self.status_bright
        )
    }

    pub fn parse(v: &str) -> Option<Self> {
        let f: Vec<&str> = v.split(',').map(str::trim).collect();
        if f.len() != 7 {
            return None;
        }
        let c = u32::from_str_radix(f[1], 16).ok()?;
        Some(Self {
            effect: Effect::from_key(f[0])?,
            color: [(c >> 16) as u8, (c >> 8) as u8, c as u8],
            period_ms: f[2].parse().ok()?,
            spread_pct: f[3].parse().ok()?,
            count: f[4].parse::<u8>().ok()?.clamp(1, MAX_LEDS),
            bright: f[5].parse().ok()?,
            status_bright: f[6].parse().ok()?,
        })
    }
}

fn rgb(c: [u8; 3]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
}

/// フレームヘッダ。フェード速度は「1ms ごとに 16bit の位相へ足す量」で、
/// FPGA に割り算をさせないためにここで割っておく。0 はフェードなし。
fn header(hold_ms: u32, fade_ms: u32) -> u32 {
    let rate = if fade_ms == 0 { 0 } else { (65536 / fade_ms).clamp(1, 0xFFFF) };
    (rate << 16) | hold_ms.min(0xFFFF)
}

/// 彩度・明度最大の色相 h(0〜1)。
fn hue(h: f64) -> [u8; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let x = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let f = h.fract();
    match h as u32 {
        0 => [255, x(f), 0],
        1 => [x(1.0 - f), 255, 0],
        2 => [0, 255, x(f)],
        3 => [0, x(1.0 - f), 255],
        4 => [x(f), 0, 255],
        _ => [255, 0, x(1.0 - f)],
    }
}

/// 設定をキーフレーム表に直す。戻り値は (表の語, フレーム数)。
pub fn build(s: &LedSettings) -> (Vec<u32>, u8) {
    let n = s.count.clamp(1, MAX_LEDS) as usize;
    let period = s.period_ms.clamp(200, 60_000);
    let mut frames: Vec<(u32, Vec<u32>)> = Vec::new();
    match s.effect {
        Effect::Off => return (Vec::new(), 0),
        Effect::Solid => frames.push((header(1000, 0), vec![rgb(s.color); n])),
        Effect::Blink => {
            frames.push((header(period / 2, 0), vec![rgb(s.color); n]));
            frames.push((header(period / 2, 0), vec![0; n]));
        }
        Effect::Breathe => {
            // 明るさを線形に振ると暗い側が一瞬で終わって見えるので、
            // ガンマ 2.2 の曲線を8段の折れ線で近似する(上り8段 + 下り8段)
            const STEPS: u32 = 8;
            let fade = period / (2 * STEPS);
            let level = |k: u32| {
                let v = (k as f64 / STEPS as f64).powf(2.2);
                let c = s.color.map(|c| (c as f64 * v).round() as u8);
                rgb(c)
            };
            for k in (0..STEPS).chain((1..=STEPS).rev()) {
                frames.push((header(0, fade), vec![level(k); n]));
            }
        }
        Effect::Rainbow => {
            // 12段。原色と二次色の間を RGB で線形補間すると色相の一周と一致する
            // ので6段でも足りるが、LED ごとにずらすと段の途中の色が要るため倍にする
            const STEPS: usize = 12;
            let fade = period / STEPS as u32;
            let spread = s.spread_pct.min(100) as f64 / 100.0;
            for k in 0..STEPS {
                let colors = (0..n)
                    .map(|i| rgb(hue(k as f64 / STEPS as f64 + spread * i as f64 / n as f64)))
                    .collect();
                frames.push((header(0, fade), colors));
            }
        }
    }
    let words: Vec<u32> = frames
        .iter()
        .flat_map(|(h, c)| std::iter::once(*h).chain(c.iter().copied()))
        .collect();
    debug_assert!(words.len() <= BANK_WORDS);
    (words, frames.len() as u8)
}

/// Viewer が出したい状態。UI が書き、送信スレッドが読む。
#[derive(Default)]
pub struct LedRequest {
    /// None ならボードの既定(ゲートウェア内蔵のレインボー)に任せて何も送らない
    pub want: Option<LedSettings>,
    /// 最後に送れた/失敗した結果。UI の表示用
    pub status: String,
}

/// ボードに書いた結果として、読み返したときに見えるはずの値。
#[derive(Clone, PartialEq, Debug)]
struct Applied {
    mac: [u8; 6],
    settings: LedSettings,
    bank: u32,
}

struct Link {
    sock: UdpSocket,
    port: u16,
    targets: Vec<String>,
    mac: [u8; 6],
    seq: u16,
}

impl Link {
    /// 1回送って、同じキーの応答を待つ。応答の値を返す。
    fn xfer(&mut self, op: u8, key: u16, value: u32) -> Option<u32> {
        let mut buf = [0u8; 64];
        for _ in 0..5 {
            self.seq = self.seq.wrapping_add(1);
            let pkt = proto::pack_config(self.seq, 0, op, key, value, &self.mac);
            for t in &self.targets {
                let _ = self.sock.send_to(&pkt, (t.as_str(), self.port));
            }
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_millis(40) {
                let Ok((n, _)) = self.sock.recv_from(&mut buf) else { continue };
                if n >= 24
                    && buf[2] == proto::TYPE_CONFIG
                    && buf[3] & proto::CFG_FLAG_REPLY != 0
                    && buf[8..14] == self.mac
                    && u16::from_le_bytes([buf[18], buf[19]]) == key
                {
                    let v = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
                    // SET は書いた値が返るまで待つ(前の応答の遅れを掴まない)
                    if op == proto::CFG_OP_GET || v == value {
                        return Some(v);
                    }
                }
            }
        }
        None
    }

    fn set(&mut self, key: u16, value: u32) -> Result<(), String> {
        self.xfer(proto::CFG_OP_SET, key, value)
            .map(|_| ())
            .ok_or_else(|| format!("キー 0x{key:04x} の応答がありません"))
    }

    fn get(&mut self, key: u16) -> Option<u32> {
        self.xfer(proto::CFG_OP_GET, key, 0)
    }
}

/// 表を「いま再生していない方のバンク」へ書き、全語の応答を確かめてから切り替える。
///
/// 書いている途中の表が再生されないよう、構成(個数/フレーム数/バンク)は最後に
/// まとめて書く。
fn upload(link: &mut Link, s: &LedSettings) -> Result<u32, String> {
    let cur = link.get(KEY_KLED_BANK).ok_or("ボードが応答しません")?;
    let bank = (cur & 1) ^ 1;
    let (words, frames) = build(s);
    for (k, w) in words.iter().enumerate() {
        link.set(KEY_TABLE_BASE + (bank as u16) * BANK_WORDS as u16 + k as u16, *w)?;
    }
    link.set(KEY_KLED_BRIGHT, s.bright as u32)?;
    link.set(KEY_STAT_BRIGHT, s.status_bright as u32)?;
    link.set(KEY_KLED_COUNT, s.count.clamp(1, MAX_LEDS) as u32)?;
    link.set(KEY_KLED_FRAMES, frames as u32)?;
    link.set(KEY_KLED_BANK, bank)?;
    Ok(bank)
}

/// ボードがいま、こちらの書いた状態のままか。電源の入れ直しやボードの
/// 差し替えで既定(バンク0 + 内蔵レインボー)に戻っていたら false。
fn still_applied(link: &mut Link, a: &Applied) -> bool {
    let (_, frames) = build(&a.settings);
    let s = &a.settings;
    [
        (KEY_KLED_BANK, a.bank),
        (KEY_KLED_FRAMES, frames as u32),
        (KEY_KLED_COUNT, s.count.clamp(1, MAX_LEDS) as u32),
        (KEY_KLED_BRIGHT, s.bright as u32),
        (KEY_STAT_BRIGHT, s.status_bright as u32),
    ]
    .into_iter()
    .all(|(k, v)| link.get(k) == Some(v))
}

/// 送信スレッドを起こす。UI は `req.want` を書き換えるだけでよい。
pub fn spawn(shared: Arc<Shared>, port: u16, req: Arc<Mutex<LedRequest>>) {
    std::thread::spawn(move || {
        let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
        let _ = sock.set_broadcast(true);
        let _ = sock.set_read_timeout(Some(Duration::from_millis(10)));
        let mut applied: Option<Applied> = None;
        let mut checked = Instant::now() - Duration::from_secs(60);
        let mut seq = 0u16;
        while !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
            let want = req.lock().unwrap().want.clone();
            let Some(want) = want else {
                applied = None;
                continue;
            };
            let Some(mac) = *shared.claimed.lock().unwrap() else { continue };
            let targets: Vec<String> = shared
                .sub_dest
                .lock()
                .unwrap()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            if targets.is_empty() {
                continue;
            }
            let mut link = Link { sock: sock.try_clone().unwrap(), port, targets, mac, seq };
            let same = applied.as_ref().is_some_and(|a| a.mac == mac && a.settings == want);
            // 変わっていなければ5秒に1回だけ、ボードが覚えているかを確かめる
            let stale = same
                && checked.elapsed() >= Duration::from_secs(5)
                && {
                    checked = Instant::now();
                    !still_applied(&mut link, applied.as_ref().unwrap())
                };
            if !same || stale {
                let t0 = Instant::now();
                let r = upload(&mut link, &want);
                checked = Instant::now();
                let mut q = req.lock().unwrap();
                match r {
                    Ok(bank) => {
                        q.status = format!(
                            "送信済み({} ms{})",
                            t0.elapsed().as_millis(),
                            if stale { "、ボードが初期状態に戻っていたので再送" } else { "" }
                        );
                        applied = Some(Applied { mac, settings: want, bank });
                    }
                    Err(e) => {
                        q.status = format!("送れませんでした: {e}");
                        applied = None;
                    }
                }
            }
            seq = link.seq;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★**どの組み合わせでも1バンクに収まること。** 溢れるとゲートウェア側で
    ///   隣のバンクを読み、書いている途中の表が混ざって光る。
    #[test]
    fn every_effect_fits_one_bank() {
        for effect in Effect::ALL {
            for count in 1..=MAX_LEDS {
                let s = LedSettings { effect, count, ..Default::default() };
                let (w, f) = build(&s);
                assert!(w.len() <= BANK_WORDS, "{effect:?} ×{count}: {} 語", w.len());
                assert_eq!(w.len(), f as usize * (1 + count as usize), "{effect:?} ×{count}");
            }
        }
    }

    /// フェード速度は 65536/ms。周期どおりに一周すること(切り捨てで少し長くなるだけ)
    #[test]
    fn rainbow_period_matches() {
        let s = LedSettings { effect: Effect::Rainbow, period_ms: 6000, ..Default::default() };
        let (w, f) = build(&s);
        let stride = 1 + s.count as usize;
        let total: f64 = (0..f as usize)
            .map(|k| 65536.0 / (w[k * stride] >> 16) as f64 + (w[k * stride] & 0xFFFF) as f64)
            .sum();
        assert!((total - 6000.0).abs() < 60.0, "1周 {total:.0} ms");
    }

    #[test]
    fn hue_hits_primaries() {
        assert_eq!(hue(0.0), [255, 0, 0]);
        assert_eq!(hue(1.0 / 3.0), [0, 255, 0]);
        assert_eq!(hue(2.0 / 3.0), [0, 0, 255]);
    }

    #[test]
    fn settings_line_round_trips() {
        let s = LedSettings { effect: Effect::Breathe, color: [1, 2, 3], ..Default::default() };
        assert_eq!(LedSettings::parse(&s.format()), Some(s));
    }

    #[test]
    fn effect_keys_round_trip() {
        for e in Effect::ALL {
            assert_eq!(Effect::from_key(e.key()), Some(e));
        }
    }
}
