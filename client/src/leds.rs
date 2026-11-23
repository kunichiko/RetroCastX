//! ボードのシリアルLED(小亀基板のLED列と、メイン基板の状態LED)の設定。
//!
//! ★**効果(消灯・固定色・点滅・呼吸・レインボー)はゲートウェアが作る。**
//!   Viewer は効果の種類・色・周期・明るさを CONFIG で書くだけ
//!   (`gateware/retrocastx_led.py` の KokameLeds)。以前はここでキーフレーム表を
//!   作って送り、ゲートウェアは再生するだけだったが、表の再生が FPGA のロジックを
//!   約 900 LUT 使っていたので 2026-11-23 に置き換えた。
//!
//! 送るのは専用のスレッドと専用のソケット。ボードは CONFIG の応答を1つしか
//! 溜めないので、まとめて投げると応答が上書きされて届いたかどうか確かめられない。
//! 1つずつ応答を待って送る。受信スレッドのソケットを使わないのは、そちらで待つと
//! 映像の受信が止まるため。

use std::net::UdpSocket;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::protocol as proto;
use crate::receiver::Shared;

pub const KEY_KLED_COUNT: u16 = 0x0090;
pub const KEY_KLED_BRIGHT: u16 = 0x0093;
pub const KEY_STAT_BRIGHT: u16 = 0x0094;
pub const KEY_STAT_STATE: u16 = 0x0095;
/// Viewer が居ないあいだの動作。EEPROM の個体設定ページに置かれ、電源投入直後から効く
pub const KEY_KLED_IDLE: u16 = 0x0097;
/// 効果 0=消灯 1=固定色 2=点滅 3=呼吸 4=レインボー
pub const KEY_KLED_EFFECT: u16 = 0x0098;
/// 色 0x00RRGGBB(固定色・点滅・呼吸)
pub const KEY_KLED_COLOR: u16 = 0x0099;
/// 1ms ごとに 16bit の位相へ足す量 = 65536 / 周期[ms](FPGA で割らないため)
pub const KEY_KLED_RATE: u16 = 0x009A;
/// レインボーで LED ごとにずらす色相(16bit で1周)
pub const KEY_KLED_SPREAD: u16 = 0x009B;
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

    /// CONFIG 0x0098 の値(gateware/retrocastx_led.py の EFF_*)
    pub fn code(self) -> u32 {
        match self {
            Effect::Off => 0,
            Effect::Solid => 1,
            Effect::Blink => 2,
            Effect::Breathe => 3,
            Effect::Rainbow => 4,
        }
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

/// Viewer が居ないあいだ(ボードが映像を送っていないあいだ)の小亀のLED。
///
/// ★**ボードが自分でやる。** 居ないのだから Viewer は指示できない。値は EEPROM に
///   焼くので電源投入直後から効く(`gateware/retrocastx_led.py` 冒頭)。呼吸は
///   明るさだけを変えるので、色は表(固定色・レインボー…)のまま。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Idle {
    Keep,
    Breathe,
    Off,
}

impl Idle {
    pub const ALL: [Idle; 3] = [Idle::Keep, Idle::Breathe, Idle::Off];

    pub fn key(self) -> &'static str {
        match self {
            Idle::Keep => "keep",
            Idle::Breathe => "breathe",
            Idle::Off => "off",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Idle::Keep => "そのまま光らせる",
            Idle::Breathe => "呼吸",
            Idle::Off => "消灯",
        }
    }

    pub fn from_key(s: &str) -> Option<Idle> {
        Idle::ALL.into_iter().find(|e| e.key() == s)
    }

    /// CONFIG 0x0097 の値
    pub fn code(self) -> u32 {
        match self {
            Idle::Keep => 0,
            Idle::Breathe => 1,
            Idle::Off => 2,
        }
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
    /// Viewer が居ないあいだの動作
    pub idle: Idle,
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
            idle: Idle::Keep,
        }
    }
}

impl LedSettings {
    /// 設定ファイルの1行。`効果,RRGGBB,周期ms,ずらし%,個数,明るさ,状態LEDの明るさ`
    ///
    /// ★**切断中(idle)はこの行に入れない**(settings.rs の `led_idle`)。項目を
    ///   足すと、7項目しか読めない古い Viewer がこの行ごと捨てて LED の設定が
    ///   既定に戻る。別のキーなら古い Viewer は読み飛ばすだけで済む
    pub fn format(&self) -> String {
        format!(
            "{},{:06x},{},{},{},{},{}",
            self.effect.key(), rgb(self.color), self.period_ms, self.spread_pct,
            self.count, self.bright, self.status_bright
        )
    }

