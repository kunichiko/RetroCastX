//! ボードの状態: 個体情報・CONFIG レジスタ・購読・端子に繋がった機種・測定値。
//!
//! 応答の規則は `gateware/retrocastx_stream.py` に合わせてある:
//!
//! - SUBSCRIBE / CONFIG は宛先MACが自分か全FFのときだけ反応する
//! - SUBSCRIBE は種類を問わず ANNOUNCE の宛先を送信元へ変え、すぐ ANNOUNCE を返す。
//!   ANNOUNCE_ONLY でなく、RENEW でないか RENEW でも送り先が同じなら購読する(10秒)
//! - CONFIG は送信元へ REPLY を返す。知らないキーは 0 を返して SET を捨てる
//! - ANNOUNCE は購読の有無によらず1秒ごとに、最後の SUBSCRIBE の送信元へ送る

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::capture::{CapCfg, LINE_BUFFER_W, MODE_ID};
use crate::machines::{Kind, Mode, Port, Scan};
use crate::pattern::Options;
use crate::protocol as proto;
use crate::signal::Source;

pub const SUB_LIFETIME: Duration = Duration::from_secs(10);
/// ボードの sys クロック(MODE の末尾と、組の値の単位)
pub const SYS_CLK_HZ: u32 = 45_000_000;

// ANNOUNCE の caps
pub const CAP_EEPROM_MAC: u16 = 1 << 0;

/// 端子に繋いだ機種
#[derive(Clone, Debug)]
pub struct Input {
    pub machine_key: String,
    pub kind: Kind,
    pub mode: Mode,
}

impl Input {
    pub fn source(&self) -> Source {
        Source { kind: self.kind, timing: self.mode.timing.clone(), color_bits: self.mode.color_bits,
                 palette: self.mode.palette }
    }

    /// HSYNC/VSYNC が別線で来るか(D-SUB15 の RGB 機)。来ればボードが生のまま数える
    pub fn raw_sync(&self, port: Port) -> bool {
        port == Port::Rgb15 && self.kind == Kind::Rgb
    }

    /// 同期が映像に乗って TVP の SOG を通るか
    pub fn on_sog(&self, port: Port) -> bool {
        self.kind != Kind::Rgb || port == Port::Rgb21
    }

    pub fn caption(&self) -> String {
        format!("{} {}", self.machine_key.to_uppercase(), self.mode.name)
    }
}

/// 測定値(ボードが1秒窓などで数えている値)。送出のスレッドが更新する
#[derive(Clone, Debug, Default)]
pub struct Meas {
    /// 1秒窓の HSYNC 数[Hz] と DATACLK 数[Hz]
    pub hfreq: u32,
    pub dotclk: u32,
    /// 8秒窓の VSYNC 数 × 125 [mHz]
    pub vfreq_mhz: u32,
    /// 4回続けて同じだった VSYNC 間のライン数
    pub vtotal: u32,
    /// 3回続けて同じだった2周期分の組。0 = まだ無い/VSYNC が来ない
    pub frame_clocks: u32,
    pub frame_lines: u16,
    pub il_frame: bool,
    /// 直前の MODE の htotal(caps bit3 が無い古いゲートウェアは1秒遅れる)
    pub htotal_window: u32,
}

/// 届いた CONFIG / SUBSCRIBE の記録(画面に出す)
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub at: Instant,
    pub text: String,
}

pub struct Board {
    pub mac: [u8; 6],
    pub name: String,
    pub fw_version: u16,
    pub caps: u16,
    pub regs: HashMap<u16, u32>,
    pub inputs: [Option<Input>; 4],
    pub picture: Options,
    /// 絵が変わったら増やす(サンプルの作り置きの鍵)
    pub pic_gen: u64,
    pub noise: u8,
    /// LINE を捨てる割合[%]
    pub loss_pct: f32,
    /// 音声に曲(music.rs)を流す(切ると無音のパケットだけ送る。実機の ADC は常に送る)
    pub tone: bool,
    /// 映像の送り先と失効時刻。期限が切れても送り先は覚えている(RENEW の判定)
    pub sub: Option<SocketAddr>,
    pub sub_until: Option<Instant>,
    pub ann_dest: Option<SocketAddr>,
    pub last_contact: Option<Instant>,
    /// MODE をすぐ送る要求(購読したとき)
    pub mode_now: bool,
    pub ident_save: u32,
    pub meas: Meas,
    pub log: VecDeque<LogEntry>,
    seed: u32,
}

