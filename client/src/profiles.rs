//! 映像ソースのプロファイルから pll_divide を決める。
//!
//! pll_divide は「1ラインを何サンプルで取るか」= H-PLLの帰還分周比で、
//! 1サンプル=1ドットにするには入力の水平トータル(htotal)に一致させる必要がある。
//!
//! 絵の内容から探す方法(スペクトル占有率、鮮鋭度の山登り)は、絵が真っ黒だったり
//! 細かい模様が無かったりすると当てにならず、実機でも変な値に着地することが多い。
//!
//! こちらは信号だけで決める。レトロPCのドットクロックは水晶を分周した有限個の値
//! しか取らないので、
//!
//! ```text
//!     htotal = f_dot / fH
//! ```
//!
//! の f_dot に候補を入れて「整数になるもの」を選べばよい。fH は pll_divide に
//! 依存しない絶対値で、MODE が持っている(sysドメインで1秒間エッジを数えた値)。
//!
//! 精度: fH の測定誤差は ±1Hz。htotal への影響は htotal/fH で、31.5kHz・
//! htotal 1104 なら ±0.035カウント。整数を選び分けるには十分すぎる。

/// TVP7002のピクセルクロック下限[Hz]。データシートの保証範囲は12〜165MHz で、
/// 下回るとクランプが効かなくなり画面の下ほど色がずれる(実測: 12.02MHz は正常、
/// 11.25MHz から崩れ、9.72MHz では白の青/赤比が上下で0.43違った)。
const TVP_DOTCLK_MIN: f64 = 12.0e6;
pub const PLL_MIN: i32 = 200;
/// ラインバッファの幅[サンプル](gateware の TvpCapture width)。1ラインが
/// 入り切らないと外接矩形が有効映像の幅を表さなくなるので、実用上限はこちら
/// (gateware の絶対上限 2304 ではない)。
pub const LINE_BUFFER_W: i32 = 2048;

pub struct Profile {
    /// 設定ファイルに書く名前
    pub key: &'static str,
    pub label: &'static str,
    /// 実機の水晶。モードごとのドットクロックを列挙するより漏れにくい
    pub oscillators: &'static [(&'static str, f64)],
    pub dividers: &'static [i32],
    /// htotal の粒度。X68000のCRTCは水平トータルを8ドット単位で持つので、
    /// 正解は必ず8の倍数になる。この制約が候補をさらに絞る
    pub htotal_multiple: i32,
    /// 実機で裏を取れているか。**自動選択の順位付けに使う。**
    ///
    /// 水晶が違っても fH ひとつからは同じくらいよく合う候補が出ることがある。
    /// 実際 MSX の fH 15.699kHz に対し、未検証の pc98(21.0525MHz)が
    /// htotal 1341(相対誤差 6.7e-6)で、正解の msx(342、相対誤差 4.8e-5)より
    /// 良く見えてしまう。残差の閾値を通っている時点でどちらも「信号を説明できる」
    /// ので、そこから先は推測の水晶より実測済みの水晶を優先する。
    pub verified: bool,
}

/// 実測で裏を取れているのは x68000 と msx。
///
/// x68000 は3帯域すべてが2つの水晶の分周で説明でき、このプロジェクトの記録
/// (31kHz→1104 / 24kHz→1408 / 15kHz→1216)と一致した。
/// msx は実機の fH 15.699kHz から 342.0165(ずれ0.017)が出て、Viewer の表示
/// (dotclk 21.4773MHz / total 1368x262)とも一致した。
/// vga / pc98 は未検証で、候補の並びを見る参考に留めること。
pub const PROFILES: &[Profile] = &[
    Profile {
        key: "x68000",
        label: "X68000",
        oscillators: &[("69.55199MHz", 69.551_99e6), ("38.86363MHz", 38.863_63e6)],
        dividers: &[1, 2, 4, 8],
        htotal_multiple: 8,
        verified: true,
    },
    Profile {
        key: "msx",
        label: "MSX",
        // 水晶 21.477270MHz = NTSCカラーサブキャリア 3.579545MHz × 6。
        // VDP(V9938/V9958)のドットクロックはこの 1/4 = 5.3693MHz(256ドット系)と
        // 1/2 = 10.7386MHz(512ドット系 SCREEN 6/7)。どちらも htotal は
        // 342 / 684 で、TVPの下限12MHzを満たす整数倍にすると両方 pll_divide=1368 に
        // 収束する。
        //
        // 実機確認: MSX turboR の BASIC 画面で fH 15.699kHz を実測。
        //   f_dot/fH = 5.369318MHz / 15699Hz = 342.0165 (整数からのずれ 0.017)
        //   逆算した正確な fH = 15699.76Hz
        // Viewer 上の表示も dotclk 21.4773MHz / total 1368x262 で一致した。
        //
        // dividers に 1 や 8 を入れないのは、実在しないドットクロックで他機種の
        // fH に誤マッチするのを避けるため(1368/171 も同じ pll_divide に収束するので
        // 入れても得は無い)。
        oscillators: &[("21.47727MHz", 21.477_270e6)],
        dividers: &[2, 4],
        // MSXのVDPは htotal 342 固定(512ドット系でもドットクロックが倍になるだけ)
        // なので粒度の概念が無い。制約は付けず、相対誤差だけで判定させる。
        htotal_multiple: 1,
        verified: true,
    },
    Profile {
        key: "vga",
        label: "VGA互換 (未検証)",
        oscillators: &[("25.175MHz", 25.175e6), ("28.322MHz", 28.322e6)],
        dividers: &[1, 2],
        htotal_multiple: 1,
        verified: false,
    },
    Profile {
        key: "pc98",
        label: "PC-9801 (未検証)",
        oscillators: &[("21.0525MHz", 21.052_5e6), ("25.175MHz", 25.175e6)],
        dividers: &[1, 2],
        htotal_multiple: 1,
        verified: false,
    },
];

