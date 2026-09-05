#!/usr/bin/env python3
"""X68000 校正パターン(hardware/adc-frontend/tools/x68k_calib)を測る。

★**このツールは「測れなかった」と言って止まることを優先する。**

2026-09-02 のブリングアップで、測定系を継ぎ足しながら進めた結果、同じ
フレームの重なる行範囲から 0 と 119 という両立しない値が出て、そこから
組み立てた仮説を2つ撤回することになった。マスクの定義・ツール(Viewer の
クロップ済みダンプ / host 側の全幅バッファ)・有効領域の検出を混ぜたのが
原因。だからこのツールは以下を自分で検査し、通らなければ数値を出さない。

  1. **複数フレームの一致**   … 2枚取って結果が違えば止まる(過渡状態を弾く)
  2. **1サンプル=1ドット**   … 4px縞の自己相関が8サンプルでなければ止まる
  3. **プラトー幅が16**      … 階調バンドの各段が16サンプルでなければ警告
  4. **バンドの意味の検査**  … band0はR=G=B / band1はG=B=0 …が成立するか
                               (幾何の検出が正しいことをデータ自身で裏付ける)
  5. **RGB555 の正当なコード** … 中央値の産物(8と16が混ざって12になる等)を弾く。
                               代表値には**最頻値**を使う
  6. **測定条件の記録**      … 自分が設定した値を結果と一緒に必ず出す

使い方:

    python3 -m retrocastx.calib_pattern --bind 192.168.11.24
    python3 -m retrocastx.calib_pattern --bind 192.168.11.24 --pll 736

Viewer が UDP 34600 を掴んでいても動く(ストリームはエフェメラルポートで
購読する)。ただし**レジスタの読み戻しはできない**ので、条件の記録は
「このツールが設定した値」に限られる。

★★ RGB888 伝送での実測(2026-09-03、v0.9.0 実機)★★

    段(0→31): 0 0 0 0 30 38 46 53 61 69 76 84 92 99 107 115 122 130 137
              145 153 160 168 175 183 190 198 205 213 220 227 234
    段差: 最小0 最大30 平均7.55(段4以降は 7〜8 で単調)

**32階調のうち28段が7〜8コード刻みで完全に分離**した。RGB555 では隣接コードが
1しか離れておらず余裕ゼロだったので、これで目的は達成。4チャンネルとも同一の構造。

残るのは**下位4段(0〜3)が ADC でクリップされている**ことだけ。粗オフセット
(key 0x65 / 0x68 を 0x08〜0x1F)を振っても下端は動かないことを、この
クリーンな測定基盤で再確認した(以前の同じ結論は汚染された測定に基づいていたが、
結論自体は正しかった)。クランプを片チャンネルだけミッドにする実験は、
バンドの意味の検査(band0 は R≈G≈B)に引っかかって測れない。

★★ 2026-09-03 の測定結果(RGB555 伝送。上の RGB888 と対比)★★

    グレー/緑/青 : 29階調(5bit 2〜30)。失っているのは 0, 1, 31
    赤           : 28階調(5bit 3〜30)。失っているのは 0, 1, 2, 31
    縞バンド(すべてレベル31指令): 4px→30 / 2px→29 / **1px→22**(ドット復元 a=0.28 込み)

**両端が入らないのは算術的な理由で、レジスタの詰めでは取れない。**

    レベル2〜30 の29段がコード16〜247 → 1段 8.25 コード
    32階調を収めるのに必要な幅 = 31 × 8.25 = 256 コード
    使える幅 = 0〜255 = 256 コード     → **余裕ゼロ**

RGB555 では 5bit コードの刻みが 255/31 = 8.23 に固定されているので、
レベル n をコード n に載せるにはゲインが厳密に1点でなければならず、
下げれば隣接レベルが同じコードに潰れる。粗オフセット(1Fh/20h)を
0x10〜0x1F に振っても下端は戻らないことを実測で確認済み。

→ **残る3階調は RGB888 伝送(`PIXFMT_RGB888 = 0`、ゲートウェア未実装)で取る。**
  8bit なら分解能が8倍になり、ゲイン/オフセットが1コード程度ずれていても
  PC 側で32階調へ吸着できる。これは README の設計意図
  「8bit で取得し PC 側でパレット吸着」そのもの。帯域は 768x512 で約523Mbps。

★★ 2026-09-02〜03 に踏んだ罠(同じ所を踏まないこと) ★★

**取り込みは 1 行おきである。** 512ラインのパターンが 1136 行のバッファの
偶数行(あるいは奇数行)だけに入り、間の行は黒のまま残る。実測:

    内容のある行数 512 / 全 1136、間隔はすべて 2

これを知らずに「行範囲の中央値/最頻値」を取ると、**データ行と黒行が半々に
混ざるので、どちらが選ばれるか実質でたらめ**になる。同じフレームの重なる
範囲から 0 と 119 が出る、という形で現れた。

さらに悪いことに、**平均を取ると値がちょうど半分になる**:

    115 ≈ 231/2、119 ≈ 239/2   (データ行231と黒行0の平均)

これを「1chと3chで振幅が2倍違う」「band0 と band7 で白の値が違う」という
実在しない現象として2回解釈してしまった。**どちらも測定の産物で、撤回済み。**

→ **必ず「内容のある行」だけを選んでから集計する。** このツールはそうしており、
  行数が 512 でなければ止まる。

**pll_divide は必ず --pll で明示すること。** ビットストリームをロードし直すと
FPGA がリセットされ、`retrocastx_stream.py --pll-divide` のビルド既定値
(1104)に戻る。2026-09-03 に「736 に設定したのに htofal=1104 になっている」
と気付いたが、原因はポゴピン試験のための SRAM ロードだった
(当初 Viewer の上書きだと誤解した。Viewer は動いていなかった)。
検査2(サンプル/ドット)がこれを検出して止まるので、黙って誤った値で
測ることはない。

なお **Viewer の `band_pll` は fH だけをキーにしている**ので、768x512 テキスト
(pll 1104)と 512x512 グラフィック(pll 736)がどちらも 31.5kHz で衝突する。
モードごとの記憶(`modes`)は `fH_vtotal_htotal` で区別できているが、こちらは
できない。Viewer を併用するときはこれを踏む可能性がある(未修正)。
"""
import argparse
import socket
import sys
import time
from collections import Counter