    /// idle は Keep で返す(settings.rs が `led_idle` から入れ直す)
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
            idle: Idle::Keep,
        })
    }
}

fn rgb(c: [u8; 3]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
}

/// ボードに書くレジスタの値(キー, 値)。書く順もこの並び。
pub fn regs(s: &LedSettings) -> Vec<(u16, u32)> {
    let n = s.count.clamp(1, MAX_LEDS) as u32;
    let period = s.period_ms.clamp(200, 60_000);
    let spread = (65536.0 * s.spread_pct.min(100) as f64 / 100.0 / n as f64).round() as u32;
    vec![
        (KEY_KLED_COLOR, rgb(s.color)),
        (KEY_KLED_RATE, (65536 / period).max(1)),
        (KEY_KLED_SPREAD, spread.min(0xFFFF)),
        (KEY_KLED_BRIGHT, s.bright as u32),
        (KEY_STAT_BRIGHT, s.status_bright as u32),
        (KEY_KLED_COUNT, n),
        (KEY_KLED_EFFECT, s.effect.code()),
    ]
}

/// Viewer が出したい状態。UI が書き、送信スレッドが読む。
#[derive(Default)]
pub struct LedRequest {
    /// None ならボードの既定(ゲートウェア内蔵のレインボー)に任せて何も送らない
    pub want: Option<LedSettings>,
    /// 最後に送れた/失敗した結果。UI の表示用
    pub status: String,
}

