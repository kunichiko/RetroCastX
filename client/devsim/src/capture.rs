//! ゲートウェアのキャプチャと LINE パケット化の再現。
//!
//! 規則は `gateware/retrocastx_capture.py`(取り込み)と `retrocastx_stream.py`
//! (送出)から起こした。docs/protocol-v0.md に書かれていない挙動も実機に合わせる:
//!
//! - `line` は**半ライン単位のスロット**。プログレッシブは偶数スロットだけ
//!   (slot = 2·row)、インターレースは `2·row + fld_pos`
//! - VSYNC 1回にフィールド2枚の信号(X68000 24kHz)は、周期の後半を折り返して
//!   `fld_pos = 1` で同じスロット空間に重ねる
//! - `frame` は VSYNC 周期ごとに +1(480i ならフィールドごと)。間引いた周期も数える
//! - FIELD_ODD はインターレースなら `fld_pos`、プログレッシブでは**毎フレーム反転**
//! - 真っ黒なラインも `count_px = 0` の 20 バイトで送る
//! - 送る範囲は「黒でない範囲」を 4 画素単位に広げたもの(全ライン送信・YC8/CVBS8・
//!   送出範囲の固定のときはそれぞれの規則)
//! - timestamp は DATACLK(= pll_divide × fH)の自走カウンタ

use std::sync::Arc;

use crate::machines::Scan;
use crate::pattern::Picture;
use crate::protocol as proto;
use crate::signal::{self, LineRef, Source, Tvp};

/// ラインバッファの幅(gateware の TvpCapture width)
pub const LINE_BUFFER_W: u32 = 2048;
/// MODE の mode_id。実機は常に 1(変わったことは htotal / vactive の変化で知らせる)
pub const MODE_ID: u8 = 1;
/// 1パケットの UDP ペイロードの上限(MTU 1500 − IP 20 − UDP 8)
const PAYLOAD_MAX: usize = 1472;
const LINE_HDR: usize = 20;

/// キャプチャに効くボードの設定(Viewer が CONFIG で書く)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CapCfg {
    pub pll_divide: u32,
    pub phase: u32,
    pub vbp: u32,
    pub hs_offset: u32,
    pub full_line: bool,
    pub win_start: u32,
    pub win_len: u32,
    pub black_th: u32,
    pub pixfmt: u8,
    pub frame_skip: u32,
    pub field_invert: bool,
    /// RGBI4 のとき 16色(I を使う)で量子化するか。8色なら I は 0
    pub rgbi16: bool,
}

/// 取り込んだ1ライン
pub struct CapLine {
    /// 周期の頭から何ライン目に送り出すか(送出の時刻合わせ用)
    pub at_line: u32,
    pub slot: u16,
    pub field_odd: bool,
    pub frame: u16,
    pub ts: u32,
    /// 絶対位置(HSYNC から数えた DATACLK)ごとのサンプル。長さは
    /// min(pll_divide, ラインバッファ幅)
    pub samples: Arc<Vec<[u8; 3]>>,
}

/// 1回の VSYNC 周期
pub struct Period {
    /// 周期のライン数
    pub lines: u32,
    /// 1ラインの時間[秒]
    pub line_s: f64,
    /// MODE の vtotal に出す値(フィールドごとの VSYNC なら長い方のフィールド)
    pub vtotal: u32,
    /// mflags bit0(VSYNC 1回にフィールド2枚)
    pub il_frame: bool,
    pub rows: Vec<CapLine>,
}

/// 機種ごとの「生の位相の符号」。X68000 は実機で field_invert=1 が正しかった
/// (client/src/protocol.rs の CFG_KEY_FIELD_INVERT)ので、ダミーも同じにする。
/// Viewer の設定が機種と合っていなければ織り込みが逆になる
pub fn native_invert(machine_key: &str) -> bool {
    machine_key == "x68000"
}

/// サンプルの作り置き。絵と設定が同じなら作り直さない
#[derive(Default)]
struct Cache {
    key: Option<(u64, Tvp, u32, bool, u32)>,
    rows: Vec<Arc<Vec<[u8; 3]>>>,
}

