#!/usr/bin/env python3
"""映像入力の方式を切り替える。全部 CONFIG だけで済み、焼き直しは要らない。

対応する方式(**現在の手組みボードの配線**に合わせてある):

    モード       映像               同期                    TVP側
    x68k        Rin3/Gin3/Bin3     HSYNC_A / VSYNC_A       19h=0xAA 0Eh=0x52
    msx         Rin3/Gin3/Bin3     SOGin2 (CSYNC)          19h=0x6A 0Eh=0x5B
    composite   Gin3 (CVBS)        SOGin3 (Gin3から分岐)   19h=0xAA 0Eh=0x5B
    svideo      Gin3=Y / Rin3=C    SOGin3 (Y から分岐)     19h=0xAA 0Eh=0x5B

★**msx だけ SOG の入力ピンが違う**(SOGIN_2)。19h は以前ビルド時定数だったので、
  実行時に切り替えられなかった。key 0x69 を足して解決した。

    # 方式を切り替える
    python3 -m retrocastx.videoin --board 192.168.10.50 apply x68k
    python3 -m retrocastx.videoin --board 192.168.10.50 apply composite

    # ★どれが繋がっているかを当てて、その設定を入れる
    python3 -m retrocastx.videoin --board 192.168.10.50 auto

    # 同期とPLLがロックしたかを見る
    python3 -m retrocastx.videoin --board 192.168.10.50 status

    # コンポジット/S端子: 波形を実測してクランプとゲインを数値で判定する
    python3 -m retrocastx.videoin --board 192.168.10.50 probe

    # ★そのチャネルに同期パルスが本当に載っているかを切り分ける(配線の切り分け)
    python3 -m retrocastx.videoin --board 192.168.10.50 synctest

`auto` が成立する理由。3方式は排他配線で、**同期がどこに来るかが違う**:

    生HSYNC/VSYNC(TTL)に信号がある            → x68k
    SOGIN_2 に同期がある(19h=0x6A で確認)    → msx
    SOGIN_3 に同期がある(19h=0xAA で確認)    → composite / svideo

生同期は TVP を通らない経路を sys ドメインで数えているので(key 0x2A)、
TVPの設定に関係なく読める。これが最初の分岐に使えるのが大きい。

なぜ `probe` が要るか。コンポジット系では「絵が出るか」では判定できない。

  - ボトムレベルクランプだとブランキングが低い所に座るので、それを中心に
    振れるカラーバースト(±20 IRE)の**下半分が消える**。絵は出るが復調できない
  - ミッドレベルクランプにすると白側のヘッドルームが半分になるので、粗ゲインを
    同時に下げないと**白が飽和する**。これも絵は出る(白が塗り潰れるだけ)

どちらも「絵は出ているのに使えない」形で失敗する。だから同期チップ・ブランキング・
バースト振幅をコードで読んで、飽和していないことを確かめる。

設計と根拠は docs/composite-video-plan.md にある。
"""
import argparse
import math
import socket
import statistics
import time

from . import protocol as proto
from .cfg import Cfg
from .line_probe import collect

# --- NTSC の1ライン(同期立ち下がりを0とする)[µs] ---
# 8fsc(28.636MHz)なら 1サンプル = 34.925ns。
SYNC_TIP   = (0.5, 4.2)     # 同期チップ。端は避ける(遷移が入る)
BREEZEWAY  = (4.9, 5.2)
BURST      = (5.4, 7.7)     # カラーバースト 3.58MHz × 9周期
BACKPORCH  = (7.9, 9.3)     # バースト後のバックポーチ = クランプの基準
ACTIVE     = (9.6, 62.0)

FSC_NTSC = 3_579_545.0      # NTSC カラーサブキャリア [Hz]
IRE_V = 1.0 / 140.0         # コンポジット 1Vpp = 140 IRE
# バースト振幅(40 IRE)の合格下限 [8bitコード p-p]。
# ミッドレベルクランプ + 粗ゲイン0.5倍なら 1 IRE = 0.915コードで 36コード出る。
# 参考: RGB555(5bit)では同じ条件で 4〜5段階しか残らず、位相推定が成立しない。
BURST_PP_MIN = 24

# --- 入力MUX(reg 19h)の値 ---
# [7:6]=SOG [5:4]=Red [3:2]=Green [1:0]=Blue、各 00=_1 / 01=_2 / 10=_3。
MUX_SOG3 = 0xAA         # SOG=_3, R/G/B=_3 … コンポジット / S端子 / X68000
MUX_SOG2 = 0x6A         # SOG=_2, R/G/B=_3 … MSX(CSYNCがSOGIN_2に来る)

# --- 同期制御(reg 0Eh)の値 ---
SYNC_5WIRE = 0x52       # HSYNC/VSYNC を別線で受ける(X68000)
SYNC_SOG   = 0x5B       # H も V も SOG から取る(MSXのCSYNC / コンポジット)

# --- 方式ごとのレジスタ設定 ---
#
# ★**どのモードも同じキー集合を書く。** 書かないキーがあると、方式を切り替えた
#   ときに前の方式の値が残る。実際、当初はRGB系で pll_divide を省いていて、
#   composite → x68k と切り替えると 1820 のまま残った(回帰試験で発覚)。
#
#   RGB系の pll_divide/pll_ctl は**起点の値**でしかない。実際の値は機種の
#   ビデオモード(31/24/15kHz)で変わるので、Viewerの映像ソースプロファイルと
#   `pll_tune` が上書きする。ここで書くのは「前の方式の値を消す」ためで、
#   最終値を決めるためではない。コンポジット/S端子は規格で1820に決まる。
#
#   pll_ctl は pll_divide と対で決まる(ICP = 40 × KVCO / pixels_per_line、
#   VCO=00 なら KVCO=75)。片方だけ変えると H-PLL のループ利得がずれる。
#
# 値の根拠は docs/composite-video-plan.md と gateware/retrocastx_i2c.py のコメント。
#   pll_divide 1820 = 8fsc NTSC (28.636MHz / 15.734kHz)
#   pll_ctl    ICP = 40 × KVCO / pixels_per_line = 40×75/1820 = 1.65 → 2
#              → VCO=00(Ultra low) + ICP=2 = 0b00_010_000
#   clamp      既定の 0x32(=50クロック=1.75µs)は**同期チップの上**なので駄目。
#              バーストの後ろ(7.9〜9.3µs = 226〜266クロック)に置く
#   clamp_sel  bit2=Blue bit1=Green bit0=Red、1=ミッドレベル

# ファインゲイン(reg 08h/09h/0Ah)。Gain = 1 + N/256 で、**下げられない**(N=0が1.0倍)。
# 既定 35/33/39 は RGB入力で白のチャネル間バランスを取った実測値。
#
# ★コンポジットでは **G を 0(1.000倍)にする**。粗ゲインが既に最小(0.5倍)なのに
#   ファインゲインが 1.129倍 掛かっていて、**白が 4.6% クリップしていた**
#   (実測 2026-08-14: ブランキング167 / 1 IRE = 1.00コード → 白100 IREが267)。
#   0 にすると 250 に収まりクリップ 0.00%。バーストは 38→33コードに減るが
#   設計値36の近くで、合格下限24より十分上。
_FINE_GAIN_RGB = [
    (proto.CFG_KEY_GAIN_B, 35, "青ファインゲイン(RGB入力で実測した値)"),
    (proto.CFG_KEY_GAIN_G, 33, "緑ファインゲイン(同)"),
    (proto.CFG_KEY_GAIN_R, 39, "赤ファインゲイン(同)"),
]

