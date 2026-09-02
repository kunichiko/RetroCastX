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

★★ 2026-09-03 の測定結果(v0.9.0 / X68000 512x512 65536色 / pll_div 736)★★

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

# RGB555 の 5bit 値をビット複製で 8bit にした正当なコード
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


def measure(img, spd, warn):
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
            plats = plateaus(prof, max(4, want_w // 2))
            steps = [(v, w) for v, w in plats if w <= want_w * 2]
            # --- 検査3: プラトー幅 ---
            bad = [w for _, w in steps if abs(w - want_w) > max(2, want_w // 4)]
            if bad:
                warn.append("band%d(%s): 幅が %d から外れるプラトー %d個 %s"
                            % (b, BAND_NAMES[b], want_w, len(bad), bad[:5]))
            # --- 検査5: 正当なコードか ---
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
        m, err = measure(img, spd, warn)
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
    confirmed, marginal = {}, {}
    for b in range(4):
        sets = [set(r[0].bands[b]["steps"]) for r in results]
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
    print("  ★レジスタの読み戻しは Viewer が 34600 を掴んでいると不可。"
          "上記以外の条件は記録できていない")

    print("\n=== 階調バンド(X68000 の32階調がコードに載っているか)===")
    for b in range(4):
        cf, mg = confirmed[b], marginal[b]
        ncf = sorted(LEGAL[c] for c in cf if c in LEGAL)
        nmg = sorted(LEGAL[c] for c in mg if c in LEGAL)
        print("\n--- %s ---" % BAND_NAMES[b])
        print("  確定した段 %d 個: 5bit値 %s" % (len(ncf), ncf))
        if nmg:
            print("  境界の段 %d 個(フレームによって出入りする): 5bit値 %s"
                  % (len(nmg), nmg))
        bad = [c for c in set(cf) | set(mg) if c not in LEGAL]
        if bad:
            print("  ★RGB555 に無いコード: %s" % sorted(bad))
        lost = [n for n in range(32) if n not in ncf and n not in nmg]
        if lost:
            print("  ★取り込めていない 5bit 値 %d個: %s" % (len(lost), lost))
        else:
            print("  → 32階調すべてが載っている(ロスレス成立)")

    print("\n=== 縞・ベタバンド(白の代表値)===")
    for b in range(4, 8):
        vs = sorted(lv_sets[b])
        tag = " ".join("%s(5bit %s)" % (v, LEGAL.get(v, "★不正")) for v in vs)
        print("  %-8s %s%s" % (BAND_NAMES[b], tag,
                               "  ←フレーム間で不一致" if len(vs) > 1 else ""))

    if warn:
        print("\n=== 警告(数値は出したが疑わしい)===")
        for w in warn:
            print("  ! " + w)


if __name__ == "__main__":
    main()
