//! NTSCコンポジットの復調(Y/C分離 + 直交復調)。
//!
//! `host/python/retrocastx/ntsc.py` の移植。**あちらが仕様の正**で、合成6色に
//! 対する回帰試験(`tests/test_ntsc.py`、誤差2.1°以内)もあちらにある。
//! ここを直したら向こうも直すこと。
//!
//! ## なぜ 8fsc だと軽いか
//!
//! サンプルレートが副搬送波のちょうど8倍なので、`cos(2πn/8)` は
//! `n mod 8` の8点しか取らない。位相基準はライン毎のバーストから決まるので、
//! **1サンプルあたり積和2回**で直交復調できる。テーブルすら小さい。
//!
//! 実測の負荷: 1820サンプル × 262行 × 59.94フィールド/秒 = 28.6 MSa/s。
//! 1サンプルあたり20〜30演算なので 0.6〜0.9 G演算/秒。
//!
//! ## コムのペアは測って決める
//!
//! NTSCは227.5周期/ラインなので、**時間的に隣のライン**とは位相が180°ずれる。
//! ところがフレームバッファの行番号は「フィールド内の行×2 + 極性」なので、
//! 1フィールドだけ来ている間は**奇数行(または偶数行)だけ**が埋まり、
//! 時間的に隣なのは N と N±2 になる。織り込み設定によっても変わる。
//!
//! なので**行番号の差1と2の両方で位相差を測り、180°に近い方を採る**。
//! 決め打ちにすると設定が変わった瞬間に色が消える(コムが同位相の行を引く)。

/// バースト区間 [µs]。同期立ち下がりを0とする
const BURST_US: (f32, f32) = (5.4, 7.7);
/// 同期チップとバックポーチ(レベル校正に使う。絵の内容に依存しない)
const TIP_US: (f32, f32) = (0.7, 4.2);
const PORCH_US: (f32, f32) = (7.9, 9.3);

/// クロマの移動平均長[サンプル]。**8の倍数にすること。**
///
/// 直交復調の積には必ず 2fsc 成分が出る。8の倍数で平均するとちょうど整数周期
/// ぶん入って完全に消える。適当な窓長だと色に細かい縞が残る。
const CHROMA_LPF: usize = 16;

/// 3次元コムで「動いている」と判定する輝度の時間差[IRE]。
///
/// 動き検出には **2 NTSCフレーム前**(位相0°でクロマが消える)を使う。
/// 1フレーム前は位相180°なのでクロマがそのまま差に出て、**色のある所が全部
/// 「動いている」ことになる**(実測で副搬送波成分が 388 対 9315)。
///
/// 実測の動き量の分布は 中央値 2.3 IRE / 90%点 4.7 / 最大 52.3 で、ノイズ床が
/// 2〜4 IRE。閾値をノイズ床より下にすると常に2次元へ落ちて意味が無くなる。
const MOTION_IRE: f32 = 8.0;

/// バーストが取れたと判定する相関の下限。これ未満の行は無彩色にする。
///
/// ★真っ黒な領域では相関が雑音になり、**色相が乱数になる**(実測: 彩度0.03〜0.09の
///   行で色相が230〜300°をふらついた)。無彩色に倒す方が絵として正しい。
const BURST_MIN: f32 = 60.0;

/// S端子と判定する「Cチャネルのバースト / バックポーチ」比の下限。
///
/// 実測(コンポジット、赤ch未接続)で 1.0 前後、バーストが載っていれば数十になる
/// ので、間は大きく空いている。
const SVIDEO_SNR_MIN: f32 = 6.0;

/// フレームコムの位相ズレ補正で受け付ける ε の上限[度]。
///
/// 実測の |ε| は中央値 4.8°、滑らかに±15°を揺れる程度。これを大きく超える値は
/// バーストの測り損ね(暗い行、欠損行)なので、補正を掛けない方が安全。
/// tan(ε/2) が暴れると 3次元の枝ごと壊れる。
const PHASE_FIX_MAX_DEG: f32 = 30.0;

pub struct Info {
    pub lines_locked: u32,
    pub comb_step: usize,
    pub phase_delta_deg: f32,
    pub code_per_ire: f32,
    /// 3次元(動き適応フレームコム)を使えた行数
    pub lines_3d: u32,
    /// そのうち「動いている」と判定した画素の割合(0..1)
    pub motion_frac: f32,
    /// 赤ch(C)にバーストが載っていた = S端子として復調した。
    /// このときコムは一切使わない(Y と C が最初から別々に来ているため)。
    pub svideo: bool,
    /// 1 NTSCフレーム前との副搬送波位相のズレ |ε| の中央値[度]。
    /// これがフレームコムの消し残し(= フレームごとに反転するドットクロール)を
    /// 決める。残留は C·sin(ε/2)。
    pub phase_drift_deg: f32,
}

fn win(us: (f32, f32), sps: f32, w: usize) -> (usize, usize) {
    let a = (us.0 * 1e-6 * sps) as usize;
    let b = (us.1 * 1e-6 * sps) as usize;
    (a.min(w), b.min(w))
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// -180..180 に畳んだ角度差[度]
fn ang_diff(a: f32, b: f32) -> f32 {
    let mut d = (a - b) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

/// 1フィールドを復調して `fb`(RGBA)へ書く。
///
/// `raw` は 2バイト/サンプル(下位=緑ch=CVBS、上位=赤ch。コンポジットでは赤は未使用)。
/// `filled[y]` がそのラインを受信したか。受信していない行は触らない
/// (呼び出し側の欠損補間・減衰に任せる)。
/// 3次元コム用の履歴。`p2` は1 NTSCフレーム前(位相180°)、`p4` は2フレーム前
/// (位相0°、動き検出用)。`hist_n[y] >= 3` の行だけ3次元を使う。
/// 見た目の調整。**復調の正しさとは別に持つ。**
///
/// 信号内の基準(同期40 IRE / バースト40 IRE p-p / 黒0 IRE / 白100 IRE)に合わせた
/// 結果が「正しい」絵で、既定値はそこを指す。ここはその上に載せる好みの調整で、
/// 復調の校正を触らない(校正を歪めると、後で数値で追えなくなる)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Adjust {
    /// 色相[度]。NTSCの tint。バースト位相にそのまま足す
    pub hue_deg: f32,
    /// 彩度。1.0 = 信号どおり
    pub saturation: f32,
    /// 明るさ[IRE]。黒レベルを上下する
    pub brightness: f32,
    /// コントラスト。1.0 = 信号どおり。輝度と色差の両方に掛ける
    /// (色差に掛けないと、コントラストを上げたとき色が薄く見える)
    pub contrast: f32,
}

impl Default for Adjust {
    fn default() -> Self {
        Self { hue_deg: 0.0, saturation: 1.0, brightness: 0.0, contrast: 1.0 }
    }
}

pub struct History<'a> {
    pub p2: &'a [u8],
    pub p4: &'a [u8],
    pub hist_n: &'a [u8],
}

/// 1ラインの区間 [x0,x1) から (振幅, cos成分, sin成分) を測る。
///
/// `ch` は 0=緑ch(CVBS または Y) / 1=赤ch(S端子の C)。
/// バーストが `A·cos(2π(n-x0)/8 - φ)` と表せる φ を `si.atan2(ci)` で得る。
fn burst(row: &[u8], x0: usize, x1: usize, ch: usize) -> (f32, f32, f32) {
    let mut mean = 0.0f32;
    for n in x0..x1 {
        mean += row[n * 2 + ch] as f32;
    }
    mean /= (x1 - x0) as f32;
    let (mut ci, mut si) = (0.0f32, 0.0f32);
    for n in x0..x1 {
        let v = row[n * 2 + ch] as f32 - mean;
        let k = (n - x0) & 7;
        ci += v * COS8[k];
        si += v * SIN8[k];
    }
    ((ci * ci + si * si).sqrt(), ci, si)
}