impl Board {
    pub fn new() -> Board {
        let mut b = Board {
            // ローカル管理アドレス。実機のフォールバック 02:52:43:58:00:01 とは変えておく
            // (同じ LAN に実機がいても区別できるように)
            mac: [0x02, 0x52, 0x43, 0x58, 0xD5, 0x01],
            name: "devsim".into(),
            // gw-v1.6.1 と同じ (major<<12)|(minor<<6)|patch
            fw_version: (1 << 12) | (6 << 6) | 1,
            caps: CAP_EEPROM_MAC | proto::CAP_COMPACT | proto::CAP_LED_IDLE
                | proto::CAP_FRAME_CLOCKS | proto::CAP_DRGB,
            regs: HashMap::new(),
            inputs: [None, None, None, None],
            picture: Options { pattern: crate::pattern::Pattern::Composite, moving: true, caption: String::new() },
            pic_gen: 1,
            noise: 0,
            loss_pct: 0.0,
            tone: true,
            sub: None,
            sub_until: None,
            ann_dest: None,
            last_contact: None,
            mode_now: false,
            ident_save: 0,
            meas: Meas::default(),
            log: VecDeque::new(),
            seed: 0x2545_F491,
        };
        for &(k, v) in DEFAULTS {
            b.regs.insert(k, v);
        }
        b
    }

    pub fn reg(&self, key: u16) -> u32 {
        self.regs.get(&key).copied().unwrap_or(0)
    }

    pub fn subscribed(&self, now: Instant) -> bool {
        self.sub_until.is_some_and(|t| now < t)
    }

    /// TVP がいま見ている端子(入力MUX 19h)
    pub fn tvp_port(&self) -> Port {
        Port::from_mux(self.reg(proto::CFG_KEY_IN_MUX1))
    }

    pub fn input(&self, port: Port) -> Option<&Input> {
        self.inputs[port as usize].as_ref()
    }

    /// TVP に届いている信号。デジタルRGB は TVP を通らないので無い
    pub fn tvp_input(&self) -> Option<&Input> {
        let p = self.tvp_port();
        if p == Port::Digital { None } else { self.input(p) }
    }

    /// デジタルRGB を取り込んでいるか(0x3A = 1。CAP_DRGB を外したボードでは常に TVP)
    pub fn digital(&self) -> bool {
        self.caps & proto::CAP_DRGB != 0 && self.reg(proto::CFG_KEY_VIDEO_SRC) == 1
    }

    /// 取り込んでいる端子(デジタルRGB を選んでいれば入力4、でなければ TVP の端子)
    pub fn capture_port(&self) -> Port {
        if self.digital() { Port::Digital } else { self.tvp_port() }
    }

    /// 取り込んで送っている信号。デジタルRGB を選んでいれば入力4、でなければ TVP の端子
    pub fn capture_input(&self) -> Option<&Input> {
        if self.digital() { self.input(Port::Digital) } else { self.tvp_input() }
    }

    pub fn cap_cfg(&self) -> CapCfg {
        let r = |k| self.reg(k);
        CapCfg {
            pll_divide: r(proto::CFG_KEY_PLL_DIVIDE),
            phase: r(proto::CFG_KEY_PHASE),
            vbp: r(proto::CFG_KEY_VBP),
            // デジタルRGB は TVP を通らないので、TVP の HSYNC 直後の捨てを効かせない
            hs_offset: if self.digital() { 0 } else { r(proto::CFG_KEY_HS_OFFSET) },
            full_line: r(proto::CFG_KEY_FULL_LINE) != 0,
            win_start: r(proto::CFG_KEY_WIN_START),
            win_len: r(proto::CFG_KEY_WIN_LEN),
            black_th: r(proto::CFG_KEY_BLACK_TH),
            // デジタルRGB は 0x36 を見ず、いつも RGBI4
            pixfmt: if self.digital() { proto::PIXFMT_RGBI4 } else { r(proto::CFG_KEY_PIXFMT) as u8 },
            // 色数は機種で決まる(engine が入力から入れる)
            palette: Default::default(),
            frame_skip: r(proto::CFG_KEY_FRAME_SKIP),
            field_invert: r(proto::CFG_KEY_FIELD_INVERT) != 0,
        }
    }

    pub fn log(&mut self, text: String) {
        self.log.push_back(LogEntry { at: Instant::now(), text });
        while self.log.len() > 300 {
            self.log.pop_front();
        }
    }