pub struct Capture {
    frame: u16,
    dataclk: u32,
    /// プログレッシブで毎フレーム反転する FIELD_ODD
    toggle: bool,
    /// インターレースのフィールド(周期ごとに交互)
    field: u32,
    /// 通しのライン番号の偶奇(副搬送波の位相)
    abs_odd: bool,
    /// 間引きの単位を数える
    unit: u32,
    cache: [Cache; 2],
    seed: u32,
}

impl Default for Capture {
    fn default() -> Self {
        Capture {
            frame: 0, dataclk: 0, toggle: false, field: 0, abs_odd: false, unit: 0,
            cache: Default::default(), seed: 0x1234_5678,
        }
    }
}

impl Capture {
    /// 次の VSYNC 周期を作る。`pic_gen` は絵が変わるたびに変わる値(作り置きの鍵)
    #[allow(clippy::too_many_arguments)]
    pub fn next_period(
        &mut self, src: &Source, native_inv: bool, pic: &Picture, pic_gen: u64, cfg: &CapCfg,
        noise: u8,
    ) -> Period {
        let t = &src.timing;
        let pll = cfg.pll_divide.clamp(1, 4095);
        let line_s = t.htotal as f64 / t.dotclk_hz;
        let (lines, field, il_frame, interlaced, vtotal) = match t.scan {
            Scan::Progressive => (t.vtotal, 0, false, false, t.vtotal),
            Scan::Interlace => {
                let long = t.vtotal.div_ceil(2);
                let f = self.field;
                self.field ^= 1;
                (if f == 0 { long } else { t.vtotal / 2 }, f, false, true, long)
            }
            Scan::FieldPair => (t.vtotal, 0, true, true, t.vtotal),
        };
        self.frame = self.frame.wrapping_add(1);
        self.toggle = !self.toggle;
        // 間引き。対モードはフィールドごとに VSYNC が来るインターレースだけ
        let skip = cfg.frame_skip & 15;
        let pair = cfg.frame_skip & 16 != 0 && t.scan == Scan::Interlace;
        let unit = if pair { self.unit / 2 } else { self.unit };
        self.unit = self.unit.wrapping_add(1);
        let send = unit % (skip + 1) == 0;

        let flip = native_inv ^ cfg.field_invert;
        let n = lines.saturating_sub(cfg.vbp).min(1024);
        let half = t.vtotal / 2;
        let mut rows = Vec::new();
        if send {
            let samples = self.sample_period(src, pic, pic_gen, cfg, lines, field, noise);
            for k in 0..n {
                let s = k + cfg.vbp; // 周期の頭(VSYNC)から数えたライン
                let (fl, fld) = if il_frame && k >= half { (k - half, 1) } else { (k, field) };
                let fld_pos = (fld != 0) ^ flip;
                rows.push(CapLine {
                    at_line: s + 1,
                    slot: (2 * fl + fld_pos as u32) as u16,
                    field_odd: if interlaced { fld_pos } else { self.toggle },
                    frame: self.frame,
                    ts: self.dataclk.wrapping_add(s.wrapping_mul(pll)),
                    samples: samples[s as usize].clone(),
                });
            }
        }
        self.dataclk = self.dataclk.wrapping_add(lines.wrapping_mul(pll));
        if lines % 2 == 1 {
            self.abs_odd = !self.abs_odd;
        }
        Period { lines, line_s, vtotal, il_frame, rows }
    }