_RGB_ANALOG = _FINE_GAIN_RGB + [
    (proto.CFG_KEY_CLAMP_SEL,      0b000, "R/G/B全てボトムレベルクランプ"),
    (proto.CFG_KEY_COARSE_GAIN_GB,  0x77, "G/B粗ゲイン1.2倍(TVP既定)"),
    (proto.CFG_KEY_COARSE_GAIN_R,   0x07, "R粗ゲイン1.2倍(TVP既定。1Bhとビット割りが違う)"),
    (proto.CFG_KEY_COARSE_OFF_G,    0x10, "G粗オフセット既定(+64コード)"),
    (proto.CFG_KEY_COARSE_OFF_R,    0x10, "R粗オフセット既定(+64コード)"),
    (proto.CFG_KEY_CLAMP_START,     0x32, "クランプ開始(既定)"),
    (proto.CFG_KEY_CLAMP_WIDTH,     0x20, "クランプ幅(既定)"),
    (proto.CFG_KEY_PIXFMT, proto.PIXFMT_RGB555, "伝送はRGB555"),
]

# コンポジット/S端子で共通のタイミング(8fsc NTSC)とクランプ窓
_CVBS_TIMING = [
    (proto.CFG_KEY_IN_MUX1,     MUX_SOG3, "SOG/R/G/B すべて _3"),
    (proto.CFG_KEY_SYNC_CTL,    SYNC_SOG, "HもVもSOGから取る"),
    (proto.CFG_KEY_PLL_DIVIDE,      1820, "PLL分周 = 8fsc NTSC"),
    (proto.CFG_KEY_PLL_CTL,         0x10, "VCO=UltraLow + チャージポンプ2"),
    (proto.CFG_KEY_CLAMP_START,      230, "クランプ開始をバーストの後ろへ"),
    (proto.CFG_KEY_CLAMP_WIDTH,       30, "クランプ幅"),
    (proto.CFG_KEY_IN_MUX2,         0x12, "SOG LPF 2.5MHz(バーストで誤トリガしない)"),
    (proto.CFG_KEY_PIXFMT, proto.PIXFMT_YC8, "生8bit伝送(復調に必要)"),
]

MODES = {
    "x68k": {
        "label": "X68000 RGB (Rin3/Gin3/Bin3 + HSYNC_A/VSYNC_A)",
        "roles": ("rgb", None),
        "regs": [
            (proto.CFG_KEY_IN_MUX1, MUX_SOG3, "SOG/R/G/B すべて _3(SOGは使わない)"),
            (proto.CFG_KEY_SYNC_CTL, SYNC_5WIRE, "HSYNC/VSYNCを別線で受ける"),
            (proto.CFG_KEY_IN_MUX2, 0x12, "HSYNC_A/VSYNC_A を選択(bit0=0 bit2=0)"),
            # 31kHz(768x512)の起点。Viewerのプロファイル/自動調整が上書きする
            (proto.CFG_KEY_PLL_DIVIDE, 1104, "起点=31kHzのhtotal(Viewerが上書きする)"),
            (proto.CFG_KEY_PLL_CTL, 0x18, "ICP = 40×75/1104 = 2.7 → 3"),
        ] + _RGB_ANALOG,
    },
    "msx": {
        "label": "MSX RGB (Rin3/Gin3/Bin3 + SOGin2にCSYNC)",
        "roles": ("rgb", None),
        "regs": [
            # ★ここが 19h キーを足した理由。SOGだけ _2 にする
            (proto.CFG_KEY_IN_MUX1, MUX_SOG2, "★SOGだけ _2(CSYNCがSOGIN_2に来る)"),
            (proto.CFG_KEY_SYNC_CTL, SYNC_SOG, "HもVもSOG(=CSYNC)から取る"),
            (proto.CFG_KEY_IN_MUX2, 0x12, "SOG LPF 2.5MHz + クランプLPF 0.5MHz"),
            # 同期セパレータ(11h/12h/13h/22h)は**触らない**。ゲートウェアの既定が
            # 15.7kHz族向けに調整済み(sep 0x75 / pre 3 / post 3)で、実測の根拠も
            # retrocastx_i2c.py に書かれている。ここで上書きすると出所が二重になる。
            # lines/frame が倍(523/1046を行き来する等)になったら 0x5B を 0x09 に
            # する手がある — status がその症状を見て指摘する。
            #
            # MSXの水晶 21.47727MHz(NTSC副搬送波×6)の1/2 = 10.7386MHz で
            # htotal 342×4 = 1368(TVPの下限12MHzを満たす整数倍。profiles.rs 参照)
            (proto.CFG_KEY_PLL_DIVIDE, 1368, "起点=MSXのhtotal(Viewerが上書きする)"),
            (proto.CFG_KEY_PLL_CTL, 0x10, "ICP = 40×75/1368 = 2.2 → 2"),
        ] + _RGB_ANALOG,
    },
    "composite": {
        "label": "コンポジット NTSC (Gin3にCVBS、SOGin3へ分岐)",
        "roles": ("cvbs", None),
        "regs": _CVBS_TIMING + [
            (proto.CFG_KEY_CLAMP_SEL, 0b010, "Greenだけミッドレベル(バーストを丸ごと入れる)"),
            (proto.CFG_KEY_COARSE_GAIN_GB, 0x07,
             "★Green粗ゲイン0.5倍。ミッドレベルのヘッドルーム半減を吸収する"),
            (proto.CFG_KEY_COARSE_GAIN_R, 0x07, "Redは未使用(既定)"),
            (proto.CFG_KEY_COARSE_OFF_G, 0x10, "G粗オフセット既定(ファインクランプ下では効かない)"),
            (proto.CFG_KEY_COARSE_OFF_R, 0x10, "R粗オフセット既定"),
            (proto.CFG_KEY_GAIN_B, 35, "青は未使用(RGBの既定)"),
            (proto.CFG_KEY_GAIN_G, 0,
             "★緑ファインゲイン1.000倍。33(=1.129倍)だと白が4.6%クリップした"),
            (proto.CFG_KEY_GAIN_R, 39, "赤は未使用(RGBの既定)"),
        ],
    },
    "svideo": {
        "label": "S端子 (Gin3=Y / Rin3=C、SOGin3へYから分岐)",
        "roles": ("y", "c"),
        "regs": _CVBS_TIMING + [
            # ★**YもCもミッドレベル。** 当初 Y をボトムレベルにしていたが
            #   **理由を取り違えていた**(実機で判明、2026-08-15)。ヘッドルームが
            #   要るのはバーストではなく**同期**。S端子のYは同期を含む1Vppなので、
            #   バックポーチを底に置くと、その40 IRE下の同期が必ずはみ出す。
            #   ゲインをいくら下げても直らない(実測: 同期チップが0に張り付き、
            #   ブランキング18・白242 → 1 IRE=2.24コードなのに、デコーダは
            #   (18-0)/40=0.45と誤認して**輝度が5倍**になり白飛びした)。
            #   Y はコンポジットの緑chと同じ設定にするのが正解。
            (proto.CFG_KEY_CLAMP_SEL, 0b011, "★YもCもミッドレベル(Yは同期を含む1Vpp)"),
            (proto.CFG_KEY_COARSE_GAIN_GB, 0x07,
             "★Y粗ゲイン0.5倍。ミッドレベルのヘッドルーム半減を吸収する(コンポジットと同じ)"),
            (proto.CFG_KEY_COARSE_GAIN_R, 0x07,
             "C粗ゲイン1.2倍。実測でバースト88.5コード=設計値ちょうどだった"),
            (proto.CFG_KEY_COARSE_OFF_G, 0x10, "G粗オフセット既定"),
            (proto.CFG_KEY_COARSE_OFF_R, 0x10, "R粗オフセット既定"),
            # 配線したら probe で飽和を見て詰める(コンポジットではGを0にした)
            (proto.CFG_KEY_GAIN_B, 35, "青は未使用(RGBの既定)"),
            (proto.CFG_KEY_GAIN_G, 0, "Yのファインゲイン1.000倍(コンポジットと同じ)"),
            (proto.CFG_KEY_GAIN_R, 0, "Cのファインゲイン1.000倍(実測後に詰める)"),
        ],
    },
}