    fn rand(&mut self) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed
    }

    /// LINE を1つ捨てるか(パケットロスの模擬)
    pub fn drop_line(&mut self) -> bool {
        self.loss_pct > 0.0 && (self.rand() % 100_000) as f32 / 1000.0 < self.loss_pct
    }

    /// 受けたパケットを処理し、返すパケットを (宛先, 中身) で返す
    pub fn handle(&mut self, pkt: &[u8], from: SocketAddr, seq: &mut u16, now: Instant)
        -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        if pkt.len() < 8 || pkt[0] != proto::MAGIC || pkt[1] != proto::VERSION {
            return out;
        }
        let mac_ok = |m: &[u8]| m == self.mac || m == proto::WILDCARD_MAC;
        match pkt[2] {
            proto::TYPE_SUBSCRIBE if pkt.len() == 16 && mac_ok(&pkt[8..14]) => {
                let flags = pkt[3];
                let only = flags & 1 != 0;
                let renew = flags & proto::SUB_FLAG_RENEW != 0;
                self.ann_dest = Some(from);
                self.last_contact = Some(now);
                let take = !only && (!renew || self.sub == Some(from));
                if take {
                    if self.sub != Some(from) || !self.subscribed(now) {
                        self.log(format!("購読: {from}{}", if renew { " (RENEW)" } else { "" }));
                    }
                    self.sub = Some(from);
                    self.sub_until = Some(now + SUB_LIFETIME);
                    self.mode_now = true;
                } else if !only {
                    self.log(format!("RENEW を断った: {from}(送り先は {:?})", self.sub));
                }
                out.push((from, self.announce(from, seq, now)));
            }
            proto::TYPE_CONFIG if pkt.len() == 24 && mac_ok(&pkt[8..14]) && pkt[3] & 1 == 0 => {
                self.last_contact = Some(now);
                let target = pkt[16];
                let op = pkt[17];
                let key = u16::from_le_bytes([pkt[18], pkt[19]]);
                let value = u32::from_le_bytes([pkt[20], pkt[21], pkt[22], pkt[23]]);
                let v = if target != 0 {
                    // ArgusX: 1語の保存場所だけ
                    if op == proto::CFG_OP_SET {
                        self.regs.insert(0xFFFF, value);
                    }
                    self.reg(0xFFFF)
                } else {
                    if op == proto::CFG_OP_SET {
                        self.set(key, value);
                    }
                    self.get(key, now)
                };
                if op == proto::CFG_OP_SET || !QUIET_GETS.contains(&key) {
                    let what = if op == proto::CFG_OP_SET { format!("SET {value}") } else { "GET".into() };
                    self.log(format!("CONFIG 0x{key:04X} {} {what} → {v}", key_name(key)));
                }
                let mut p = pkt.to_vec();
                p[3] = proto::CFG_FLAG_REPLY;
                p[4..6].copy_from_slice(&0u16.to_le_bytes());
                p[6..8].copy_from_slice(&seq.to_le_bytes());
                *seq = seq.wrapping_add(1);
                p[8..14].copy_from_slice(&self.mac);
                p[14] = 0;
                p[15] = 0;
                p[20..24].copy_from_slice(&v.to_le_bytes());
                out.push((from, p));
            }
            _ => {}
        }
        out
    }

    fn set(&mut self, key: u16, value: u32) {
        if UNIMPLEMENTED.contains(&key) || READ_ONLY.contains(&key) {
            return;
        }
        if key == proto::CFG_KEY_IDENT_SAVE {
            // 焼いたことにする(成功)
            self.ident_save = if matches!(value, 1 | 2) { 2 } else { 0 };
            return;
        }
        let v = value & width_mask(key);
        if key == proto::CFG_KEY_PIXFMT || key == proto::CFG_KEY_FRAME_SKIP {
            // 形式を変えたら作り置きは使えるが、範囲の規則が変わるので MODE をすぐ出す
            self.mode_now = true;
        }
        if (proto::CFG_KEY_NAME0..=proto::CFG_KEY_NAME3).contains(&key) {
            self.regs.insert(key, v);
            let mut bytes = Vec::new();
            for k in proto::CFG_KEY_NAME0..=proto::CFG_KEY_NAME3 {
                bytes.extend_from_slice(&self.reg(k).to_le_bytes());
            }
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(16);
            self.name = String::from_utf8_lossy(&bytes[..end]).into_owned();
            return;
        }
        self.regs.insert(key, v);
    }

    /// 名前を変えたら CONFIG の 0x48〜0x4B も合わせる
    pub fn set_name(&mut self, name: &str) {
        let mut b = [0u8; 16];
        let src = name.as_bytes();
        let mut n = src.len().min(16);
        while !name.is_char_boundary(n) {
            n -= 1;
        }
        b[..n].copy_from_slice(&src[..n]);
        for (i, k) in (proto::CFG_KEY_NAME0..=proto::CFG_KEY_NAME3).enumerate() {
            self.regs.insert(k, u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]));
        }
        self.name = String::from_utf8_lossy(&b[..n]).into_owned();
    }

    /// GET の値。測定値はいまの信号から作る
    fn get(&mut self, key: u16, now: Instant) -> u32 {
        if UNIMPLEMENTED.contains(&key) {
            return 0;
        }
        let tvp = self.tvp_input().cloned();
        let tvp_port = self.tvp_port();
        let rgb15 = self.input(Port::Rgb15).cloned();
        let raw = rgb15.as_ref().filter(|i| i.raw_sync(Port::Rgb15));
        let fh = |i: &Input| i.mode.timing.fh();
        let fv = |i: &Input| i.mode.timing.fv();
        // VSYNC 間のライン数(フィールドごとの VSYNC なら長い方)
        let lines_vs = |i: &Input| match i.mode.timing.scan {
            Scan::Interlace => i.mode.timing.vtotal.div_ceil(2),
            _ => i.mode.timing.vtotal,
        };
        let pll = self.reg(proto::CFG_KEY_PLL_DIVIDE);
        match key {
            0x0006 => 1, // EEPROM から MAC を読めた
            0x0007 => u32::from_le_bytes([self.mac[5], self.mac[4], self.mac[3], self.mac[2]]),
            0x0008 => u16::from_le_bytes([self.mac[1], self.mac[0]]) as u32,
            proto::CFG_KEY_BUILD_ID => 0x0de5_1a00,
            proto::CFG_KEY_IDENT_SAVE => self.ident_save,
            proto::CFG_KEY_IDENT_EE => 1,
            0x004E => 0x0110, // LAN0 にリンク、EUI-48 を読めた
            0x0095 => {
                if self.subscribed(now) {
                    2
                } else if self.last_contact.is_some_and(|t| now.duration_since(t) < Duration::from_secs(10)) {
                    1
                } else {
                    0
                }
            }
            // --- D-SUB15 の生同期(TVP を通らない) ---
            proto::CFG_KEY_FH_RAW => raw.map_or(0, |i| fh(i).round() as u32),
            0x002B => raw.map_or(0, |i| (fv(i) * 1000.0).round() as u32),
            0x002C | 0x0023 => raw.map_or(0, |i| match i.mode.timing.scan {
                Scan::FieldPair => i.mode.timing.vtotal / 2,
                _ => lines_vs(i),
            }),
            // --- TVP ---
            proto::CFG_KEY_FH_TVP => match &tvp {
                Some(i) => fh(i).round() as u32,
                // ★未接続の SOG 入力は範囲外をゆっくり漂う(Viewer は2回読んで判定する)
                None => 3_000 + self.rand() % 9_000,
            },
            0x0059 => tvp.as_ref().map_or(0, |i| (fv(i) * 1000.0).round() as u32),
            0x005A => tvp.as_ref().map_or(0, lines_vs),
            proto::CFG_KEY_LINES_PER_FRAME => tvp.as_ref().map_or(0, |i| i.mode.timing.vtotal & 0xFFF),
            proto::CFG_KEY_LPF_MSBS => tvp.as_ref().map_or(0, |i| {
                let prog = (i.mode.timing.scan == Scan::Progressive) as u32;
                (prog << 5) | ((i.mode.timing.vtotal >> 8) & 0xF)
            }),
            0x0056 => if tvp.is_some() { pll } else { 0 },
            proto::CFG_KEY_STAT_IL => tvp.as_ref().map_or(0, |i| {
                let il = i.mode.timing.scan != Scan::Progressive;
                let has_raw = i.raw_sync(tvp_port);
                (il && has_raw) as u32
                    | (has_raw as u32) << 1
                    | (il as u32) << 2
                    | ((i.mode.timing.scan == Scan::FieldPair) as u32) << 3
            }),
            proto::CFG_KEY_SOG_HLEN => tvp.as_ref().filter(|i| i.on_sog(tvp_port)).map_or(0, |_| pll),
            proto::CFG_KEY_SOG_LOWMAX => match &tvp {
                // 垂直ブロードパルス(約27µs)を pix クロックで
                Some(i) if i.on_sog(tvp_port) => (27e-6 * fh(i) * pll as f64).round() as u32,
                _ => 0xFFFF,
            },
            proto::CFG_KEY_DRGB_FH => self.input(Port::Digital).map_or(0, |i| fh(i).round() as u32),
            0x0072 => self.input(Port::Digital).map_or(0, |i| i.mode.timing.vtotal),
            0x0033 => 0, // キャプチャの取りこぼし
            _ => self.reg(key),
        }
    }

    /// ANNOUNCE(40B)。flags は宛先から見た送り先の状態
    pub fn announce(&self, dest: SocketAddr, seq: &mut u16, now: Instant) -> Vec<u8> {
        let streaming = self.subscribed(now);
        let yours = streaming && self.sub == Some(dest);
        let flags = proto::ANN_FLAG_OWNER_INFO
            | if streaming { proto::ANN_FLAG_STREAMING } else { 0 }
            | if yours { proto::ANN_FLAG_YOURS } else { 0 };
        let mut p = vec![proto::MAGIC, proto::VERSION, proto::TYPE_INFO, flags, 0, 0];
        p.extend_from_slice(&seq.to_le_bytes());
        *seq = seq.wrapping_add(1);
        p.extend_from_slice(&self.mac);
        // 自認IP: 静的IPが有効ならそれ、無ければ MAC 由来のリンクローカル
        let ip = if self.reg(proto::CFG_KEY_NET_MODE) & 1 != 0 && self.reg(proto::CFG_KEY_STATIC_IP) != 0 {
            self.reg(proto::CFG_KEY_STATIC_IP).to_be_bytes()
        } else {
            [169, 254, self.mac[4].clamp(1, 254), self.mac[5]]
        };
        p.extend_from_slice(&ip);
        p.extend_from_slice(&proto::DEFAULT_PORT.to_le_bytes());
        p.extend_from_slice(&self.fw_version.to_le_bytes());
        p.extend_from_slice(&self.caps.to_le_bytes());
        let mut name = [0u8; 16];
        let n = self.name.len().min(16);
        name[..n].copy_from_slice(&self.name.as_bytes()[..n]);
        p.extend_from_slice(&name);
        p
    }

    /// MODE(caps bit3 があれば 44B、無ければ 32B)
    pub fn mode_packet(&self, frame: u16, seq: &mut u16) -> Vec<u8> {
        let m = &self.meas;
        let fresh = self.caps & proto::CAP_FRAME_CLOCKS != 0;
        let pll = self.reg(proto::CFG_KEY_PLL_DIVIDE).clamp(200, 2304);
        let htotal = if fresh { pll } else { m.htotal_window.clamp(200, 2304) };
        let vbp = self.reg(proto::CFG_KEY_VBP);
        let vactive = 2 * m.vtotal.saturating_sub(vbp).min(1024);
        let mut p = vec![proto::MAGIC, proto::VERSION, proto::TYPE_MODE, 0];
        p.extend_from_slice(&frame.to_le_bytes());
        p.extend_from_slice(&seq.to_le_bytes());
        *seq = seq.wrapping_add(1);
        p.push(MODE_ID);
        p.push(self.cap_cfg().pixfmt);
        p.extend_from_slice(&(m.il_frame as u16).to_le_bytes());
        p.extend_from_slice(&(LINE_BUFFER_W as u16).to_le_bytes());
        p.extend_from_slice(&(htotal as u16).to_le_bytes());
        p.extend_from_slice(&(vactive as u16).to_le_bytes());
        p.extend_from_slice(&(m.vtotal as u16).to_le_bytes());
        p.extend_from_slice(&m.dotclk.to_le_bytes());
        p.extend_from_slice(&(m.hfreq.saturating_mul(1000)).to_le_bytes());
        p.extend_from_slice(&m.vfreq_mhz.to_le_bytes());
        if fresh {
            p.extend_from_slice(&m.frame_clocks.to_le_bytes());
            p.extend_from_slice(&m.frame_lines.to_le_bytes());
            p.extend_from_slice(&2u16.to_le_bytes());
            p.extend_from_slice(&SYS_CLK_HZ.to_le_bytes());
        }
        p
    }
}