    /// 周期の全ラインをサンプルする(作り置きがあれば使う)
    #[allow(clippy::too_many_arguments)]
    fn sample_period(
        &mut self, src: &Source, pic: &Picture, pic_gen: u64, cfg: &CapCfg, lines: u32,
        field: u32, noise: u8,
    ) -> Vec<Arc<Vec<[u8; 3]>>> {
        let tvp = Tvp { pll_divide: cfg.pll_divide.clamp(1, 4095), phase: cfg.phase & 31 };
        let key = (pic_gen, tvp, field, self.abs_odd, lines);
        let c = &mut self.cache[field as usize & 1];
        if noise == 0 && c.key == Some(key) {
            return c.rows.clone();
        }
        let t = &src.timing;
        let width = tvp.pll_divide.min(LINE_BUFFER_W) as usize;
        let half = t.vtotal / 2;
        let abs_odd0 = self.abs_odd;
        let line_ref = |s: u32| {
            let (fl, f) = if t.scan == Scan::FieldPair && s >= half {
                (s - half, 1)
            } else {
                (s, field)
            };
            LineRef { field_line: fl, field: f, abs_odd: abs_odd0 ^ (s % 2 == 1) }
        };
        // 31kHz で1周期 60万サンプルを超えるので、コアに分けて作る
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        let per = (lines as usize).div_ceil(threads).max(1);
        let seed0 = self.seed;
        self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223) | 1;
        let mut out: Vec<Vec<[u8; 3]>> = vec![Vec::new(); lines as usize];
        std::thread::scope(|sc| {
            for (ci, chunk) in out.chunks_mut(per).enumerate() {
                let line_ref = &line_ref;
                sc.spawn(move || {
                    let mut seed = seed0 ^ (ci as u32 + 1).wrapping_mul(0x9E37_79B9) | 1;
                    for (j, row) in chunk.iter_mut().enumerate() {
                        let s = (ci * per + j) as u32;
                        let mut v = vec![[0u8; 3]; width];
                        signal::sample_line(src, pic, line_ref(s), tvp, noise, &mut seed, &mut v);
                        // HSYNC の直後 hs_offset までは取り込まない
                        let hs = (cfg.hs_offset.min(tvp.pll_divide / 2) as usize).min(width);
                        v[..hs].fill([0; 3]);
                        *row = v;
                    }
                });
            }
        });
        let rows: Vec<Arc<Vec<[u8; 3]>>> = out.into_iter().map(Arc::new).collect();
        c.key = Some(key);
        c.rows = rows.clone();
        rows
    }
}

/// 送る範囲 [lo, hi)(絶対位置)。stream.py の規則どおり
pub fn send_range(samples: &[[u8; 3]], cfg: &CapCfg) -> (u32, u32) {
    let (lo, hi) = if cfg.full_line || proto::is_raw8(cfg.pixfmt)
        || cfg.pixfmt == proto::PIXFMT_RGBI4
    {
        // デジタルRGB はライン全体(1パケットに収まる。黒でない範囲の判定もしない)
        (cfg.hs_offset, cfg.pll_divide)
    } else if cfg.win_len != 0 {
        (cfg.win_start, cfg.win_start + cfg.win_len)
    } else {
        // 2画素(entry)単位で、どれかの成分の上位5bitが black_th を超えたら「黒でない」
        let th = cfg.black_th & 31;
        let lit = |c: &[u8; 3]| c.iter().any(|&v| (v >> 3) as u32 > th);
        let entries = samples.len() / 2;
        let is_lit = |e: usize| lit(&samples[2 * e]) || lit(&samples[2 * e + 1]);
        let first = (0..entries).find(|&e| is_lit(e));
        match first {
            None => (0, 0),
            Some(f) => {
                let l = (0..entries).rev().find(|&e| is_lit(e)).unwrap();
                (2 * f as u32, 2 * l as u32 + 2)
            }
        }
    };
    let lo = lo & !3;
    let hi = (hi.div_ceil(4) * 4).min(LINE_BUFFER_W);
    if lo >= hi { (0, 0) } else { (lo, hi) }
}

/// 1パケットに入る画素数(4の倍数)
pub fn max_px(pixfmt: u8) -> u32 {
    if pixfmt == proto::PIXFMT_RGBI4 {
        return ((PAYLOAD_MAX - LINE_HDR) * 2) as u32 & !3;
    }
    let bpp = match pixfmt {
        proto::PIXFMT_RGB888 => 3,
        proto::PIXFMT_CVBS8 => 1,
        _ => 2,
    };
    ((PAYLOAD_MAX - LINE_HDR) / bpp) as u32 & !3
}