pub fn by_key(key: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.key == key)
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub htotal: i32,
    pub f_dot: f64,
    pub label: String,
    /// 整数からのずれ[カウント]
    pub residual: f64,
    pub multiple_ok: bool,
    /// 実際に設定する値(TVPの下限を満たすよう整数倍した後)
    pub pll_divide: i32,
    /// 何倍にしたか(1なら1サンプル=1ドット)
    pub oversample: i32,
}

impl Candidate {
    /// 相対周波数誤差。どの水晶から来たかを見分けるのはこちら。
    ///
    /// 残差[カウント]は htotal に比例するので、同じ水晶の分周違い
    /// (276/552/1104/2208)を比べると小さい方が必ず良く見えてしまい、選定に
    /// 使えない。残差/htotal にすると分周に依らない量になり、水晶が合っていれば
    /// 同じ値(実測 3.2e-5)、違う水晶なら桁で外れる(1.6e-4)。
    pub fn rel_err(&self) -> f64 {
        self.residual / (self.htotal.max(1) as f64)
    }
}

/// fH[Hz] から htotal 候補を出す。
///
/// `max_residual` は「整数からどれだけ離れていても候補に残すか」[カウント]。
/// fH の測定誤差(±1Hz)が htotal に効く量は htotal/fH なので、31.5kHz なら
/// 0.035程度。0.15 は4倍以上の余裕。
pub fn candidates(p: &Profile, fh_hz: f64, max_residual: f64, max_pll: i32) -> Vec<Candidate> {
    let mut out = Vec::new();
    if !(fh_hz > 0.0) {
        return out;
    }
    for (name, f) in p.oscillators {
        for d in p.dividers {
            let f_dot = f / (*d as f64);
            let ht = f_dot / fh_hz;
            let n = ht.round();
            if !(n >= 8.0) || n > 1.0e6 {
                continue;
            }
            let residual = (ht - n).abs();
            if residual > max_residual {
                continue;
            }
            let n = n as i32;
            // TVPの下限を満たす最小の整数倍にする。オーバーサンプリングなので
            // 情報は失われず、8の倍数のままでもある(15kHz 512x512 の 1:1 は
            // 608 = 9.7MHz で下限割れなので、2倍の1216を使う)
            let (mut pll, mut over) = (n, 1);
            while f_dot * (over as f64) < TVP_DOTCLK_MIN && pll * 2 <= max_pll {
                pll *= 2;
                over *= 2;
            }
            if pll < PLL_MIN || pll > max_pll {
                continue;
            }
            out.push(Candidate {
                htotal: n,
                f_dot,
                label: if *d == 1 {
                    (*name).to_string()
                } else {
                    format!("{name}/{d}")
                },
                residual,
                multiple_ok: n % p.htotal_multiple == 0,
                pll_divide: pll,
                oversample: over,
            });
        }
    }
    out.sort_by(|a, b| {
        (a.multiple_ok == b.multiple_ok)
            .then_some(std::cmp::Ordering::Equal)
            .unwrap_or_else(|| b.multiple_ok.cmp(&a.multiple_ok))
            .then(a.rel_err().partial_cmp(&b.rel_err()).unwrap())
            .then(b.pll_divide.cmp(&a.pll_divide))
    });
    out
}