/// 既定値(gateware の reset 値)
const DEFAULTS: &[(u16, u32)] = &[
    (0x0001, 0xF), // 音声: 全系統
    (proto::CFG_KEY_PLL_DIVIDE, 1104),
    (proto::CFG_KEY_PHASE, 16),
    (proto::CFG_KEY_PIXFMT, proto::PIXFMT_RGB555 as u32),
    (proto::CFG_KEY_VIDEO_BW, 0xF),
    (proto::CFG_KEY_PLL_CTL, 0x87),
    (0x0018, 0x18),
    (proto::CFG_KEY_CLAMP_START, 0x32),
    (proto::CFG_KEY_CLAMP_WIDTH, 0x20),
    (proto::CFG_KEY_GAIN_B, 57),
    (proto::CFG_KEY_GAIN_G, 61),
    (proto::CFG_KEY_GAIN_R, 64),
    (proto::CFG_KEY_SYNC_CTL, 0x52),
    (proto::CFG_KEY_SOG_THRESH, 0x0B),
    (0x0051, 0x75),
    (0x0052, 3),
    (0x0053, 3),
    (proto::CFG_KEY_SYNC_CTL2, 0x08),
    (proto::CFG_KEY_IN_MUX2, 0x12),
    (proto::CFG_KEY_COARSE_GAIN_GB, 0x77),
    (proto::CFG_KEY_COARSE_OFF_G, 0x10),
    (proto::CFG_KEY_COARSE_GAIN_R, 0x07),
    (proto::CFG_KEY_COARSE_OFF_R, 0x10),
    (proto::CFG_KEY_IN_MUX1, 0xAA),
    (proto::CFG_KEY_SOG_VTH, 400),
    // 小亀のLED(gateware/retrocastx_led.py の KokameLeds の既定 = レインボー)
    (0x0090, 4),
    (0x0093, 64),
    (0x0094, 32),
    (0x0098, 4),
    (0x0099, 0x0080FF),
    (0x009A, 65536 / 4000),
    (0x009B, 65536 / 2 / 4),
];

