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
//!
//! ## 入力方式の切替も持つ
//!
//! 「映像ソース」は配線の方式(RGB / コンポジット / S端子)でもある。TVPの入力MUX・
//! 同期の取り方・クランプ・ゲインが方式ごとに違うので、選んだときに `input_regs` を
//! そのままボードへ書く。**焼き直しは要らない**(全部 CONFIG で済む)。

use crate::protocol as proto;

/// TVP7002のピクセルクロック下限[Hz]。データシートの保証範囲は12〜165MHz で、
/// 下回るとクランプが効かなくなり画面の下ほど色がずれる(実測: 12.02MHz は正常、
/// 11.25MHz から崩れ、9.72MHz では白の青/赤比が上下で0.43違った)。
const TVP_DOTCLK_MIN: f64 = 12.0e6;
pub const PLL_MIN: i32 = 200;
/// ラインバッファの幅[サンプル](gateware の TvpCapture width)。1ラインが
/// 入り切らないと外接矩形が有効映像の幅を表さなくなるので、実用上限はこちら
/// (gateware の絶対上限 2304 ではない)。
pub const LINE_BUFFER_W: i32 = 2048;

/// 映像ソースを選んだときにボードへ書く TVP のレジスタ設定。
///
/// `host/python/retrocastx/videoin.py` の MODES の移植。**あちらが仕様の正**で、
/// 値の根拠(なぜミッドレベルクランプなのか、なぜ粗ゲインを一緒に下げるのか等)は
/// あちらのコメントと docs/composite-video-plan.md にある。
///
/// ★**どのモードも同じキー集合を書く。** 書かないキーがあると、方式を切り替えた
///   ときに前の方式の値が残る。Python側で実際にやらかしていて(RGB系で pll_divide を
///   省いていて composite → x68k で 1820 が残った)、回帰試験で見つかった。
///   ここでも `all_modes_write_the_same_keys` で担保する。
pub type InputRegs = &'static [(u16, u32, &'static str)];

/// 実機の水晶ひとつと、**その水晶で実在する分周比**。
///
/// 分周比を水晶ごとに持つのが要点。機種の中でも系統によって使える分周比が違い、
/// 共通のリストにすると実在しないドットクロックが候補に混ざる。X68000がまさに
/// そうで、69.552MHz側は /2,3,4,6,8 が全部使われるのに 38.864MHz側は /4,8 しか
/// 無い。共通リストにすると 38.864/2 = 19.432MHz という存在しない値が生まれ、
/// 他機種の fH に誤マッチする余地を作ってしまう。
pub struct Clock {
    pub name: &'static str,
    pub hz: f64,
    pub dividers: &'static [i32],
}

