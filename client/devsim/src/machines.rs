//! 機種ごとの映像タイミング。
//!
//! ここに並べるのは「その機種のケーブルに何が出ているか」で、ボードの設定とは
//! 関係ない。ボード側(TVP の pll_divide など)は Viewer が CONFIG で書いてくる。
//!
//! ★**確かめてある値と推定の値を分けて書く。** `verified` が false のものは
//!   資料からの推定で、実機と食い違っている可能性がある。Viewer の
//!   `client/src/profiles.rs` が同じ区別をしているのに合わせた。
//!
//! 水平は「ドット」、垂直は「ライン」で持つ。水平の位置は HSYNC の立ち上がり
//! (同期の始まり)を 0 とする。ゲートウェアはそこから取り込む(hs_offset=0)。

/// 走査の方式
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scan {
    /// ノンインターレース
    Progressive,
    /// インターレース。VSYNC がフィールドごとに来る(NTSC 480i、MSX のインターレース)。
    /// `vtotal` は2フィールド分(奇数。525 など)
    Interlace,
    /// 1回の VSYNC の間にフィールドが2枚並ぶ(X68000 24kHz 1024x848)。
    /// `vtotal` は VSYNC 間のライン数(931 など)
    FieldPair,
}

/// 信号の種類(どのピンに何が載るか)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// R/G/B と別線(または複合)の同期
    Rgb,
    /// コンポジット(緑ch に CVBS)
    Composite,
    /// S端子(緑ch に Y、赤ch に C)
    SVideo,
    /// コンポーネント(赤=Pr / 緑=Y / 青=Pb)
    Component,
}

/// ボードの映像入力端子。Viewer の `profiles::Port` と同じ並び
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Port {
    /// 入力1: ビデオ(小亀基板。TVP 1番系統)
    Video,
    /// 入力2: RGB 21ピン(ミニDIN8。TVP 2番系統)
    Rgb21,
    /// 入力3: RGB 15ピン(D-SUB15。TVP 3番系統、HSYNC/VSYNC を別線で数えられる)
    Rgb15,
    /// 入力4: デジタルRGB(FPGA 直結)
    Digital,
}

impl Port {
    pub const ALL: [Port; 4] = [Port::Video, Port::Rgb21, Port::Rgb15, Port::Digital];