/// protocol.py に名前はあるがゲートウェアに実装の無いキー(0 を返し、SET は捨てる)
const UNIMPLEMENTED: &[u16] = &[0x0013, 0x0014, 0x0015, 0x0016];

/// 読み取り専用(SET は効かない)
const READ_ONLY: &[u16] = &[
    0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0007, 0x0008, 0x0009, 0x0023, 0x002A, 0x002B,
    0x002C, 0x002D, 0x0033, 0x004D, 0x004E, 0x004F, 0x0054, 0x0055, 0x0056, 0x0057, 0x0058,
    0x0059, 0x005A, 0x0060, 0x0061, 0x0070, 0x0072, 0x0095,
];

/// 画面の記録を埋めないよう、Viewer が定期的に読むキーの GET は記録しない
const QUIET_GETS: &[u16] = &[
    proto::CFG_KEY_BUILD_ID, proto::CFG_KEY_FH_TVP, proto::CFG_KEY_FH_RAW, proto::CFG_KEY_DRGB_FH,
    proto::CFG_KEY_IN_MUX1, proto::CFG_KEY_LINES_PER_FRAME, proto::CFG_KEY_LPF_MSBS,
    proto::CFG_KEY_STAT_IL, proto::CFG_KEY_SOG_LOWMAX, proto::CFG_KEY_SOG_HLEN,
    proto::CFG_KEY_PLAT_STATE, 0x0095,
];