def _write_all(c: Cfg, items, label: str) -> bool:
    print(label)
    ok = True
    for key, val, why in items:
        got = c.set(key, val)
        if got is None:
            print("  0x%04X = %-6s 応答なし ★このファームは未対応かもしれない"
                  % (key, val))
            ok = False
        elif got != val:
            print("  0x%04X = %-6s → 反映 %d ★不一致  %s" % (key, val, got, why))
            ok = False
        else:
            print("  0x%04X = %-6s OK   %s" % (key, val, why))
    return ok


def apply_mode(c: Cfg, mode: str, quiet: bool = False) -> bool:
    m = MODES[mode]
    return _write_all(c, m["regs"], "%s の設定を書く:" % m["label"]) if not quiet \
        else all(c.set(k, v) == v for k, v, _ in m["regs"])


def cmd_apply(c: Cfg, args) -> int:
    return 0 if apply_mode(c, args.mode) else 1


# --- 自動判別 ---------------------------------------------------------------
#
# 3方式は排他配線で、**同期がどこに来るかが違う**のでそこで見分けられる。
# 絵の内容も解像度も見ない(見ると真っ黒な画面で誤判定する)。
#
#   1. 生HSYNC/VSYNC(TTLピン)に信号があるか
#      key 0x2A は **TVPを通らない経路**を sysドメインで1秒数えた絶対値なので、
#      TVPの設定に一切依存せず読める。ここが最初の分岐に使えるのが大きい。
#   2. 無ければ SOG。19h を _2 と _3 で切り替えて、どちらでロックするかを見る。
#
# 各段で待つのは、H-PLLのロックとTVPの測定窓(フレーム単位)のぶん。

SETTLE_S = 3.0          # 入力MUXを切り替えてから fH が落ち着くまで(実測 約3秒)
RECHECK_S = 1.2         # 安定性を見るための2回目の読みまでの間隔
FH_MIN, FH_MAX = 10_000, 70_000      # 同期を「有る」とみなす範囲 [Hz]
FH_DRIFT_MAX = 0.005                 # 2回の読みの相対差の上限

MUX_SOG1 = 0x2A         # SOG=_1(このボードでは未接続)。**陰性対照に使う**


def _sync_quality(c: Cfg):
    """その入力に本物の同期が来ているかを (安定か, fH) で返す。

    ★**syncdet(reg 14h)は使えない。** 実測(2026-08-14)では、**何も繋がって
      いない SOGIN_1 / SOGIN_2 でも SOGD=1 / AHS=1 / AVS=1 を返した**
      (syncdet=0x6D)。スライサが浮いた入力で自己発振するためと思われる。
      これを信号の有無の判定に使うと必ず誤判定する(実際した)。

    使えるのは **fH の安定性**。同じ実測で:

        SOG=_1 未接続      fH = 28309 → 29529 Hz(単調に漂う)
        SOG=_2 未接続      fH = 29766 → 30734 Hz(同上)
        SOG=_3 コンポジット fH = 15734 → 15733 Hz(収束して動かない)

    未接続は「範囲外の値をゆっくり漂う」、本物は「規定値に収束して動かない」
    という形で必ず分かれる。だから2回読んで一致するかを見る。
    """
    f1 = c.get(proto.CFG_KEY_FH_TVP)
    time.sleep(RECHECK_S)
    f2 = c.get(proto.CFG_KEY_FH_TVP)
    if not f1 or not f2:
        return False, None
    if not (FH_MIN <= f2 <= FH_MAX):
        return False, f2
    return abs(f2 - f1) / f2 < FH_DRIFT_MAX, f2


def detect_mode(c: Cfg, verbose: bool = True) -> str:
    """繋がっている方式を当てる。当たらなければ None。"""
    def say(s):
        if verbose:
            print(s)

    fh_raw = c.get(proto.CFG_KEY_FH_RAW)
    say("生HSYNC(TTLピン、TVPを通らない経路) = %s Hz" % fh_raw)
    if fh_raw is not None and FH_MIN <= fh_raw <= FH_MAX:
        say("  → TTLの同期が来ている = **x68k**")
        return "x68k"
    say("  → TTLの同期は無い。SOGの入力ピンを順に試す")

    # SOGモードにしてから 19h だけを振る。0Eh を先に入れておかないと
    # SOGを同期源として使わないので、どのピンでもロックしない。
    c.set(proto.CFG_KEY_SYNC_CTL, SYNC_SOG)

    def probe_mux(mux, label):
        c.set(proto.CFG_KEY_IN_MUX1, mux)
        time.sleep(SETTLE_S)
        stable, fh = _sync_quality(c)
        say("  19h=0x%02X (%-16s): fH=%-6s Hz  %s"
            % (mux, label, fh, "安定" if stable else "不安定/範囲外"))
        return stable

    # ★陰性対照を先に取る。未接続のはずの SOGIN_1 が「安定」と出るなら、
    #   判定そのものが信用できないので、誤った答えを返すより止める。
    if probe_mux(MUX_SOG1, "SOG=_1 陰性対照"):
        say("  ★陰性対照(未接続のはずのSOGIN_1)が安定と出た。"
            "判定できないので中止する。SOGIN_1 に何か繋がっていないか確認する")
        return None

    for mux, label, name in ((MUX_SOG2, "SOG=_2", "msx"),
                             (MUX_SOG3, "SOG=_3", "composite")):
        if probe_mux(mux, label):
            say("  → **%s**" % name)
            return name
    say("  → どのピンでも安定しない。信号が来ていないか、"
        "SOG閾値(0x50)が合っていない")
    return None


