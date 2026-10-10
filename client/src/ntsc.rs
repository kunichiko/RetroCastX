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
/// 有効映像の区間 [µs]。**この外は黒にする。**
///
/// ★実機で管面の左端に細い緑の帯が出た(実測 R2.5 / G21 / B0)。カラーバーストを
///   そのまま復調した色だった。バーストは輝度0 IREのところに色だけ乗っているので、
///   RGBに直すと R と B が0にクランプされて G だけ残る。
///   実際のブラウン管はこの区間ビームが消えているので何も映らない。
///   管面は時間で窓を決めるので、窓を広げると帰線区間まで映ってしまう。
///   生の波形を見たいときは「復調しない(生のYを見る)」を使う。
///
/// ★**幅はビデオデッキの HDMI 出力に合わせてある**(2026-10-09 実測)。白黒の
///   格子パターンで、中央の点を目印に同じ物差しで比べると、デッキは
///   9.95〜60.87µs(50.9µs)を映していて、**中心が中央の点にぴったり**だった。
///   以前の (9.6, 62.0) は規格上の有効区間ほぼいっぱいで、中心が 0.35µs 右へ
///   ずれ、右端はデッキに無い縦線まで見えていた。
///   表示側の初期の管面(main.rs の `MON_NTSC`)もこの窓と同じにしてある。
///
/// ★左端がチラつくのを見て、一度は窓を 10.4µs まで詰めた。**原因は窓ではなく
///   TVPのクランプ窓が映像の頭に食い込んでいたこと**だった(profiles.rs の
///   CLAMP_START の注記)。ラインの頭が半分の明るさになり、その終わりが
///   フレームごとに動いていた。ジェネレータの信号の揺れと誤読しかけたが、
///   デッキは同じ時刻を真っ白で安定して出していた。クランプを直せば
///   デッキと同じ幅で揺れない。
pub const ACTIVE_US: (f32, f32) = (9.95, 60.87);

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

/// 動き量のうち「動きではない」とみなす分[IRE]。これを超えた分だけで
/// 2次元へ寄せる(MOTION_IRE で完全に2次元)。
///
/// ★以前は `a = 動き量 / MOTION_IRE` で**0から比例**させていたので、雑音だけの
///   静止部分でも 1〜2割、カラーバーの境界では最大7割ほど2次元が混ざっていた
///   (実測 2026-10-09)。2次元の輝度は境界でクロマを引き残すので、それが
///   フレームごとに反転してちらついた。サンプル位置の揺れを補正した後の
///   静止部分の動き量は 99%点で 1.7コード(≒1.9 IRE)なので、その上に置く。
const MOTION_CORE_IRE: f32 = 3.0;

/// 2次元で輝度から引くクロマを選ぶ「縦の変化」(上下の行の差)[IRE]。
/// LO 以下なら2次元コムのクロマ(境界まで正確)、HI 以上なら帯域制限した再変調
/// (横棒を上下に広げない)。間は線形に混ぜる。
const VDETAIL_LO_IRE: f32 = 3.0;
const VDETAIL_HI_IRE: f32 = 10.0;

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

/// 白黒として出すのに要る同期の深さ(バックポーチ − 同期チップ)[コード]。
///
/// バーストが無いときは同期だけが頼りなので、同期が見えていない(無信号、
/// 横位置が大きくずれている)ときに無理に校正すると、ゲインが暴れて
/// 雑音を全面に引き伸ばした絵になる。そのときは生のYを残す。
const MONO_SYNC_MIN: f32 = 10.0;

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
    /// バーストが無いので白黒として出した(レベル校正だけ掛けた Y)。
    /// 白黒のパターンジェネレータなど。テレビのカラーキラーと同じ扱い
    pub mono: bool,
    /// TBC で並べ直した量(行ごとのずれの二乗平均平方根)[サンプル]。
    /// 取り込み位置の揺れの大きさそのもの。0 なら並べ直していない
    pub tbc_rms: f32,
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
    /// ガンマ。1.0 = 信号どおり(素通し)。出力を `v^(1/gamma)` にする。
    ///
    /// ★**1.0 が正しい。素通しが正解で、ここは好みの調整。**
    ///
    /// ## なぜ素通しが正しいか(「ブラウン管のガンマを補正すべき」は誤り)
    ///
    ///     被写体の光(リニア)
    ///       → カメラの OETF ≈ v^(1/2.2) で符号化   ← 信号は既にガンマ済み
    ///       → コンポジット/S端子はこの符号化済みの値を運ぶ
    ///       → ブラウン管の EOTF ≈ 2.2〜2.5 で復号 → 光
    ///
    /// **符号化は最初からブラウン管のガンマを見越して掛けられている。** 受け側で
    /// 改めて補正するものではない。そして現代の液晶(sRGB)も同じ EOTF ≈ 2.2 を
    /// 意図的に実装しているので、符号化済みの値をそのまま8bitへ書けば液晶が
    /// ブラウン管の代わりを務める。
    ///
    /// もし本当にブラウン管ガンマを補正(リニア化)したら、sRGB表示でもう一度2.2が
    /// 掛かって**暗くなる**。持ち上がる方向にはならない。
    ///
    /// 厳密にはブラウン管の EOTF は2.4〜2.5で、符号化が1/2.2なので系全体のガンマは
    /// 1.1〜1.2 になる。これは意図的(暗い部屋での dim surround 補償)。sRGB液晶で
    /// 素通しすると系全体は1.0なので、**ブラウン管より気持ち平坦**になる。
    ///
    /// ## 実測(2026-08-15)と、その**帰属の訂正**(2026-09-03)
    ///
    /// 同じ画面のYouTube版と分位で比べると、端点(黒0・白255)は一致し中間だけ違った。
    /// v^(1/g) を当てはめると **g=2.2 で9点すべてが平均誤差1.8コード**で一致した。
    ///
    /// 当時は「**あちら**が符号化済みの値をリニアとして扱って二重に符号化した絵
    /// (washed out)」と結論した。★**これは帰属が逆だった。暗かったのはこちら。**
    ///
    /// 2026-09-03、映像テクスチャを `Rgba8UnormSrgb` 決め打ちで作っていたため、
    /// 描画先が非sRGBのとき**サンプル時の sRGB→リニア復号だけが残る**バグを
    /// 見つけた(render::video_tex_format で修正)。実測でフレームバッファの
    /// 8/16/24/32/41/49/57 が画面上で 1/1/2/4/6/8/11 になっていた。
    /// **sRGB→リニアの曲線はガンマ2.2相当**で、上の「9点が誤差1.8コードで一致」は
    /// 偶然ではなくこのバグの指紋だった。窓経路がこのテクスチャを使い始めたのは
    /// 2026-08-09(EguiBlit 導入)で、ガンマを足した 08-15 の6日前にあたる。
    ///
    /// つまり YouTube 版が正しく、こちらが2.2ぶん暗かった。**ガンマを 1.2〜1.6 に
    /// 上げると暗部が見える」という当時の実用上の助言も、実際には自分のバグの
    /// 相殺だった。** 修正後は既定の 1.0 で32階調が8刻みに並ぶことを実機で確認済み。
    ///
    /// 素通しが正しいという上の議論そのものは変わらない。変わったのは
    /// 「観測された2.2の差を誰のせいにするか」だけ。
    /// ★**測った差を相手のせいにする前に、自分の出力経路を疑うこと。**
    pub gamma: f32,
}