/// ボードに書いた設定。
#[derive(Clone, PartialEq, Debug)]
struct Applied {
    mac: [u8; 6],
    settings: LedSettings,
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
                    // SET は書いた値が返るまで待つ(前の応答の遅れを掴まない)。
                    // EEPROM へ焼く(0x004C)は応答が保存の進み具合なので値を見ない
                    if op == proto::CFG_OP_GET || v == value || key == proto::CFG_KEY_IDENT_SAVE {
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

/// 効果の値を書く(1つずつ応答を確かめる)。
fn upload(link: &mut Link, s: &LedSettings) -> Result<(), String> {
    for (k, v) in regs(s) {
        link.set(k, v)?;
    }
    Ok(())
}

/// Viewer が居ないあいだの動作をボードに合わせ、違っていれば EEPROM へ焼く。
/// 焼いたら true。
///
/// ★**焼くのは値が違うときだけ。** EEPROM には書き換え回数の寿命がある。
///   起動時の値は EEPROM から読んだものなので、読み返して同じなら焼き直す必要もない。
/// ★**LED の欄だけを焼く(0x004C に 2)。** 1 だと名前・IP の「適用したが
///   まだ焼いていない」試しまで一緒に焼いてしまう。
fn sync_idle(link: &mut Link, idle: Idle) -> Result<bool, String> {
    let cur = link.get(KEY_KLED_IDLE).ok_or("ボードが応答しません")?;
    if cur == idle.code() {
        return Ok(false);
    }
    link.set(KEY_KLED_IDLE, idle.code())?;
    link.set(proto::CFG_KEY_IDENT_SAVE, proto::IDENT_SAVE_LED_ONLY)?;
    // 焼き終わるまで(8バイト×4ページ、各最大5ms)待つ。0=未実行 1=実行中 2=成功 3=失敗
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(2) {
        match link.get(proto::CFG_KEY_IDENT_SAVE) {
            Some(2) => return Ok(true),
            Some(3) => return Err("EEPROM へ焼けませんでした".into()),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    Err("EEPROM へ焼き終わりません".into())
}

/// ボードがいま、こちらの書いた状態のままか。電源の入れ直しやボードの
/// 差し替えで既定(内蔵のレインボー)に戻っていたら false。
fn still_applied(link: &mut Link, a: &Applied) -> bool {
    regs(&a.settings).into_iter().all(|(k, v)| link.get(k) == Some(v))
}

/// 送信スレッドを起こす。UI は `req.want` を書き換えるだけでよい。
pub fn spawn(shared: Arc<Shared>, port: u16, req: Arc<Mutex<LedRequest>>) {
    std::thread::spawn(move || {
        let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
        let _ = sock.set_broadcast(true);
        let _ = sock.set_read_timeout(Some(Duration::from_millis(10)));
        let mut applied: Option<Applied> = None;
        // 切断中の動作をどのボードにどの値で合わせたか(合わせたら繋ぎ直すまで触らない)
        let mut idle_done: Option<([u8; 6], Idle)> = None;
        let mut checked = Instant::now() - Duration::from_secs(60);
        let mut seq = 0u16;
        while !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
            let want = req.lock().unwrap().want.clone();
            let Some(want) = want else {
                applied = None;
                idle_done = None;
                continue;
            };
            let idle = want.idle;
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
            // 切断中の動作は表と別に合わせる(下の sync_idle)ので、比べるときは除く
            let same = applied.as_ref().is_some_and(|a| {
                a.mac == mac && LedSettings { idle: a.settings.idle, ..want.clone() } == a.settings
            });
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
                    Ok(()) => {
                        q.status = format!(
                            "送信済み({} ms{})",
                            t0.elapsed().as_millis(),
                            if stale { "、ボードが初期状態に戻っていたので再送" } else { "" }
                        );
                        applied = Some(Applied { mac, settings: want });
                    }
                    Err(e) => {
                        q.status = format!("送れませんでした: {e}");
                        applied = None;
                    }
                }
            }
            // ★**繋いだときと変えたときだけ合わせる。** 定期的に確かめると、違う設定の
            //   Viewer が2つあるとき交互に焼き合って EEPROM を擦り減らす
            let caps = shared.boards.lock().unwrap().get(&mac).map_or(0, |b| b.caps);
            if caps & proto::CAP_LED_IDLE != 0 && idle_done != Some((mac, idle)) {
                let r = sync_idle(&mut link, idle);
                let mut q = req.lock().unwrap();
                match r {
                    Ok(saved) => {
                        if saved {
                            q.status = format!("切断中は「{}」をボードに保存しました",
                                               idle.label());
                        }
                        idle_done = Some((mac, idle));
                    }
                    // 失敗したら次の周回でやり直す
                    Err(e) => q.status = format!("切断中の動作を保存できませんでした: {e}"),
                }
            }
            seq = link.seq;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 周期は 65536 / ms。ゲートウェアは 1ms ごとに足すので、1周がほぼ周期どおり
    #[test]
    fn rate_matches_period() {
        let s = LedSettings { effect: Effect::Rainbow, period_ms: 6000, ..Default::default() };
        let rate = regs(&s).into_iter().find(|(k, _)| *k == KEY_KLED_RATE).unwrap().1;
        let ms = 65536.0 / rate as f64;
        assert!((ms - 6000.0).abs() < 600.0, "1周 {ms:.0} ms");
    }

    /// 既定の設定はゲートウェアの電源投入時の値と同じ(retrocastx_led.py の KokameLeds)。
    /// 同じなら、Viewer が繋いだ瞬間に色が飛ばない
    #[test]
    fn defaults_match_gateware() {
        let r: std::collections::HashMap<u16, u32> =
            regs(&LedSettings::default()).into_iter().collect();
        assert_eq!(r[&KEY_KLED_EFFECT], 4);
        assert_eq!(r[&KEY_KLED_COLOR], 0x0080FF);
        assert_eq!(r[&KEY_KLED_RATE], 65536 / 4000);
        assert_eq!(r[&KEY_KLED_SPREAD], 65536 / 2 / 4);
        assert_eq!(r[&KEY_KLED_COUNT], 4);
        assert_eq!(r[&KEY_KLED_BRIGHT], 64);
    }

    #[test]
    fn settings_line_round_trips() {
        let s = LedSettings { effect: Effect::Breathe, color: [1, 2, 3], ..Default::default() };
        assert_eq!(LedSettings::parse(&s.format()), Some(s));
    }

    /// ★**LED の行は7項目のまま。** 増やすと古い Viewer がこの行を捨てる
    #[test]
    fn settings_line_stays_seven_fields() {
        let s = LedSettings { idle: Idle::Off, ..Default::default() };
        assert_eq!(s.format().split(',').count(), 7);
    }

    #[test]
    fn idle_keys_round_trip() {
        for e in Idle::ALL {
            assert_eq!(Idle::from_key(e.key()), Some(e));
        }
    }

    #[test]
    fn effect_keys_round_trip() {
        for e in Effect::ALL {
            assert_eq!(Effect::from_key(e.key()), Some(e));
        }
    }
}