def cmd_auto(c: Cfg, args) -> int:
    """判別して、その方式の設定を入れる。"""
    mode = detect_mode(c)
    if mode is None:
        return 1
    # composite と svideo は同期の出方が同じなので判別できない。
    # C(赤ch)に信号があるかで分かるが、それには絵を見る必要がある(probeの仕事)。
    if mode == "composite" and args.svideo:
        mode = "svideo"
    print()
    ok = apply_mode(c, mode)
    if mode == "composite":
        print("\n(S端子を繋いでいる場合は `auto --svideo` を使う。"
              "同期の出方が同じなので同期だけでは区別できない)")
    return 0 if ok else 1


def cmd_capture(c: Cfg, args) -> int:
    """生サンプルをファイルに落とす。**復調はオフラインで開発する。**

    実機を占有したまま係数を詰めていると回転が遅いうえ、ボードの購読先は1つ
    しかないので他の作業と両立しない。一度録ればオフラインで何通りも試せる。

    保存するのは「ラインの絶対位置つきの生サンプル」だけで、解釈は入れない。
    NumPyの .npz:
        y      uint8 [n, htotal]  緑ch(CVBS または Y)
        c      uint8 [n, htotal]  赤ch(S端子のC。コンポジットでは≒0)
        line   uint16[n]          フレーム内のライン番号
        frame  uint16[n]          フレーム番号
        field  uint8 [n]          LINEヘッダの FIELD_ODD ビット
        meta   0次元 dict         dotclk_hz / htotal / fH / mflags 等
    """
    import numpy as np

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    # ★SO_REUSEPORT で Viewer と共存させようとしたが**駄目だった**(2026-08-15)。
    #   macOS では bind は通るがブロードキャストが片方にしか配られず、無言で0行になる。
    #   下の「Viewerを閉じてから実行する」と明示的に失敗する方がまだ良い。
    try:
        sock.bind(("0.0.0.0", args.port))
    except OSError as e:
        print("UDP %d を bind できません (%s)。Viewerを閉じてから実行する"
              % (args.port, e))
        return 1
    sock.settimeout(1.0)
    print("LINEを %.1f 秒集める" % args.seconds)
    mode, by_frame = collect(sock, (args.board, args.port), args.seconds)
    sock.close()
    if mode is None or not by_frame:
        print("MODEかLINEが来ない")
        return 1
    if mode.pixfmt != proto.PIXFMT_YC8:
        print("★pixfmt が %d。YC8(3)で録らないと8bitにならない "
              "(`apply composite` を先に)" % mode.pixfmt)
        return 1

    ys, cs, lns, frs, flds = [], [], [], [], []
    for f in sorted(by_frame):
        rows = {}
        for p in by_frame[f]:
            if p.mode_id != mode.mode_id:
                continue
            rows.setdefault(p.line, []).append(p)
        for ln in sorted(rows):
            pk = sorted(rows[ln], key=lambda p: p.offset_px)
            if pk[0].offset_px != 0:
                continue
            y, cch = [], []
            ok = True
            for p in pk:
                if p.offset_px != len(y):
                    ok = False
                    break
                y.extend(_ch_samples(p, mode.pixfmt, 0))
                cch.extend(_ch_samples(p, mode.pixfmt, 1))
            # htotal ぴったり揃った行だけ採る(短い行は復調の位相基準が狂う)
            if ok and len(y) >= mode.htotal:
                ys.append(y[:mode.htotal])
                cs.append(cch[:mode.htotal])
                lns.append(ln)
                frs.append(f)
                # LINE flags bit1 = FIELD_ODD
                flds.append((pk[0].flags >> 1) & 1)
    if not ys:
        print("完全な行が取れなかった")
        return 1
    np.savez_compressed(
        args.out,
        y=np.array(ys, dtype=np.uint8), c=np.array(cs, dtype=np.uint8),
        line=np.array(lns, dtype=np.uint16), frame=np.array(frs, dtype=np.uint16),
        field=np.array(flds, dtype=np.uint8),
        meta=np.array({
            "dotclk_hz": mode.dotclk_hz, "htotal": mode.htotal,
            "hactive": mode.hactive, "vactive": mode.vactive,
            "vtotal": mode.vtotal, "mflags": mode.mflags,
            "fh_hz": mode.hfreq_mhz_x1000 / 1000.0,
            "fv_hz": mode.vfreq_mhz_x1000 / 1000.0,
        }, dtype=object))
    print("保存: %s  %d行 × %dサンプル  (フレーム %d〜%d)"
          % (args.out, len(ys), mode.htotal, min(frs), max(frs)))
    return 0


def cmd_synctest(c: Cfg, args) -> int:
    """そのチャネルに**同期パルスが本当に載っているか**を切り分ける。

    「絵は出ているのに同期チップが見えない」とき、クランプの効き方の問題なのか、
    信号そのものに同期が無いのかを、オシロ無しで区別する。

    クランプ窓(reg 05h)を動かすと、**クランプした区間が基準レベルに座る**。
    本物の同期パルスがあれば、

        clamp_start=230(バースト後) → バックポーチが基準に座る
        clamp_start=50 (同期チップ上) → 同期チップが基準に座る

    の2条件で「同期チップ域 - バックポーチ域」が 40 IRE ぶん入れ替わる。
    同期が無ければどちらも同じ値のままで、差はノイズぶんしか動かない。
    """
    import statistics as st

    def measure():
        time.sleep(args.settle)
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        sock.bind(("0.0.0.0", args.port))
        sock.settimeout(1.0)
        mode, by_frame = collect(sock, (args.board, args.port), args.seconds)
        sock.close()
        if mode is None:
            return None
        sps = mode.dotclk_hz or 0
        lines = collect_lines(by_frame, sorted(by_frame), mode, sps, args.chan, 40)
        if not lines or sps <= 0:
            return None
        rs = [r for r in (analyze_line(y, sps) for y in lines) if r]
        if not rs:
            return None
        return (st.median(r["tip"] for r in rs), st.median(r["blank"] for r in rs))

    print("クランプ窓を動かして、基準レベルがどう動くかを見る\n")
    print("%-26s %10s %14s %8s" % ("クランプ位置", "同期チップ域", "バックポーチ域", "差"))
    got = {}
    for start, label in ((230, "バースト後 (230)"), (50, "同期チップ上 (50)")):
        # Cfg はポート34600をbindするので、受信の前に手放す
        cfg = Cfg(args.board, args.port)
        cfg.set(proto.CFG_KEY_CLAMP_START, start)
        del cfg
        m = measure()
        if m is None:
            print("%-26s 測定できず(LINEが来ない)" % label)
            Cfg(args.board, args.port).set(proto.CFG_KEY_CLAMP_START, 230)
            return 1
        tip, blank = m
        got[start] = blank - tip
        print("%-26s %10.1f %14.1f %8.1f" % (label, tip, blank, blank - tip))
    Cfg(args.board, args.port).set(proto.CFG_KEY_CLAMP_START, 230)

    swing = abs(got[230] - got[50])
    print("\nクランプ移動による差の入れ替わり = %.1f コード" % swing)
    # 粗ゲイン0.5倍なら 40 IRE = 約37コード。ノイズは数コード。
    if swing < 10:
        print("★ **このチャネルには同期パルスが載っていない。**"
              "\n  クランプをどこに置いても基準が動かない = ブランキング期間が"
              "\n  ひとつの平坦なレベルしかない、ということ。"
              "\n  同期が別配線でしか来ていないか、そのチャネルの配線を確認する")
        return 1
    print("判定: 同期パルスは載っている(40 IRE 相当のレベル差がある)")
    return 0