impl Default for Adjust {
    fn default() -> Self {
        Self { hue_deg: 0.0, saturation: 1.0, brightness: 0.0, contrast: 1.0,
               gamma: 1.0 }
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

/// `burst` の浮動小数版(TBC で並べ直した行を測る)。
fn burst_f(row: &[f32], x0: usize, x1: usize) -> (f32, f32, f32) {
    let mean = row[x0..x1].iter().sum::<f32>() / (x1 - x0) as f32;
    let (mut ci, mut si) = (0.0f32, 0.0f32);
    for n in x0..x1 {
        let v = row[n] - mean;
        let k = (n - x0) & 7;
        ci += v * COS8[k];
        si += v * SIN8[k];
    }
    ((ci * ci + si * si).sqrt(), ci, si)
}

/// TBC(タイムベース補正)で受け付ける1行のずれの上限[サンプル]。
/// 実測の揺れは標準偏差 0.06〜0.1、最大 0.4 程度。これを大きく超えるのは
/// バーストの測り損ね(暗い行・欠損)なので、その行は並べ直さない。
const TBC_MAX_SAMPLES: f32 = 1.0;

/// 緑ch(CVBS)を浮動小数の1面に取り出し、`tbc` なら**行ごとの取り込み位置の
/// 揺れをバーストの位相で測って並べ直す**。`rows` の行だけを扱う。
///
/// ★DATACLK は HSYNC に PLL でロックしているので、行ごとに取り込み位置が
///   揺れる(実測 2026-10-09: 2フレーム前との横ずれが標準偏差 0.10サンプル、
///   最大 0.39。隣の行とは相関 0.77、8行離れるとほぼ無相関)。絵そのものが
///   行ごとに横へ動くので、色の境界では色の混ざり方が変わってちらつく。
///   ジェネレータの副搬送波は安定しているので、**バーストの位相のずれが
///   そのまま取り込み位置のずれ**になる(8fsc で 45°/サンプル。絵から測った
///   ずれとの相関 0.91)。各行を「フィールド内の基準位相」に揃えるように
///   3次補間でずらすと、2フレーム前との横ずれは 0.10 → 0.042サンプルになった
///   (録った生信号での試算)。
///
///   基準は全行の位相を **180°を法として**平均したもの(2φ の円周平均)。
///   隣の行は副搬送波が180°反転しているので、行番号の偶奇に頼らずに済む。
///   行ごとの位相の傾き(副搬送波と fH の比のずれ)は実測 0.01°/行程度で
///   無視できる。フィールド上部の決まった曲がり(垂直同期の後の PLL の乱れ、
///   上端で -8.6°)も一緒に直る。
///
/// 戻り値は (面, 並べ直した量の二乗平均平方根[サンプル])。
fn tbc_plane(src: &[u8], w: usize, h: usize, rows: &[bool], ba: usize, bb: usize,
             step: usize, tbc: bool) -> (Vec<f32>, f32) {
    let mut out = vec![0.0f32; w * h];
    let used = |y: usize| rows.get(y).copied().unwrap_or(false);
    let mut tau = vec![0.0f32; h];
    let mut has = vec![false; h];
    if tbc {
        let mut ph = vec![f32::NAN; h];
        let (mut c2, mut s2) = (0.0f32, 0.0f32);
        for y in 0..h {
            if !used(y) {
                continue;
            }
            let (m, ci, si) = burst(&src[y * w * 2..(y + 1) * w * 2], ba, bb, 0);
            if m <= BURST_MIN {
                continue;
            }
            let p = si.atan2(ci);
            ph[y] = p;
            c2 += (2.0 * p).cos();
            s2 += (2.0 * p).sin();
        }
        let refp = 0.5 * s2.atan2(c2);
        for y in 0..h {
            if ph[y].is_nan() {
                continue;
            }
            // (-90°, 90°] に畳む
            let mut d = ph[y] - refp;
            let half = std::f32::consts::FRAC_PI_2;
            while d > half { d -= std::f32::consts::PI; }
            while d <= -half { d += std::f32::consts::PI; }
            let t = d / std::f32::consts::FRAC_PI_4;
            if t.abs() <= TBC_MAX_SAMPLES {
                tau[y] = t;
                has[y] = true;
            }
        }
    }
    let (mut ss, mut nn) = (0.0f32, 0usize);
    let mut pad: Vec<f32> = Vec::with_capacity(w + 16);
    for y in 0..h {
        if !used(y) {
            continue;
        }
        let row = &src[y * w * 2..(y + 1) * w * 2];
        let o = &mut out[y * w..(y + 1) * w];
        let t = tau[y];
        // ★**行の中でもずれは伸びる**(速度誤差)。取り込みクロックの周波数が
        //   行ごとにわずかに違うので、バーストで揃えた後の残りが行の右へ行くほど
        //   直線的に増えていた(実測: 13.6µs で 0.019 → 57.6µs で 0.069サンプル)。
        //   クロックは行をまたいで連続しているので、**時間的に次の行のバースト**が
        //   この行の終わりのずれを表す。その間を直線で結ぶと右端の残りは 0.043 に
        //   なった(前後4本の3次補間でも 0.041 で、差は小さいので直線にした)。
        //   時間的に次の行 = 同じフィールドの隣 = コム間隔 `step` 先の行。
        let dt = if step > 0 && y + step < h && has[y] && has[y + step] {
            (tau[y + step] - t) / w as f32
        } else {
            0.0
        };
        let nb = (ba + bb) as f32 * 0.5;
        if t == 0.0 && dt == 0.0 {
            for n in 0..w {
                o[n] = row[n * 2] as f32;
            }
            continue;
        }
        ss += t * t;
        nn += 1;
        // Catmull-Rom。線形補間だと副搬送波の振幅がずれ量で変わる
        // (0.5サンプルで 7.6%)ので、行ごとに彩度が揺れてしまう。
        // 端の処理を内側のループから追い出すため、両端を延ばした行を作る
        // (ずれは TBC_MAX_SAMPLES と速度ぶんで高々数サンプル)
        const PAD: usize = 8;
        pad.clear();
        pad.extend(std::iter::repeat(row[0] as f32).take(PAD));
        pad.extend((0..w).map(|n| row[n * 2] as f32));
        pad.extend(std::iter::repeat(row[(w - 1) * 2] as f32).take(PAD));
        let lim = (PAD - 2) as f32;
        for n in 0..w {
            let sh = (t + dt * (n as f32 - nb)).clamp(-lim, lim);
            let x = n as f32 + sh;
            let i = x.floor();
            let f = x - i;
            let i = (i as isize + PAD as isize) as usize;
            let (p0, p1, p2, p3) = (pad[i - 1], pad[i], pad[i + 1], pad[i + 2]);
            o[n] = p1 + 0.5 * f * (p2 - p0
                + f * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3
                + f * (3.0 * (p1 - p2) + p3 - p0)));
        }
    }
    (out, if nn > 0 { (ss / nn as f32).sqrt() } else { 0.0 })
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

/// `cur` が `prev` に対して横に何サンプルずれているか(cur(n) ≈ prev(n - s) の s)。
///
/// 差を prev の傾きで最小二乗に当てる。ずれが1サンプルより十分小さい前提の
/// 1次近似なので、大きく外れたら(絵が本当に動いたなど)0 として扱う。
fn line_shift(cur: impl Fn(usize) -> f32, prev: impl Fn(usize) -> f32,
              a: usize, b: usize) -> f32 {
    let (mut dg, mut gg) = (0.0f32, 0.0f32);
    for n in a..b {
        let g = (prev(n + 1) - prev(n - 1)) * 0.5;
        dg += (cur(n) - prev(n)) * g;
        gg += g * g;
    }
    if gg < 1e-3 {
        return 0.0;
    }
    let s = -dg / gg;
    if s.abs() > 1.0 { 0.0 } else { s }
}

/// 同期チップとバックポーチの緑chレベル(中央値)を測る。`use_line(y)` が真の
/// 受信済みラインだけを使う。使える行が無ければ None。
fn levels(sample: impl Fn(usize, usize) -> f32, w: usize, h: usize, filled: &[bool],
          sps: f32, use_line: impl Fn(usize) -> bool) -> Option<(f32, f32)> {
    let (ta, tb) = win(TIP_US, sps, w);
    let (pa, pb) = win(PORCH_US, sps, w);
    let mut tips = Vec::new();
    let mut porches = Vec::new();
    for y in 0..h {
        if !filled.get(y).copied().unwrap_or(false) || !use_line(y) {
            continue;
        }
        let mut s = 0.0f32;
        for n in ta..tb {
            s += sample(y, n);
        }
        tips.push(s / (tb - ta).max(1) as f32);
        let mut s = 0.0f32;
        for n in pa..pb {
            s += sample(y, n);
        }
        porches.push(s / (pb - pa).max(1) as f32);
    }
    if tips.is_empty() {
        return None;
    }
    Some((median(&mut tips), median(&mut porches)))
}

/// ガンマは256エントリの表にしておく。画素ごとに powf を3回呼ぶと
/// 28.6 MSa/s では到底間に合わない(1.0 のときは表自体を使わない)。
fn gamma_lut(adj: Adjust) -> Option<[u8; 256]> {
    if (adj.gamma - 1.0).abs() <= 1e-3 {
        return None;
    }
    let inv = 1.0 / adj.gamma.max(0.1);
    let mut t = [0u8; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = ((i as f32 / 255.0).powf(inv) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    }
    Some(t)
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
                      svideo: false, mono: false, tbc_rms: 0.0, phase_drift_deg: 0.0 };
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
    let (aa, ab) = win(ACTIVE_US, sps, w);
    if comb_step == 0 {
        // 180°になるペアが見つからない = バーストが取れていない。
        //
        // ★**白黒信号として出す。** 白黒のパターンジェネレータはバーストを
        //   載せない。以前はここで何もせず戻っていたので生のADC値の
        //   グレースケールが残り、**黒(0 IRE)が同期より40 IRE上=灰色**に見えた
        //   (2026-10-09、黒地に白の格子が灰色地になった。テレビでは黒地)。
        //   テレビもバーストが無ければカラーキラーで色を止め、黒レベルは
        //   バックポーチで合わせる。レベル校正にバーストは要らない
        //   (ntsc.py も全ラインで校正している)。
        let none = Info { lines_locked: 0, comb_step: 0, phase_delta_deg: 0.0,
                          code_per_ire: 0.0, lines_3d: 0, motion_frac: 0.0,
                          svideo: false, mono: false, tbc_rms: 0.0, phase_drift_deg: 0.0 };
        let Some((tip, porch)) =
            levels(|y, n| raw[(y * w + n) * 2] as f32, w, h, filled, sps, |_| true) else {
            return none;
        };
        if porch - tip < MONO_SYNC_MIN {
            // 同期が見えていない。生のYを残す
            return none;
        }
        let code_per_ire = (porch - tip) / 40.0;
        let inv_100ire = 1.0 / (code_per_ire * 100.0);
        let lut = gamma_lut(adj);
        for y in 0..h {
            if !filled.get(y).copied().unwrap_or(false) {
                continue;
            }
            let o0 = y * w * 4;
            for n in 0..w {
                let o = o0 + n * 4;
                let v = if n >= aa && n < ab {
                    let yy = (raw[(y * w + n) * 2] as f32 - porch) * inv_100ire
                        * adj.contrast + adj.brightness * 0.01;
                    let v = to8(yy);
                    lut.as_ref().map_or(v, |t| t[v as usize])
                } else {
                    // 帰線区間は黒(カラーのときと同じ)
                    0
                };
                fb[o] = v;
                fb[o + 1] = v;
                fb[o + 2] = v;
            }
        }
        return Info { mono: true, code_per_ire, ..none };
    }

    // --- 2b. TBC: 行ごとの取り込み位置の揺れを直す(tbc_plane の注記) ---
    //
    // ここから先は生の8bitではなく、並べ直した浮動小数の面を読む。8bitへ
    // 戻すと丸め誤差(±0.5コード)が乗るため。S端子はコンポジットの経路と
    // 別なので並べ直さない(面は素通しの写し)。
    let (cur, tbc_rms) = tbc_plane(raw, w, h, filled, ba, bb, comb_step, !svideo);
    // 2フレーム前(p4)は動き検出にしか使わないので並べ直さない。行ごとのずれは
    // 動き検出の側で line_shift が吸収し、速度ぶんの残り(右端で最大 0.07サンプル
    // ≒ 境界で 1.8コード)は動きと数えない下限(MOTION_CORE_IRE)より小さい。
    // 並べ直すと復調の時間が 1.5倍になる(実測)。
    let q2 = match hist.as_ref() {
        Some(hh) if hh.p2.len() == raw.len() && hh.p4.len() == raw.len() =>
            Some(tbc_plane(hh.p2, w, h, filled, ba, bb, comb_step, !svideo).0),
        _ => None,
    };
    // 並べ直した行のバースト位相を測り直す(復調の位相基準はこちら)
    if !svideo {
        for y in 0..h {
            if !filled.get(y).copied().unwrap_or(false) {
                continue;
            }
            let (m, ci, si) = burst_f(&cur[y * w..(y + 1) * w], ba, bb);
            mag[y] = m;
            phase[y] = si.atan2(ci);
            let m = m.max(1e-6);
            cosp[y] = ci / m;
            sinp[y] = si / m;
        }
    }

    // --- 3. レベル校正。同期チップ(-40 IRE)とバックポーチ(0 IRE)から求める ---
    //     絵の内容に依存しないのがこの校正の利点。
    let (tip, porch) = levels(|y, n| cur[y * w + n], w, h, filled, sps,
                              |y| mag[y] > BURST_MIN)
        .unwrap_or((0.0, 0.0));
    let code_per_ire = ((porch - tip) / 40.0).max(0.05);
    let inv_100ire = 1.0 / (code_per_ire * 100.0);
    let (pa, pb) = win(PORCH_US, sps, w);

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
    // フレームコムが成り立つ行(2フィールド前の副搬送波が180°反転している行)
    let mut frame_ok = vec![false; h];
    let mut drifts = Vec::new();
    if let Some(q2) = q2.as_ref().filter(|_| !svideo) {
        {
            for y in 0..h {
                if !filled.get(y).copied().unwrap_or(false) || mag[y] <= BURST_MIN {
                    continue;
                }
                let (m2, ci, si) = burst_f(&q2[y * w..(y + 1) * w], ba, bb);
                if m2 <= BURST_MIN {
                    continue;
                }
                let e = ang_diff(si.atan2(ci).to_degrees(), phase[y].to_degrees() + 180.0);
                drifts.push(e.abs());
                // 外れ値(暗い行でバーストを測り損ねた等)では補正を掛けない。
                // tan(ε/2) が暴れると3次元の枝ごと壊れる方が高くつく。
                if e.abs() <= PHASE_FIX_MAX_DEG {
                    tan_half[y] = (e.to_radians() * 0.5).tan();
                    frame_ok[y] = true;
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

    let gamma_lut = gamma_lut(adj);

    // --- 4. コム → 直交復調 → RGB ---
    let mut u = vec![0.0f32; w];
    let mut v = vec![0.0f32; w];
    let mut yl = vec![0.0f32; w];
    let mut mot = vec![0.0f32; w];
    let mut c3buf = vec![0.0f32; w];
    // 画素ごとの「2次元へ落とした割合」(0 = 静止でフレームコム、1 = 2次元)。
    // 輝度の作り方も同じ割合で切り替えるので残しておく
    let mut amix = vec![1.0f32; w];
    // 2次元コムのクロマと、上下の行の差(縦の変化。輝度の作り方を選ぶのに使う)
    let mut c2buf = vec![0.0f32; w];
    let mut vdet = vec![f32::INFINITY; w];
    let vd_lo = VDETAIL_LO_IRE * code_per_ire;
    let vd_hi = VDETAIL_HI_IRE * code_per_ire;
    let mut locked = 0u32;
    let mut lines_3d = 0u32;
    let (mut moving, mut n3) = (0u32, 0u32);
    let motion_th = MOTION_IRE * code_per_ire;
    let motion_core = MOTION_CORE_IRE * code_per_ire;
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
        let px = |i: usize, j: usize| cur[i * w + j];
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
        // ★**2フィールド前が180°反転していない行ではフレームコムを使わない。**
        //
        //   プログレッシブ(240p)の NTSC は1フィールド 262行なら 262×227.5 が整数で、
        //   副搬送波の位相が毎フィールド同じ(263行でも2フィールド前は同位相)。
        //   フレームコムの差を取るとクロマが消え、色が出ずに輝度へ市松が残った
        //   (実機 2026-10-10、ジェネレータの「プログレッシブNTSC」。位相ズレ 179.6°)。
        //   以前は ε が外れた行で位相補正を掛けないだけで、3次元は使い続けていた。
        //   ε が測れて補正の範囲に入っている行だけを3次元にする。
        let use3d = !svideo && frame_ok[y] && hist.as_ref().map_or(false, |hh| {
            hh.hist_n.get(y).copied().unwrap_or(0) >= 3
                && q2.is_some()
        });
        if use3d {
            lines_3d += 1;
            // 動き検出は **2 NTSCフレーム前**(位相0°)との差。1フレーム前だと
            // 位相180°でクロマが差に出てしまい、色のある所が全部「動いている」
            // ことになる(実測で副搬送波成分が 388 対 9315)。
            let hh = hist.as_ref().unwrap();
            // ★**2フレーム前の行を、サンプル位置の揺れの分ずらしてから比べる。**
            //
            //   DATACLK は HSYNC に PLL でロックしているので、行ごとに取り込み位置が
            //   わずかに揺れる(実測 2026-10-09: 2フレーム前との差で標準偏差
            //   0.10サンプル = 3.6ns、最大0.39。行の左半分と右半分で測ったずれの
            //   相関は 0.977 で、**行の中ではほぼ一定**)。カラーバーの境界は
            //   傾きが最大26コード/サンプルあるので、このずれだけで差が10コード
            //   近くになり、静止しているのに「動いている」と判定されていた。
            //   ずれを行ごとに最小二乗で求めて線形補間で戻すと、境界の動き量は
            //   99%点で 4.8 → 1.7コードになった(平坦部と同じ)。
            let prow = |n: usize| hh.p4[(y * w + n) * 2] as f32;
            let sh = line_shift(|n| px(y, n), prow, aa.max(1), ab.min(w - 1));
            for n in 0..w {
                let t = n as f32 - sh;
                let i0 = (t.floor().max(0.0) as usize).min(w - 1);
                let i1 = (i0 + 1).min(w - 1);
                let fr = (t - i0 as f32).clamp(0.0, 1.0);
                let r = prow(i0) * (1.0 - fr) + prow(i1) * fr;
                mot[n] = (px(y, n) - r).abs();
            }
            // 副搬送波1周期(8サンプル)で平均してノイズを落とす
            boxcar(&mut mot, 8);
            // フレームコムのクロマ。位相ズレ ε を 2サンプル遅延で戻す(3b参照)。
            for n in 0..w {
                c3buf[n] = (px(y, n) - q2.as_ref().unwrap()[y * w + n]) * 0.5;
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
            amix[n] = 1.0;
            if cnt == 0.0 {
                u[n] = 0.0;
                v[n] = 0.0;
                continue;
            }
            let mut c = (px(y, n) - acc / cnt) * 0.5;
            c2buf[n] = c;
            // 上下が両方あるときだけ縦の変化を測る。上と下は 2×コム間隔 離れて
            // いて副搬送波が同位相なので、差ではクロマが打ち消し、縦の変化だけ残る
            vdet[n] = match (up, dn) {
                (Some(i), Some(j)) => (px(i, n) - px(j, n)).abs(),
                _ => f32::INFINITY,
            };
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
                let a = ((mot[n] - motion_core) / (motion_th - motion_core).max(1e-6))
                    .clamp(0.0, 1.0);
                if a >= 0.5 { moving += 1; }
                n3 += 1;
                c = (1.0 - a) * c3 + a * c;
                amix[n] = a;
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
            // ★**静止部分ではフレームコムのクロマ c3 をそのまま引く。**
            //
            //   再変調の Ĉ は帯域制限(CHROMA_LPF)したクロマなので、クロマが急に
            //   変わる所(カラーバーの色の境界)では信号のクロマと合わず、引き残しが
            //   輝度に出る。その残りは副搬送波なので**フレームごとに符号が反転し、
            //   30Hz でちらつく縞**になる(実機 2026-10-09。強さはクロマの変化量の
            //   順で、緑/マゼンタの境界が最大)。
            //   静止部分の c3 は境界まで正確なクロマで、x - c3 = (x + p2)/2 は
            //   前後2フレームの平均そのもの。水平にも垂直にも広がらないので、
            //   上の表の「x - Ĉ」の利点はそのまま残る。動いている所(amix → 1)は
            //   フレームコムが成立しないので従来どおり Ĉ を引く。
            for n in 0..w {
                let (cos_psi, sin_psi) = psi(n);
                let c_hat = -u[n] * cos_psi + v[n] * sin_psi;
                // ★**2次元でも、縦に変化の無い所は2次元コムのクロマを引く。**
                //
                //   上の表のとおり x - C_comb は横棒を上下に広げるが、それは縦の
                //   変化がある所の話。縦の変化が無い所(カラーバーの色の境界など)
                //   では C_comb は境界まで正確で、帯域制限した Ĉ を引くと境界に
                //   引き残しの縞が出る。プログレッシブ(240p)は3次元が使えない
                //   (progressive_ntsc_keeps_color)ので、インターレースで直した
                //   縞がそのまま出ていた(実機 2026-10-10)。縦の変化で両者を混ぜる。
                let c2d = if chroma_ok && vdet[n].is_finite() {
                    let k = ((vdet[n] - vd_lo) / (vd_hi - vd_lo).max(1e-6)).clamp(0.0, 1.0);
                    (1.0 - k) * c2buf[n] + k * c_hat
                } else {
                    c_hat
                };
                let a = amix[n];
                let c_sub = if use3d && chroma_ok && a < 1.0 {
                    (1.0 - a) * c3buf[n] + a * c2d
                } else {
                    c2d
                };
                yl[n] = px(y, n) - c_sub;
            }
        }
        // --- 3) YUV → RGB ---
        //
        // 水平帰線区間(有効映像の外)は**黒にする**。実際のブラウン管はここで
        // ビームが消えている。残しておくとバーストが緑の帯として管面に出る。
        let o0 = y * w * 4;
        for n in 0..w.min(aa) {
            let o = o0 + n * 4;
            fb[o] = 0;
            fb[o + 1] = 0;
            fb[o + 2] = 0;
        }
        for n in ab.min(w)..w {
            let o = o0 + n * 4;
            fb[o] = 0;
            fb[o + 1] = 0;
            fb[o + 2] = 0;
        }
        for n in aa.min(w)..ab.min(w) {
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
            let (r, g, b) = (to8(r), to8(g), to8(b));
            let (r, g, b) = match &gamma_lut {
                Some(t) => (t[r as usize], t[g as usize], t[b as usize]),
                None => (r, g, b),
            };
            fb[o] = r;
            fb[o + 1] = g;
            fb[o + 2] = b;
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
        mono: false,
        tbc_rms,
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

    /// バーストの無い白黒信号は、黒レベルを校正して白黒で出すこと。
    ///
    /// ★実機で踏んだ(2026-10-09): 白黒のパターンジェネレータの「黒地に白の格子」が
    ///   **灰色地**になった。バーストが無いと何もせず戻っていたので、生のADC値
    ///   (黒が同期より40 IRE上)のグレースケールが残っていた。
    #[test]
    fn no_burst_is_shown_as_monochrome_with_black_at_zero() {
        let (w, h, sps) = (1820usize, 64usize, 28.6362e6f32);
        let (cpi, porch) = (0.78f32, 158.0f32);
        let sync_end = (4.7e-6 * sps) as usize;
        let (aa, ab) = win(ACTIVE_US, sps, w);
        let mid = (aa + ab) / 2;
        let mut raw = vec![0u8; w * h * 2];
        let filled = vec![true; h];
        for y in 0..h {
            for n in 0..w {
                let val = if n < sync_end {
                    porch - 40.0 * cpi
                } else if n >= mid && n < ab {
                    porch + 100.0 * cpi       // 白
                } else {
                    porch                     // 黒(バーストは載せない)
                };
                raw[(y * w + n) * 2] = val as u8;
            }
        }
        let mut fb = vec![0x55u8; w * h * 4];
        let info = decode_field(&raw, w, h, &filled, sps as u32, &mut fb, None,
                                Adjust::default());
        assert!(info.mono, "バーストが無いのに白黒にならない");
        assert_eq!(info.comb_step, 0);
        let px = |n: usize| &fb[(h / 2 * w + n) * 4..(h / 2 * w + n) * 4 + 3];
        let black = px((aa + mid) / 2);
        let white = px((mid + ab) / 2);
        assert!(black.iter().all(|&v| v <= 6), "黒が黒でない: {black:?}");
        assert!(white.iter().all(|&v| v >= 245), "白が白でない: {white:?}");
        assert_eq!(px(sync_end / 2), &[0, 0, 0], "帰線区間が黒でない");
    }

    /// 同期が見えない(無信号で平坦)ときは校正せず、生のYを残すこと。
    /// 雑音をゲインで引き伸ばした絵にしない。
    #[test]
    fn no_sync_leaves_raw_luma_untouched() {
        let (w, h, sps) = (1820usize, 32usize, 28.6362e6f32);
        let raw: Vec<u8> = (0..w * h).flat_map(|i| [100 + (i % 3) as u8, 0]).collect();
        let filled = vec![true; h];
        let mut fb = vec![0x55u8; w * h * 4];
        let info = decode_field(&raw, w, h, &filled, sps as u32, &mut fb, None,
                                Adjust::default());
        assert!(!info.mono);
        assert!(fb.iter().all(|&v| v == 0x55), "同期が無いのに fb を書き換えた");
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

        // 水平帰線区間が黒であること。**バーストが緑の帯として管面に出ていた。**
        {
            let (ba2, bb2) = win(BURST_US, sps, w);
            let (aa2, _) = win((9.6, 62.0), sps, w);
            for n in [ba2 + 4, (ba2 + bb2) / 2, bb2 - 4, aa2 - 8, 8] {
                let o = (y * w + n) * 4;
                assert_eq!((base[o], base[o + 1], base[o + 2]), (0, 0, 0),
                           "帰線区間 x={n} が黒でない");
            }
            // 有効映像の中は黒でない(全部黒にしてしまっていないこと)
            let o = (y * w + aa2 + per / 2) * 4;
            assert!(base[o] as u32 + base[o + 1] as u32 + base[o + 2] as u32 > 30,
                    "有効映像まで黒にしている");
        }

        // ガンマ。**端点は動かさず中間だけ持ち上げる**(明るさとはここが違う)。
        let gm = dec(Adjust { gamma: 2.2, ..Default::default() });
        let lumv = |fb: &[u8], n: usize| {
            let o = (y * w + n) * 4;
            0.299 * fb[o] as f32 + 0.587 * fb[o + 1] as f32 + 0.114 * fb[o + 2] as f32
        };
        // ★ガンマは 0 と 255 以外を全部持ち上げるので、「暗い画素は動かない」は
        //   期待として誤り(最初そう書いて落ちた: 23.9 → 83.3)。
        //   見るべきは **単調に上がるだけで、下がりも溢れもしない** こと。
        let mut lifted = 0usize;
        for n in (aa..aa + per * 4).step_by(7) {
            let o = (y * w + n) * 4;
            for k in 0..3 {
                assert!(gm[o + k] >= base[o + k],
                        "ガンマ2.2で下がった画素がある: x={n} ch{k} {} → {}",
                        base[o + k], gm[o + k]);
                if gm[o + k] > base[o + k] { lifted += 1; }
            }
        }
        assert!(lifted > 100, "ガンマ2.2でほとんど持ち上がっていない({lifted}点)");
        // 完全な黒(帰線区間)は0のまま
        assert_eq!((gm[(y * w + 8) * 4], gm[(y * w + 8) * 4 + 1]), (0, 0),
                   "ガンマで黒が持ち上がった");
        // 中間調は上がる。ガンマ2.2 なら 0.5 → 0.5^(1/2.2) = 0.73
        let mid = aa + per / 2;
        let (b0, g0) = (lumv(&base, mid), lumv(&gm, mid));
        assert!(g0 > b0 + 15.0,
                "ガンマ2.2で中間調が上がっていない: {b0:.1} → {g0:.1}");
        // 白は255のまま(上限を超えて壊れない)
        for fbx in [&gm] {
            assert!(fbx.chunks_exact(4).all(|p| p[0] <= 255 && p[1] <= 255),
                    "ガンマで値が壊れた");
        }

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
        //
        // ★判定は絶対値にしてある。以前は `r6 < r0 + 1.0` だったが、静止部分の
        //   輝度にフレームコムのクロマを使うようにして(static_color_edges_do_not_flicker)
        //   r0 が 0.15 → 0.00 に減り、基準ごと動いて落ちた。r6 そのものは変更の
        //   前後とも約 1.1 で同じ(ε=2.5°/6°で確認、2026-10-09)。
        let _ = r0;
        assert!(r6 < 1.5,
                "位相が6°ずれると輝度の副搬送波が増える: {r0:.2} → {r6:.2} コード");
    }

    /// 縦の色帯(カラーバー)。全行が同じ色で、境界は1サンプルで切り替わる。
    fn synth_bars(colors: &[(f32, f32, f32)], w: usize, h: usize, sps: f32,
                  phase_off: f32) -> Vec<u8> {
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
                    let (r, g, b) = colors[((n - aa) / per).min(colors.len() - 1)];
                    let yy = 0.299 * r + 0.587 * g + 0.114 * b;
                    let uu = 0.493 * (b - yy);
                    let vv = 0.877 * (r - yy);
                    let c = (-uu * psi.cos() + vv * psi.sin()) * 0.5;
                    val = porch + (yy * 100.0 + c * 100.0) * cpi;
                }
                raw[(y * w + n) * 2] = val.clamp(0.0, 255.0) as u8;
            }
        }
        raw
    }

    /// **静止したカラーバーの色の境界が、フレームごとにちらつかないこと。**
    ///
    /// ★実機で踏んだ(2026-10-09、NTSCカラーバー): 黄/シアン・緑/マゼンタ・
    ///   赤/青の境界に縞が出て、30Hz でちらついた。強さはクロマの変化量の順
    ///   (緑/マゼンタが最大)だった。輝度を「信号 − 帯域制限したクロマの再変調」
    ///   で作っていたので、クロマが急に変わる境界では引き残しが出る。その残りは
    ///   副搬送波なので**フレームごとに符号が反転する**。
    ///   静止部分はフレームコムでクロマが境界まで正確に取れているので、輝度にも
    ///   そちらを使えば消える。
    #[test]
    fn static_color_edges_do_not_flicker() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        // 100% カラーバー(白 黄 シアン 緑 マゼンタ 赤 青 黒)
        let colors = [(1.0, 1.0, 1.0), (1.0, 1.0, 0.0), (0.0, 1.0, 1.0), (0.0, 1.0, 0.0),
                      (1.0, 0.0, 1.0), (1.0, 0.0, 0.0), (0.0, 0.0, 1.0), (0.0, 0.0, 0.0)];
        let pi = std::f32::consts::PI;
        let filled = vec![true; h];
        let hn = vec![3u8; h];
        // 連続する2フレーム。副搬送波の位相は1フレームごとに180°反転する
        let a0 = synth_bars(&colors, w, h, sps, 0.0);
        let a1 = synth_bars(&colors, w, h, sps, pi);
        let dec = |cur: &[u8], p2: &[u8], p4: &[u8]| {
            let mut fb = vec![0u8; w * h * 4];
            decode_field(cur, w, h, &filled, sps as u32, &mut fb,
                         Some(History { p2, p4, hist_n: &hn }), Adjust::default());
            fb
        };
        let f0 = dec(&a0, &a1, &a0);
        let f1 = dec(&a1, &a0, &a1);
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / colors.len();
        let y = h / 2;
        // 境界ごとに、前後16サンプルで2フレームの差の最大を取る
        let mut worst = (0usize, 0.0f32);
        for k in 1..colors.len() {
            let e = aa + k * per;
            for n in e - 16..e + 16 {
                for ch in 0..3 {
                    let i = (y * w + n) * 4 + ch;
                    let d = (f0[i] as f32 - f1[i] as f32).abs();
                    if d > worst.1 {
                        worst = (k, d);
                    }
                }
            }
        }
        assert!(worst.1 <= 3.0,
                "静止した境界がフレームごとに {:.0} コード変わる(境界{})", worst.1, worst.0);
    }

    /// 録った生信号を、受信側(assembler)と同じ履歴の回し方で復調し直す。
    /// 実機を占有せずに復調の変更を比べるための道具。
    ///
    ///     RCX_REPLAY=bars.bin cargo test --release replay -- --ignored --nocapture
    ///
    /// bars.bin は videoin capture の .npz から作る(u32 w,h,dotclk,nframes の後に
    /// フレームごとの u32 行数、行ごとの u16 line + w バイト)。
    /// 出すのは境界ごとの「同じ画素のフレーム間の標準偏差」(Y)。
    #[test]
    #[ignore]
    fn replay() {
        let Ok(path) = std::env::var("RCX_REPLAY") else { return };
        let d = std::fs::read(path).unwrap();
        let rd = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize;
        let (w, h, dotclk, nf) = (rd(0), rd(4), rd(8) as u32, rd(12));
        let mut o = 16;
        let mut raw = vec![0u8; w * h * 2];
        let mut p2 = raw.clone();
        let mut p4 = raw.clone();
        let mut hn = vec![0u8; h];
        let xs: Vec<usize> = std::env::var("RCX_COLS").map(|v| {
            v.split(',').map(|x| x.parse().unwrap()).collect()
        }).unwrap_or_else(|_| vec![309, 484, 669, 844, 1028, 1204, 1388, 1567, 610]);
        // [列][行] ごとの Y の系列
        let mut acc: Vec<Vec<Vec<f32>>> = vec![vec![Vec::new(); h]; xs.len()];
        let mut accb: Vec<Vec<Vec<f32>>> = vec![vec![Vec::new(); h]; xs.len()];
        // 輝度に残った副搬送波(静止した縞)。|Y(n) - (Y(n-4)+Y(n+4))/2| の平均
        let mut rip = vec![(0.0f64, 0usize); xs.len()];
        let mut fb = vec![0u8; w * h * 4];
        let mut info_last = None;
        for f in 0..nf {
            let n = rd(o);
            o += 4;
            let mut filled = vec![false; h];
            for _ in 0..n {
                let l = u16::from_le_bytes(d[o..o + 2].try_into().unwrap()) as usize;
                o += 2;
                let row = &d[o..o + w];
                o += w;
                if l >= h { continue; }
                let (a, b) = (l * w * 2, (l + 1) * w * 2);
                p4[a..b].copy_from_slice(&p2[a..b]);
                p2[a..b].copy_from_slice(&raw[a..b]);
                hn[l] = hn[l].saturating_add(1);
                for (i, &v) in row.iter().enumerate() {
                    raw[a + i * 2] = v;
                    raw[a + i * 2 + 1] = 0;
                }
                filled[l] = true;
            }
            let info = decode_field(&raw, w, h, &filled, dotclk, &mut fb,
                                    Some(History { p2: &p2, p4: &p4, hist_n: &hn }),
                                    Adjust::default());
            if f < 20 { continue; }
            for (k, &x) in xs.iter().enumerate() {
                for y in 100..400 {
                    if !filled[y] { continue; }
                    // 境界の前後 ±12 を1本ずつ
                    for dx in 0..24 {
                        let i = (y * w + x + dx - 12) * 4;
                        let yy = 0.299 * fb[i] as f32 + 0.587 * fb[i + 1] as f32
                            + 0.114 * fb[i + 2] as f32;
                        acc[k][y].push(yy);
                        accb[k][y].push(fb[i + 2] as f32 - yy);
                        let yat = |n: usize| {
                            let j = (y * w + n) * 4;
                            0.299 * fb[j] as f32 + 0.587 * fb[j + 1] as f32
                                + 0.114 * fb[j + 2] as f32
                        };
                        let n = x + dx - 12;
                        rip[k].0 += (yat(n) - 0.5 * (yat(n - 4) + yat(n + 4))).abs() as f64;
                        rip[k].1 += 1;
                    }
                }
            }
            info_last = Some(info);
        }
        let i = info_last.unwrap();
        println!("locked {} comb {} 3d {} motion {:.1}% drift {:.1}",
                 i.lines_locked, i.comb_step, i.lines_3d, 100.0 * i.motion_frac,
                 i.phase_drift_deg);
        for (k, &x) in xs.iter().enumerate() {
            // 行ごと・列ごとの系列(24本が交互に入っている)の標準偏差の平均
            let sd = |acc: &Vec<Vec<Vec<f32>>>| {
                let (mut s, mut c) = (0.0f64, 0usize);
                for y in 100..400 {
                    let v = &acc[k][y];
                    if v.len() < 48 { continue; }
                    for dx in 0..24 {
                        let ser: Vec<f64> =
                            v.iter().skip(dx).step_by(24).map(|&a| a as f64).collect();
                        let m = ser.iter().sum::<f64>() / ser.len() as f64;
                        let var = ser.iter().map(|a| (a - m) * (a - m)).sum::<f64>()
                            / ser.len() as f64;
                        s += var.sqrt();
                        c += 1;
                    }
                }
                s / c.max(1) as f64
            };
            // 連続する(同じ行の)2回の値の相関。-1 に近ければフレームごとに反転
            let lag1 = |acc: &Vec<Vec<Vec<f32>>>| {
                let (mut num, mut den) = (0.0f64, 0.0f64);
                for y in 100..400 {
                    let v = &acc[k][y];
                    if v.len() < 48 { continue; }
                    for dx in 0..24 {
                        let ser: Vec<f64> =
                            v.iter().skip(dx).step_by(24).map(|&a| a as f64).collect();
                        let m = ser.iter().sum::<f64>() / ser.len() as f64;
                        for i in 1..ser.len() {
                            num += (ser[i] - m) * (ser[i - 1] - m);
                        }
                        den += ser.iter().map(|a| (a - m) * (a - m)).sum::<f64>();
                    }
                }
                num / den.max(1e-9)
            };
            if std::env::var("RCX_PROFILE").is_ok() {
                // 境界の前後 ±12 の1本ずつの、B-Y の平均と標準偏差
                let mut line = String::new();
                for dx in 0..24 {
                    let mut ser = Vec::new();
                    for y in 100..400 {
                        let v = &accb[k][y];
                        if v.len() < 48 { continue; }
                        ser.extend(v.iter().skip(dx).step_by(24).map(|&a| a as f64));
                    }
                    let m = ser.iter().sum::<f64>() / ser.len().max(1) as f64;
                    let sd = (ser.iter().map(|a| (a - m) * (a - m)).sum::<f64>()
                        / ser.len().max(1) as f64).sqrt();
                    line += &format!(" {:+.0}/{:.1}", m, sd);
                }
                println!("  B-Y mean/std @{x}:{line}");
            }
            println!("col {x:5}: Y std {:.2} ({:+.2})  B-Y std {:.2} ({:+.2})  縞 {:.2}",
                     sd(&acc), lag1(&acc), sd(&accb), lag1(&accb),
                     rip[k].0 / rip[k].1.max(1) as f64);
        }
    }

    /// カラーバーを `shift` サンプル右へずらして合成する。境界は3サンプルの
    /// 傾斜にして帯域制限された実信号に寄せる(1サンプルで切り替わる境界は
    /// サブサンプルのずれを表せない)。同期・バーストも一緒にずれる(実機の
    /// サンプル位置の揺れと同じ)。
    fn synth_bars_shift(colors: &[(f32, f32, f32)], w: usize, h: usize, sps: f32,
                        phase_off: f32, shift: f32) -> Vec<u8> {
        let (ba, _) = win(BURST_US, sps, w);
        let (bs, be) = win(BURST_US, sps, w);
        let (porch, cpi) = (158.0f32, 0.78f32);
        let sync_end = 4.7e-6 * sps;
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = ((w - aa) / colors.len()) as f32;
        let yuv = |(r, g, b): (f32, f32, f32)| {
            let yy = 0.299 * r + 0.587 * g + 0.114 * b;
            (yy, 0.493 * (b - yy), 0.877 * (r - yy))
        };
        let mut raw = vec![0u8; w * h * 2];
        for y in 0..h {
            let flip = std::f32::consts::PI * y as f32 + phase_off;
            for n in 0..w {
                let t = n as f32 - shift;
                let psi = 2.0 * std::f32::consts::PI * (t - ba as f32) / 8.0 - flip;
                let mut val = porch;
                if t < sync_end {
                    val = porch - 40.0 * cpi;
                } else if t >= bs as f32 && t < be as f32 {
                    val = porch + 20.0 * cpi * psi.cos();
                } else if t >= aa as f32 {
                    let p = (t - aa as f32) / per;
                    let ci = (p.floor() as usize).min(colors.len() - 1);
                    // 境界の手前3サンプルで次の色へ線形に移る
                    let k = ((p - p.floor()) * per - (per - 3.0)).clamp(0.0, 3.0) / 3.0;
                    let (y0, u0, v0) = yuv(colors[ci]);
                    let (y1, u1, v1) = yuv(colors[(ci + 1).min(colors.len() - 1)]);
                    let (yy, uu, vv) = (y0 + (y1 - y0) * k, u0 + (u1 - u0) * k,
                                        v0 + (v1 - v0) * k);
                    let c = (-uu * psi.cos() + vv * psi.sin()) * 0.5;
                    val = porch + (yy * 100.0 + c * 100.0) * cpi;
                }
                raw[(y * w + n) * 2] = (val + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
        raw
    }

    /// 取り込み位置の揺れを入れたカラーバー。行 y(同じフィールドの隣は y+1)の
    /// 頭で `jit[y]` サンプルずれ、行の中で次の行のずれへ直線的に移る
    /// (クロックが行をまたいで連続していて、周波数が行ごとに違う = 実機の形)。
    fn synth_bars_jitter(colors: &[(f32, f32, f32)], w: usize, h: usize, sps: f32,
                         phase_off: f32, jit: &[f32]) -> Vec<u8> {
        let (ba, bb) = win(BURST_US, sps, w);
        let nb = (ba + bb) as f32 * 0.5;
        let (porch, cpi) = (158.0f32, 0.78f32);
        let sync_end = 4.7e-6 * sps;
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = ((w - aa) / colors.len()) as f32;
        let yuv = |(r, g, b): (f32, f32, f32)| {
            let yy = 0.299 * r + 0.587 * g + 0.114 * b;
            (yy, 0.493 * (b - yy), 0.877 * (r - yy))
        };
        let mut raw = vec![0u8; w * h * 2];
        for y in 0..h {
            let flip = std::f32::consts::PI * y as f32 + phase_off;
            let (j0, j1) = (jit[y], jit[(y + 1).min(h - 1)]);
            for n in 0..w {
                // サンプル n が実際に取り込んだ時刻
                let t = n as f32 + j0 + (j1 - j0) * (n as f32 - nb) / w as f32;
                let psi = 2.0 * std::f32::consts::PI * (t - ba as f32) / 8.0 - flip;
                let mut val = porch;
                if t < sync_end {
                    val = porch - 40.0 * cpi;
                } else if t >= ba as f32 && t < bb as f32 {
                    val = porch + 20.0 * cpi * psi.cos();
                } else if t >= aa as f32 {
                    let p = (t - aa as f32) / per;
                    let ci = (p.floor() as usize).min(colors.len() - 1);
                    let k = ((p - p.floor()) * per - (per - 3.0)).clamp(0.0, 3.0) / 3.0;
                    let (y0, u0, v0) = yuv(colors[ci]);
                    let (y1, u1, v1) = yuv(colors[(ci + 1).min(colors.len() - 1)]);
                    let (yy, uu, vv) = (y0 + (y1 - y0) * k, u0 + (u1 - u0) * k,
                                        v0 + (v1 - v0) * k);
                    let c = (-uu * psi.cos() + vv * psi.sin()) * 0.5;
                    val = porch + (yy * 100.0 + c * 100.0) * cpi;
                }
                raw[(y * w + n) * 2] = (val + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
        raw
    }

    /// **取り込み位置が行ごとに揺れても、静止したカラーバーの色が揺れないこと(TBC)。**
    ///
    /// ★実機(2026-10-09): 取り込み位置が行ごとに揺れ(2フレーム前に対して標準偏差
    ///   0.10サンプル)、しかも**行の中でも伸びる**(右端で頭の 3.6倍)。その結果、
    ///   赤・マゼンタなど R-Y の大きい色の B が 3〜5コード揺れ、色の境界がちらついた。
    ///   バーストで行の頭を、次の行のバーストで行の終わりを揃えると消える。
    #[test]
    fn tbc_removes_line_jitter() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let pi = std::f32::consts::PI;
        // 2フレームで別々の揺れ(±0.3サンプル)。決まった擬似乱数で作る
        let jit = |seed: u32| -> Vec<f32> {
            let mut st = seed;
            (0..h).map(|_| {
                st = st.wrapping_mul(1664525).wrapping_add(1013904223);
                ((st >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.6
            }).collect()
        };
        let (ja, jb, jc) = (jit(7), jit(11), jit(23));
        // 75% カラーバー。100% だと青が 0/255 に張り付いて揺れが見えない
        let bars: Vec<(f32, f32, f32)> =
            BARS100.iter().map(|&(r, g, b)| (r * 0.75, g * 0.75, b * 0.75)).collect();
        // 連続する3フレーム。前後2フレームの組で復調するので、2フレームだけで
        // 比べると (a,b) と (b,a) が対称になり、揺れがあっても必ず一致してしまう
        let a = synth_bars_jitter(&bars, w, h, sps, 0.0, &ja);
        let b = synth_bars_jitter(&bars, w, h, sps, pi, &jb);
        let c = synth_bars_jitter(&bars, w, h, sps, 0.0, &jc);
        let filled = vec![true; h];
        let hn = vec![3u8; h];
        let dec = |cur: &[u8], p2: &[u8]| {
            let mut fb = vec![0u8; w * h * 4];
            let info = decode_field(cur, w, h, &filled, sps as u32, &mut fb,
                                    Some(History { p2, p4: cur, hist_n: &hn }),
                                    Adjust::default());
            (fb, info)
        };
        let (fa, ia) = dec(&b, &a);
        let (fbb, _) = dec(&c, &b);
        assert!(ia.tbc_rms > 0.1, "揺れを測れていない: {:.3}", ia.tbc_rms);
        // 色帯の中(境界の傾斜を避ける)で、2フレームの青の差の平均
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / bars.len();
        let mut worst = 0.0f32;
        for k in 0..bars.len() {
            let (x0, x1) = (aa + k * per + 24, aa + (k + 1) * per - 24);
            let mut sum = 0.0f32;
            let mut n = 0;
            for y in 4..h - 4 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4 + 2;
                    sum += (fa[i] as f32 - fbb[i] as f32).abs();
                    n += 1;
                }
            }
            worst = worst.max(sum / n as f32);
        }
        // TBC 無しで 8.9、行の頭だけ揃えると 8.9、行内の速度まで直すと 1.3(実測)
        assert!(worst < 2.5, "揺れで色帯の青がフレームごとに {worst:.1} コード変わる");
    }

    /// **プログレッシブ(240p)の NTSC でも色が出ること。**
    ///
    /// ★実機(2026-10-10、ジェネレータの「プログレッシブNTSC」カラーバー): 絵が白黒に
    ///   なり、輝度に市松の網目が乗った。1フィールド 262行だと 262×227.5 が整数で、
    ///   副搬送波の位相が**毎フィールド同じ**(263行でも2フィールド前は同位相)。
    ///   フレームコムは「2フィールド前は180°反転」が前提なので、差を取るとクロマが
    ///   消え(色が出ない)、引かれなかったクロマが輝度に残る。状態表示は「位相ズレ
    ///   179.6°」だった。この関係が成り立たない行は2次元へ落とす。
    #[test]
    fn progressive_ntsc_keeps_color() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let filled = vec![true; h];
        let hn = vec![3u8; h];
        let bars: Vec<(f32, f32, f32)> =
            BARS100.iter().map(|&(r, g, b)| (r * 0.75, g * 0.75, b * 0.75)).collect();
        // 3フィールドとも同じ位相(プログレッシブ)
        let cur = synth_bars(&bars, w, h, sps, 0.0);
        let mut fb = vec![0u8; w * h * 4];
        let info = decode_field(&cur, w, h, &filled, sps as u32, &mut fb,
                                Some(History { p2: &cur, p4: &cur, hist_n: &hn }),
                                Adjust::default());
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / bars.len();
        // 赤の帯(6本目)の中央。R が G・B より十分大きいこと
        let x = aa + 5 * per + per / 2;
        let i = (h / 2 * w + x) * 4;
        let (r, g, b) = (fb[i] as i32, fb[i + 1] as i32, fb[i + 2] as i32);
        assert!(r - g.max(b) > 80,
                "プログレッシブで赤の帯に色が出ない: RGB=({r},{g},{b}) 位相ズレ {:.0}°",
                info.phase_drift_deg);
    }

    /// **プログレッシブでも、色の境界に縞が出ないこと。**
    ///
    /// ★実機(2026-10-10): プログレッシブは3次元が使えないので、輝度を
    ///   「x − 帯域制限した Ĉ」で作っていて、インターレースで直した境界の縞
    ///   (static_color_edges_do_not_flicker)がそのまま出た。縦に変化の無い所は
    ///   2次元コムのクロマを引けば消える。インターレース(静止部分はフレームコム)
    ///   で復調した輝度を正解として比べる。
    #[test]
    fn progressive_color_edges_have_no_stripes() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let filled = vec![true; h];
        let hn = vec![3u8; h];
        let bars: Vec<(f32, f32, f32)> =
            BARS100.iter().map(|&(r, g, b)| (r * 0.75, g * 0.75, b * 0.75)).collect();
        let cur = synth_bars(&bars, w, h, sps, 0.0);
        let inv = synth_bars(&bars, w, h, sps, std::f32::consts::PI);
        let dec = |p2: &[u8]| {
            let mut fb = vec![0u8; w * h * 4];
            decode_field(&cur, w, h, &filled, sps as u32, &mut fb,
                         Some(History { p2, p4: &cur, hist_n: &hn }), Adjust::default());
            fb
        };
        let prog = dec(&cur);
        let intl = dec(&inv);
        let (aa, _) = win((9.6, 62.0), sps, w);
        let per = (w - aa) / bars.len();
        let yat = |fb: &[u8], n: usize| {
            let i = (h / 2 * w + n) * 4;
            0.299 * fb[i] as f32 + 0.587 * fb[i + 1] as f32 + 0.114 * fb[i + 2] as f32
        };
        let mut worst = (0usize, 0.0f32);
        for k in 1..bars.len() {
            let e = aa + k * per;
            for n in e - 16..e + 16 {
                let d = (yat(&prog, n) - yat(&intl, n)).abs();
                if d > worst.1 {
                    worst = (k, d);
                }
            }
        }
        assert!(worst.1 < 4.0,
                "プログレッシブの境界{}で輝度がインターレースと {:.0} コード違う(縞)",
                worst.0, worst.1);
    }

    const BARS100: [(f32, f32, f32); 8] = [
        (1.0, 1.0, 1.0), (1.0, 1.0, 0.0), (0.0, 1.0, 1.0), (0.0, 1.0, 0.0),
        (1.0, 0.0, 1.0), (1.0, 0.0, 0.0), (0.0, 0.0, 1.0), (0.0, 0.0, 0.0)];

    /// **サンプル位置の揺れを「動き」と取り違えないこと。**
    ///
    /// ★実機(2026-10-09、カラーバー): 取り込み位置が行ごとに 0.1サンプル
    ///   (最大0.4)揺れるため、傾きの大きい色の境界で2フレーム前との差が出て、
    ///   静止しているのに2次元コムが混ざり、境界がちらついた。
    #[test]
    fn sampling_jitter_is_not_motion() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let pi = std::f32::consts::PI;
        let filled = vec![true; h];
        let hn = vec![3u8; h];
        let cur = synth_bars_shift(&BARS100, w, h, sps, 0.0, 0.0);
        let p2 = synth_bars_shift(&BARS100, w, h, sps, pi, 0.0);
        let p4 = synth_bars_shift(&BARS100, w, h, sps, 0.0, 0.3);
        let mut fb = vec![0u8; w * h * 4];
        let info = decode_field(&cur, w, h, &filled, sps as u32, &mut fb,
                                Some(History { p2: &p2, p4: &p4, hist_n: &hn }),
                                Adjust::default());
        assert!(info.motion_frac < 0.005,
                "0.3サンプルの揺れを動きと判定した: {:.1}%", 100.0 * info.motion_frac);
    }

    /// 本当に動いた絵は、これまでどおり動きと判定すること(下限を設けても
    /// 動き検出が死んでいない)。
    #[test]
    fn real_motion_is_still_detected() {
        let sps = 8.0 * 3_579_545.0f32;
        let (w, h) = (1820usize, 24usize);
        let pi = std::f32::consts::PI;
        let filled = vec![true; h];
        let hn = vec![3u8; h];
        // 2フレーム前から色帯が32サンプル(1.1µs)動いた。
        // ★ずらす量は8の倍数にする。この合成は同期とバーストごとずらすので、
        //   半端にずらすとバーストの位相まで変わり、「2フィールド前が180°反転して
        //   いない」としてフレームコム(と動き判定)の対象から外れてしまう。実際の
        //   映像では絵が動いてもバーストは動かない
        let cur = synth_bars_shift(&BARS100, w, h, sps, 0.0, 0.0);
        let p2 = synth_bars_shift(&BARS100, w, h, sps, pi, 16.0);
        let p4 = synth_bars_shift(&BARS100, w, h, sps, 0.0, 32.0);
        let mut fb = vec![0u8; w * h * 4];
        let info = decode_field(&cur, w, h, &filled, sps as u32, &mut fb,
                                Some(History { p2: &p2, p4: &p4, hist_n: &hn }),
                                Adjust::default());
        // 境界7か所 × 30サンプル前後 ≒ 有効幅の1割以上
        assert!(info.motion_frac > 0.05,
                "動いた色帯を動きと判定しない: {:.1}%", 100.0 * info.motion_frac);
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