/// いちばん確からしい候補を1つ返す。
///
/// 選び方は2段構え:
///
/// 1. 相対誤差でどの水晶かを決める。正しい水晶の分周違いは相対誤差が同じ値に
///    揃うので、これで一族が分かる
/// 2. その一族の中で「ラインバッファに収まる最大の pll_divide」を採る
///
/// 2. の向きが重要。真の htotal より小さい値を選ぶとドットを取りこぼす
/// (1104が正解のときに552を選ぶと1ドットおきにしか読まない = 破壊的)のに対し、
/// 大きい値は単なる整数倍オーバーサンプルで情報は失われない。だから迷ったら
/// 大きい側へ倒す。上限はラインバッファ幅。
pub fn best(p: &Profile, fh_hz: f64) -> Option<Candidate> {
    let cs = candidates(p, fh_hz, 0.15, LINE_BUFFER_W);
    if cs.is_empty() {
        return None;
    }
    let any_mult = cs.iter().any(|c| c.multiple_ok);
    let pool: Vec<&Candidate> = cs.iter().filter(|c| !any_mult || c.multiple_ok).collect();
    let lo = pool.iter().map(|c| c.rel_err()).fold(f64::INFINITY, f64::min);
    // 同じ水晶から来たものだけに絞る。相対誤差は桁で分かれるので3倍で十分切れる
    pool.iter()
        .filter(|c| c.rel_err() <= (lo * 3.0).max(1e-9))
        .max_by_key(|c| c.pll_divide)
        .map(|c| (*c).clone())
}

/// プロファイルを人が選ばない場合。全部試していちばん確からしいものを返す。
pub fn best_over_all(fh_hz: f64) -> Option<(&'static Profile, Candidate)> {
    let mut ranked: Vec<(&'static Profile, Candidate)> = PROFILES
        .iter()
        .filter_map(|p| best(p, fh_hz).map(|c| (p, c)))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.multiple_ok
            .cmp(&a.1.multiple_ok)
            // 残差の閾値を通った時点でどちらも信号を説明できているので、
            // 相対誤差の僅差より「実機で裏を取れているか」を優先する
            .then(b.0.verified.cmp(&a.0.verified))
            .then(a.1.rel_err().partial_cmp(&b.1.rel_err()).unwrap())
    });
    ranked.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実機で確かめた3帯域。ここが崩れたら選定ロジックが壊れている。
    ///
    /// 正しい htotal の約数はすべて整数になる(1104 が正解なら 552 も 276 も
    /// 整数)ので、残差の小さい順に採ると必ず小さい方へ落ちる。実際、最初の
    /// 実装は 31.5kHz で 552、24.7kHz で 704 を選んでいた。
    #[test]
    fn x68000_three_bands() {
        let p = by_key("x68000").unwrap();
        for (fh, want) in [(31499.0, 1104), (24699.0, 1408), (15980.0, 1216)] {
            let c = best(p, fh).expect("候補なし");
            assert_eq!(c.pll_divide, want, "fH={fh} で {} を選んだ", c.pll_divide);
        }
    }

    /// MSX実機(fH 15.699kHz)。htotal 342 の4倍オーバーサンプルで 1368。
    #[test]
    fn msx_basic() {
        let p = by_key("msx").unwrap();
        let c = best(p, 15699.0).expect("候補なし");
        assert_eq!(c.pll_divide, 1368, "選んだのは {}", c.pll_divide);
        assert_eq!(c.htotal * c.oversample, 1368);
    }

    /// x68000 の15kHz(fH 15.98kHz)を msx と取り違えないこと。
    /// 両者は帯域が近いので、プロファイル自動選択がここで崩れると実害が出る。
    #[test]
    fn msx_and_x68000_15khz_are_distinguished() {
        let (p, c) = best_over_all(15980.0).expect("候補なし");
        assert_eq!(p.key, "x68000", "X68000の15kHzで {} を選んだ", p.key);
        assert_eq!(c.pll_divide, 1216);

        let (p, c) = best_over_all(15699.0).expect("候補なし");
        assert_eq!(p.key, "msx", "MSXのfHで {} を選んだ", p.key);
        assert_eq!(c.pll_divide, 1368);
    }

    #[test]
    fn auto_picks_x68000() {
        for (fh, want) in [(31499.0, 1104), (24699.0, 1408), (15980.0, 1216)] {
            let (p, c) = best_over_all(fh).expect("候補なし");
            assert_eq!(p.key, "x68000", "fH={fh} で {} を選んだ", p.key);
            assert_eq!(c.pll_divide, want);
        }
    }

    /// ラインバッファに入らない値は出さない(1ラインが入り切らないと
    /// 外接矩形が有効映像の幅を表さなくなる)
    #[test]
    fn never_exceeds_line_buffer() {
        for p in PROFILES {
            for fh in [15980.0, 24699.0, 31499.0, 15734.0] {
                for c in candidates(p, fh, 0.15, LINE_BUFFER_W) {
                    assert!(c.pll_divide <= LINE_BUFFER_W);
                    assert!(c.pll_divide >= PLL_MIN);
                }
            }
        }
    }
}