def cmd_status(c: Cfg, args) -> int:
    """同期とPLLがロックしたかを、TVP自身の測定値で見る。"""
    det = c.get(proto.CFG_KEY_SYNCDET)
    lpf = c.get(proto.CFG_KEY_LINES_PER_FRAME)
    cpl = c.get(proto.CFG_KEY_CLOCKS_PER_LINE)
    pll = c.get(proto.CFG_KEY_PLL_DIVIDE)
    fh = c.get(proto.CFG_KEY_FH_TVP)
    mux1 = c.get(proto.CFG_KEY_IN_MUX1)
    sctl = c.get(proto.CFG_KEY_SYNC_CTL)
    fh_raw = c.get(proto.CFG_KEY_FH_RAW)
    if det is None:
        print("応答なし。--board と、Viewerがポートを掴んでいないかを確認する")
        return 1
    if mux1 is not None:
        print("in_mux1 (reg 19h) = 0x%02X  SOG=_%d R=_%d G=_%d B=_%d"
              % (mux1, ((mux1 >> 6) & 3) + 1, ((mux1 >> 4) & 3) + 1,
                 ((mux1 >> 2) & 3) + 1, (mux1 & 3) + 1))
    if sctl is not None:
        print("sync_ctl (reg 0Eh) = 0x%02X  (0x52=5線 / 0x5B=SOG)" % sctl)
    print("生HSYNC(TTL)       = %s Hz  (0ならTTL同期は来ていない)" % fh_raw)

    # ★「信号が来ているか」を**派生値より先に**言う。
    #   これを後回しにすると、fH や lines/frame が全部それらしい数字で出てきて
    #   紛らわしい。実際に踏んだ: 映像ソースの電源が落ちただけなのに
    #   fH=1578Hz / lines/frame=1 / vtotal=4096 と出て、Viewer や設定の不具合を
    #   疑って時間を使った。
    #
    #   SOGOUT は FPGA が pix ドメインで直接数えている(TVPの内部状態と独立)。
    #   信号が無いとスライサの出力がLowに張り付き、Low期間のカウンタが飽和する。
    if sctl is not None and (sctl & 0x08):
        lowmax = c.get(proto.CFG_KEY_SOG_LOWMAX)
        hlen = c.get(proto.CFG_KEY_SOG_HLEN)
        print("SOGOUT             = 水平周期 %s / 最長Low %s [pixクロック]"
              % (hlen, lowmax))
        # ★**カウンタの飽和だけで「信号なし」と断じてはいけない**(2026-08-15)。
        #   S端子の配線で、fH 15734 / vtotal 263 / lines/frame 525 が全部正常で
        #   絵も出ているのに lowmax が 65535 に張り付く状態を実測した。
        #   この判定を信じて2回とも「信号が来ていない」と誤診し、その間に実機の
        #   本当の症状(断続的な同期外れ)を追えなかった。
        #
        #   **TVP自身の測定を裏取りに使う。** lines/frame(reg 37h:38h)は
        #   セパレータが数えた値で、SOGOUTの波形とは独立に読める。ここが
        #   NTSC/PALとして筋の通る値なら、信号は来ている。
        if lowmax is not None and lowmax >= 0xFFFF:
            sane = lpf is not None and (250 <= lpf <= 270 or 500 <= lpf <= 540)
            if not sane:
                print("\n  ★ **SOGOUTがLowに張り付いていて、TVPのlines/frame(%s)も"
                      "筋が通らない = 信号が来ていない。**" % lpf)
                print("     映像ソースの電源とケーブルを確認する。")
                print("     この状態では fH も lines/frame も当てにならない値が出るので、")
                print("     以下は表示しない。")
                return 1
            print("  (最長Lowが飽和しているが、TVPのlines/frame=%s は正常なので"
                  "信号は来ている。SOGOUTの測定はこの配線では当てにならない)" % lpf)
    # reg 14h: bit6=SOGD(SOG検出) bit3=AVS(垂直有効) bit2=AHS(水平有効)
    print("syncdet (reg 14h) = 0x%02X   SOGD=%d AVS=%d AHS=%d"
          % (det, (det >> 6) & 1, (det >> 3) & 1, (det >> 2) & 1))
    print("lines/frame        = %s   (期待: 15.7kHz系なら262〜263、480iなら525前後)"
          % lpf)
    # ★clocks/line(reg 39h:3Ah)は **DATACLK ではなく内部基準クロック(約6.5MHz)**で
    #   数えた値。データシート: 「fH = clock reference frequency / clocks per line」。
    #   pll_divide と比べるのは誤り(実測 410 は 6.44MHz/15700Hz で正しい値だった)。
    #   ここでは逆算して基準クロックが6.5MHz付近に出るかで健全性を見る。
    if cpl and fh:
        ref = cpl * fh
        print("clocks/line        = %s   (内部基準 %.2f MHz 相当。DATACLKではない。"
              "6.5MHz前後なら正常)" % (cpl, ref / 1e6))
    else:
        print("clocks/line        = %s" % cpl)
    if fh:
        print("fH (TVP HSOUT)     = %d Hz  (NTSC/MSXは約15700Hz)" % fh)
        if pll:
            print("→ DATACLK          = %.4f MHz  (pll_divide %d × fH)"
                  % (pll * fh / 1e6, pll))

    sog_mode = sctl is not None and (sctl & 0x08)
    bad = []
    if sog_mode and not (det >> 6) & 1:
        bad.append("SOGを検出していない → 入力ピンの選択(0x69)が合っているか確認。"
                   "合っていれば 0x50(SOG閾値、既定0x0B=124mV)と "
                   "0x5D(SOG LPFを0x52=10MHzへ)を振る")
    if not (det >> 2) & 1:
        bad.append("水平がロックしていない → 0x19(チャージポンプ)を 0x08/0x10/0x18 で振る")
    # 内部基準クロックはデータシートで「typically 6.5 MHz、精確な値とみなさないこと」。
    # 大きく外れるのは同期が取れていないとき(clocks/lineが別の周期を数えている)。
    if cpl and fh and not (5.0e6 <= cpl * fh <= 8.0e6):
        bad.append("clocks/line × fH = %.2f MHz で内部基準(約6.5MHz)から外れている → "
                   "同期が安定していない" % (cpl * fh / 1e6))
    # 「lines/frame がほぼ倍」= ハーフライン積算器がVSOUTを作っている症状。
    # 262ライン progressive を SOG で受けると2フレームに1回しかVが出ず、
    # 523/1046 を行き来する双安定になる(retrocastx_stream.py の 22h の説明)。
    #
    # ★**525(真のインタレースNTSC)と 523(262 progressive の倍)は数値が
    #   ほとんど同じで、lines/frame だけでは区別できない。** TVPの P/I 検出
    #   (reg 38h bit5、0=インターレース)を併せて見ないと誤検出する。
    #   実際、当初は 480〜560 を無条件に警告していて、正常なインタレースNTSC
    #   (525・fH 15734Hz・P/I=インタレース)を誤って指摘した。
    if lpf and 480 <= lpf <= 560 and fh and 15000 <= fh <= 16500:
        pi = c.get(proto.CFG_KEY_LPF_MSBS)
        progressive = pi is not None and (pi >> 5) & 1
        if progressive:
            bad.append("lines/frame が %d で 262 の約2倍、かつ P/I 検出は "
                       "progressive。ハーフライン積算器がVSOUTを作っている症状 → "
                       "0x5B(reg 22h)を 0x09 にして VSOUT を同期セパレータ直結にする"
                       % lpf)
        elif pi is not None:
            print("  (P/I検出=インターレース なので lines/frame %d は正常)" % lpf)
    for b in bad:
        print("  ★ " + b)
    return 1 if bad else 0