/// 赤ch(C)にバーストが載っているか。バースト区間の fsc 相関を、信号の無い
/// バックポーチのそれと比べた比で返す。
///
/// **S端子かコンポジットかを測って決める**ために使う。絶対値で閾値を切ると
/// チャネルのゲイン設定に依存するので、同じチャネルの信号の無い区間を基準にする。
/// 配線と設定が食い違っても絵が出る(コムの間隔やインタレースと同じ方針)。
fn c_burst_snr(raw: &[u8], w: usize, h: usize, filled: &[bool],
               ba: usize, bb: usize, pa: usize, pb: usize) -> f32 {
    let n = (bb - ba).min(pb.saturating_sub(pa));
    if n < 16 {
        return 0.0;
    }
    let (mut bs, mut ps) = (Vec::new(), Vec::new());
    for y in 0..h {
        if !filled.get(y).copied().unwrap_or(false) {
            continue;
        }
        let row = &raw[y * w * 2..(y + 1) * w * 2];
        bs.push(burst(row, ba, ba + n, 1).0);
        ps.push(burst(row, pa, pa + n, 1).0);
    }
    if bs.len() < 8 {
        return 0.0;
    }
    median(&mut bs) / median(&mut ps).max(1e-6)
}

pub fn decode_field(
    raw: &[u8],
    w: usize,
    h: usize,
    filled: &[bool],
    dotclk_hz: u32,
    fb: &mut [u8],
    hist: Option<History<'_>>,
    adj: Adjust,
) -> Info {
    let sps = dotclk_hz as f32;
    let (ba, bb) = win(BURST_US, sps, w);
    if sps <= 0.0 || bb <= ba + 8 || w < 16 {
        return Info { lines_locked: 0, comb_step: 0, phase_delta_deg: 0.0,
                      code_per_ire: 0.0, lines_3d: 0, motion_frac: 0.0,
                      svideo: false, phase_drift_deg: 0.0 };
    }

    // --- 1. ラインごとのバースト位相 ---
    //
    // バースト区間の先頭 ba を基準に、バーストが A·cos(2π(n-ba)/8 - φ) と
    // 表せる φ を求める。**ライン毎に測るのが要点。** ライン番号のパリティから
    // 予測すると、行が1本落ちただけで以降の色が全部反転する。
    //
    // ★どちらのチャネルにクロマが載っているかを**測って**決める。S端子では
    //   赤ch(C)にバーストが載り、コンポジットでは赤chに何も繋がらない。
    let (pa0, pb0) = win(PORCH_US, sps, w);
    let svideo = c_burst_snr(raw, w, h, filled, ba, bb, pa0, pb0) >= SVIDEO_SNR_MIN;
    let cch = if svideo { 1 } else { 0 };
    let mut cosp = vec![0.0f32; h];
    let mut sinp = vec![0.0f32; h];
    let mut mag = vec![0.0f32; h];
    let mut phase = vec![0.0f32; h];
    for y in 0..h {
        if !filled.get(y).copied().unwrap_or(false) {
            continue;
        }
        let (m, ci, si) = burst(&raw[y * w * 2..(y + 1) * w * 2], ba, bb, cch);
        mag[y] = m;
        phase[y] = si.atan2(ci);
        // 復調で使うのは φ の cos/sin だけなので、正規化して持つ
        let m = m.max(1e-6);
        cosp[y] = ci / m;
        sinp[y] = si / m;
    }

    // --- 2. コムのペアを測って決める ---
    //
    // 行番号の差1と2で位相差を測り、180°に近い方を採る。**決め打ちにしない。**
    let mut best = (0usize, 999.0f32, 0.0f32);
    for step in [1usize, 2] {
        let mut ds = Vec::new();
        for y in step..h {
            if mag[y] > BURST_MIN && mag[y - step] > BURST_MIN {
                ds.push(ang_diff(phase[y].to_degrees(), phase[y - step].to_degrees()).abs());
            }
        }
        if ds.len() < 8 {
            continue;
        }
        let m = median(&mut ds);
        let err = (m - 180.0).abs();
        if err < best.1 {
            best = (step, err, m);
        }
    }
    let (comb_step, _, phase_delta) = best;
    if comb_step == 0 {
        // 180°になるペアが見つからない = バーストが取れていない。
        // 何もしないで戻る(呼び出し側のグレースケール表示が残る)。
        return Info { lines_locked: 0, comb_step: 0, phase_delta_deg: 0.0,
                      code_per_ire: 0.0, lines_3d: 0, motion_frac: 0.0,
                      svideo: false, phase_drift_deg: 0.0 };
    }

    // --- 3. レベル校正。同期チップ(-40 IRE)とバックポーチ(0 IRE)から求める ---
    //     絵の内容に依存しないのがこの校正の利点。
    let (ta, tb) = win(TIP_US, sps, w);
    let (pa, pb) = win(PORCH_US, sps, w);
    let mut tips = Vec::new();
    let mut porches = Vec::new();
    for y in 0..h {
        if !filled.get(y).copied().unwrap_or(false) || mag[y] <= BURST_MIN {
            continue;
        }
        let row = &raw[y * w * 2..(y + 1) * w * 2];
        let mut s = 0.0f32;
        for n in ta..tb {
            s += row[n * 2] as f32;
        }
        tips.push(s / (tb - ta).max(1) as f32);
        let mut s = 0.0f32;
        for n in pa..pb {
            s += row[n * 2] as f32;
        }
        porches.push(s / (pb - pa).max(1) as f32);
    }
    let tip = median(&mut tips);
    let porch = median(&mut porches);
    let code_per_ire = ((porch - tip) / 40.0).max(0.05);
    let inv_100ire = 1.0 / (code_per_ire * 100.0);

    // --- 3b. 1フレーム前との副搬送波位相のズレ ε を測る ---
    //
    // ★**フレームごとに入れ替わる縞の正体はこれ**(実測 2026-08-15)。
    //
    // DATACLK は HSYNC にロックしていて **副搬送波にはロックしていない**ので、
    // フレームをまたぐと副搬送波とサンプル格子の位相関係が歩く。実測した ε は
    // |中央値| 4.80°で、行方向に滑らかに ±15° を揺れる(= 測定ノイズではなく
    // 本物のドリフト。隣接行との相関で確認した)。
    //
    // フレームコムは「1フレーム前は厳密に180°反転」を前提にしているので ε ぶん
    // 消し残る。残留は C·sin(ε/2) で、彩度 38.4 IRE のときの予測 1.57 IRE が
    // **実測の残留 1.57 IRE と一致した**(独立な2通りの測り方で同じ値)。
    // 副搬送波成分なのでフレームごとに符号が反転し、「赤黒赤黒」が「黒赤黒赤」に
    // 入れ替わって見える。
    //
    // 直し方は c3 の位相を ε/2 戻すこと。8fsc では **2サンプル遅延がちょうど90°**
    // なので、ヒルベルト変換を持ち出さずに1サンプルあたり積和1回で回せる:
    //
    //     c3(n)   = K·cos(ψ+θ-ε/2)
    //     c3(n-2) = K·cos(ψ+θ-ε/2 - 90°) = K·sin(ψ+θ-ε/2)
    //     K·cos(ψ+θ) = c3(n)·cos(ε/2) - c3(n-2)·sin(ε/2)
    //     振幅も戻すので cos(ε/2) で割って  **c3(n) - c3(n-2)·tan(ε/2)**
    //
    // 実測(静止・平坦・彩度の高い画素で、輝度に残る副搬送波の中央値):
    //     補正なし 1.57 IRE / 補正あり 1.20 IRE / 符号を逆にすると 2.38 IRE
    // 信号の無い区間のノイズ床が 1.45 IRE なので、ここが底。**残りは基板側。**
    let mut tan_half = vec![0.0f32; h];
    let mut drifts = Vec::new();
    if let Some(hh) = hist.as_ref().filter(|_| !svideo) {
        if hh.p2.len() == raw.len() {
            for y in 0..h {
                if !filled.get(y).copied().unwrap_or(false) || mag[y] <= BURST_MIN {
                    continue;
                }
                let (m2, ci, si) = burst(&hh.p2[y * w * 2..(y + 1) * w * 2], ba, bb, cch);
                if m2 <= BURST_MIN {
                    continue;
                }
                let e = ang_diff(si.atan2(ci).to_degrees(), phase[y].to_degrees() + 180.0);
                drifts.push(e.abs());
                // 外れ値(暗い行でバーストを測り損ねた等)では補正を掛けない。
                // tan(ε/2) が暴れると3次元の枝ごと壊れる方が高くつく。
                if e.abs() <= PHASE_FIX_MAX_DEG {
                    tan_half[y] = (e.to_radians() * 0.5).tan();
                }
            }
        }
    }
    let phase_drift_deg = median(&mut drifts);

    // --- 3c. クロマのスケール ---
    //
    // ★S端子では C 側の校正が別に要る。赤chはクランプもゲインも緑chと別設定
    //   なので、Y の code_per_ire では合わない。バーストは規格で 40 IRE p-p
    //   (= 振幅 20 IRE)と決まっているので、それをものさしにする。
    //   チャネル間のゲイン差が自動的に打ち消えるのが利点。
    let c_per_ire = if svideo {
        let mut ms: Vec<f32> = (0..h)
            .filter(|&y| filled.get(y).copied().unwrap_or(false) && mag[y] > BURST_MIN)
            .map(|y| mag[y])
            .collect();
        // 相関 mag = A·N/2 なので 振幅 A = 2·mag/N、それが 20 IRE にあたる
        let amp = 2.0 * median(&mut ms) / (bb - ba).max(1) as f32;
        (amp / 20.0).max(0.05)
    } else {
        code_per_ire
    };
    let inv_100ire_c = 1.0 / (c_per_ire * 100.0);

    // --- 4. コム → 直交復調 → RGB ---
    let mut u = vec![0.0f32; w];
    let mut v = vec![0.0f32; w];
    let mut yl = vec![0.0f32; w];
    let mut mot = vec![0.0f32; w];
    let mut c3buf = vec![0.0f32; w];
    let mut locked = 0u32;
    let mut lines_3d = 0u32;
    let (mut moving, mut n3) = (0u32, 0u32);
    let motion_th = MOTION_IRE * code_per_ire;
    for y in 0..h {
        if !filled.get(y).copied().unwrap_or(false) {
            continue;
        }
        // 上下の相手。片方しか無ければそれだけを使う(端の行)。
        let up = y.checked_sub(comb_step)
            .filter(|&i| filled.get(i).copied().unwrap_or(false));
        let dn = (y + comb_step < h)
            .then(|| y + comb_step)
            .filter(|&i| filled.get(i).copied().unwrap_or(false));
        let chroma_ok = mag[y] > BURST_MIN && (svideo || up.is_some() || dn.is_some());
        locked += chroma_ok as u32;

        // 色相は「復調の位相基準をずらす」ことで入れる。ψ = 2π(n-ba)/8 - φ なので、
        // th = ψ + hue は φ' = φ - hue と同じ。ライン毎に1回の回転で済む。
        let (cp, sp) = {
            let (c0, s0) = (cosp[y], sinp[y]);
            if adj.hue_deg == 0.0 {
                (c0, s0)
            } else {
                let h = adj.hue_deg.to_radians();
                (c0 * h.cos() + s0 * h.sin(), s0 * h.cos() - c0 * h.sin())
            }
        };
        let px = |i: usize, j: usize| raw[(i * w + j) * 2] as f32;
        // S端子では C(赤ch)がそのままクロマ。ミッドレベルクランプなので
        // バックポーチを0点にする
        let c_porch = if svideo {
            let mut s = 0.0f32;
            for n in pa..pb {
                s += raw[(y * w + n) * 2 + 1] as f32;
            }
            s / (pb - pa).max(1) as f32
        } else {
            0.0
        };
        // この行で3次元が使えるか。履歴が3回以上書かれている行だけ。
        // **S端子ではコムを一切使わない**(Y と C が最初から別々に来ている)
        let use3d = !svideo && hist.as_ref().map_or(false, |hh| {
            hh.hist_n.get(y).copied().unwrap_or(0) >= 3
                && hh.p2.len() == raw.len() && hh.p4.len() == raw.len()
        });
        if use3d {
            lines_3d += 1;
            // 動き検出は **2 NTSCフレーム前**(位相0°)との差。1フレーム前だと
            // 位相180°でクロマが差に出てしまい、色のある所が全部「動いている」
            // ことになる(実測で副搬送波成分が 388 対 9315)。
            let hh = hist.as_ref().unwrap();
            for n in 0..w {
                mot[n] = (px(y, n) - hh.p4[(y * w + n) * 2] as f32).abs();
            }
            // 副搬送波1周期(8サンプル)で平均してノイズを落とす
            boxcar(&mut mot, 8);
            // フレームコムのクロマ。位相ズレ ε を 2サンプル遅延で戻す(3b参照)。
            for n in 0..w {
                c3buf[n] = (px(y, n) - hh.p2[(y * w + n) * 2] as f32) * 0.5;
            }
            let t = tan_half[y];
            if t != 0.0 {
                // 後ろから回すので c3buf[n-2] は**補正前の値**のまま使える
                for n in (2..w).rev() {
                    c3buf[n] -= c3buf[n - 2] * t;
                }
            }
        }
        // ψ(n) = 2π(n-ba)/8 - φ を加法定理で展開する。cos/sin の値は
        // n mod 8 の8点しかないので、積和2回で済む(8fsc の利点)。
        let psi = |n: usize| {
            let k = (n + 8 - (ba & 7)) & 7;
            let (ck, sk) = (COS8[k], SIN8[k]);
            (ck * cp + sk * sp, sk * cp - ck * sp)
        };
        // --- 1) クロマをコムで取り出して復調する ---
        //
        // 上下2本を平均してから引くのは、片側だけだと C の重心が垂直方向に
        // 半ラインずれるため。隣接ラインは副搬送波が180°反転しているので、
        // 差で輝度が打ち消える。
        for n in 0..w {
            // --- S端子: C(赤ch)がそのままクロマ。コムは一切要らない ---
            if svideo {
                let c = raw[(y * w + n) * 2 + 1] as f32 - c_porch;
                let (cos_psi, sin_psi) = psi(n);
                u[n] = -2.0 * c * cos_psi;
                v[n] = 2.0 * c * sin_psi;
                continue;
            }
            let mut acc = 0.0f32;
            let mut cnt = 0.0f32;
            if let Some(i) = up {
                acc += px(i, n);
                cnt += 1.0;
            }
            if let Some(i) = dn {
                acc += px(i, n);
                cnt += 1.0;
            }
            if cnt == 0.0 {
                u[n] = 0.0;
                v[n] = 0.0;
                continue;
            }
            let mut c = (px(y, n) - acc / cnt) * 0.5;
            // --- 3次元(動き適応フレームコム) ---
            //
            // 静止部分では **フレームコムが原理的に正解**。同じライン番号の
            // 1 NTSCフレーム前は副搬送波が180°反転しているので、差が厳密に 2C に
            // なる(輝度がフレーム間で同一だから)。垂直方向を一切見ないので、
            // 2次元コムのように垂直detailで崩れない。
            //
            // 実測(静止部分でフレームコムを正解としたときの2次元コムの誤差):
            //     垂直detailが小さい所(下位50%)  0.79 IRE  ← ノイズ床以下
            //     垂直detailが大きい所(上位10%)  9.47 IRE  ← **12倍**
            //
            // 動いている所は成立しないので 2次元へ落とす(= 動き適応)。
            if use3d {
                let c3 = c3buf[n];
                // 動き量は mot[] に入れてある(2フレーム前との差を平滑したもの)
                let a = (mot[n] / motion_th.max(1e-6)).clamp(0.0, 1.0);
                if a >= 0.5 { moving += 1; }
                n3 += 1;
                c = (1.0 - a) * c3 + a * c;
            }
            let (cos_psi, sin_psi) = psi(n);
            // バーストは -(B-Y) 軸(位相180°)。V の符号は実測で決めた
            // (既知の2色が回転では合わず、V反転で合った。ntsc.py のコメント参照)
            u[n] = -2.0 * c * cos_psi;
            v[n] = 2.0 * c * sin_psi;
        }
        if chroma_ok {
            boxcar(&mut u, CHROMA_LPF);
            boxcar(&mut v, CHROMA_LPF);
        } else {
            u.iter_mut().for_each(|x| *x = 0.0);
            v.iter_mut().for_each(|x| *x = 0.0);
        }
        // --- 2) 輝度は「帯域制限した U,V を再変調して引いた残り」 ---
        //
        // ★**コムでもノッチでも駄目だった。** どちらも1本の線を3本に広げる:
        //
        //     Y の作り方        縦線への水平応答   横棒への垂直応答
        //     x - C_comb        100% の1本  ○     25%/50%/25%  ×
        //     x - C_notch       25%/50%/25% ×     100% の1本   ○
        //     x - Ĉ (これ)      100% の1本  ○     100% の1本   ○
        //
        //   実機で最初に漢字の横棒が二重に見え、ノッチにしたら今度は鼻の縦線が
        //   二重になった。**artefact を垂直から水平へ付け替えただけだった。**
        //
        // C = a·cos(ψ) + b·sin(ψ) と書けるとき、復調とLPFで a = -u, b = v が出る。
        // 同じ基底で再変調すれば、実際に色として使う帯域制限されたクロマだけを引ける:
        //     Ĉ = -u·cos(ψ) + v·sin(ψ)      Y = x - Ĉ
        //
        // 素通しになる理由:
        //   - 垂直detailの無い縦線は C_comb = 0 なので Ĉ = 0 → Y = x
        //   - fsc成分の無い横棒は 復調+LPF で u,v ≈ 0 → Ĉ ≈ 0 → Y = x
        // つまり**「色として取り出した分だけ」を引く**ので、余計な広がりが出ない。
        // ★S端子では **何も引かない**。Y が最初から独立に来ているので、
        //   コムもノッチも再変調も要らず、輝度は送出されたまま素通しになる。
        //   (クロスカラーもドットクロールも原理的に発生しない)
        if svideo {
            for n in 0..w {
                yl[n] = px(y, n);
            }
        } else {
            for n in 0..w {
                let (cos_psi, sin_psi) = psi(n);
                yl[n] = px(y, n) - (-u[n] * cos_psi + v[n] * sin_psi);
            }
        }
        // --- 3) YUV → RGB ---
        let o0 = y * w * 4;
        for n in 0..w {
            // コントラストは輝度と色差の両方へ、彩度は色差だけへ掛ける
            let yy = (yl[n] - porch) * inv_100ire * adj.contrast
                + adj.brightness * 0.01;
            let cgain = inv_100ire_c * adj.contrast * adj.saturation;
            let b_y = u[n] * cgain / 0.493;
            let r_y = v[n] * cgain / 0.877;
            let r = yy + r_y;
            let g = yy - 0.5094 * r_y - 0.1942 * b_y;
            let b = yy + b_y;
            let o = o0 + n * 4;
            fb[o] = to8(r);
            fb[o + 1] = to8(g);
            fb[o + 2] = to8(b);
        }
    }
    Info {
        lines_locked: locked,
        comb_step,
        phase_delta_deg: phase_delta,
        code_per_ire,
        lines_3d,
        motion_frac: if n3 > 0 { moving as f32 / n3 as f32 } else { 0.0 },
        svideo,
        phase_drift_deg,
    }
}

