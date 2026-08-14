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

/// バーストが取れたと判定する相関の下限。これ未満の行は無彩色にする。
///
/// ★真っ黒な領域では相関が雑音になり、**色相が乱数になる**(実測: 彩度0.03〜0.09の
///   行で色相が230〜300°をふらついた)。無彩色に倒す方が絵として正しい。
const BURST_MIN: f32 = 60.0;

pub struct Info {
    pub lines_locked: u32,
    pub comb_step: usize,
    pub phase_delta_deg: f32,
    pub code_per_ire: f32,
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
pub fn decode_field(
    raw: &[u8],
    w: usize,
    h: usize,
    filled: &[bool],
    dotclk_hz: u32,
    fb: &mut [u8],
) -> Info {
    let sps = dotclk_hz as f32;
    let (ba, bb) = win(BURST_US, sps, w);
    if sps <= 0.0 || bb <= ba + 8 || w < 16 {
        return Info { lines_locked: 0, comb_step: 0, phase_delta_deg: 0.0,
                      code_per_ire: 0.0 };
    }

    // --- 1. ラインごとのバースト位相 ---
    //
    // バースト区間の先頭 ba を基準に、バーストが A·cos(2π(n-ba)/8 - φ) と
    // 表せる φ を求める。**ライン毎に測るのが要点。** ライン番号のパリティから
    // 予測すると、行が1本落ちただけで以降の色が全部反転する。
    let mut cosp = vec![0.0f32; h];
    let mut sinp = vec![0.0f32; h];
    let mut mag = vec![0.0f32; h];
    let mut phase = vec![0.0f32; h];
    for y in 0..h {
        if !filled.get(y).copied().unwrap_or(false) {
            continue;
        }
        let row = &raw[y * w * 2..(y + 1) * w * 2];
        let mut mean = 0.0f32;
        for n in ba..bb {
            mean += row[n * 2] as f32;
        }
        mean /= (bb - ba) as f32;
        let (mut ci, mut si) = (0.0f32, 0.0f32);
        for n in ba..bb {
            let v = row[n * 2] as f32 - mean;
            let k = (n - ba) & 7;
            ci += v * COS8[k];
            si += v * SIN8[k];
        }
        mag[y] = (ci * ci + si * si).sqrt();
        phase[y] = si.atan2(ci);
        // 復調で使うのは φ の cos/sin だけなので、正規化して持つ
        let m = mag[y].max(1e-6);
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
                      code_per_ire: 0.0 };
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

    // --- 4. コム → 直交復調 → RGB ---
    let mut u = vec![0.0f32; w];
    let mut v = vec![0.0f32; w];
    let mut yl = vec![0.0f32; w];
    let mut locked = 0u32;
    for y in 0..h {
        if !filled.get(y).copied().unwrap_or(false) {
            continue;
        }
        let row = &raw[y * w * 2..(y + 1) * w * 2];
        // 上下の相手。片方しか無ければそれだけを使う(端の行)。
        let up = y.checked_sub(comb_step)
            .filter(|&i| filled.get(i).copied().unwrap_or(false));
        let dn = (y + comb_step < h)
            .then(|| y + comb_step)
            .filter(|&i| filled.get(i).copied().unwrap_or(false));
        let chroma_ok = mag[y] > BURST_MIN && (up.is_some() || dn.is_some());
        locked += chroma_ok as u32;

        let (cp, sp) = (cosp[y], sinp[y]);
        let px = |i: usize, j: usize| raw[(i * w + j) * 2] as f32;
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
            let c = (px(y, n) - acc / cnt) * 0.5;
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
        for n in 0..w {
            let (cos_psi, sin_psi) = psi(n);
            yl[n] = px(y, n) - (-u[n] * cos_psi + v[n] * sin_psi);
        }
        // --- 3) YUV → RGB ---
        let o0 = y * w * 4;
        for n in 0..w {
            let yy = (yl[n] - porch) * inv_100ire;
            let b_y = u[n] * inv_100ire / 0.493;
            let r_y = v[n] * inv_100ire / 0.877;
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
        decode_field(&raw, w, h, &filled, sps as u32, &mut fb);
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
        // 1回目は warm-up
        decode_field(&raw, w, h, &filled, sps as u32, &mut fb);
        let n = 120;
        let t0 = std::time::Instant::now();
        for _ in 0..n {
            decode_field(&raw, w, h, &filled, sps as u32, &mut fb);
        }
        let per = t0.elapsed().as_secs_f64() / n as f64;
        let samples = (w * h / 2) as f64;      // 1フィールドで埋まるのは半分の行
        println!("1フィールド {:.3} ms  ({:.1} MSa/s 相当)  \
                  59.94フィールド/秒なら1コアの {:.1}%",
                 per * 1e3, samples / per / 1e6, per * 59.94 * 100.0);
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
        let info = decode_field(&raw, w, h, &filled, (sps as u32), &mut fb);
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
        decode_field(&raw, w, h, &filled, sps as u32, &mut fb);
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