    pub fn label(self) -> &'static str {
        match self {
            Port::Video => "入力1 ビデオ",
            Port::Rgb21 => "入力2 RGB 21ピン",
            Port::Rgb15 => "入力3 RGB 15ピン",
            Port::Digital => "入力4 デジタルRGB",
        }
    }

    /// TVP の入力MUX(reg 19h)の G 選択 [3:2] から、TVP が見ている端子を決める。
    /// 00=_1(ビデオ) / 01=_2(21ピン) / 10=_3(15ピン)
    pub fn from_mux(mux: u32) -> Port {
        match (mux >> 2) & 3 {
            0 => Port::Video,
            1 => Port::Rgb21,
            _ => Port::Rgb15,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timing {
    pub dotclk_hz: f64,
    /// 1ラインのドット数
    pub htotal: u32,
    /// HSYNC の幅[ドット]
    pub hsync: u32,
    /// 有効映像の始まり[ドット](HSYNC の立ち上がりから)
    pub hstart: u32,
    pub hactive: u32,
    /// VSYNC 間(インターレースは2フィールド分)のライン数
    pub vtotal: u32,
    /// VSYNC の幅[ライン]
    pub vsync: u32,
    /// 有効映像の始まり[ライン](各フィールドの VSYNC から)
    pub vstart: u32,
    /// 有効映像のライン数(インターレース/FieldPair は2フィールドの合計)
    pub vactive: u32,
    pub scan: Scan,
    /// 絵の1行を何ラインで出すか(31kHz の 256 ライン系は同じ行を2回走査する)
    pub line_repeat: u32,
}

impl Timing {
    pub fn fh(&self) -> f64 {
        self.dotclk_hz / self.htotal as f64
    }

    /// VSYNC の周波数
    pub fn fv(&self) -> f64 {
        match self.scan {
            Scan::Interlace => self.fh() * 2.0 / self.vtotal as f64,
            _ => self.fh() / self.vtotal as f64,
        }
    }

    /// 絵(パターン)の大きさ
    pub fn picture_size(&self) -> (u32, u32) {
        (self.hactive, self.vactive / self.line_repeat.max(1))
    }

    /// タイミングとして成り立っているか(カスタム入力の検査)
    pub fn validate(&self) -> Result<(), String> {
        if !(1.0e6..=200.0e6).contains(&self.dotclk_hz) {
            return Err("ドットクロックは 1〜200MHz".into());
        }
        if self.hstart + self.hactive > self.htotal {
            return Err("hstart + hactive が htotal を超えています".into());
        }
        if self.hsync >= self.hstart.max(1) {
            return Err("HSYNC が有効映像に掛かっています".into());
        }
        let field_lines = match self.scan {
            Scan::Progressive => self.vtotal,
            _ => self.vtotal / 2,
        };
        let field_active = match self.scan {
            Scan::Progressive => self.vactive,
            _ => self.vactive / 2,
        };
        if self.vstart + field_active > field_lines {
            return Err("vstart + vactive が1フィールドに収まりません".into());
        }
        if self.vsync >= self.vstart.max(1) {
            return Err("VSYNC が有効映像に掛かっています".into());
        }
        if self.line_repeat == 0 || self.vactive % self.line_repeat != 0 {
            return Err("line_repeat は vactive を割り切る値".into());
        }
        let fh = self.fh();
        if !(10_000.0..=100_000.0).contains(&fh) {
            return Err(format!("fH {:.2}kHz は範囲外(10〜100kHz)", fh / 1e3));
        }
        Ok(())
    }
}

/// 1つの画面モード
#[derive(Clone, Debug)]
pub struct Mode {
    pub name: &'static str,
    pub timing: Timing,
    /// 1チャンネルの階調のビット数(X68000 は 5、MSX2 は 3、デジタル8色は 1)。
    /// パターンの色をこの段数に丸める
    pub color_bits: u32,
    /// 実機または一次資料で確かめてあるか
    pub verified: bool,
    pub note: &'static str,
}

/// 1つの機種
pub struct Machine {
    pub key: &'static str,
    pub label: &'static str,
    pub port: Port,
    pub kind: Kind,
    pub modes: Vec<Mode>,
}

#[allow(clippy::too_many_arguments)]
fn t(
    dotclk_mhz: f64, htotal: u32, hsync: u32, hstart: u32, hactive: u32,
    vtotal: u32, vsync: u32, vstart: u32, vactive: u32, scan: Scan,
) -> Timing {
    Timing {
        dotclk_hz: dotclk_mhz * 1e6, htotal, hsync, hstart, hactive,
        vtotal, vsync, vstart, vactive, scan, line_repeat: 1,
    }
}

fn rep(mut tm: Timing, n: u32) -> Timing {
    tm.line_repeat = n;
    tm
}

fn m(name: &'static str, timing: Timing, color_bits: u32, verified: bool, note: &'static str) -> Mode {
    Mode { name, timing, color_bits, verified, note }
}

/// 組み込みの機種一覧
pub fn machines() -> Vec<Machine> {
    use Scan::*;
    // X68000 の水晶(Viewer の profiles.rs と同じ。XEiJ の CRTC 実装表が出典)
    const X_HI: f64 = 69.551_99; // 高解像度(31kHz/24kHz)
    const X_LO: f64 = 38.863_63; // 低解像度(15kHz)
    const X_VGA: f64 = 50.349_8;
    const MSX: f64 = 21.477_27;
    const FSC8: f64 = 28.636_36; // NTSC 8fsc
    vec![
        Machine {
            key: "x68000",
            label: "X68000",
            port: Port::Rgb15,
            kind: Kind::Rgb,
            modes: vec![
                // 31kHz 系。fH 31.50kHz / fV 55.46Hz。CRTC の既定値(R00=$89 …)
                m("768x512 31kHz", t(X_HI / 2.0, 1104, 112, 232, 768, 568, 6, 40, 512, Progressive),
                  5, true, "CRTMOD 16。69.552MHz/2、htotal 1104(実機で確認済み)"),
                m("512x512 31kHz", t(X_HI / 3.0, 736, 72, 152, 512, 568, 6, 40, 512, Progressive),
                  5, true, "CRTMOD 12 など。69.552MHz/3、htotal 736(実機で確認済み)"),
                m("256x256 31kHz", rep(t(X_HI / 6.0, 368, 36, 76, 256, 568, 6, 40, 512, Progressive), 2),
                  5, false, "69.552MHz/6、htotal 368。同じ行を2回走査する"),
                m("640x480 31kHz (VGA)", t(X_VGA / 2.0, 800, 96, 144, 640, 525, 2, 35, 480, Progressive),
                  5, false, "50.3498MHz/2、htotal 800"),
                // 24kHz。VSYNC 1回の間にフィールドが2枚入る(design-notes / retrocastx_capture.py)
                m("1024x848 24kHz", t(X_HI / 2.0, 1408, 128, 296, 1024, 931, 4, 30, 848, FieldPair),
                  5, true, "69.552MHz/2、htotal 1408、vtotal 931。VSYNC 1回にフィールド2枚"),
                // 15kHz 系。fH 15.98kHz
                m("256x240 15kHz", t(X_LO / 8.0, 304, 24, 40, 256, 260, 3, 16, 240, Progressive),
                  5, false, "38.864MHz/8、htotal 304、vtotal 260(fV 61.46Hz)"),
                m("512x240 15kHz", t(X_LO / 4.0, 608, 48, 80, 512, 260, 3, 16, 240, Progressive),
                  5, false, "38.864MHz/4、htotal 608"),
                m("512x480 15kHz インターレース", t(X_LO / 4.0, 608, 48, 80, 512, 521, 3, 16, 480, Interlace),
                  5, false, "1フィールド 260.5 ライン。VSYNC はフィールドごと"),
            ],
        },
        Machine {
            key: "pc98",
            label: "PC-9801",
            port: Port::Rgb15,
            kind: Kind::Rgb,
            modes: vec![
                m("640x400 24kHz", t(21.052_5, 848, 64, 136, 640, 440, 8, 25, 400, Progressive),
                  4, false, "21.0525MHz、fH 24.83kHz / fV 56.4Hz(推定)"),
                m("640x480 31kHz", t(25.175, 800, 96, 144, 640, 525, 2, 35, 480, Progressive),
                  4, false, "PC-9821 の 31kHz(VGA 相当、推定)"),
                m("640x200 15kHz", t(14.318_18, 896, 64, 136, 640, 262, 3, 20, 200, Progressive),
                  1, false, "初代系のデジタル8色。推定"),
            ],
        },
        Machine {
            key: "vga",
            label: "VGA互換",
            port: Port::Rgb15,
            kind: Kind::Rgb,
            modes: vec![
                m("640x480 60Hz", t(25.175, 800, 96, 144, 640, 525, 2, 35, 480, Progressive),
                  8, true, "VESA DMT"),
                m("720x400 70Hz", t(28.322, 900, 108, 153, 720, 449, 2, 37, 400, Progressive),
                  8, true, "VGA テキスト"),
                m("640x400 70Hz", t(25.175, 800, 96, 144, 640, 449, 2, 37, 400, Progressive),
                  8, true, "VGA グラフィック"),
                m("800x600 56Hz", t(36.0, 1024, 72, 200, 800, 625, 2, 24, 600, Progressive),
                  8, true, "VESA DMT"),
            ],
        },
        Machine {
            key: "fmtowns",
            label: "FM TOWNS",
            port: Port::Rgb15,
            kind: Kind::Rgb,
            modes: vec![
                m("640x480 31kHz", t(25.175, 800, 96, 144, 640, 525, 2, 35, 480, Progressive),
                  5, false, "推定"),
                m("320x240 15kHz", t(28.636_36 / 4.0, 455, 34, 66, 320, 262, 3, 18, 240, Progressive),
                  5, false, "推定(NTSC 準拠の 15kHz)"),
            ],
        },
        Machine {
            key: "msx",
            label: "MSX2",
            port: Port::Rgb21,
            kind: Kind::Rgb,
            modes: vec![
                m("256x192 (SCREEN 5)", t(MSX / 4.0, 342, 26, 58, 256, 262, 3, 30, 192, Progressive),
                  3, true, "21.477MHz/4、htotal 342(実機 turboR で確認済み)"),
                m("256x212 (SCREEN 5, 212行)", t(MSX / 4.0, 342, 26, 58, 256, 262, 3, 20, 212, Progressive),
                  3, true, "同上、212 ライン"),
                m("512x212 (SCREEN 6/7)", t(MSX / 2.0, 684, 52, 116, 512, 262, 3, 20, 212, Progressive),
                  3, true, "21.477MHz/2、htotal 684"),
                m("512x424 インターレース", t(MSX / 2.0, 684, 52, 116, 512, 525, 3, 20, 424, Interlace),
                  3, false, "VSYNC はフィールドごと"),
            ],
        },
        Machine {
            key: "arcade15",
            label: "アーケード基板 (15kHz)",
            port: Port::Rgb21,
            kind: Kind::Rgb,
            modes: vec![
                m("320x224 59.6Hz", t(6.0, 384, 28, 60, 320, 262, 3, 24, 224, Progressive),
                  5, false, "代表的な 6MHz 系(推定)"),
                m("256x224 60Hz", t(MSX / 4.0, 342, 26, 58, 256, 262, 3, 24, 224, Progressive),
                  5, false, "5.37MHz 系(推定)"),
                m("384x224 59.2Hz", t(8.0, 512, 36, 92, 384, 264, 3, 24, 224, Progressive),
                  5, false, "8MHz 系(推定)"),
            ],
        },
        Machine {
            key: "composite",
            label: "コンポジット (NTSC)",
            port: Port::Video,
            kind: Kind::Composite,
            modes: vec![
                // ★ドットは 8fsc のサンプル。有効映像はおよそ 9.6〜62µs
                m("480i", t(FSC8, 1820, 135, 275, 1490, 525, 3, 18, 480, Interlace),
                  8, true, "NTSC。8fsc で 1820 サンプル/ライン"),
                m("240p", t(FSC8, 1820, 135, 275, 1490, 262, 3, 18, 240, Progressive),
                  8, false, "ゲーム機の 240p(262 ライン)"),
            ],
        },
        Machine {
            key: "svideo",
            label: "S端子 (NTSC)",
            port: Port::Video,
            kind: Kind::SVideo,
            modes: vec![
                m("480i", t(FSC8, 1820, 135, 275, 1490, 525, 3, 18, 480, Interlace),
                  8, false, "Y と C が別"),
                m("240p", t(FSC8, 1820, 135, 275, 1490, 262, 3, 18, 240, Progressive),
                  8, false, ""),
            ],
        },
        Machine {
            key: "component",
            label: "コンポーネント",
            port: Port::Video,
            kind: Kind::Component,
            modes: vec![
                m("480i", t(13.5, 858, 64, 122, 720, 525, 3, 18, 480, Interlace),
                  8, false, "BT.601 13.5MHz"),
                m("480p", t(27.0, 858, 62, 122, 720, 525, 6, 36, 480, Progressive),
                  8, false, "BT.1358 27MHz"),
                m("240p", t(13.5, 858, 64, 122, 720, 262, 3, 18, 240, Progressive),
                  8, false, ""),
            ],
        },
        Machine {
            key: "pc6001mk2sr",
            label: "PC-6001mkIISR",
            port: Port::Digital,
            kind: Kind::Rgb,
            modes: vec![
                m("640x200", t(14.318_18, 910, 68, 150, 640, 262, 3, 30, 200, Progressive),
                  1, false, "予想(Viewer の profiles.rs と同じ推論)"),
                m("320x200", t(14.318_18 / 2.0, 455, 34, 75, 320, 262, 3, 30, 200, Progressive),
                  1, false, "予想"),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_timings_are_consistent() {
        for mc in machines() {
            for md in &mc.modes {
                md.timing.validate().unwrap_or_else(|e| panic!("{} {}: {e}", mc.label, md.name));
            }
        }
    }

    /// Viewer の profiles.rs の実測値と合っていること
    #[test]
    fn x68000_known_frequencies() {
        let ms = machines();
        let x = ms.iter().find(|m| m.key == "x68000").unwrap();
        let fh = |name: &str| x.modes.iter().find(|m| m.name == name).unwrap().timing.fh();
        assert!((fh("768x512 31kHz") - 31_500.0).abs() < 10.0);
        assert!((fh("1024x848 24kHz") - 24_699.0).abs() < 10.0);
        assert!((fh("256x240 15kHz") - 15_980.0).abs() < 10.0);
        let t = &x.modes[0].timing;
        assert!((t.fv() - 55.46).abs() < 0.01, "fV {}", t.fv());
    }
}