/// レジスタの幅(書いた値はこの幅で切られて読み返される)
fn width_mask(key: u16) -> u32 {
    match key {
        0x0001 => 0xF,
        proto::CFG_KEY_PLL_DIVIDE => 0xFFF,
        proto::CFG_KEY_VBP | proto::CFG_KEY_HS_OFFSET => 0x1FFF,
        proto::CFG_KEY_PHASE | proto::CFG_KEY_FRAME_SKIP | proto::CFG_KEY_BLACK_TH
        | proto::CFG_KEY_SOG_THRESH => 0x1F,
        proto::CFG_KEY_FULL_LINE | proto::CFG_KEY_FIELD_INVERT | proto::CFG_KEY_NO_RAW_PHASE
        | proto::CFG_KEY_VIDEO_SRC => 1,
        proto::CFG_KEY_PIXFMT => 0xFF,
        proto::CFG_KEY_WIN_START | proto::CFG_KEY_WIN_LEN => 0xFFFF,
        proto::CFG_KEY_CLAMP_SEL => 0x7,
        // 小亀のLED(KokameLeds の effect / color / rate / spread の幅)
        0x0098 => 0x7,
        0x0099 => 0xFF_FFFF,
        0x009A | 0x009B => 0xFFFF,
        _ => u32::MAX,
    }
}