pub struct Profile {
    /// 設定ファイルに書く名前
    pub key: &'static str,
    pub label: &'static str,
    /// この映像ソースを選んだときにボードへ書く TVP レジスタ。
    /// 空なら pll_divide の推定に使うだけで、ボードには何も書かない。
    pub input_regs: InputRegs,
    /// 「自動」(プロファイル未選択)のときの候補に入れるか。
    ///
    /// コンポジット/S端子を入れてはいけない。8fsc(28.636MHz)は fH 31.5kHz でも
    /// htotal 909.1 という「それらしい」候補を出すので、RGB機の自動選択を横取りする。
    pub auto_pick: bool,
    /// 実機の水晶と分周比。モードごとのドットクロックを列挙するより漏れにくい。
    ///
    /// **ここが「簡易スキャン」の探索空間そのもの**でもある。fH と突き合わせて
    /// 整数の htotal になる組だけが候補になるので、機種を足すときはこの表だけを
    /// 正確に書けば、探索側は何も変えなくてよい。
    pub oscillators: &'static [Clock],
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

/// 入力MUX(19h)。[7:6]=SOG [5:4]=R [3:2]=G [1:0]=B、各 00=_1 / 01=_2 / 10=_3
///
/// ★**v0.9.0 で系統の割り当てが手組み機から変わっている。**
///   手組み機では S端子/コンポジット/MSX の映像をすべて3番系統(D-SUB)に
///   仮配線して同期だけ振り分けていたが、v0.9.0 は入力ごとに系統が分かれた
///   (main.ato で確認):
///
///     1番系統 … S端子/コンポジット(J5 2x4ヘッダ)
///                 svideo.y -> gin_1 / svideo.c -> rin_1
///                 Y から分岐した同期 -> sogin_1
///                 ※コンポジットは Y ピンに CVBS を入れる(同期も同じ1本)
///     2番系統 … 汎用の第2入力(aux 2x5ヘッダ。MSXや2台目のX68000)
///                 aux.r/g/b -> rin_2/gin_2/bin_2
///                 aux.csync -> 75Ω終端 -> 1/2分圧 -> sogin_2
///     3番系統 … D-SUB(X68000)  vin_r/g/b -> rin_3/gin_3/bin_3, sogin_3
const MUX_ALL3: u32 = 0xAA; // SOG/R/G/B すべて _3 … D-SUB(X68000)
const MUX_ALL2: u32 = 0x55; // SOG/R/G/B すべて _2 … 2x5ヘッダ(MSX/2台目)
const MUX_ALL1: u32 = 0x00; // SOG/R/G/B すべて _1 … S端子/コンポジット(J5)
/// 同期制御(0Eh)
const SYNC_5WIRE: u32 = 0x52; // HSYNC/VSYNC を別線で受ける(X68000)
const SYNC_SOG: u32 = 0x5B; // H も V も SOG から取る(MSXのCSYNC / コンポジット)

/// X68000 RGB: Rin3/Gin3/Bin3 + HSYNC_A/VSYNC_A
const REGS_X68000: InputRegs = &[
    (proto::CFG_KEY_IN_MUX1, MUX_ALL3, "SOG/R/G/B すべて _3(SOGは使わない)"),
    (proto::CFG_KEY_SYNC_CTL, SYNC_5WIRE, "HSYNC/VSYNCを別線で受ける"),
    (proto::CFG_KEY_IN_MUX2, 0x12, "HSYNC_A/VSYNC_A を選択"),
    (proto::CFG_KEY_SOG_THRESH, 0x0B, "SOGは使わないので既定"),
    // 31kHz(768x512)の起点。この後は下の「→ 適用」ボタンと自動調整が詰める
    (proto::CFG_KEY_PLL_DIVIDE, 1104, "起点=31kHzのhtotal(この後で詰める)"),
    (proto::CFG_KEY_PLL_CTL, 0x18, "ICP = 40×75/1104 = 2.7 → 3"),
    (proto::CFG_KEY_CLAMP_SEL, 0b000, "R/G/B全てボトムレベルクランプ"),
    (proto::CFG_KEY_COARSE_GAIN_GB, 0x77, "G/B粗ゲイン1.2倍(TVP既定)"),
    (proto::CFG_KEY_COARSE_GAIN_R, 0x07, "R粗ゲイン1.2倍(1Bhとビット割りが違う)"),
    (proto::CFG_KEY_COARSE_OFF_G, 0x10, "G粗オフセット既定(+64コード)"),
    (proto::CFG_KEY_COARSE_OFF_R, 0x10, "R粗オフセット既定(+64コード)"),
    (proto::CFG_KEY_CLAMP_START, 0x32, "クランプ開始(既定)"),
    (proto::CFG_KEY_CLAMP_WIDTH, 0x20, "クランプ幅(既定)"),
    // ★**細ゲインは基板ごとに測り直すこと。** アナログ3経路のばらつきを
    //   埋める値なので、基板が変われば正解が変わる。
    //
    //   2026-09-02 に v0.9.0 実機で再校正した。旧値(手組み試作機で測ったもの)は
    //   B=35 / G=33 / R=39 で、白ベタで **G だけが1コード低かった**
    //   (R=231 B=231 に対し G=222。RGB555 の隣接コードそのもので比は 0.961。
    //   量子化ステップ 1/28 = 3.6% がそのまま見えるので、1コードのずれが目に付く)。
    //
    //   校正手順: 白ベタの**内側だけ**(3ch>120 かつ左右の隣も同条件。縁の
    //   立ち上がり途中を除く)の中央値を見ながら、各chのゲインを振って
    //   「コード231(5bit 28)に載る範囲」の両端を出し、**中央を採る**。
    //   端に置くと温度ドリフトで1コード揺れる。実測したプラトー:
    //
    //       R: 39〜47 → 43     G: 35〜43 → 39     B: 31〜39 → 35
    //
    //   きれいに4ずつずれている(Rが最も弱くBが最も強い)。旧値では
    //   G=33 がプラトーの1つ下、R=39 が下端ぎりぎりだった。
    //   採用値で R=G=B=231 が5回連続で再現した。
    //
    //   ★測り方を途中で変えないこと。マスクの定義やツール(Viewerの
    //     クロップ済みダンプ / host側の全幅バッファ)を混ぜると、同じゲインで
    //     違う値が出て切り分けが壊れる(実際に一度混乱した)。
    //
    //   緑が落ちるのは、3chの中で緑だけが SOGIN_3 への結合コンデンサという
    //   追加の負荷を持つためと考えられる(Sync-on-Green 対応の分岐)。
    //   設計上の想定内で、それを埋めるのがこのレジスタ。
    //   ★2026-09-03: ohnakaさんの目視で **R=64 / G=61 / B=57 の方が鮮やか**
    //     とのことで既定にし、**実測でも裏づけが取れた**
    //     (x68k_calib のグレースケールを pll_divide=736 の1サンプル=1ドットで測定):
    //
    //       ゲイン      白             取り込めた段  欠けている5bit値
    //       39/33/35   239 (5bit 29)   28個         1, 2, 30, 31
    //       64/61/57   255 (5bit 31)   30個         1, 2
    //
    //     白が255なのは潰れているのではなく最上位の階調に到達したということ。
    //     飽和しているなら 30 と 31 が merge して段が減るはずだが、逆に2段増えた。
    //     残る問題は下端(5bit の 1,2 が落ちる)で、これはオフセット/クランプ側。
    (proto::CFG_KEY_GAIN_B, 57, "青ファインゲイン(×1.223。校正値35から目視で調整)"),
    (proto::CFG_KEY_GAIN_G, 61, "緑ファインゲイン(×1.238。校正値39から目視で調整)"),
    (proto::CFG_KEY_GAIN_R, 64, "赤ファインゲイン(×1.250。校正値43から目視で調整)"),
    (proto::CFG_KEY_PIXFMT, proto::PIXFMT_RGB555 as u32, "伝送はRGB555"),
    // ★インターレースのフィールド極性。**X68000は反転が正しい。**
    //   2026-09-03 に24kHz 1024x848 で実測: 織り込みのずれが 0=17.5 / 1=0.68。
    //   VSOUTがフレーム単位でもフィールド単位でも同じ結論だった。
    (proto::CFG_KEY_FIELD_INVERT, 1, "★フィールド極性を反転(実測で25倍良い)"),
    // ★VSOUTをフィールドごとに出させる。既定0x08だとハーフライン積算器が
    //   2フィールドに1回しかVを出さず、捕捉側が vtotal/2 で折り返す羽目になる。
    //   折り返し点は固定なのに実フィールド長は465/466で1行違うので、片方が必ずずれる。
    //   実測(24kHz 1024x848)の織り込みのずれ: 0x08 → 4.6〜5.0 / 0x09 → 1.39〜1.44
    (proto::CFG_KEY_SYNC_CTL2, 0x09, "★VSOUTを同期セパレータ直結(フィールドごと)"),
];

/// MSX RGB: Rin2/Gin2/Bin2 + SOGin2 に CSYNC (aux 2x5ヘッダ)
const REGS_MSX: InputRegs = &[
    // ★ここが 19h を CONFIG キーにした理由。SOGだけ _2 にする
    (proto::CFG_KEY_IN_MUX1, MUX_ALL2, "映像もCSYNCも2番系統(aux 2x5ヘッダ)"),
    (proto::CFG_KEY_SYNC_CTL, SYNC_SOG, "HもVもSOG(=CSYNC)から取る"),
    (proto::CFG_KEY_IN_MUX2, 0x12, "SOG LPF 2.5MHz + クランプLPF 0.5MHz"),
    (proto::CFG_KEY_SOG_THRESH, 0x0B, "CSYNCのスライス位置(実機で成立している既定)"),
    (proto::CFG_KEY_PLL_DIVIDE, 1368, "起点=MSXのhtotal(この後で詰める)"),
    (proto::CFG_KEY_PLL_CTL, 0x10, "ICP = 40×75/1368 = 2.2 → 2"),
    (proto::CFG_KEY_CLAMP_SEL, 0b000, "R/G/B全てボトムレベルクランプ"),
    (proto::CFG_KEY_COARSE_GAIN_GB, 0x77, "G/B粗ゲイン1.2倍(TVP既定)"),
    (proto::CFG_KEY_COARSE_GAIN_R, 0x07, "R粗ゲイン1.2倍"),
    (proto::CFG_KEY_COARSE_OFF_G, 0x10, "G粗オフセット既定"),
    (proto::CFG_KEY_COARSE_OFF_R, 0x10, "R粗オフセット既定"),
    (proto::CFG_KEY_CLAMP_START, 0x32, "クランプ開始(既定)"),
    (proto::CFG_KEY_CLAMP_WIDTH, 0x20, "クランプ幅(既定)"),
    (proto::CFG_KEY_GAIN_B, 35, "青ファインゲイン"),
    (proto::CFG_KEY_GAIN_G, 33, "緑ファインゲイン"),
    (proto::CFG_KEY_GAIN_R, 39, "赤ファインゲイン"),
    (proto::CFG_KEY_PIXFMT, proto::PIXFMT_RGB555 as u32, "伝送はRGB555"),
    (proto::CFG_KEY_FIELD_INVERT, 0, "フィールド極性は既定(MSXでは未実測)"),
    (proto::CFG_KEY_SYNC_CTL2, 0x08, "TVP既定のまま(MSXでは未実測)"),
];

/// コンポジット NTSC: Gin3 に CVBS、SOGin3 へ分岐
const REGS_COMPOSITE: InputRegs = &[
    (proto::CFG_KEY_IN_MUX1, MUX_ALL1, "SOG/R/G/B すべて _1(J5)"),
    (proto::CFG_KEY_SYNC_CTL, SYNC_SOG, "HもVもSOGから取る"),
    (proto::CFG_KEY_IN_MUX2, 0x12, "SOG LPF 2.5MHz(バーストで誤トリガしない)"),
    (proto::CFG_KEY_SOG_THRESH, 0x0B, "CVBSのスライス位置(実機で成立している既定)"),
    (proto::CFG_KEY_PLL_DIVIDE, 1820, "PLL分周 = 8fsc NTSC(規格で決まる)"),
    (proto::CFG_KEY_PLL_CTL, 0x10, "VCO=UltraLow + チャージポンプ2"),
    (proto::CFG_KEY_CLAMP_SEL, 0b010, "Greenだけミッドレベル(バーストを丸ごと入れる)"),
    (proto::CFG_KEY_COARSE_GAIN_GB, 0x07,
     "★Green粗ゲイン0.5倍。ミッドレベルのヘッドルーム半減を吸収する"),
    (proto::CFG_KEY_COARSE_GAIN_R, 0x07, "Redは未使用(既定)"),
    (proto::CFG_KEY_COARSE_OFF_G, 0x10, "G粗オフセット既定(ファインクランプ下では効かない)"),
    (proto::CFG_KEY_COARSE_OFF_R, 0x10, "R粗オフセット既定"),
    (proto::CFG_KEY_CLAMP_START, 230, "クランプ開始をバーストの後ろへ"),
    (proto::CFG_KEY_CLAMP_WIDTH, 30, "クランプ幅"),
    (proto::CFG_KEY_GAIN_B, 35, "青は未使用(RGBの既定)"),
    (proto::CFG_KEY_GAIN_G, 0,
     "★緑ファインゲイン1.000倍。33(=1.129倍)だと白が4.6%クリップした"),
    (proto::CFG_KEY_GAIN_R, 39, "赤は未使用(RGBの既定)"),
    (proto::CFG_KEY_PIXFMT, proto::PIXFMT_YC8 as u32, "生8bit伝送(復調に必要)"),
    (proto::CFG_KEY_FIELD_INVERT, 0, "フィールド極性は既定(SOG経路は別の符号)"),
    (proto::CFG_KEY_SYNC_CTL2, 0x08, "TVP既定のまま(この方式では未実測)"),
];

/// S端子: Gin3 = Y / Rin3 = C、SOGin3 へ Y から分岐
const REGS_SVIDEO: InputRegs = &[
    (proto::CFG_KEY_IN_MUX1, MUX_ALL1, "SOG/R/G/B すべて _1(J5)"),
    (proto::CFG_KEY_SYNC_CTL, SYNC_SOG, "HもVもSOG(Yから分岐)から取る"),
    (proto::CFG_KEY_IN_MUX2, 0x12, "SOG LPF 2.5MHz"),
    (proto::CFG_KEY_SOG_THRESH, 20,
     "★Yのスライス位置。既定0x0Bだと暗部を同期と誤認して極性が判定できず、\n\
      絵が半ライン上下に震えた。実測で16〜24が正しい窓、その中央"),
    (proto::CFG_KEY_PLL_DIVIDE, 1820, "PLL分周 = 8fsc NTSC"),
    (proto::CFG_KEY_PLL_CTL, 0x10, "VCO=UltraLow + チャージポンプ2"),
    // ★**Yはボトムレベル + クランプ窓を同期チップの中へ。** ここは2回間違えた。
    //
    //   1回目: 窓をバックポーチ(230)に置いたままボトムにした → バックポーチが底に
    //          張り付き、その40 IRE下の同期がクリップ。デコーダは
    //          (ブランキング-同期チップ)/40 で校正するので**輝度が5倍**になり白飛びした
    //   2回目: そこで「Yはミッドレベル一択」と結論した → **これも誤り**。
    //          ミッドはブランキングが中央(164)に座るので上に91コードしか残らず、
    //          粗ゲインを最小0.5倍にしても白がコード255に当たって 1.87% 飽和していた
    //
    //   正解は**窓を同期チップの中(60〜100)へ動かしてボトム**。同期チップ自体が
    //   底に来るので、上が丸ごと空く。実測(粗ゲインを振って比較):
    //
    //       ミッド/窓230/粗0.5倍   ブランク164  1 IRE=0.900  白飽和 1.87%
    //       ボトム/窓60幅40/粗0.8倍 ブランク 58  1 IRE=1.450  白飽和 0.00%
    //
    //   輝度の分解能が1.61倍になり、白の飽和が消える。黒0 IRE・白100 IREは維持。
    //   窓は同期チップ(0〜127サンプル)の**中に十分収める**こと。60幅40は通るが、
    //   20幅60(同期の縁に掛かる)では壊れた。
    //   粗ゲインは0.9倍でも飽和しないが、白の上の余裕が13 IREしか無い。0.8倍なら
    //   34 IRE残るので135 IREのピークまで耐える。
    (proto::CFG_KEY_CLAMP_SEL, 0b001, "★Yはボトム(同期チップをクランプ)/ Cはミッド"),
    (proto::CFG_KEY_COARSE_GAIN_GB, 0x37,
     "★Y粗ゲイン0.8倍。1 IRE=1.450コード(ミッド時の0.900の1.61倍)"),
    (proto::CFG_KEY_COARSE_GAIN_R, 0x07,
     "C粗ゲイン1.2倍。実測でバースト88.5コード=設計値ちょうどだった"),
    (proto::CFG_KEY_COARSE_OFF_G, 0x10, "G粗オフセット既定"),
    (proto::CFG_KEY_COARSE_OFF_R, 0x10, "R粗オフセット既定"),
    (proto::CFG_KEY_CLAMP_START, 60, "★クランプ窓を同期チップの中へ(上記)"),
    (proto::CFG_KEY_CLAMP_WIDTH, 40, "同期チップ(0〜127)に十分収まる幅"),
    (proto::CFG_KEY_GAIN_B, 35, "青は未使用(RGBの既定)"),
    (proto::CFG_KEY_GAIN_G, 0, "Yのファインゲイン1.000倍(粗ゲインで合わせる)"),
    (proto::CFG_KEY_GAIN_R, 0, "Cのファインゲイン1.000倍(実測後に詰める)"),
    (proto::CFG_KEY_PIXFMT, proto::PIXFMT_YC8 as u32, "生8bit伝送(復調に必要)"),
    (proto::CFG_KEY_FIELD_INVERT, 0, "フィールド極性は既定(SOG経路は別の符号)"),
    (proto::CFG_KEY_SYNC_CTL2, 0x08, "TVP既定のまま(この方式では未実測)"),
];

/// 実測で裏を取れているのは x68000 と msx。
///
/// x68000 は3帯域すべてが2つの水晶の分周で説明でき、このプロジェクトの記録
/// (31kHz→1104 / 24kHz→1408 / 15kHz→1216)と一致した。
/// msx は実機の fH 15.699kHz から 342.0165(ずれ0.017)が出て、Viewer の表示
/// (dotclk 21.4773MHz / total 1368x262)とも一致した。
/// vga / pc98 は未検証で、候補の並びを見る参考に留めること。
///
/// composite / svideo は「配線の方式」で決まるソースで、pll_divide は規格値。
/// 選ぶと `input_regs` がボードへ書かれる。

pub const PROFILES: &[Profile] = &[
    Profile {
        key: "x68000",
        label: "X68000 RGB",
        input_regs: REGS_X68000,
        auto_pick: true,
        // ★**XEiJ(X68000エミュレータ)の CRTC.java の実装表から起こした。**
        //   https://stdkmd.net/xeij/source/xeij-CRTC.java.htm
        //
        //   ドットクロックは R20 の bit4(解像度) と bit1-0(水平解像度)、それに
        //   $E8E007 bit1 の HRL で決まる。XEiJ の CRT_OSCS / CRT_DIVS を展開すると:
        //
        //     低解像度(bit4=0) … 38.86363MHz の /8(256系) と /4(512系) だけ。
        //                        bit1-0 が 2/3 でも /8 に落ちるので新しい値は出ない
        //     高解像度(bit4=1) … 69.55199MHz の /6(256) /3(512) /2(768)、
        //                        HRL=1 で /6→/8、/3→/4 に変わる
        //     VGA(bit4=1, bit1-0=3) … 50.3498MHz の /2 のみ
        //
        //   これを水晶ごとの分周比として書くと下の3行になる。XEiJ の実測表
        //   (垂直周期を $00〜$1F × HRL 0/1 の64通りで測ったもの)が理論値と
        //   数μs以内で一致しているので、実機の裏付けもある。
        //
        //   ★**/6 と 50.3498MHz は 2026-09-03 に追加した。** それまでは水晶2つ×
        //     共通の分周比 [1,2,3,4,8] で、(a) 高解像度256x256(/6=11.592MHz、
        //     htotal 368)と (b) VGA 640x480(25.1749MHz、htotal 800)が候補に
        //     出せなかった。(b) は「このプロファイルでは説明できない fH」に
        //     なっていた。逆に実在しない 38.864/1,/2,/3 と 69.552/1 は落とした。
        //
        //   ★**3 が要る。** 512ドット系のグラフィック画面(_CRTMOD 12 など)は
        //     69.55199MHz/3 = 23.184MHz で、31.5kHz なら htotal 736 になる。
        //     2026-09-02 に v0.9.0 実機で実測(校正パターンの4px縞の自己相関で
        //     8サンプル/8ドット = 1.000 サンプル/ドットを確認)。
        //     3 が無いと 1104(=/2)を「一致」と誤判定して 1.5倍オーバーサンプリングに
        //     なり、階調が隣と混ざって校正が成立しない。
        oscillators: &[
            Clock { name: "69.55199MHz", hz: 69.551_99e6, dividers: &[2, 3, 4, 6, 8] },
            Clock { name: "38.86363MHz", hz: 38.863_63e6, dividers: &[4, 8] },
            Clock { name: "50.3498MHz", hz: 50.349_8e6, dividers: &[2] },
        ],
        htotal_multiple: 8,
        verified: true,
    },
    Profile {
        key: "msx",
        label: "MSX RGB",
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
        input_regs: REGS_MSX,
        auto_pick: true,
        oscillators: &[Clock {
            name: "21.47727MHz",
            hz: 21.477_270e6,
            dividers: &[2, 4],
        }],
        // MSXのVDPは htotal 342 固定(512ドット系でもドットクロックが倍になるだけ)
        // なので粒度の概念が無い。制約は付けず、相対誤差だけで判定させる。
        htotal_multiple: 1,
        verified: true,
    },
    Profile {
        key: "vga",
        label: "VGA互換 (未検証)",
        input_regs: &[],
        auto_pick: true,
        oscillators: &[
            Clock { name: "25.175MHz", hz: 25.175e6, dividers: &[1, 2] },
            Clock { name: "28.322MHz", hz: 28.322e6, dividers: &[1, 2] },
        ],
        htotal_multiple: 1,
        verified: false,
    },
    Profile {
        key: "pc98",
        label: "PC-9801 (未検証)",
        input_regs: &[],
        auto_pick: true,
        oscillators: &[
            Clock { name: "21.0525MHz", hz: 21.052_5e6, dividers: &[1, 2] },
            Clock { name: "25.175MHz", hz: 25.175e6, dividers: &[1, 2] },
        ],
        htotal_multiple: 1,
        verified: false,
    },
    // --- ここから下は「配線の方式」で決まる映像ソース。選ぶと入力設定を書く ---
    //
    // pll_divide は規格で 1820(= 8fsc NTSC)に決まるので、水晶を1つ置いておけば
    // 下の推定表示もそのまま 1820 で一致する。ただし auto_pick は false。
    // 8fsc は fH 31.5kHz でも htotal 909.1 という「それらしい」候補を出すので、
    // 「自動」に混ぜるとRGB機の選択を横取りしてしまう。
    Profile {
        key: "composite",
        label: "コンポジットビデオ (NTSC)",
        input_regs: REGS_COMPOSITE,
        auto_pick: false,
        oscillators: &[Clock {
            name: "NTSC 8fsc 28.63636MHz",
            hz: 28.636_360e6,
            dividers: &[1],
        }],
        htotal_multiple: 1,
        verified: true,
    },
    Profile {
        key: "svideo",
        label: "S端子 (NTSC)",
        input_regs: REGS_SVIDEO,
        auto_pick: false,
        oscillators: &[Clock {
            name: "NTSC 8fsc 28.63636MHz",
            hz: 28.636_360e6,
            dividers: &[1],
        }],
        htotal_multiple: 1,
        verified: false,
    },
];

impl Profile {
    /// この映像ソースが要求する伝送形式。`input_regs` が持っていなければ None。
    ///
    /// ボードが返してくる pixfmt と突き合わせると「電源を入れ直して設定が消えた」
    /// のが分かる。ビットストリームはフラッシュに残るが、TVPのレジスタは消える。
    pub fn pixfmt(&self) -> Option<u8> {
        self.input_regs
            .iter()
            .find(|(k, _, _)| *k == proto::CFG_KEY_PIXFMT)
            .map(|(_, v, _)| *v as u8)
    }
}

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
    for osc in p.oscillators {
        let (name, f) = (osc.name, osc.hz);
        for d in osc.dividers {
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
                    name.to_string()
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

/// 簡易スキャンが測る1点。
///
/// ★**htotal と pll_divide を別々に持つのが要点。** TVPのピクセルクロック下限
///   12MHz のせいで、両者は一致しないことがある。X68000の高解像度256x256は
///   ドットクロック 11.592MHz・htotal 368 で、そのまま設定すると下限を割るので
///   pll_divide は2倍の 736 にする(整数倍なので位相ロックし、情報は落ちない)。
///
///   スキャン中に占有率から推定できるのは**入力の htotal の方**で、pll_divide の
///   方ではない。設定値 P で測った占有率は「真の htotal H ÷ P」になるので
///   P×占有率 = H。ここを取り違えると、下限で強制的に2倍オーバーサンプルに
///   なる帯域(X68000の15kHz)で必ず1段下へ降り過ぎる。
#[derive(Clone, Debug)]
pub struct ScanPoint {
    /// 入力の水平トータル[ドット]。占有率から推定するのはこちら
    pub htotal: u32,
    /// 実際に設定する値
    pub pll_divide: u32,
    /// pll_divide / htotal。1なら1サンプル=1ドット
    pub oversample: u32,
}

/// 「簡易スキャン」の探索空間。**この fH で実在しうるモードの全部**を、
/// htotal の小さい順に返す。
///
/// `best()` との違いは、1つに絞らないこと。fH だけでは決まらないからで、
/// X68000 の 31.5kHz がそのまま例になる:
///
/// ```text
///     69.552/2 = 34.776MHz  htotal 1104 → pll 1104      768x512 / 1024x848
///     69.552/3 = 23.184MHz  htotal  736 → pll  736      512x512(高解像度)
///     69.552/4 = 17.388MHz  htotal  552 → pll  552      HRL=1(下記)
///     69.552/6 = 11.592MHz  htotal  368 → pll  736 (×2) 256x256(高解像度)
/// ```
///
/// どれも fH は 31.500kHz ちょうどで、信号だけからは見分けられない。`best()` は
/// 「迷ったら大きい側」で 1104 を返すので、実際が 736 なら 1.5倍オーバーサンプルの
/// ままになり、隣のドットが混ざる。**この4点(設定値としては552/736/1104の3通り)
/// だけを絵で測って選ぶ**のが簡易スキャン。フルスキャンが8刻みで数十点を舐めるのに
/// 対し、原理的にこれで足りる。
///
/// 5つめの 69.552/8 = 8.694MHz は htotal 276 になるが、**8の倍数でないので落ちる**
/// (276 = 34.5カラム。X68000のCRTCは水平トータルをカラム=8ドット単位で持つので
/// 設定できない)。`htotal_multiple` の制約がここで効いている。
///
/// 552 は標準の CRTMOD には出てこない(HRL=1 で /4 になったうえで水平トータルが
/// 69カラムのとき)が、CRTCのレジスタは自由に書けるので候補には残す。粒度を
/// 満たす=信号を説明できる以上、除く根拠が無い。
///
/// htotal が同じ点はまとめる(同じモードを別の水晶で説明しただけ)。pll_divide が
/// 同じでも htotal が違う点(上の 736 と 368)は**残す**。設定値は同じでも
/// 「1:1なのか2倍オーバーサンプルなのか」が違い、最後の判定に効く。
pub fn scan_candidates(p: &Profile, fh_hz: f64) -> Vec<ScanPoint> {
    let cs = candidates(p, fh_hz, 0.15, LINE_BUFFER_W);
    // 粒度(X68000なら8の倍数)を満たすものがあるなら、満たさないものは捨てる。
    // best() と同じ判断。粒度を満たさない候補は「たまたま整数に近かった」だけ
    let any_mult = cs.iter().any(|c| c.multiple_ok);
    let pool: Vec<&Candidate> = cs.iter().filter(|c| !any_mult || c.multiple_ok).collect();
    // ★**水晶の一族で絞る。`best()` と同じ判断をここでもやる。**
    //
    //   相対誤差(残差÷htotal)は分周比に依らない量なので、正しい水晶から来た
    //   候補は同じ値に揃い、違う水晶なら桁で外れる。これを捨てると測る必要の
    //   無い候補が混ざる。実例: X68000の15.98kHz は
    //
    //       38.864/4 → htotal  608(相対誤差 1.2e-6)… 正解。pll は×2の1216
    //       69.552/4 → htotal 1088(相対誤差 9.2e-5)… 80倍悪い。別の水晶
    //
    //   1088 は8の倍数なので粒度では落ちず、しかも 608 とは倍率が半端(1.79倍)。
    //   絵で測って見分けるのは難しいのに、信号の側では80倍の差で決着している。
    //   絞ると 15.98kHz の候補は pll 1216 の1通りだけになり、測らずに確定する。
    let lo = pool.iter().map(|c| c.rel_err()).fold(f64::INFINITY, f64::min);
    let mut v: Vec<ScanPoint> = pool
        .iter()
        .filter(|c| c.rel_err() <= (lo * 3.0).max(1e-9))
        .map(|c| ScanPoint {
            htotal: c.htotal as u32,
            pll_divide: c.pll_divide as u32,
            oversample: c.oversample as u32,
        })
        .collect();
    v.sort_by_key(|s| s.htotal);
    v.dedup_by_key(|s| s.htotal);
    v
}

/// プロファイルを人が選ばない場合。全部試していちばん確からしいものを返す。
pub fn best_over_all(fh_hz: f64) -> Option<(&'static Profile, Candidate)> {
    let mut ranked: Vec<(&'static Profile, Candidate)> = PROFILES
        .iter()
        .filter(|p| p.auto_pick)
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

    /// X68000の31.5kHz帯は**1つの fH に5つのモードが乗っている**。
    ///
    /// XEiJ の CRTC.java から起こしたドットクロック表がそのまま出てくるかを見る。
    /// ここが縮んだら簡易スキャンが選べる範囲が縮んだということで、
    /// 「そのモードでは1.5倍オーバーサンプルのまま」になる。
    #[test]
    fn x68000_31khz_scan_covers_every_mode() {
        let p = by_key("x68000").unwrap();
        let got: Vec<(u32, u32, u32)> = scan_candidates(p, 31500.0)
            .iter()
            .map(|s| (s.htotal, s.pll_divide, s.oversample))
            .collect();
        // (htotal, pll_divide, oversample)
        //   368 = 69.552/6 (高解像度256x256)  11.592MHz は12MHz未満なので×2
        //   552 = 69.552/4 (HRL=1)
        //   736 = 69.552/3 (高解像度512x512)
        //  1104 = 69.552/2 (768x512 / 1024x848)
        //
        // 69.552/8 = 8.694MHz の htotal 276 は**8の倍数でないので落ちる**
        // (34.5カラムはCRTCに設定できない)。落ちるのが正しい
        assert_eq!(
            got,
            vec![(368, 736, 2), (552, 552, 1), (736, 736, 1), (1104, 1104, 1)],
            "31.5kHzの候補が変わった"
        );
        assert!(
            scan_candidates(p, 31500.0).iter().all(|s| s.htotal % 8 == 0),
            "8の倍数でない htotal が混ざった"
        );
    }

    /// **VGAモード(50.3498MHz)を説明できること。**
    ///
    /// 50.3498MHz の水晶は X68000 Compact(IPLROM 1.2)で追加されたもので、
    /// これが表に無いと fH 31.469kHz が「説明できない fH」になる(実際になっていた)。
    /// 候補は1つだけなので、簡易スキャンは測らずに確定して位相だけ詰める。
    #[test]
    fn x68000_vga_mode_is_explained() {
        let p = by_key("x68000").unwrap();
        let c = best(p, 31469.0).expect("VGAモードの候補が無い");
        assert_eq!(c.pll_divide, 800, "選んだのは {}", c.pll_divide);
        assert_eq!(c.htotal, 800);
        assert_eq!(c.oversample, 1);
        let sc = scan_candidates(p, 31469.0);
        assert_eq!(sc.len(), 1, "VGAは候補1つのはず: {sc:?}");
    }

    /// 15kHz帯は**TVPの下限12MHzで強制的に過剰サンプルになる**帯域。
    ///
    /// 真の htotal は 608(38.864/4 = 9.716MHz)なので、pll_divide は2倍の1216。
    /// ★このとき 69.552/4 = 17.388MHz も htotal 1088 で8の倍数なので粒度では
    ///   落ちないが、相対誤差が 9.2e-5 と正解(1.2e-6)より80倍悪い。**水晶の
    ///   一族で絞る**ので候補から外れ、結果として設定値は1216の1通りだけになる。
    ///   絵で見分けるのが難しい半端な倍率(1088/608 = 1.79倍)の相手を、
    ///   信号の側で決着させている。
    #[test]
    fn x68000_15khz_scan_is_a_single_setting() {
        let p = by_key("x68000").unwrap();
        let sc = scan_candidates(p, 15980.0);
        assert!(
            sc.iter().all(|s| s.pll_divide == 1216),
            "15kHzの候補に別の設定値が混ざった: {sc:?}"
        );
        let truth = sc.iter().find(|s| s.htotal == 608).expect("htotal 608 が無い");
        assert_eq!(truth.oversample, 2);
        assert!(
            !sc.iter().any(|s| s.htotal == 1088),
            "別の水晶(69.552/4)の候補が残っている: {sc:?}"
        );
    }

    /// 簡易スキャンの降り方の再現。
    ///
    /// 判定は**位相感度**(同じ設定値での鮮鋭度の最悪÷最良)で、1:1のときだけ
    /// 小さくなる。ここでは「設定値が真の htotal と一致するときだけ感度が立つ」と
    /// 置いて、htotal の大きい方から降りるループを机上で回す。
    ///
    /// ★占有率(スペクトルがNyquistまで埋まる割合)では判定しない。入力の帯域が
    ///   足りない機種で破綻するため。X68000の768x512は1ドットが白の64%までしか
    ///   立たないので、正解の1104で測っても占有率は0.7程度しか出ず、
    ///   1104×0.7 = 773 から「736だ」と1段降りてしまう(実機で発生)。
    #[test]
    fn quick_scan_descends_only_on_evidence() {
        let p = by_key("x68000").unwrap();
        let mut all = scan_candidates(p, 31500.0);
        all.sort_by(|a, b| b.htotal.cmp(&a.htotal));
        // 測る順番は設定値の重複を除いたもの
        let mut ladder: Vec<&ScanPoint> = Vec::new();
        for c in &all {
            if !ladder.iter().any(|l| l.pll_divide == c.pll_divide) {
                ladder.push(c);
            }
        }
        assert_eq!(
            ladder.iter().map(|l| l.pll_divide).collect::<Vec<_>>(),
            vec![1104, 736, 552],
            "測る順番(htotal降順・設定値の重複なし)"
        );
        for &(truth_h, want) in &[(1104u32, 1104u32), (736, 736), (552, 552)] {
            let hit = ladder.iter().find(|l| l.htotal == truth_h);
            assert_eq!(
                hit.map(|l| l.pll_divide),
                Some(want),
                "真の htotal {truth_h} が候補に無い"
            );
            // 上にある候補では感度が立たない(過剰サンプル)ので素通りし、
            // 一致した点で止まる = want に着く
            let stop = ladder
                .iter()
                .find(|l| l.htotal == truth_h)
                .unwrap()
                .pll_divide;
            assert_eq!(stop, want);
        }
        // 真の htotal 368(高解像度256x256、11.592MHz)は ladder に無い。
        // 12MHzの下限で1:1にできないモードなので、どの候補でも位相感度は立たず、
        // フォールバック = 安全側の最大値(pll 1104)に戻る。1104 は 368 の
        // ちょうど3倍なので位相ロックしていて、眠いだけでドットは混ざらない。
        assert_eq!(
            ladder[0].pll_divide % 368, 0,
            "戻り先の pll {} が htotal 368 の整数倍でない(位相ロックせず混ざる)",
            ladder[0].pll_divide
        );
    }

    /// 実在しない分周は候補に出さない。
    ///
    /// 低解像度側(38.864MHz)は /4 と /8 だけで、/1 /2 /3 は存在しない
    /// (XEiJ の CRT_DIVS を見れば分かる)。実在しない値を混ぜると、他機種の
    /// fH に「それらしい」候補を出して自動選択を横取りする余地を作る。
    #[test]
    fn x68000_has_only_real_dividers() {
        let p = by_key("x68000").unwrap();
        let want: &[(&str, &[i32])] = &[
            ("69.55199MHz", &[2, 3, 4, 6, 8]),
            ("38.86363MHz", &[4, 8]),
            ("50.3498MHz", &[2]),
        ];
        assert_eq!(p.oscillators.len(), want.len());
        for (osc, (name, divs)) in p.oscillators.iter().zip(want) {
            assert_eq!(osc.name, *name);
            assert_eq!(osc.dividers, *divs, "{name} の分周比");
        }
    }

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

    /// ★**入力設定を持つソースは全部おなじキー集合を書くこと。**
    ///
    /// 書かないキーがあると、方式を切り替えたときに前の方式の値が残る。
    /// Python側で実際にやらかしていて(RGB系で pll_divide を省いていたので
    /// composite → x68k と切り替えると 1820 が残った)、回帰試験で見つかった。
    /// 「絵は出るが様子がおかしい」形で失敗するので、目視では気づきにくい。
    #[test]
    fn all_input_modes_write_the_same_keys() {
        let sets: Vec<(&str, Vec<u16>)> = PROFILES
            .iter()
            .filter(|p| !p.input_regs.is_empty())
            .map(|p| {
                let mut ks: Vec<u16> = p.input_regs.iter().map(|(k, _, _)| *k).collect();
                ks.sort_unstable();
                (p.key, ks)
            })
            .collect();
        assert!(sets.len() >= 4, "入力設定を持つソースが足りない: {}", sets.len());
        let (first_key, first) = &sets[0];
        for (key, ks) in &sets[1..] {
            assert_eq!(
                ks, first,
                "{key} と {first_key} で書くキーが違う。\
                 足りない側は方式を切り替えたときに前の値が残る"
            );
        }
        // キーの重複も禁止(同じキーを2回書くと後勝ちで意図が消える)
        for (key, ks) in &sets {
            let mut u = ks.clone();
            u.dedup();
            assert_eq!(u.len(), ks.len(), "{key} に重複したキーがある");
        }
    }

    /// コンポジット/S端子は「自動」の候補に入れてはいけない。
    ///
    /// 8fsc(28.636MHz)は fH 31.5kHz でも htotal 909.1 という残差の小さい候補を
    /// 出すので、混ぜるとRGB機の自動選択を横取りする。
    #[test]
    fn composite_never_wins_auto() {
        for fh in [31499.0, 24699.0, 15980.0, 15699.0] {
            let (p, _) = best_over_all(fh).expect("候補なし");
            assert!(
                p.auto_pick,
                "fH={fh} で自動が {} を選んだ(auto_pick=false のはず)",
                p.key
            );
        }
        // 明示的に選んだときは 1820 が出ること(規格値)
        let p = by_key("composite").unwrap();
        let c = best(p, 15734.264).expect("候補なし");
        assert_eq!(c.pll_divide, 1820, "コンポジットの pll_divide が規格値でない");
        assert_eq!(p.pixfmt(), Some(crate::protocol::PIXFMT_YC8));
        assert_eq!(by_key("x68000").unwrap().pixfmt(),
                   Some(crate::protocol::PIXFMT_RGB555));
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