/// RGB を RGBI の4bit(bit0=R bit1=G bit2=B bit3=I)にする。
///
/// 8色は各色の上半分を 1、I は 0(8色の機種には I の線が無い)。16色は Viewer と
/// 同じ CGA の 16 色(色の bit で 170、I で +85)のうち最も近いもの
pub fn rgbi_of(c: [u8; 3], rgbi16: bool) -> u8 {
    if !rgbi16 {
        return (c[0] >= 128) as u8 | ((c[1] >= 128) as u8) << 1 | ((c[2] >= 128) as u8) << 2;
    }
    let dist = |v: u8| -> u32 {
        (0..3).map(|ch| {
            let lv = ((v >> ch & 1) as i32) * 170 + ((v >> 3) as i32) * 85;
            let d = c[ch] as i32 - lv;
            (d * d) as u32
        }).sum()
    };
    (0..16u8).min_by_key(|&v| dist(v)).unwrap()
}

/// 画素を伝送形式に詰める
fn pack_pixels(px: &[[u8; 3]], pixfmt: u8, rgbi16: bool, out: &mut Vec<u8>) {
    match pixfmt {
        // 1バイト2画素、下位4bitが先
        proto::PIXFMT_RGBI4 => {
            for p in px.chunks(2) {
                let lo = rgbi_of(p[0], rgbi16);
                let hi = p.get(1).map_or(0, |c| rgbi_of(*c, rgbi16));
                out.push(lo | hi << 4);
            }
        }
        proto::PIXFMT_RGB888 => {
            for c in px {
                out.extend_from_slice(c);
            }
        }
        proto::PIXFMT_YC8 => {
            for c in px {
                out.extend_from_slice(&[c[1], c[0]]);
            }
        }
        proto::PIXFMT_CVBS8 => out.extend(px.iter().map(|c| c[1])),
        proto::PIXFMT_YCC422 => {
            // 2画素で Y0 Cb0 Y1 Cr0(色差は偶数画素の値)。R=Pr / G=Y / B=Pb
            for p in px.chunks(2) {
                let (a, b) = (p[0], *p.get(1).unwrap_or(&p[0]));
                out.extend_from_slice(&[a[1], a[2], b[1], a[0]]);
            }
        }
        // RGB555(RGB565 や未知の値もゲートウェアは RGB555 で詰める)
        _ => {
            for c in px {
                let v = ((c[0] as u16 >> 3) << 10) | ((c[1] as u16 >> 3) << 5) | (c[2] as u16 >> 3);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// 1ラインを LINE パケット(断片)にする。`seq` はパケットごとに進める
pub fn packetize(line: &CapLine, cfg: &CapCfg, seq: &mut u16, out: &mut Vec<Vec<u8>>) {
    let (lo, hi) = send_range(&line.samples, cfg);
    let maxp = max_px(cfg.pixfmt);
    let mut off = lo;
    loop {
        let count = (hi - off).min(maxp);
        let last = off + count >= hi;
        let mut p = Vec::with_capacity(LINE_HDR + count as usize * 3);
        let flags = last as u8 | ((line.field_odd as u8) << 1);
        p.extend_from_slice(&[proto::MAGIC, proto::VERSION, proto::TYPE_LINE, flags]);
        p.extend_from_slice(&line.frame.to_le_bytes());
        p.extend_from_slice(&seq.to_le_bytes());
        p.extend_from_slice(&line.slot.to_le_bytes());
        p.extend_from_slice(&(off as u16).to_le_bytes());
        p.extend_from_slice(&(count as u16).to_le_bytes());
        p.push(cfg.pixfmt);
        p.push(MODE_ID);
        p.extend_from_slice(&line.ts.wrapping_add(off).to_le_bytes());
        let s = &line.samples;
        let px: Vec<[u8; 3]> =
            (off..off + count).map(|x| *s.get(x as usize).unwrap_or(&[0; 3])).collect();
        pack_pixels(&px, cfg.pixfmt, cfg.rgbi16, &mut p);
        *seq = seq.wrapping_add(1);
        out.push(p);
        off += count;
        if last {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::machines;
    use crate::pattern::{self, Options, Pattern};
    use proto::Packet;

    fn cfg(pll: u32) -> CapCfg {
        CapCfg {
            pll_divide: pll, phase: 16, vbp: 0, hs_offset: 0, full_line: false, win_start: 0,
            win_len: 0, black_th: 0, pixfmt: proto::PIXFMT_RGB555, frame_skip: 0,
            field_invert: false, rgbi16: false,
        }
    }

    fn src(key: &str, name: &str) -> Source {
        let ms = machines();
        let mc = ms.iter().find(|m| m.key == key).unwrap();
        let md = mc.modes.iter().find(|m| m.name == name).unwrap();
        Source { kind: mc.kind, timing: md.timing.clone(), color_bits: md.color_bits }
    }

    fn pic(s: &Source, p: Pattern) -> Picture {
        let (w, h) = s.timing.picture_size();
        pattern::render(w, h, s.color_bits, &Options { pattern: p, moving: false, caption: "".into() }, 0)
    }

    /// Viewer のパーサで読めて、範囲・スロット・タイムスタンプが規則どおり
    #[test]
    fn x68000_31k_progressive_slots_and_range() {
        let s = src("x68000", "768x512 31kHz");
        let p = pic(&s, Pattern::White);
        let mut cap = Capture::default();
        let c = cfg(1104);
        let per = cap.next_period(&s, false, &p, 1, &c, 0);
        assert_eq!(per.rows.len(), 568);
        assert!(per.rows.iter().all(|r| r.slot % 2 == 0));
        assert_eq!(per.rows[1].ts - per.rows[0].ts, 1104);
        let mut seq = 0;
        let mut pk = Vec::new();
        packetize(&per.rows[0], &c, &mut seq, &mut pk); // VSYNC のライン = 黒
        assert_eq!(pk.len(), 1);
        assert_eq!(pk[0].len(), 20);
        let Ok(Packet::Line(l)) = proto::parse(&pk[0]) else { panic!() };
        assert_eq!((l.offset_px, l.count_px, l.flags & 1), (0, 0, 1));
        pk.clear();
        packetize(&per.rows[100], &c, &mut seq, &mut pk);
        // 白は 264..1032、4画素単位に広げて 264..1032(どちらも4の倍数)
        let lines: Vec<_> = pk.iter().map(|b| match proto::parse(b).unwrap() {
            Packet::Line(l) => (l.offset_px, l.count_px, l.flags & 1),
            _ => panic!(),
        }).collect();
        assert_eq!(lines, vec![(264, 724, 0), (988, 44, 1)]);
        // FIELD_ODD はプログレッシブで毎フレーム反転する
        let per2 = cap.next_period(&s, false, &p, 1, &c, 0);
        assert_ne!(per.rows[0].field_odd, per2.rows[0].field_odd);
        assert_eq!(per2.rows[0].frame, per.rows[0].frame.wrapping_add(1));
    }

    /// 480i: フィールドごとに VSYNC、スロットは偶奇が交互、vtotal は長い方
    #[test]
    fn ntsc_480i_fields_alternate() {
        let s = src("composite", "480i");
        let p = pic(&s, Pattern::ColorBars);
        let mut cap = Capture::default();
        let mut c = cfg(1820);
        c.pixfmt = proto::PIXFMT_YC8;
        let a = cap.next_period(&s, false, &p, 1, &c, 0);
        let b = cap.next_period(&s, false, &p, 1, &c, 0);
        assert_eq!((a.lines, b.lines, a.vtotal), (263, 262, 263));
        assert!(a.rows.iter().all(|r| r.slot % 2 == 0 && !r.field_odd));
        assert!(b.rows.iter().all(|r| r.slot % 2 == 1 && r.field_odd));
        // YC8 は1ライン丸ごと: 1820 = 724 + 724 + 372
        let mut seq = 0;
        let mut pk = Vec::new();
        packetize(&a.rows[50], &c, &mut seq, &mut pk);
        let counts: Vec<u16> = pk.iter().map(|b| match proto::parse(b).unwrap() {
            Packet::Line(l) => l.count_px,
            _ => panic!(),
        }).collect();
        assert_eq!(counts, vec![724, 724, 372]);
    }

    /// VSYNC 1回にフィールド2枚: 後半は折り返して奇数スロット
    #[test]
    fn x68000_24k_field_pair_folds() {
        // VSOUT をフレーム単位にした X68000 24kHz(931 ライン)。機種の表には無いので作る
        let mut s = src("x68000", "1024x848 24kHz インターレース");
        s.timing.scan = crate::machines::Scan::FieldPair;
        s.timing.vstart = 30;
        let p = pic(&s, Pattern::White);
        let mut cap = Capture::default();
        let per = cap.next_period(&s, false, &p, 1, &cfg(1408), 0);
        assert!(per.il_frame);
        assert_eq!(per.rows.len(), 931);
        assert_eq!(per.rows[465].slot, 1);
        assert_eq!(per.rows[464].slot, 928);
    }

    #[test]
    fn frame_skip_drops_periods_but_counts_frames() {
        let s = src("x68000", "256x256 15kHz");
        let p = pic(&s, Pattern::Black);
        let mut cap = Capture::default();
        let mut c = cfg(1216);
        c.frame_skip = 1;
        let sent: Vec<bool> = (0..4).map(|_| !cap.next_period(&s, false, &p, 1, &c, 0).rows.is_empty()).collect();
        assert_eq!(sent, vec![true, false, true, false]);
    }

    #[test]
    fn ycc422_and_cvbs8_sizes_parse() {
        let samples: Vec<[u8; 3]> = (0..2048).map(|i| [i as u8, 100, 200]).collect();
        let line = CapLine {
            at_line: 0, slot: 0, field_odd: false, frame: 0, ts: 0, samples: Arc::new(samples),
        };
        for f in [proto::PIXFMT_YCC422, proto::PIXFMT_CVBS8, proto::PIXFMT_RGB888] {
            let mut c = cfg(1820);
            c.pixfmt = f;
            c.full_line = true;
            let mut seq = 0;
            let mut pk = Vec::new();
            packetize(&line, &c, &mut seq, &mut pk);
            let mut total = 0;
            for b in &pk {
                assert!(b.len() <= PAYLOAD_MAX);
                let Ok(Packet::Line(l)) = proto::parse(b) else { panic!("fmt {f}") };
                total += l.count_px as u32;
            }
            assert_eq!(total, 1820);
        }
    }

    /// デジタルRGB: PC-6001mkIISR 640x200 を pll 910 で取り込むと、1ライン 910 ドットが
    /// RGBI4 の1パケット(455 バイト)で届き、カラーバーの白が 16色の白(0xF)になる
    #[test]
    fn rgbi4_one_packet_per_line() {
        let s = src("pc6001mk2sr", "640x200");
        let p = pic(&s, Pattern::White);
        let mut cap = Capture::default();
        let mut c = cfg(910);
        c.pixfmt = proto::PIXFMT_RGBI4;
        c.rgbi16 = true;
        let per = cap.next_period(&s, false, &p, 1, &c, 0);
        let row = per.rows.iter().find(|r| r.samples.iter().any(|v| v[0] > 0)).unwrap();
        let mut seq = 0;
        let mut pk = Vec::new();
        packetize(row, &c, &mut seq, &mut pk);
        assert_eq!(pk.len(), 1);
        let Ok(Packet::Line(l)) = proto::parse(&pk[0]) else { panic!() };
        assert_eq!((l.offset_px, l.count_px, l.pixels.len()), (0, 912, 456));
        // 有効映像の中ほど(hstart 150 + 320)は白
        assert_eq!(l.pixels[235], 0xFF);
        // HSYNC の直後は黒
        assert_eq!(l.pixels[0], 0x00);
    }

    #[test]
    fn rgbi_quantize() {
        // 8色: 上半分が 1、I は 0
        assert_eq!(rgbi_of([255, 255, 255], false), 0x7);
        assert_eq!(rgbi_of([200, 10, 0], false), 0x1);
        // 16色: CGA で最も近いもの
        assert_eq!(rgbi_of([255, 255, 255], true), 0xF);
        assert_eq!(rgbi_of([170, 170, 170], true), 0x7);
        assert_eq!(rgbi_of([85, 85, 85], true), 0x8);
        assert_eq!(rgbi_of([0, 0, 170], true), 0x4);
    }
}