/// 記録に出すキーの名前
pub fn key_name(key: u16) -> &'static str {
    match key {
        0x0001 => "audio_mask",
        proto::CFG_KEY_BUILD_ID => "build_id",
        proto::CFG_KEY_VBP => "vbp",
        proto::CFG_KEY_HS_OFFSET => "hs_offset",
        proto::CFG_KEY_PLL_DIVIDE => "pll_divide",
        0x0013..=0x0016 => "(未実装)",
        proto::CFG_KEY_VIDEO_BW => "video_bw",
        proto::CFG_KEY_PLL_CTL => "pll_ctl",
        proto::CFG_KEY_CLAMP_START => "clamp_start",
        proto::CFG_KEY_CLAMP_WIDTH => "clamp_width",
        proto::CFG_KEY_GAIN_B => "gain_b",
        proto::CFG_KEY_GAIN_G => "gain_g",
        proto::CFG_KEY_GAIN_R => "gain_r",
        proto::CFG_KEY_PHASE => "phase",
        proto::CFG_KEY_SYNC_CTL => "sync_ctl",
        proto::CFG_KEY_FH_RAW => "fh_raw",
        proto::CFG_KEY_STAT_IL => "stat_il",
        proto::CFG_KEY_FULL_LINE => "full_line",
        proto::CFG_KEY_FRAME_SKIP => "frame_skip",
        proto::CFG_KEY_PIXFMT => "pixfmt",
        proto::CFG_KEY_VIDEO_SRC => "video_src",
        proto::CFG_KEY_BLACK_TH => "black_th",
        proto::CFG_KEY_WIN_START => "win_start",
        proto::CFG_KEY_WIN_LEN => "win_len",
        proto::CFG_KEY_NET_MODE => "net_mode",
        proto::CFG_KEY_STATIC_IP => "static_ip",
        0x0048..=0x004B => "name",
        proto::CFG_KEY_IDENT_SAVE => "ident_save",
        proto::CFG_KEY_SOG_THRESH => "sog_thresh",
        proto::CFG_KEY_LINES_PER_FRAME => "lines_per_frame",
        proto::CFG_KEY_LPF_MSBS => "lpf_msbs",
        proto::CFG_KEY_FH_TVP => "fh_tvp",
        proto::CFG_KEY_SYNC_CTL2 => "sync_ctl2",
        proto::CFG_KEY_IN_MUX2 => "in_mux2",
        proto::CFG_KEY_CLAMP_SEL => "clamp_sel",
        proto::CFG_KEY_COARSE_GAIN_GB => "coarse_gain_gb",
        proto::CFG_KEY_SOG_HLEN => "sog_hlen",
        proto::CFG_KEY_SOG_LOWMAX => "sog_lowmax",
        proto::CFG_KEY_SOG_VTH => "sog_vth",
        proto::CFG_KEY_COARSE_OFF_G => "coarse_off_g",
        proto::CFG_KEY_FIELD_INVERT => "field_invert",
        proto::CFG_KEY_COARSE_GAIN_R => "coarse_gain_r",
        proto::CFG_KEY_COARSE_OFF_R => "coarse_off_r",
        proto::CFG_KEY_IN_MUX1 => "in_mux1",
        proto::CFG_KEY_NO_RAW_PHASE => "no_raw_phase",
        proto::CFG_KEY_DRGB_FH => "drgb_fh",
        proto::CFG_KEY_KOKAME_SEL => "kokame_sel",
        0x0090 | 0x0093..=0x0095 | 0x0097..=0x009B => "led",
        proto::CFG_KEY_PLAT_CTL..=proto::CFG_KEY_PLAT_SUM => "photo_lat",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::Packet;

    fn addr() -> SocketAddr {
        "192.168.1.20:50000".parse().unwrap()
    }

    #[test]
    fn subscribe_takes_stream_and_announces() {
        let mut b = Board::new();
        let mut seq = 0;
        let now = Instant::now();
        let p = proto::pack_subscribe(0, false, &proto::WILDCARD_MAC);
        let out = b.handle(&p, addr(), &mut seq, now);
        assert!(b.subscribed(now));
        let Ok(Packet::Announce(a)) = proto::parse(&out[0].1) else { panic!() };
        assert_eq!(a.flags & 7, 7);
        assert_eq!(a.name, "devsim");
        // 別の Viewer の RENEW は奪わない
        let other: SocketAddr = "192.168.1.21:50000".parse().unwrap();
        let out = b.handle(&proto::pack_renew(1, &proto::WILDCARD_MAC), other, &mut seq, now);
        assert_eq!(b.sub, Some(addr()));
        let Ok(Packet::Announce(a)) = proto::parse(&out[0].1) else { panic!() };
        assert_eq!(a.flags & 6, 2, "STREAMING だが YOURS ではない");
        // 他のボード宛は無視
        let out = b.handle(&proto::pack_subscribe(0, false, &[1, 2, 3, 4, 5, 6]), other, &mut seq, now);
        assert!(out.is_empty());
    }

    #[test]
    fn config_set_get_reply() {
        let mut b = Board::new();
        let mut seq = 0;
        let now = Instant::now();
        let p = proto::pack_config(0, 0, proto::CFG_OP_SET, proto::CFG_KEY_PLL_DIVIDE, 0x1_0568, &proto::WILDCARD_MAC);
        let out = b.handle(&p, addr(), &mut seq, now);
        let r = &out[0].1;
        assert_eq!(r[3], proto::CFG_FLAG_REPLY);
        assert_eq!(&r[8..14], &b.mac);
        assert_eq!(u32::from_le_bytes(r[20..24].try_into().unwrap()), 0x568, "12bit で切れる");
        // 未実装のキーは 0
        let p = proto::pack_config(0, 0, proto::CFG_OP_SET, proto::CFG_KEY_INTERLACE, 2, &proto::WILDCARD_MAC);
        let out = b.handle(&p, addr(), &mut seq, now);
        assert_eq!(u32::from_le_bytes(out[0].1[20..24].try_into().unwrap()), 0);
        // 自分の送った REPLY が戻ってきても反応しない(ブロードキャストのループバック)
        let mut echo = out[0].1.clone();
        echo[8..14].copy_from_slice(&proto::WILDCARD_MAC);
        assert!(b.handle(&echo, addr(), &mut seq, now).is_empty());
    }

    #[test]
    fn name_roundtrip_through_config() {
        let mut b = Board::new();
        b.set_name("テスト基板");
        let mut seq = 0;
        let now = Instant::now();
        let get = |b: &mut Board, k, seq: &mut u16| {
            let p = proto::pack_config(0, 0, proto::CFG_OP_GET, k, 0, &proto::WILDCARD_MAC);
            let out = b.handle(&p, addr(), seq, now);
            u32::from_le_bytes(out[0].1[20..24].try_into().unwrap())
        };
        let mut bytes = Vec::new();
        for k in proto::CFG_KEY_NAME0..=proto::CFG_KEY_NAME3 {
            bytes.extend_from_slice(&get(&mut b, k, &mut seq).to_le_bytes());
        }
        let end = bytes.iter().position(|&c| c == 0).unwrap_or(16);
        assert_eq!(std::str::from_utf8(&bytes[..end]).unwrap(), "テスト基板");
    }

    #[test]
    fn mode_packet_parses_with_viewer_parser() {
        let mut b = Board::new();
        b.meas = Meas {
            hfreq: 31_499, dotclk: 34_776_000, vfreq_mhz: 55_500, vtotal: 568,
            frame_clocks: 1_622_826, frame_lines: 1136, il_frame: false, htotal_window: 1104,
        };
        let mut seq = 0;
        let p = b.mode_packet(7, &mut seq);
        assert_eq!(p.len(), 44);
        let Ok(Packet::Mode(m)) = proto::parse(&p) else { panic!() };
        assert_eq!((m.hactive, m.htotal, m.vactive, m.vtotal), (2048, 1104, 1136, 568));
        assert!((m.hfreq_mhz_x1000 as i64 - 31_500_000).abs() < 2_000, "{}", m.hfreq_mhz_x1000);
        assert!((m.vfreq_mhz_x1000 as i64 - 55_459).abs() < 3);
        b.caps &= !proto::CAP_FRAME_CLOCKS;
        assert_eq!(b.mode_packet(7, &mut seq).len(), 32);
    }
}