def _ch_samples(pkt, pixfmt, chan=0):
    """LINE のペイロードから 1チャネルぶんの 8bit 列を取り出す。

    chan=0 は緑ch(CVBS または Y)、chan=1 は赤ch(S端子のC)。
    """
    px = pkt.pixels
    if pixfmt == proto.PIXFMT_YC8:
        return [px[i + chan] for i in range(0, len(px), 2)]
    if pixfmt == proto.PIXFMT_RGB555:
        # 0RRRRRGGGGGBBBBB を5bit→8bitのビット複製で。値の刻みは粗いので
        # 「潰れているかどうか」の判定にしか使えない(だからYC8が要る)
        shift = 10 if chan else 5           # 赤は[14:10]、緑は[9:5]
        out = []
        for i in range(0, len(px), 2):
            v = px[i] | (px[i + 1] << 8)
            n5 = (v >> shift) & 0x1F
            out.append((n5 << 3) | (n5 >> 2))
        return out
    return None


def _win(us_range, sps):
    """[µs] の範囲を [サンプル] のスライス範囲にする。"""
    a, b = us_range
    return int(a * 1e-6 * sps), int(b * 1e-6 * sps)


def analyze_line(y, sps):
    """1ラインの 8bit 列から、同期チップ/ブランキング/バースト振幅を読む。

    ネットワークから切り離してある(合成波形で試験できるように)。
    None を返すのは「区間が取れなかった」= ラインが短すぎる場合。
    """
    def seg(rng):
        a, b = _win(rng, sps)
        return y[a:min(b, len(y))]
    tip, bp, bu, act = seg(SYNC_TIP), seg(BACKPORCH), seg(BURST), seg(ACTIVE)
    if not (tip and bp and bu):
        return None
    return {
        "tip": statistics.median(tip),
        "blank": statistics.median(bp),
        "burst_pp": max(bu) - min(bu),
        "act_min": min(act) if act else 0,
        "act_max": max(act) if act else 0,
    }


def burst_period(y, blank, sps):
    """バースト区間の周期[サンプル/周期]と位相[rad]を相関で推定する。

    バーストは9周期しかないのでFFTより素直。8fsc なら 8.00 が出るはずで、
    そこがずれていれば H-PLL の分周比が違う(= サンプルレートが 8fsc でない)。
    """
    a, b = _win(BURST, sps)
    bu = [v - blank for v in y[a:min(b, len(y))]]
    if len(bu) < 16:
        return None
    best = None
    # 1周期あたり 4.0〜16.0 サンプルを 0.25 刻みで走査する
    for n_per_cycle in [x / 4.0 for x in range(16, 65)]:
        w = 2 * math.pi / n_per_cycle
        re = sum(v * math.cos(w * i) for i, v in enumerate(bu))
        im = sum(v * math.sin(w * i) for i, v in enumerate(bu))
        mag = math.hypot(re, im)
        if best is None or mag > best[0]:
            best = (mag, n_per_cycle, math.atan2(im, re))
    return best[1], best[2]


def ascii_plot(y, sps, until_us=11.0, cols=72, rows=14):
    """ラインの先頭を縦棒グラフにする。数値より先に形で分かることがある。

    列ごとに **min と max の間を塗る**(平均ではない)。8fscのバーストは
    1周期8サンプルなので平均を取ると消えてしまうが、min/maxなら**帯の太さ**
    として残る。つまりこの図では、

        同期チップ = 低い位置の細い線     バースト = 中ほどの太い帯
        バックポーチ = 細い線             飽和 = 上端/下端に張り付いた線

    となり、「バーストが潰れている」「白が飽和している」が一目で分かる。
    """
    n = min(len(y), int(until_us * 1e-6 * sps))
    if n < cols:
        return []
    step = n / cols
    bands = []
    for c in range(cols):
        a, b = int(c * step), max(int((c + 1) * step), int(c * step) + 1)
        chunk = y[a:b]
        bands.append((min(chunk), max(chunk)))
    out = []
    for r in range(rows):
        # 上の行が大きいコード。1行あたり 256/rows コード
        hi = 255 - r * 256 // rows
        lo = 255 - (r + 1) * 256 // rows
        line = "".join("#" if (bmax >= lo and bmin <= hi) else " "
                       for bmin, bmax in bands)
        out.append("%3d |%s" % (lo, line))
    # 時間軸の目安。NTSCの区間の境目に印を打つ
    ruler = [" "] * cols
    for us, mark in ((4.7, "S"), (5.3, "b"), (7.8, "B"), (9.4, "P")):
        c = int(us * 1e-6 * sps / step)
        if 0 <= c < cols:
            ruler[c] = mark
    out.append("    +" + "-" * cols)
    out.append("     " + "".join(ruler))
    out.append("     S=同期チップ終わり b=バースト開始 B=バースト終わり P=映像開始")
    return out


# S端子の C は 1.2倍ゲイン + ミッドレベルで バースト 0.286Vpp ≒ 88コード。
# コンポジット(36)より広く取れるので下限も上げる。
BURST_PP_MIN_C = 40