import numpy as np

from . import protocol as proto
from .receiver import FrameAssembler

# RGB555 の 5bit 値をビット複製で 8bit にした正当なコード。
# ★RGB888 伝送ではこの制約が無くなる(8bitの生値がそのまま来る)ので、
#   判定は「32段が別々の値か」「間隔が一様か」に変わる。
LEGAL = {((n << 3) | (n >> 2)): n for n in range(32)}
BAND_NAMES = ["グレー", "赤", "緑", "青", "1px縞", "2px縞", "4px縞", "白/黒"]
STEP_DOTS = 16          # 階調1段の幅[ドット]
NBANDS = 8


class Measured:
    """1フレームから取り出した測定結果。__eq__ でフレーム間の一致を見る。"""

    def __init__(self, bands, samples_per_dot, geom):
        self.bands = bands                      # band -> {"kind":..., "steps":[...]}
        self.samples_per_dot = samples_per_dot
        self.geom = geom                        # (x0, x1, y0, y1)

    def key(self):
        return tuple(
            (b, tuple(v["steps"]) if "steps" in v else v.get("level"))
            for b, v in sorted(self.bands.items()))


def mode_of(a):
    """最頻値。中央値だと隣接コードが混ざったとき存在しない値を返す。"""
    if a.size == 0:
        return None
    return int(Counter(a.ravel().tolist()).most_common(1)[0][0])