#[inline]
fn to8(v: f32) -> u8 {
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// 移動平均(その場書き換え)。窓の外は端の値で延長する。
///
/// ★**中心を合わせること。そして滑らせる更新で足し引きを取り違えないこと。**
/// 最初の実装は初期の窓が n/2 ずれていて、更新も既に窓に入っている要素を
/// 足していた。結果として 2fsc が消えず、**平坦な色面に周期4サンプルの縞**が
/// 出た(実機の赤ベタで「赤黒赤黒」に見えた)。
///
/// この関数は「2fsc をきっちり消す」ために存在する。窓長が副搬送波1周期(8)の
/// 倍数なら 2fsc は整数周期ぶん入って完全に消えるはずで、消えないなら実装が
/// 壊れている。回帰試験 `boxcar_nulls_2fsc` がそれを見る。
fn boxcar(x: &mut [f32], n: usize) {
    if n <= 1 || x.len() < 2 {
        return;
    }
    let src: Vec<f32> = x.to_vec();
    let len = src.len() as isize;
    let at = |i: isize| src[i.clamp(0, len - 1) as usize];
    let half = (n / 2) as isize;
    let inv = 1.0 / n as f32;
    // i=0 のときの窓 [-half, -half+n-1] から始める(中心が i に来る)
    let mut sum: f32 = (0..n as isize).map(|k| at(-half + k)).sum();
    for i in 0..len {
        x[i as usize] = sum * inv;
        // 窓を1つ右へ: 新しく入る要素を足し、出る要素を引く
        sum += at(i - half + n as isize) - at(i - half);
    }
}

/// cos(2πk/8) / sin(2πk/8) の8点。8fsc なのでこれしか出てこない
const R2: f32 = std::f32::consts::FRAC_1_SQRT_2;
const COS8: [f32; 8] = [1.0, R2, 0.0, -R2, -1.0, -R2, 0.0, R2];
const SIN8: [f32; 8] = [0.0, R2, 1.0, R2, 0.0, -R2, -1.0, -R2];

#[cfg(test)]
mod tests {
    use super::*;

    /// 既知の色から合成したNTSCラインを復調して、色相が戻るか。
    ///
    /// **必ず複数の色を通す。** 1色だと V 軸の符号の誤りが「色相オフセット」に
    /// 化けて見え、通ってしまう(Python側で実際に踏んだ)。回転は色と色の
    /// 「間の関係」を変えないので、2色以上あれば回転では消せない誤りとして出る。
    fn synth(colors: &[(f32, f32, f32)], w: usize, h: usize, sps: f32,
             step: usize) -> (Vec<u8>, Vec<bool>) {
        let (ba, _) = win(BURST_US, sps, w);
        let cpi = 0.78f32;
        let porch = 158.0f32;
        let mut raw = vec![0u8; w * h * 2];
        let mut filled = vec![false; h];
        let sync_end = (4.7e-6 * sps) as usize;
        let (bs, be) = win(BURST_US, sps, w);
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();
        for y in (0..h).step_by(step) {
            filled[y] = true;
            // 時間的に隣の行(=step行おき)ごとに180°反転させる
            let flip = std::f32::consts::PI * (y / step) as f32;
            for n in 0..w {
                let psi = 2.0 * std::f32::consts::PI * (n as f32 - ba as f32) / 8.0 - flip;
                let mut val = porch;
                if n < sync_end {
                    val = porch - 40.0 * cpi;
                } else if n >= bs && n < be {
                    val = porch + 20.0 * cpi * psi.cos();
                } else if n >= aa {
                    let ci = ((n - aa) / per).min(colors.len() - 1);
                    let (r, g, b) = colors[ci];
                    let yy = 0.299 * r + 0.587 * g + 0.114 * b;
                    let uu = 0.493 * (b - yy);
                    let vv = 0.877 * (r - yy);
                    let c = (-uu * psi.cos() + vv * psi.sin()) * 0.5;
                    val = porch + (yy * 100.0 + c * 100.0) * cpi;
                }
                raw[(y * w + n) * 2] = val.clamp(0.0, 255.0) as u8;
            }
        }
        (raw, filled)
    }

    /// S端子の Y/C を別々に合成する。
    ///
    /// Y(byte0)には同期と輝度だけ、C(byte1)にはバーストとクロマだけ。
    /// **これが S端子の本質**で、コムが要らないのは Y と C が最初から別だから。
    /// `c_gain` は赤chの粗ゲイン違いを模す(バースト基準の校正の試験に使う)。
    fn synth_svideo(colors: &[(f32, f32, f32)], w: usize, h: usize, sps: f32,
                    c_gain: f32) -> (Vec<u8>, Vec<bool>) {
        let (ba, _) = win(BURST_US, sps, w);
        let (cpi, porch) = (0.78f32, 158.0f32);
        let sync_end = (4.7e-6 * sps) as usize;
        let (bs, be) = win(BURST_US, sps, w);
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();
        let mut raw = vec![0u8; w * h * 2];
        let filled = vec![true; h];
        for y in 0..h {
            let flip = std::f32::consts::PI * y as f32;
            for n in 0..w {
                let psi = 2.0 * std::f32::consts::PI * (n as f32 - ba as f32) / 8.0 - flip;
                let (mut yv, mut cv) = (porch, porch);   // C はミッドレベルクランプ
                if n < sync_end {
                    yv = porch - 40.0 * cpi;             // 同期は Y 側だけ
                } else if n >= bs && n < be {
                    cv = porch + c_gain * 20.0 * cpi * psi.cos();
                } else if n >= aa {
                    let ci = ((n - aa) / per).min(colors.len() - 1);
                    let (r, g, b) = colors[ci];
                    let yy = 0.299 * r + 0.587 * g + 0.114 * b;
                    let uu = 0.493 * (b - yy);
                    let vv = 0.877 * (r - yy);
                    let c = (-uu * psi.cos() + vv * psi.sin()) * 0.5;
                    yv = porch + yy * 100.0 * cpi;
                    cv = porch + c_gain * c * 100.0 * cpi;
                }
                raw[(y * w + n) * 2] = yv.clamp(0.0, 255.0) as u8;
                raw[(y * w + n) * 2 + 1] = cv.clamp(0.0, 255.0) as u8;
            }
        }
        (raw, filled)
    }

    /// 見た目の調整が**期待どおりの向きと量で効くこと**。
    ///
    /// ★調整は復調の校正を触らないので、既定値では**1コードも変わらない**のが要点。
    ///   ここが崩れると「調整を戻したのに絵が違う」という追えない状態になる。
    #[test]
    fn adjust_moves_the_picture_in_the_expected_direction() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let colors = [(0.75, 0.0, 0.0), (0.0, 0.75, 0.0), (0.0, 0.0, 0.75),
                      (0.75, 0.75, 0.0), (0.0, 0.75, 0.75), (0.75, 0.0, 0.75)];
        let (raw, filled) = synth(&colors, w, h, sps, 1);
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();
        let dec = |a: Adjust| {
            let mut fb = vec![255u8; w * h * 4];
            decode_field(&raw, w, h, &filled, sps as u32, &mut fb, None, a);
            fb
        };
        let base = dec(Adjust::default());

        // 既定値は素通し。**1バイトも変わらないこと**
        assert_eq!(base, dec(Adjust { ..Default::default() }),
                   "既定値なのに絵が変わった");

        // 彩度。無彩色にすると色差が消えてR=G=Bになる
        let flat = dec(Adjust { saturation: 0.0, ..Default::default() });
        let y = 12usize;
        let n = aa + per / 2;
        let o = (y * w + n) * 4;
        let (r, g, b) = (flat[o] as i32, flat[o + 1] as i32, flat[o + 2] as i32);
        assert!((r - g).abs() <= 2 && (g - b).abs() <= 2,
                "彩度0なのに無彩色にならない: {r},{g},{b}");

        // 明るさ。+20 IRE で全体が上がる(飽和していない所で)
        let br = dec(Adjust { brightness: 20.0, ..Default::default() });
        let lum = |fb: &[u8], n: usize| {
            let o = (y * w + n) * 4;
            0.299 * fb[o] as f32 + 0.587 * fb[o + 1] as f32 + 0.114 * fb[o + 2] as f32
        };
        let d = lum(&br, n) - lum(&base, n);
        assert!(d > 30.0 && d < 70.0,
                "明るさ+20 IRE の差が {d:.1}(期待 0.20×255=51 前後)");

        // 色相。**回転量そのものは一致しない。** NTSCの副搬送波位相と HSV の色相は
        // 一様に対応しないので、位相を30°回してもHSVでは色によって違う角度になる
        // (実測: 赤で42°)。見るべきは「全色が同じ向きに回る」ことと
        // 「hue_deg に比例する」こと。
        let h15 = dec(Adjust { hue_deg: 15.0, ..Default::default() });
        let h30 = dec(Adjust { hue_deg: 30.0, ..Default::default() });
        let rot = |fb: &[u8], i: usize| {
            let x0 = aa + i * per + per / 4;
            let x1 = aa + i * per + per * 3 / 4;
            ang_diff(hue_of(fb, w, y, x0, x1), hue_of(&base, w, y, x0, x1))
        };
        let s0 = rot(&h30, 0).signum();
        for i in 0..colors.len() {
            let (a, b) = (rot(&h15, i), rot(&h30, i));
            assert!(b.signum() == s0 && a.signum() == s0,
                    "色{i} だけ回る向きが違う: 15°で{a:.1}° 30°で{b:.1}°");
            assert!(a.abs() > 5.0, "色{i} が15°でほとんど回らない: {a:.1}°");
            let r = b / a;
            assert!(r > 1.6 && r < 2.4,
                    "色{i} が hue_deg に比例しない: 15°で{a:.1}° 30°で{b:.1}°(比{r:.2})");
        }
    }

    /// **S端子はコムを使わない。** 赤chのバーストで自動判定し、Yは素通しにする。
    ///
    /// 実測(PS2、2026-08-15): コンポジットでは細い縦罫線が二重になり、オシロで
    /// AC結合前を見ると PS2 の出力の時点で谷底に段があった。同じラインを S端子の
    /// Y で見ると単一の深い谷。**送出側のコンポジット輝度処理が原因**なので、
    /// S端子にすれば消える。
    #[test]
    fn svideo_uses_c_channel_and_passes_luma_through() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let colors = [(0.75, 0.0, 0.0), (0.0, 0.75, 0.0), (0.0, 0.0, 0.75),
                      (0.75, 0.75, 0.0), (0.0, 0.75, 0.75), (0.75, 0.0, 0.75)];
        let want = [0.0f32, 120.0, 240.0, 60.0, 180.0, 300.0];
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();

        let (raw, filled) = synth_svideo(&colors, w, h, sps, 1.0);
        let mut fb = vec![255u8; w * h * 4];
        let info = decode_field(&raw, w, h, &filled, sps as u32, &mut fb, None, Adjust::default());
        assert!(info.svideo, "赤chのバーストからS端子と判定できていない");
        let mut worst = 0.0f32;
        for i in 0..colors.len() {
            let hh = hue_of(&fb, w, 12, aa + i * per + per / 4, aa + i * per + per * 3 / 4);
            worst = worst.max(ang_diff(hh, want[i]).abs());
        }
        assert!(worst < 8.0, "S端子の色相誤差 {worst:.1}°");

        // ★負の対照。C側が無信号(コンポジット配線)なら誤判定しないこと
        let (mut cvbs, f2) = synth(&colors, w, h, sps, 1);
        for n in 0..w * h {
            cvbs[n * 2 + 1] = 158;
        }
        let mut fb2 = vec![255u8; w * h * 4];
        let i2 = decode_field(&cvbs, w, h, &f2, sps as u32, &mut fb2, None, Adjust::default());
        assert!(!i2.svideo, "C側が無信号なのにS端子と判定した");

        // ★赤chの粗ゲインが違っても同じ絵になること(バースト基準で校正している)
        let (raw_g, fg) = synth_svideo(&colors, w, h, sps, 0.5);
        let mut fb3 = vec![255u8; w * h * 4];
        decode_field(&raw_g, w, h, &fg, sps as u32, &mut fb3, None, Adjust::default());
        let d = fb.iter().zip(fb3.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
        assert!(d <= 12, "C側のゲインが半分で絵が変わった: 最大差 {d}");

        // ★輝度が素通しであること。1サンプルの山が隣へ漏れない
        let (mut raw_i, fi) = synth_svideo(&colors, w, h, sps, 1.0);
        let tgt = aa + 400;
        for y in 0..h {
            let o = (y * w + tgt) * 2;
            raw_i[o] = (raw_i[o] as u16 + 60).min(255) as u8;
        }
        let mut fb4 = vec![255u8; w * h * 4];
        decode_field(&raw_i, w, h, &fi, sps as u32, &mut fb4, None, Adjust::default());
        let lum = |n: usize| {
            let o = (12 * w + n) * 4;
            0.299 * fb4[o] as f32 + 0.587 * fb4[o + 1] as f32 + 0.114 * fb4[o + 2] as f32
        };
        let base = lum(tgt - 20);
        let pk = lum(tgt) - base;
        let side = [tgt - 1, tgt + 1, tgt - 4, tgt + 4]
            .iter().map(|&n| (lum(n) - base).abs()).fold(0.0f32, f32::max);
        assert!(pk > 20.0 && side / pk < 0.05,
                "輝度が素通しでない: 山 {pk:.1} 隣への漏れ {:.0}%", 100.0 * side / pk);
    }

    fn hue_of(fb: &[u8], w: usize, y: usize, x0: usize, x1: usize) -> f32 {
        let (mut r, mut g, mut b) = (0.0f32, 0.0, 0.0);
        for n in x0..x1 {
            let o = (y * w + n) * 4;
            r += fb[o] as f32;
            g += fb[o + 1] as f32;
            b += fb[o + 2] as f32;
        }
        let k = (x1 - x0) as f32;
        let (r, g, b) = (r / k / 255.0, g / k / 255.0, b / k / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        if d < 1e-6 {
            return -1.0;
        }
        let h = if max == r {
            60.0 * (((g - b) / d) % 6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        (h + 360.0) % 360.0
    }

    /// **1本の線が1本のまま出ること。水平と垂直の両方を見る。**
    ///
    /// 輝度の作り方を2回間違えた。どちらも「1本を3本に広げる」形で出た:
    ///
    ///     Y = x - C_comb    横棒が 25%/50%/25% に広がる(垂直)
    ///     Y = x - C_notch   縦線が 25%/50%/25% に広がる(水平)
    ///
    /// 実機では「漢字の横棒が二重」→(ノッチに変更)→「鼻の縦線が二重」と
    /// **artefact が付け替わっただけ**だった。片方向だけ見る試験では防げない。
    fn impulse_response(vertical: bool) -> (f32, f32) {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let (ba, _) = win(BURST_US, sps, w);
        let (bs, be) = win(BURST_US, sps, w);
        let (porch, cpi) = (158.0f32, 0.78f32);
        let sync_end = (4.7e-6 * sps) as usize;
        let aa = (9.6e-6 * sps) as usize;
        let (ty, tx) = (h / 2, aa + 400);
        let mut raw = vec![0u8; w * h * 2];
        let filled = vec![true; h];
        for y in 0..h {
            let flip = std::f32::consts::PI * y as f32;
            for n in 0..w {
                let psi = 2.0 * std::f32::consts::PI * (n as f32 - ba as f32) / 8.0 - flip;
                let mut v = porch;
                if n < sync_end {
                    v = porch - 40.0 * cpi;
                } else if n >= bs && n < be {
                    v = porch + 20.0 * cpi * psi.cos();
                } else if vertical {
                    if n >= aa && y == ty { v = porch + 80.0 * cpi; }   // 横棒
                } else if n == tx {
                    v = porch + 80.0 * cpi;                             // 縦線
                }
                raw[(y * w + n) * 2] = v.clamp(0.0, 255.0) as u8;
            }
        }
        let mut fb = vec![255u8; w * h * 4];
        decode_field(&raw, w, h, &filled, sps as u32, &mut fb, None, Adjust::default());
        // 輝度の代表として G を見る
        let at = |y: usize, n: usize| fb[(y * w + n) * 4 + 1] as f32;
        let (peak, base, side) = if vertical {
            let n = aa + 400;
            (at(ty, n), at(ty + 4, n),
             at(ty - 1, n).max(at(ty + 1, n)))
        } else {
            (at(ty, tx), at(ty, tx + 40),
             // ノッチは n±4 へ漏らすので、そこも見る
             at(ty, tx - 1).max(at(ty, tx + 1))
                 .max(at(ty, tx - 4)).max(at(ty, tx + 4)))
        };
        ((peak - base).abs(), (side - base).abs())
    }

    #[test]
    fn luma_keeps_vertical_resolution() {
        let (peak, side) = impulse_response(true);
        assert!(peak > 20.0, "横棒が暗すぎる: {peak:.0}");
        assert!(side < peak * 0.25,
                "横棒が上下へ漏れている(垂直解像度が落ちている): \
                 山{peak:.0} 隣{side:.0} = {:.0}%", 100.0 * side / peak);
    }

    /// ★こちらが「鼻の縦線が二重に見える」を捕まえる試験。
    #[test]
    fn luma_keeps_horizontal_resolution() {
        let (peak, side) = impulse_response(false);
        assert!(peak > 20.0, "縦線が暗すぎる: {peak:.0}");
        assert!(side < peak * 0.25,
                "縦線が左右へ漏れている(水平解像度が落ちている): \
                 山{peak:.0} 隣{side:.0} = {:.0}%", 100.0 * side / peak);
    }

    /// **クロマLPFが 2fsc をきっちり消すこと。**
    ///
    /// 直交復調の積には必ず 2fsc(周期4サンプル)が出る。窓長が副搬送波1周期(8)の
    /// 倍数なら整数周期ぶん入って完全に消える — はずだった。最初の実装は窓の中心が
    /// n/2 ずれていて、滑らせる更新で既に窓にある要素を足していたため 2fsc が残り、
    /// **平坦な色面に周期4サンプルの縞**が出た(実機の赤ベタで「赤黒赤黒」に見えた)。
    ///
    /// 見た目でしか分からない不具合だったので、数値で押さえる。
    #[test]
    fn boxcar_nulls_2fsc() {
        let n = 1024;
        // 2fsc = 周期4サンプル。DCを乗せて「平均は保つ」ことも一緒に見る
        let mut x: Vec<f32> = (0..n)
            .map(|i| 10.0 + (std::f32::consts::PI * 0.5 * i as f32).cos())
            .collect();
        boxcar(&mut x, 16);
        // 端は窓が延長で埋まるので中央だけ見る
        let mid = &x[64..n - 64];
        let ripple = mid.iter().fold(0.0f32, |a, v| a.max((v - 10.0).abs()));
        assert!(ripple < 1e-3, "2fscが残っている: 振幅 {ripple:.4} (元は1.0)");

        // 副搬送波そのもの(周期8)も窓長16なら消える
        let mut y: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::PI * 0.25 * i as f32).sin())
            .collect();
        boxcar(&mut y, 16);
        let r2 = y[64..n - 64].iter().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!(r2 < 1e-3, "fscが残っている: 振幅 {r2:.4}");

        // 位相がずれていないこと。ステップ応答が段の位置 c を中心に対称になる
        // (s[c-k] + s[c+k] = 1)。窓が n/2 ずれていると崩れる。
        // 偶数長の窓は中心が半サンプルずれるが、**c を挟んだ対称性は保たれる**
        // (窓 [i-8, i+7] で s[c]=0.5 / s[c-1]=7/16 / s[c+1]=9/16)。
        let mut s: Vec<f32> = (0..n).map(|i| if i < n / 2 { 0.0 } else { 1.0 }).collect();
        boxcar(&mut s, 16);
        let c = n / 2;
        assert!((s[c] - 0.5).abs() < 0.02, "段の位置で0.5にならない: {:.3}", s[c]);
        for k in 1..7 {
            let (a, b) = (s[c - k], s[c + k]);
            assert!((a + b - 1.0).abs() < 0.02,
                    "ステップ応答が非対称(中心がずれている): k={k} {a:.3}+{b:.3}");
        }
    }

    /// **3次元(動き適応フレームコム)**。
    ///
    /// 2次元コムは「上下のラインの色が同じ」を前提にしている。**行ごとに色が
    /// 交互する模様**はその前提を壊すので原理的に失敗する。3次元は同じライン番号の
    /// 1 NTSCフレーム前(位相180°)と引くので、静止していれば垂直方向を見ない。
    ///
    /// 履歴の位相は実測済み: 2ボードフレーム差 175.8° / 4ボードフレーム差 4.2°。
    fn synth_alt(colors: &[(f32, f32, f32)], w: usize, h: usize, sps: f32,
                 phase_off: f32, lum_shift: f32) -> Vec<u8> {
        let (ba, _) = win(BURST_US, sps, w);
        let (bs, be) = win(BURST_US, sps, w);
        let (porch, cpi) = (158.0f32, 0.78f32);
        let sync_end = (4.7e-6 * sps) as usize;
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();
        let mut raw = vec![0u8; w * h * 2];
        for y in 0..h {
            let flip = std::f32::consts::PI * y as f32 + phase_off;
            for n in 0..w {
                let psi = 2.0 * std::f32::consts::PI * (n as f32 - ba as f32) / 8.0 - flip;
                let mut val = porch;
                if n < sync_end {
                    val = porch - 40.0 * cpi;
                } else if n >= bs && n < be {
                    val = porch + 20.0 * cpi * psi.cos();
                } else if n >= aa {
                    let ci = ((n - aa) / per).min(colors.len() - 1);
                    // 行ごとに色をずらす = 垂直方向に色が交互になる
                    let (r, g, b) = colors[(ci + y) % colors.len()];
                    let yy = 0.299 * r + 0.587 * g + 0.114 * b;
                    let uu = 0.493 * (b - yy);
                    let vv = 0.877 * (r - yy);
                    let c = (-uu * psi.cos() + vv * psi.sin()) * 0.5;
                    val = porch + (yy * 100.0 + c * 100.0) * cpi + lum_shift;
                }
                raw[(y * w + n) * 2] = val.clamp(0.0, 255.0) as u8;
            }
        }
        raw
    }

    /// 副搬送波1周期(8サンプル)ぶんの振幅を測る。**輝度に残った副搬送波**の量。
    fn fsc_ripple(fb: &[u8], w: usize, y: usize, x0: usize, x1: usize) -> f32 {
        let v: Vec<f32> = (x0..x1).map(|n| fb[(y * w + n) * 4] as f32).collect();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let (mut c, mut s) = (0.0f32, 0.0f32);
        for (i, val) in v.iter().enumerate() {
            let p = 2.0 * std::f32::consts::PI * i as f32 / 8.0;
            c += (val - mean) * p.cos();
            s += (val - mean) * p.sin();
        }
        2.0 * (c * c + s * s).sqrt() / v.len() as f32
    }

    /// **フレームコムは副搬送波の位相ドリフトに耐えること。**
    ///
    /// DATACLK は HSYNC にロックしていて副搬送波にはロックしていないので、
    /// 1フレーム前との位相は実測で |ε| 中央値 4.8°ずれる(行方向に滑らかに
    /// ±15°を揺れる本物のドリフト)。補正が無いと (x+p2)/2 に C·sin(ε/2) が
    /// 残り、**フレームごとに符号が反転するドットクロール**になる。実機で
    /// 「赤黒赤黒」と「黒赤黒赤」がフレームごとに入れ替わって見えた症状がこれ。
    #[test]
    fn frame_comb_survives_subcarrier_phase_drift() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let colors = [(0.75, 0.0, 0.0); 6];   // 一様な赤 = 本来まったく平坦
        let cur = synth_alt(&colors, w, h, sps, 0.0, 0.0);
        let p4 = synth_alt(&colors, w, h, sps, 0.0, 0.0);
        let filled = vec![true; h];
        let hn = vec![3u8; h];
        let (aa, _) = win((9.6, 62.0), sps, w);
        let (x0, x1) = (aa + 200, aa + 600);

        let run = |eps_deg: f32| {
            let p2 = synth_alt(&colors, w, h, sps,
                               std::f32::consts::PI + eps_deg.to_radians(), 0.0);
            let mut fb = vec![255u8; w * h * 4];
            let info = decode_field(&cur, w, h, &filled, sps as u32, &mut fb,
                                    Some(History { p2: &p2, p4: &p4, hist_n: &hn }),
                                    Adjust::default());
            (fsc_ripple(&fb, w, 12, x0, x1), info.phase_drift_deg)
        };
        let (r0, d0) = run(0.0);
        let (r6, d6) = run(6.0);
        // ε を測れていること(測れなければ補正のしようがない)
        assert!(d0 < 1.0, "ズレが無いのに ε={d0:.1}° と測った");
        assert!((d6 - 6.0).abs() < 1.0, "ε を 6° と測れていない: {d6:.1}°");
        // 補正が効いていること。**補正を消すと 0.15 → 2.84 コードに増える**ことを
        // 確認済み(2026-08-15)。ここが緩いと回帰を素通しする。
        assert!(r6 < r0 + 1.0,
                "位相が6°ずれると輝度の副搬送波が増える: {r0:.2} → {r6:.2} コード");
    }

    #[test]
    fn comb3d_fixes_vertical_chroma_detail() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let colors = [(0.75, 0.0, 0.0), (0.0, 0.75, 0.0), (0.0, 0.0, 0.75),
                      (0.75, 0.75, 0.0), (0.0, 0.75, 0.75), (0.75, 0.0, 0.75)];
        let want = [0.0f32, 120.0, 240.0, 60.0, 180.0, 300.0];
        let cur = synth_alt(&colors, w, h, sps, 0.0, 0.0);
        // prev2 は1 NTSCフレーム前 → 位相180° / prev4 は2フレーム前 → 位相0°
        let p2 = synth_alt(&colors, w, h, sps, std::f32::consts::PI, 0.0);
        let p4 = synth_alt(&colors, w, h, sps, 0.0, 0.0);
        let filled = vec![true; h];
        let hn = vec![3u8; h];
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();
        let y = 12usize;

        let worst = |fb: &[u8]| {
            let mut m = 0.0f32;
            for i in 0..colors.len() {
                let h0 = hue_of(fb, w, y, aa + i * per + per / 4, aa + i * per + per * 3 / 4);
                m = m.max(ang_diff(h0, want[(i + y) % colors.len()]).abs());
            }
            m
        };
        let mut fb2 = vec![255u8; w * h * 4];
        decode_field(&cur, w, h, &filled, sps as u32, &mut fb2, None, Adjust::default());
        let e2 = worst(&fb2);
        let mut fb3 = vec![255u8; w * h * 4];
        let i3 = decode_field(&cur, w, h, &filled, sps as u32, &mut fb3,
                              Some(History { p2: &p2, p4: &p4, hist_n: &hn }), Adjust::default());
        let e3 = worst(&fb3);
        assert_eq!(i3.lines_3d, h as u32, "3次元を使えた行数が足りない");
        assert!(i3.motion_frac < 0.02, "静止なのに動きと判定した: {:.1}%",
                100.0 * i3.motion_frac);
        assert!(e2 > 20.0,
                "2次元コムでも誤差 {e2:.1}° しか出ない = 試験になっていない");
        assert!(e3 < 8.0, "3次元でも誤差 {e3:.1}°(2次元は {e2:.1}°)");

        // 動いている所は2次元へ落ちること(輝度を大きくずらして「動き」を作る)
        let p2m = synth_alt(&colors, w, h, sps, std::f32::consts::PI, 40.0);
        let p4m = synth_alt(&colors, w, h, sps, 0.0, 40.0);
        let mut fbm = vec![255u8; w * h * 4];
        let im = decode_field(&cur, w, h, &filled, sps as u32, &mut fbm,
                              Some(History { p2: &p2m, p4: &p4m, hist_n: &hn }), Adjust::default());
        assert!(im.motion_frac > 0.8, "動きを検出できていない: {:.1}%",
                100.0 * im.motion_frac);
    }

    /// 実寸(1820×526、1フィールド263行)での所要時間を測る。
    ///
    /// 常時走らせる試験ではない(機械の速さに依存するので落ちる)。
    ///     cargo test --release -- --ignored --nocapture ntsc::tests::timing
    #[test]
    #[ignore]
    fn timing() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 526usize);
        let colors = [(0.75, 0.0, 0.0), (0.0, 0.75, 0.0), (0.0, 0.0, 0.75)];
        let (raw, filled) = synth(&colors, w, h, sps, 2);
        let mut fb = vec![255u8; w * h * 4];
        let p2 = raw.clone();
        let p4 = raw.clone();
        let hn = vec![3u8; h];
        let samples = (w * h / 2) as f64;      // 1フィールドで埋まるのは半分の行
        for (tag, use3) in [("2次元", false), ("3次元", true)] {
            let mk = || if use3 {
                Some(History { p2: &p2, p4: &p4, hist_n: &hn })
            } else { None };
            decode_field(&raw, w, h, &filled, sps as u32, &mut fb, mk(),
                         Adjust::default());  // warm-up
            let n = 120;
            let t0 = std::time::Instant::now();
            for _ in 0..n {
                decode_field(&raw, w, h, &filled, sps as u32, &mut fb, mk(),
                         Adjust::default());
            }
            let per = t0.elapsed().as_secs_f64() / n as f64;
            println!("{tag}: 1フィールド {:.3} ms  ({:.1} MSa/s 相当)  \
                      59.94フィールド/秒なら1コアの {:.1}%",
                     per * 1e3, samples / per / 1e6, per * 59.94 * 100.0);
        }
    }

    /// 6色の色相が真値に戻り、コムのペアも自力で当てられること
    fn run_case(step: usize) {
        let sps = 8.0 * 3_579_545.0f32;
        let w = 1820;
        let h = 48;
        let colors = [(0.75, 0.0, 0.0), (0.0, 0.75, 0.0), (0.0, 0.0, 0.75),
                      (0.75, 0.75, 0.0), (0.0, 0.75, 0.75), (0.75, 0.0, 0.75)];
        let want = [0.0f32, 120.0, 240.0, 60.0, 180.0, 300.0];
        let (raw, filled) = synth(&colors, w, h, sps, step);
        let mut fb = vec![255u8; w * h * 4];
        let info = decode_field(&raw, w, h, &filled, sps as u32, &mut fb, None, Adjust::default());
        assert_eq!(info.comb_step, step,
                   "コムのペアを自力で当てられていない (測定した位相差 {:.1}°)",
                   info.phase_delta_deg);
        assert!((info.phase_delta_deg - 180.0).abs() < 3.0,
                "位相差 {:.1}°", info.phase_delta_deg);
        assert!((info.code_per_ire - 0.78).abs() < 0.05,
                "1 IRE = {:.3} コード", info.code_per_ire);
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();
        // 端はクロマLPFの過渡が乗るので中央だけ見る
        let y = step * 4;
        for (i, wnt) in want.iter().enumerate() {
            let x0 = aa + i * per + per / 4;
            let x1 = aa + i * per + per * 3 / 4;
            let got = hue_of(&fb, w, y, x0, x1);
            let e = ang_diff(got, *wnt).abs();
            assert!(e < 8.0, "色{i}: 色相 {got:.1}° 期待 {wnt:.1}° 誤差 {e:.1}°");
        }
    }

    #[test]
    fn decodes_six_hues_step1() {
        run_case(1);
    }

    /// ★1フィールドだけ来ている状態(奇数行だけ埋まる)でも当てられること。
    /// ここを決め打ちにしていると、織り込み設定が変わった瞬間に色が消える。
    #[test]
    fn decodes_six_hues_step2() {
        run_case(2);
    }

    /// V軸の符号が逆だと、どう色相を回しても6色は同時に合わない。
    /// この試験自体が効いていることの確認(常にPASSする試験になっていないか)。
    #[test]
    fn wrong_v_sign_cannot_be_fixed_by_rotation() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 48usize);
        let colors = [(0.75, 0.0, 0.0), (0.0, 0.75, 0.0), (0.0, 0.0, 0.75)];
        let want = [0.0f32, 120.0, 240.0];
        // V の符号を逆にした信号を作る(= 実機で踏んだ誤りの再現)
        let (mut raw, filled) = synth(&colors, w, h, sps, 1);
        {
            // 作り直す方が簡単なので、色差の V だけ反転した版で上書きする
            let (ba, _) = win(BURST_US, sps, w);
            let (aa, _) = win((9.6, 62.0), sps, w);
            let per = (w - aa) / colors.len();
            let (cpi, porch) = (0.78f32, 158.0f32);
            for y in 0..h {
                let flip = std::f32::consts::PI * y as f32;
                for n in aa..w {
                    let psi = 2.0 * std::f32::consts::PI * (n as f32 - ba as f32) / 8.0 - flip;
                    let ci = ((n - aa) / per).min(colors.len() - 1);
                    let (r, g, b) = colors[ci];
                    let yy = 0.299 * r + 0.587 * g + 0.114 * b;
                    let uu = 0.493 * (b - yy);
                    let vv = 0.877 * (r - yy);
                    let c = (-uu * psi.cos() - vv * psi.sin()) * 0.5; // ← V反転
                    let val = porch + (yy * 100.0 + c * 100.0) * cpi;
                    raw[(y * w + n) * 2] = val.clamp(0.0, 255.0) as u8;
                }
            }
        }
        let mut fb = vec![255u8; w * h * 4];
        decode_field(&raw, w, h, &filled, sps as u32, &mut fb, None, Adjust::default());
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();
        let mut best = f32::MAX;
        for rot in (0..360).step_by(5) {
            let mut worst = 0.0f32;
            for (i, wnt) in want.iter().enumerate() {
                let x0 = aa + i * per + per / 4;
                let x1 = aa + i * per + per * 3 / 4;
                let got = hue_of(&fb, w, 4, x0, x1) + rot as f32;
                worst = worst.max(ang_diff(got, *wnt).abs());
            }
            best = best.min(worst);
        }
        assert!(best > 20.0,
                "符号が逆でも回転で合ってしまう(最良で最大誤差 {best:.1}°)= 試験が無意味");
    }
}