def verdict(tip, blank, burst_pp, act_max, code_per_ire, role="cvbs"):
    """判定と、次に振るべきキーを返す。probe の結論はここに集約する。

    role でチャネルの役割ごとに期待値を変える。**同じ数値でも役割が違えば
    合否が逆になる**ため:

      cvbs  緑ch。同期もクロマも1本に乗っている。ミッドレベルなのでブランキング
            は128付近、同期チップは見えていなければならない(40 IRE校正の基準)
      y     S端子の緑ch。輝度専用でボトムレベルなので、**同期チップは飽和して
            いるのが正常**。バーストも無い。cvbsの判定をそのまま当てると全部
            誤検出になる
      c     S端子の赤ch。ミッドレベルで、同期区間は無彩色=ブランキングと同じ値。
            バーストは cvbs より大きく出るはず
    """
    bad = []
    if role == "cvbs":
        if tip <= 1:
            bad.append("同期チップが0に張り付いている → クランプがボトムレベルか、"
                       "粗ゲインが高すぎる。0x5E=0b010 / 0x5F=0x07 を確認")
        if act_max >= 254:
            bad.append("映像が255に飽和している → 粗ゲイン(0x5F)を下げる。"
                       "ミッドレベルクランプでは白側のヘッドルームが半分しかない")
        # ミッドレベルクランプ + 粗ゲイン0.5倍なら 40 IRE = 36コード出る計算。
        # 24を下回るのは「クリップしている」か「ゲインが低すぎる」かのどちらか。
        if burst_pp < BURST_PP_MIN:
            bad.append("バーストが %.0f コードしかない(設計値は36コード)。"
                       "ミッドレベルクランプになっているか(0x5E)、粗ゲインを"
                       "上げられないか(0x5F)を確認" % burst_pp)
        if code_per_ire is None:
            bad.append("同期チップとブランキングの差が無い → クランプ位置(0x1A/0x1B)が"
                       "同期チップの上にある。230/30 になっているか確認")
        elif abs(blank - 128) > 40 and burst_pp >= BURST_PP_MIN:
            bad.append("ブランキングが %.0f で128から離れている。ミッドレベルなら128付近、"
                       "ボトムレベルなら60付近になる" % blank)
    elif role == "y":
        # ★**同期チップが見えていることが必須。** S端子のYは同期を含む1Vppなので、
        #   コンポジットの緑chと同じくミッドレベルでなければ同期が下へはみ出す。
        #   デコーダは (ブランキング - 同期チップ)/40 でIREを校正するので、
        #   同期が潰れると校正が壊れて輝度が数倍になる(実機で白飛びした)。
        if tip <= 1:
            bad.append("Yの同期チップが %.0f で潰れている。**IRE校正が壊れて輝度が"
                       "数倍になる。** Yはミッドレベル(0x5E bit1=1)+粗ゲイン0.5倍"
                       "(0x5F=0x07)にする" % tip)
        elif blank - tip < 20:
            bad.append("Yの同期チップ〜ブランキングが %.0f コードしかない(40 IRE)。"
                       "粗ゲイン(0x5F)が低すぎるか、同期が潰れかけている"
                       % (blank - tip))
        if act_max >= 254:
            bad.append("Yが255に飽和している → 0x5F(G/B粗ゲイン)を下げる")
        if act_max - blank < 20:
            bad.append("Yの振幅が %.0f コードしかない。信号が来ていないか、"
                       "粗ゲイン(0x5F)が低すぎる" % (act_max - blank))
    elif role == "c":
        if abs(blank - 128) > 40:
            bad.append("Cのブランキングが %.0f で128から離れている。Cはミッドレベル"
                       "(0x5E bit0=1)でなければならない" % blank)
        if act_max >= 254:
            bad.append("Cが255に飽和している → 0x67(赤の粗ゲイン)を下げる")
        if burst_pp < BURST_PP_MIN_C:
            bad.append("Cのバーストが %.0f コードしかない(設計値は88コード)。"
                       "配線(C→Rin3)と 0x67(赤の粗ゲイン)を確認" % burst_pp)
    return bad


def cmd_probe(c: Cfg, args) -> int:
    """波形を実測する。**この段の合否はここで決まる。**

    絵が出ているかではなく、
      - 同期チップとバーストが飽和していないか(0 や 255 に張り付いていないか)
      - ブランキングがどのコードに座っているか(ミッドレベルなら128付近)
      - バーストが何コード p-p あるか(位相推定に足りるか)
    を見る。
    """
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    # ★SO_REUSEPORT で Viewer と共存させようとしたが**駄目だった**(2026-08-15)。
    #   macOS では bind は通るがブロードキャストが片方にしか配られず、無言で0行になる。
    #   下の「Viewerを閉じてから実行する」と明示的に失敗する方がまだ良い。
    try:
        sock.bind(("0.0.0.0", args.port))
    except OSError as e:
        print("UDP %d を bind できません (%s)。Viewerを閉じてから実行する"
              % (args.port, e))
        return 1
    sock.settimeout(1.0)
    print("LINEを %.1f 秒集める(このツールが購読先を奪うのでViewerは止まる)"
          % args.seconds)
    mode, by_frame = collect(sock, (args.board, args.port), args.seconds)
    sock.close()
    if mode is None:
        print("MODEが来ない。ボードのIPとサブネットを確認する")
        return 1
    frames = sorted(by_frame)
    if not frames:
        print("LINEが来ない")
        return 1

    sps = mode.dotclk_hz or 0
    fmt_name = {proto.PIXFMT_RGB555: "RGB555", proto.PIXFMT_YC8: "YC8(生8bit)"}.get(
        mode.pixfmt, "pixfmt %d" % mode.pixfmt)
    print("\nMODE: %s  %dx%d  htotal=%d  dotclk=%.4f MHz  fH=%.1f Hz"
          % (fmt_name, mode.hactive, mode.vactive, mode.htotal,
             sps / 1e6, mode.hfreq_mhz_x1000 / 1000.0))
    if sps <= 0:
        print("dotclk が 0。PLLがロックしていない")
        return 1
    if mode.pixfmt == proto.PIXFMT_RGB555:
        print("★RGB555で観測している。5bitでは刻みが 4.4 IRE なので、"
              "潰れの有無しか分からない。`apply --raw` で生8bitにすること")

    roles = MODES[args.mode]["roles"]
    bad_any = False
    for chan, role in enumerate(roles):
        if role is None:
            continue
        if role == "rgb":
            print("★%s は RGB モード。probe はコンポジット/S端子の波形解析用で、"
                  "RGBには意味のある判定ができない" % args.mode)
            return 1
        lines = collect_lines(by_frame, frames, mode, sps, chan, args.lines)
        if not lines:
            print("★ラインの先頭から取れた行が無い。offset_px が0の断片が来ていない。"
                  "\n  key 0x30(full_line)を1にする(YC8なら自動で有効になるはず)")
            return 1
        label = {"cvbs": "CVBS (緑ch)", "y": "Y (緑ch)", "c": "C (赤ch)"}[role]
        print("\n=== %s === 解析対象 %d 行(1行 %d サンプル)"
              % (label, len(lines), len(lines[0])))
        rows_out = [r for r in (analyze_line(y, sps) for y in lines) if r]
        if not rows_out:
            print("解析できる範囲が取れなかった")
            return 1
        print(report(rows_out, lines, sps, role))
        if verdict(*_summary(rows_out), role=role):
            bad_any = True
    return 1 if bad_any else 0


def collect_lines(by_frame, frames, mode, sps, chan, limit):
    """1ラインぶんが揃っている行を集める。断片は offset_px 順に連結する。

    先頭(offset_px=0)が無い行は捨てる。同期チップが見えないと校正できないため。
    """
    lines = []
    for f in frames:
        rows = {}
        for p in by_frame[f]:
            if p.mode_id != mode.mode_id:
                continue
            rows.setdefault(p.line, []).append(p)
        for _row, pkts in rows.items():
            pkts.sort(key=lambda p: p.offset_px)
            if pkts[0].offset_px != 0:
                continue
            y = []
            for p in pkts:
                if p.offset_px != len(y):
                    y = []
                    break
                s = _ch_samples(p, mode.pixfmt, chan)
                if s is None:
                    return []
                y.extend(s)
            if len(y) >= int(ACTIVE[0] * 1e-6 * sps):
                lines.append(y)
    return lines[:limit]