def samples_per_dot(img, rowidx):
    """4px縞バンド(周期8ドット)の自己相関から サンプル/ドット を出す。

    有効領域の検出に依存しないので、これが幾何の基準になる。
    ★内容のある行だけを使う(黒行を混ぜると縞が薄まって相関が落ちる)。
    """
    if len(rowidx) < 512:
        return None, None
    sel = rowidx[6 * 64:7 * 64]
    sel = sel[len(sel) // 4: len(sel) - len(sel) // 4]
    seg = img[sel].max(axis=2).mean(axis=0)
    nz = np.flatnonzero(seg > seg.max() * 0.05)
    if len(nz) < 64:
        return None, None
    seg = seg[nz[0]:nz[-1] + 1].astype(np.float64)
    seg -= seg.mean()
    ac = np.correlate(seg, seg, mode="full")[len(seg) - 1:]
    if ac[0] == 0:
        return None, None
    ac /= ac[0]
    lo, hi = 3, min(40, len(ac) - 1)
    k = lo + int(np.argmax(ac[lo:hi]))
    return k / 8.0, float(ac[k])


def plateaus(prof, min_w):
    out = []
    i = 0
    while i < len(prof):
        j = i
        while j + 1 < len(prof) and prof[j + 1] == prof[i]:
            j += 1
        if j - i + 1 >= min_w:
            out.append((int(prof[i]), j - i + 1))
        i = j + 1
    return out


def content_rows(img):
    """★内容のある行だけを返す。取り込みは1行おきなので、これを飛ばすと
    データ行と黒行が混ざって集計が壊れる(docstring の罠を参照)。"""
    lum = img.max(axis=2)
    idx = np.flatnonzero(lum.max(axis=1) > 24)
    return idx


def measure(img, spd, warn, is555=True):
    lum = img.max(axis=2).astype(np.int32)
    xs = np.flatnonzero(lum.max(axis=0) > 24)
    if len(xs) < 64:
        return None, "有効領域が見つからない"
    x0, x1 = int(xs[0]), int(xs[-1])

    rowidx = content_rows(img)
    # パターンは 512 ライン。1行おきに入るので、内容のある行が 512 本あるはず
    if len(rowidx) != 512:
        return None, ("内容のある行が %d 本(512 のはず)。フレームが揃って\n"
                      "  いないか、パターン以外が写っている" % len(rowidx))
    gaps = set(np.diff(rowidx).tolist())
    if gaps and gaps != {2}:
        warn.append("内容のある行の間隔が一定でない: %s" % sorted(gaps)[:5])
    y0, y1 = int(rowidx[0]), int(rowidx[-1])

    # ★原点(パターンの x=0 に対応するサンプル位置)を band7 から取る。
    #   band7 は左半分が白ベタなので、その白の開始位置がそのまま x=0。
    #   有効領域の検出に頼るより確実(暗い段は閾値を超えないため)。
    x_org = None
    sel7 = rowidx[7 * 64:8 * 64]
    sel7 = sel7[len(sel7) // 3: len(sel7) - len(sel7) // 3]
    if len(sel7) >= 8:
        p7 = np.median(img[sel7].max(axis=2), axis=0)
        w7 = np.flatnonzero(p7 > p7.max() * 0.5)
        if len(w7) >= 64:
            x_org = int(w7[0])

    bands = {}
    for b in range(NBANDS):
        # ★8等分ではなく「内容のある行を64本ずつ」で切る。これが正しい境界
        sel = rowidx[b * 64:(b + 1) * 64]
        sel = sel[len(sel) // 3: len(sel) - len(sel) // 3]
        if len(sel) < 8:
            return None, "バンド%dの行が足りない" % b
        blk = img[sel]

        # --- 検査4: バンドの意味 ---
        chmax = [int(blk[:, :, c].max()) for c in range(3)]
        if b == 0:
            if not (abs(chmax[0] - chmax[1]) <= 8 and abs(chmax[1] - chmax[2]) <= 8):
                return None, ("band0 はグレーのはずだが R/G/B の最大が %s。"
                              "幾何の検出がずれている" % chmax)
        elif b in (1, 2, 3):
            on = b - 1
            off = [c for c in range(3) if c != on]
            if chmax[on] < 32:
                return None, ("band%d(%s)の該当chが暗い(最大%d)。"
                              "幾何の検出がずれている" % (b, BAND_NAMES[b], chmax[on]))
            for c in off:
                if chmax[c] > 24:
                    return None, ("band%d(%s)で非該当ch %d が %d ある。"
                                  "幾何の検出がずれている"
                                  % (b, BAND_NAMES[b], c, chmax[c]))

        if b <= 3:
            ci = None if b == 0 else b - 1
            src = (blk.max(axis=2) if ci is None
                   else blk[:, :, ci]).astype(np.int32)
            # 列ごとの最頻値。中央値だと隣接コードの混在で存在しない値になる
            prof = np.array([mode_of(src[:, x]) for x in range(src.shape[1])])
            want_w = int(round(STEP_DOTS * spd))
            # ★**位置から窓を切って読む。**
            #   プラトー(同じ値が続く区間)で拾う方式は RGB555 では成立したが、
            #   RGB888 では量子化によるノイズ抑制が無く、1サンプルのノイズで
            #   16連続が8〜9に割れて段を取りこぼす(実測)。
            #   1段=16ドットという構造は既知なので、原点から16サンプルごとに
            #   窓を切り、その中央の最頻値を段の値とする方が確実。
            #   原点は band7(左半分が白ベタ)の白の開始位置から取る。
            if x_org is None:
                plats = plateaus(prof, max(4, want_w // 2))
                steps = [(v, w) for v, w in plats if w <= want_w * 2]
            else:
                steps = []
                for i in range(32):
                    a = x_org + int(round(i * want_w))
                    z = x_org + int(round((i + 1) * want_w))
                    if z > src.shape[1]:
                        break
                    win = src[:, a + want_w // 4: z - want_w // 4]
                    v = mode_of(win)
                    if v is None:
                        break
                    steps.append((v, want_w))
            # --- 検査3: プラトー幅 ---
            bad = [w for _, w in steps if abs(w - want_w) > max(2, want_w // 4)]
            if bad:
                warn.append("band%d(%s): 幅が %d から外れるプラトー %d個 %s"
                            % (b, BAND_NAMES[b], want_w, len(bad), bad[:5]))
            # --- 検査5: 正当なコードか ---
            if is555:
                illegal = [v for v, _ in steps if v not in LEGAL]
                if illegal:
                    warn.append("band%d(%s): RGB555 に無いコード %s"
                                % (b, BAND_NAMES[b], sorted(set(illegal))[:6]))
            bands[b] = {"kind": "ramp", "steps": [v for v, _ in steps],
                        "widths": [w for _, w in steps]}
        else:
            src = blk.max(axis=2).astype(np.int32)
            bands[b] = {"kind": "stripe", "level": mode_of(src[src > 24])}

    return Measured(bands, spd, (x0, x1, y0, y1)), None


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--bind", default="0.0.0.0",
                    help="送信元アドレス。VPN接続中は必須(例 192.168.11.24)")
    ap.add_argument("--board", default="255.255.255.255")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--pll", type=int, default=None,
                    help="測定前に pll_divide を設定する(512ドット系は 736)")
    ap.add_argument("--set", action="append", default=[], metavar="KEY=VAL",
                    help="測定前に送る CONFIG(16進可、複数指定可)。"
                         "例 --set 0x65=0x18 --set 0x68=0x18。"
                         "★設定した値は結果と一緒に必ず出力される")
    ap.add_argument("--frames", type=int, default=3,
                    help="一致を確認するフレーム数(既定2)")
    args = ap.parse_args()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    sock.bind((args.bind, 0))
    sock.settimeout(0.5)
    seq = [0]

    def send(pkt):
        sock.sendto(pkt, (args.board, args.port))
        seq[0] = (seq[0] + 1) & 0xFFFF

    cond = []
    if args.pll is not None:
        send(proto.pack_config(seq[0], proto.CFG_TARGET_BOARD,
                               proto.CFG_OP_SET, proto.CFG_KEY_PLL_DIVIDE, args.pll))
        cond.append("pll_divide=%d (このツールが設定)" % args.pll)
        time.sleep(3.0)
    for kv in args.set:
        k, _, v = kv.partition("=")
        key, val = int(k, 0), int(v, 0)
        send(proto.pack_config(seq[0], proto.CFG_TARGET_BOARD,
                               proto.CFG_OP_SET, key, val))
        cond.append("key 0x%04X = 0x%02X (%d) (このツールが設定)" % (key, val, val))
    if args.set:
        time.sleep(2.5)

    # --- いま効いている設定を読み戻して記録する ---
    #
    # ★**測定条件が残っていない測定は後で使えない。** ゲインやクランプが何だったか
    #   分からない数値を並べても、比べられないし再現もできない。
    #
    # 読み戻しには 34600 が要る。**ボードは CONFIG の応答を送信元ポートではなく
    # 固定の 34600 に返す**ので、映像受信用のエフェメラルポート(上の bind)では
    # 受け取れない。以前ここは「Viewer が 34600 を掴んでいると不可」とだけ
    # 書いていたが、実際には Viewer が起動していなくても読み戻していなかった。
    # 誰が掴んでいるか調べずに Viewer を名指しする文言だったため、
    # ポートを空けても直らない原因を探して遠回りした(2026-09-03)。
    READBACK = [
        (proto.CFG_KEY_PLL_DIVIDE, "pll_divide"),
        (0x0036, "pixfmt"), (0x0037, "black_th"), (0x0030, "full_line"),
        (0x001E, "gain_R"), (0x001D, "gain_G"), (0x001C, "gain_B"),
        (0x005F, "coarse_gain_GB"), (0x0067, "coarse_gain_R"),
        (0x0065, "coarse_off_G"), (0x0068, "coarse_off_R"),
        (0x005E, "clamp_sel"), (0x001F, "phase"), (0x0017, "video_bw"),
    ]
    try:
        rb = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        rb.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        rb.bind((args.bind, args.port))
        rb.settimeout(0.4)
    except OSError as e:
        rb = None
        cond.append("★読み戻し不可: UDP %d を bind できない (%s)。"
                    "誰が掴んでいるかは `lsof -nP -iUDP:%d` で分かる"
                    % (args.port, e, args.port))
    if rb is not None:
        got = {}
        for key, name in READBACK:
            for _ in range(3):
                rb.sendto(proto.pack_config(seq[0], proto.CFG_TARGET_BOARD,
                                            proto.CFG_OP_GET, key, 0),
                          (args.board, args.port))
                seq[0] = (seq[0] + 1) & 0xFFFF
                try:
                    d, _ = rb.recvfrom(2048)
                except socket.timeout:
                    continue
                try:
                    t, pk = proto.parse(d)
                except ValueError:
                    continue
                if t == proto.TYPE_CONFIG and pk.is_reply and pk.key == key:
                    got[name] = pk.value
                    break
        rb.close()
        if got:
            cond.append("読み戻し: " + "  ".join(
                "%s=%d" % (n, got[n]) for _, n in READBACK if n in got))
        miss = [n for _, n in READBACK if n not in got]
        if miss:
            cond.append("★読み戻せなかったキー: %s" % ", ".join(miss))

    def grab(timeout=15.0):
        asm = FrameAssembler()
        end = time.monotonic() + timeout
        last = 0.0
        n = 0
        while time.monotonic() < end:
            if time.monotonic() - last >= 1.0:
                send(proto.pack_subscribe(seq[0]))
                last = time.monotonic()
            try:
                d, _ = sock.recvfrom(65535)
            except socket.timeout:
                continue
            for fi, im, fill in asm.feed(d):
                n += 1
                if n >= 5:
                    return im, asm.mode
        return None, None

    results = []
    warn = []
    for i in range(args.frames):
        img, mode = grab()
        if img is None:
            sys.exit("★フレームを取得できなかった。測定を中止する")
        rowidx = content_rows(img)
        spd, corr = samples_per_dot(img, rowidx)
        if spd is None:
            sys.exit("★4px縞バンドが見つからず サンプル/ドット を決められない。中止")
        # --- 検査2: 1サンプル=1ドット ---
        if abs(spd - 1.0) > 0.02:
            sys.exit("★サンプル/ドット が %.3f(相関 %.3f)。1.000 でないと階調が\n"
                     "  隣と混ざるので校正できない。pll_divide を直すこと\n"
                     "  (X68000 512ドット系は 736 = 69.55199MHz/3 ÷ 31.5kHz)"
                     % (spd, corr))
        m, err = measure(img, spd, warn, is555=(mode.pixfmt == 1))
        if err:
            sys.exit("★%s。測定を中止する" % err)
        results.append((m, mode))
        if i + 1 < args.frames:
            time.sleep(1.0)

    # --- 検査1: フレーム間の一致 ---
    #
    # 全段が一致することを要求すると、境目の1段(量子化の縁で出たり出なかったり
    # する段)だけで全部が捨てられる。**全フレームに出た段を「確定」、一部だけに
    # 出た段を「境界」として区別して報告する**方が使える。
    # ★段は順序を持つ。集合で扱うとノイズ由来の値が「異なる値」に化けて
    #   「32段揃った」と誤判定する(実際に赤で誤判定した)。
    #   **段インデックスごとにフレーム間の中央値**を取る。
    confirmed, marginal = {}, {}
    steps_by_idx = {}
    for b in range(4):
        cols = [r[0].bands[b]["steps"] for r in results]
        n = min(len(c) for c in cols) if cols else 0
        steps_by_idx[b] = [int(np.median([c[i] for c in cols])) for i in range(n)]
        sets = [set(c) for c in cols]
        inter = set.intersection(*sets) if sets else set()
        union = set.union(*sets) if sets else set()
        confirmed[b] = sorted(inter)
        marginal[b] = sorted(union - inter)
    lv_sets = {b: {r[0].bands[b]["level"] for r in results} for b in range(4, 8)}

    m, mode = results[0]
    print("=== 測定条件 ===")
    print("  MODE htotal=%d dotclk=%.4f MHz fH=%.1f Hz"
          % (mode.htotal, mode.dotclk_hz / 1e6, mode.hfreq_mhz_x1000 / 1000.0))
    for c in cond:
        print("  " + c)
    print("  サンプル/ドット %.3f   有効領域 x=%d..%d y=%d..%d"
          % (m.samples_per_dot, *m.geom))
    print("  フレーム %d 枚で一致" % args.frames)


    is555 = (mode.pixfmt == 1)
    print("\n=== 階調バンド(X68000 の32階調がコードに載っているか)===")
    print("  伝送形式 pixfmt=%d (%s)"
          % (mode.pixfmt, "RGB555 2B/px" if is555 else
             "RGB888 3B/px" if mode.pixfmt == 0 else "?"))
    for b in range(4):
        cf, mg = confirmed[b], marginal[b]
        print("\n--- %s ---" % BAND_NAMES[b])
        if is555:
            ncf = sorted(LEGAL[c] for c in cf if c in LEGAL)
            nmg = sorted(LEGAL[c] for c in mg if c in LEGAL)
            print("  確定した段 %d 個: 5bit値 %s" % (len(ncf), ncf))
            if nmg:
                print("  境界の段 %d 個(フレームによって出入り): 5bit値 %s"
                      % (len(nmg), nmg))
            bad = [c for c in set(cf) | set(mg) if c not in LEGAL]
            if bad:
                print("  ★RGB555 に無いコード: %s" % sorted(bad))
            lost = [n for n in range(32) if n not in ncf and n not in nmg]
            if lost:
                print("  ★取り込めていない 5bit 値 %d個: %s" % (len(lost), lost))
            else:
                print("  → 32階調すべてが載っている(ロスレス成立)")
        else:
            # RGB888: 段インデックス順に並べ、単調性と間隔で判定する
            v = steps_by_idx[b]
            print("  段(0→31): " + " ".join("%3d" % x for x in v))
            if len(v) < 32:
                print("  ★窓が %d 個しか取れていない(32 のはず)" % len(v))
                continue
            d = [b_ - a_ for a_, b_ in zip(v, v[1:])]
            flat = [i for i, x in enumerate(d) if x <= 0]
            print("  段差    : " + " ".join("%3d" % x for x in d))
            print("  段差の統計: 最小%d 最大%d 平均%.2f" % (min(d), max(d),
                                                       sum(d) / len(d)))
            if flat:
                print("  ★段差が0以下の箇所 %d 個(段が分離していない): 段%s→%s"
                      % (len(flat), flat[0], flat[0] + 1))
                print("     → 潰れている段: %s" % [i for i in flat])
            else:
                print("  → **32段すべてが単調に分離している(ロスレス成立)**")

    print("\n=== 縞・ベタバンド(白の代表値)===")
    for b in range(4, 8):
        vs = sorted(lv_sets[b])
        tag = " ".join(("%s(5bit %s)" % (v, LEGAL.get(v, "★不正")) if is555
                        else "%s" % v) for v in vs)
        print("  %-8s %s%s" % (BAND_NAMES[b], tag,
                               "  ←フレーム間で不一致" if len(vs) > 1 else ""))

    if warn:
        print("\n=== 警告(数値は出したが疑わしい)===")
        for w in warn:
            print("  ! " + w)


if __name__ == "__main__":
    main()