def _summary(rows_out):
    """行ごとの測定を中央値でまとめ、(tip, blank, burst_pp, act_max, code_per_ire) を返す。

    平均ではなく中央値にする。1ラインでも同期を取り逃がすと値が大きく外れ、
    平均だと全体が引っ張られるため。
    """
    def med(k):
        return statistics.median(r[k] for r in rows_out)
    tip, blank = med("tip"), med("blank")
    # 同期チップ(-40 IRE)からブランキング(0 IRE)までが 40 IRE。ここから
    # 1 IRE が何コードかが決まる。**絵の内容に依存しない**のがこの校正の利点。
    cpi = (blank - tip) / 40.0 if blank - tip > 2 else None
    return tip, blank, med("burst_pp"), med("act_max"), cpi


def report(rows_out, lines, sps, role="cvbs") -> str:
    """probe の出力を組み立てる(判定を含む)。"""
    tip, blank, burst_pp, act_max, cpi = _summary(rows_out)
    act_min = statistics.median(r["act_min"] for r in rows_out)
    out = ["       同期チップ  ブランキング  バースト p-p  映像 min..max",
           "コード %10.1f %13.1f %13.1f  %5.0f..%.0f"
           % (tip, blank, burst_pp, act_min, act_max)]
    # IRE換算は「同期チップ〜ブランキングが 40 IRE」を基準にする。**両端の
    # どちらかが飽和していたらこの校正は成立しない**(実際、同期チップが0に
    # 張り付いた測定では 1 IRE = 0.40コードと出て、バーストが85 IRE・映像が
    # 160 IRE という有り得ない値になった)。信用できないときは出さない。
    #
    # ★S端子では**どちらのチャネルでもこの校正は使えない**。Yはボトムレベルで
    #   同期チップが飽和し、Cには同期チップが無い(無彩色=ブランキングと同値)。
    #   絶対値のIRE換算が要るならコンポジットで測るしかない。
    if role != "cvbs":
        out.append("       (%s ではIRE換算を出せない。校正の基準にしている"
                   "「同期チップ〜ブランキングの40 IRE」が%s)"
                   % ({"y": "S端子のY", "c": "S端子のC"}[role],
                      "飽和している" if role == "y" else "存在しない"))
    elif cpi and tip > 1 and act_max < 254:
        out.append("IRE    %10.1f %13.1f %13.1f  %5.1f..%.1f   (1 IRE = %.2f コード)"
                   % ((tip - blank) / cpi, 0.0, burst_pp / cpi,
                      (act_min - blank) / cpi, (act_max - blank) / cpi, cpi))
    else:
        out.append("       (飽和しているのでIRE換算は出せない。校正の基準にしている"
                   "同期チップ〜ブランキングの40 IREが信用できない)")
    # バーストの周期。8fsc でサンプルできているかの直接の証拠になる。
    # ★**1本のラインで決めてはいけない。** 垂直帰線区間の行(等化パルス・
    #   切り込みパルスでバーストが無い)を引くと推定が丸ごと外れる。実際、
    #   lines[0] だけで推定していたときは正しく8.00サンプルで来ている信号に
    #   対して 14.75サンプル(1.94MHz)と誤報した。他の値は全部中央値を
    #   取っているのに、ここだけ1本に依存していたのが原因。
    periods = [bp for bp in (burst_period(y, blank, sps) for y in lines) if bp]
    if periods:
        spc = statistics.median(p[0] for p in periods)
        out += ["",
                "バーストの周期 = %.2f サンプル → %.4f MHz"
                "  (期待 8.00 サンプル / %.4f MHz、%d行の中央値)"
                % (spc, sps / spc / 1e6, FSC_NTSC / 1e6, len(periods))]
        # 位相はライン毎に180°反転するので中央値に意味が無い。
        # NTSC は 227.5周期/ライン なので**隣接ラインの位相差が180°**になる。
        # これが出ていれば2次元コムフィルタ(加算でY・減算でC)が成立する。
        if len(periods) >= 2:
            d = [abs(math.degrees(periods[i + 1][1] - periods[i][1])) % 360
                 for i in range(len(periods) - 1)]
            d = [x if x <= 180 else 360 - x for x in d]
            out.append("隣接ラインの位相差 = %.0f°(180°ならコムフィルタが成立する)"
                       % statistics.median(d))
    plot = ascii_plot(lines[0], sps)
    if plot:
        out += ["", "ラインの先頭(列ごとの min..max を塗る)"] + plot
    out.append("")
    bad = verdict(tip, blank, burst_pp, act_max, cpi, role)
    out += ["★ " + b for b in bad]
    if not bad:
        out.append("判定: クランプ位置・基準レベル・ゲインは復調に使える範囲に入っている")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", required=True,
                    help="ボードのIP(255.255.255.255 でブロードキャストも可)")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("apply", help="指定した方式の設定を全部書く")
    a.add_argument("mode", choices=sorted(MODES),
                   help=" / ".join("%s=%s" % (k, MODES[k]["label"])
                                   for k in sorted(MODES)))

    au = sub.add_parser("auto", help="繋がっている方式を当てて設定する")
    au.add_argument("--svideo", action="store_true",
                    help="SOGIN_3側でロックしたとき composite ではなく svideo にする"
                         "(同期の出方が同じなので同期だけでは区別できない)")

    sub.add_parser("status", help="入力選択と同期/PLLのロック状態を見る")
    sub.add_parser("modes", help="方式の一覧とレジスタ値を表示する")

    cap = sub.add_parser("capture", help="生サンプルを .npz に落とす(復調の開発用)")
    cap.add_argument("--out", default="cvbs.npz")
    cap.add_argument("--seconds", type=float, default=2.0)

    st = sub.add_parser("synctest",
                        help="そのチャネルに同期パルスが載っているかを切り分ける")
    st.add_argument("--chan", type=int, default=0, choices=(0, 1),
                    help="0=緑ch(CVBS/Y) 1=赤ch(C)")
    st.add_argument("--seconds", type=float, default=2.0)
    st.add_argument("--settle", type=float, default=1.5,
                    help="クランプ設定を変えてから測るまでの待ち[秒]")

    p = sub.add_parser("probe", help="波形を実測してクランプとゲインを判定する")
    p.add_argument("--mode", choices=sorted(MODES), default="composite",
                   help="どの役割でチャネルを解釈するか(既定 composite)")
    p.add_argument("--seconds", type=float, default=2.0)
    p.add_argument("--lines", type=int, default=32, help="解析する行数")

    args = ap.parse_args()

    if args.cmd == "modes":
        for k in sorted(MODES):
            print("%-10s %s" % (k, MODES[k]["label"]))
            for key, val, why in MODES[k]["regs"]:
                print("             0x%04X = %-6s %s" % (key, val, why))
            print()
        return

    if args.cmd == "synctest":
        # 内部で Cfg と受信ソケットを交互に使う(同じポートをbindするため)
        raise SystemExit(cmd_synctest(None, args))
    if args.cmd == "capture":
        raise SystemExit(cmd_capture(None, args))

    c = Cfg(args.board, args.port) if args.cmd != "probe" else None
    if args.cmd == "apply":
        raise SystemExit(cmd_apply(c, args))
    if args.cmd == "auto":
        raise SystemExit(cmd_auto(c, args))
    if args.cmd == "status":
        raise SystemExit(cmd_status(c, args))
    if args.cmd == "probe":
        raise SystemExit(cmd_probe(None, args))


if __name__ == "__main__":
    main()
