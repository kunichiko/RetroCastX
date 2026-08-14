#!/usr/bin/env python3
"""ntsc.py の復調を、既知の色で合成したNTSC信号で検証する(実機不要)。

**この試験を書いた理由。** 実機で色相を合わせていたとき、どう回しても
既知の2色(PS2の ○=赤 / △=緑)が同時に合わなかった。原因は V(R-Y)軸の
符号が逆だったこと。**1色だけで合わせていたら「色相オフセットが要るのだな」で
片付いてしまい、気づけなかった。** 回転では消せない食い違いとして出るのは、
既知の色を2つ以上使ったときだけ。

だからこの試験は**必ず複数の色**を通す。合成なので真値が分かっている。

Run:  python3 -m retrocastx.tests.test_ntsc
"""
import math
import sys

import numpy as np

from .. import ntsc

SPS = 8 * ntsc.FSC_NTSC            # 8fsc
H_US = 1e6 / 15734.264
CPI = 0.78                          # 1 IRE あたりのコード数(実測相当)
BLANK = 158.0                       # ブランキングのコード(ミッドレベルクランプ)


def make_lines(colors, n_lines=24, v_sign=1.0):
    """既知のRGBから合成NTSCラインを作る。

    ラインごとに副搬送波の位相を180°反転させる(NTSCは227.5周期/ライン)。
    これが無いとコムフィルタが試験にならない。
    """
    n = int(round(H_US * 1e-6 * SPS))
    a = int(5.4e-6 * SPS)                    # バースト区間の先頭
    out = np.zeros((n_lines, n), dtype=np.uint8)
    aa = int(ntsc.ACTIVE_US[0] * 1e-6 * SPS)
    for li in range(n_lines):
        # ライン毎に180°반전 → 位相 π·li
        flip = math.pi * li
        row = np.full(n, BLANK)
        k = np.arange(n)
        # ψ(n) = 2π(n-a)/8 - φ、バーストは φ=flip になるように置く
        psi = 2 * np.pi * (k - a) / 8.0 - flip
        # 同期チップ(-40 IRE)
        row[: int(4.7e-6 * SPS)] = BLANK - 40.0 * CPI
        # カラーバースト 40 IRE p-p。cos(ψ) の山になるよう置く
        b0, b1 = a, int(7.7e-6 * SPS)
        row[b0:b1] = BLANK + 20.0 * CPI * np.cos(psi[b0:b1])
        # アクティブ映像: 色ごとに Y/U/V を載せる
        seg = slice(aa, n)
        width = n - aa
        per = width // len(colors)
        for ci, (r, g, b) in enumerate(colors):
            s = aa + ci * per
            e = aa + (ci + 1) * per if ci < len(colors) - 1 else n
            y = 0.299 * r + 0.587 * g + 0.114 * b
            u = 0.493 * (b - y)
            v = 0.877 * (r - y)
            # ntsc.decode の逆: U = -2C·cos(ψ), V = +2C·sin(ψ)
            #  → C = (-U·cos(ψ) + V·sin(ψ)) / 2
            p = psi[s:e]
            c = (-u * np.cos(p) + v_sign * v * np.sin(p)) / 2.0
            row[s:e] = BLANK + (y * 100.0 + c * 100.0) * CPI
        out[li] = np.clip(np.round(row), 0, 255)
    return out


def hue_of(rgb, x0, x1, y0, y1):
    import colorsys
    p = rgb[y0:y1, x0:x1].reshape(-1, 3)
    m = p.mean(0)
    return colorsys.rgb_to_hsv(*m)[0] * 360.0, (m.max() - m.min())


def ang(a, b):
    return (a - b + 540) % 360 - 180


def check(name, cond, detail=""):
    print("%s %s%s" % ("PASS" if cond else "FAIL", name,
                       "" if cond else "  <- " + detail))
    return cond


def main():
    ok = []

    # --- 1本の線が1本のまま出ること(水平も垂直も) ---
    #
    # **輝度の作り方を2回間違えた。** どちらも「1本を3本に広げる」形で出た:
    #     Y = x - C_comb   横棒が 25%/50%/25% に広がる(垂直)
    #     Y = x - C_notch  縦線が 25%/50%/25% に広がる(水平)
    # 正解は「帯域制限したU,Vを再変調して引く」= 色として取り出した分だけを引く。
    # 実機では「漢字の横棒が二重」→(ノッチに変更)→「鼻の縦線が二重」と
    # artefact が付け替わっただけだった。**両方向を同時に見る試験でないと防げない。**
    def impulse(vertical):
        nn2, hh = 1820, 16
        ba = int(ntsc.BURST_US[0] * 1e-6 * SPS)
        bb = int(ntsc.BURST_US[1] * 1e-6 * SPS)
        aa = int(9.6e-6 * SPS)
        xx = np.full((hh, nn2), BLANK)
        for yy in range(hh):
            k = np.arange(nn2)
            psi = 2 * np.pi * (k - ba) / 8.0 - np.pi * yy
            xx[yy, :int(4.7e-6 * SPS)] = BLANK - 40 * CPI
            xx[yy, ba:bb] = BLANK + 20 * CPI * np.cos(psi[ba:bb])
        tgt = aa + 400
        if vertical:
            xx[hh // 2, aa:] += 80.0
        else:
            xx[:, tgt] += 80.0
        rgb, _ = ntsc.decode(xx, SPS)
        Y = rgb.mean(2) * 255.0
        line, c = (Y[:, 400], hh // 2) if vertical else (Y[hh // 2], tgt - aa)
        base = np.median(line)
        pk = line[c] - base
        side = max(abs(line[c - 1] - base), abs(line[c + 1] - base),
                   abs(line[c - 4] - base), abs(line[c + 4] - base))
        return pk, side / max(abs(pk), 1e-6)

    for vert, nm in ((False, "縦線(水平方向)"), (True, "横棒(垂直方向)")):
        pk, leak = impulse(vert)
        ok.append(check("1本の%sが1本のまま出る" % nm, abs(pk) > 20 and leak < 0.2,
                        "山 %.1f  隣への漏れ %.0f%%(3本に広がっていれば50%%)"
                        % (pk, 100 * leak)))

    # --- クロマLPFが 2fsc をきっちり消すこと ---
    #
    # 直交復調の積には必ず 2fsc(周期4サンプル)が出る。窓長が副搬送波1周期(8)の
    # 倍数なら整数周期ぶん入って完全に消える。**消えないと平坦な色面に周期4サンプルの
    # 縞が出る**(実機の赤ベタで「赤黒赤黒」に見えた)。Rustへの移植でここを壊して
    # いたので、両方に同じ試験を置く。
    nn = 1024
    b2 = ntsc._boxcar((10.0 + np.cos(np.pi * 0.5 * np.arange(nn)))[None, :], 16)[0]
    ok.append(check("クロマLPFが2fscを消す",
                    np.abs(b2[64:-64] - 10.0).max() < 1e-6,
                    "残留 %.2e" % np.abs(b2[64:-64] - 10.0).max()))
    bf = ntsc._boxcar(np.sin(np.pi * 0.25 * np.arange(nn))[None, :], 16)[0]
    ok.append(check("クロマLPFがfscを消す", np.abs(bf[64:-64]).max() < 1e-6,
                    "残留 %.2e" % np.abs(bf[64:-64]).max()))
    st = ntsc._boxcar((np.arange(nn) >= nn // 2).astype(float)[None, :], 16)[0]
    c = nn // 2
    ok.append(check("クロマLPFの中心がずれていない",
                    abs(st[c] - 0.5) < 0.02 and abs(st[c - 1] + st[c + 1] - 1.0) < 0.02,
                    "s[c]=%.4f s[c-1]+s[c+1]=%.4f" % (st[c], st[c - 1] + st[c + 1])))
    # 6色。**1色では符号の誤りを検出できない**ので必ず複数使う
    colors = [(0.75, 0.0, 0.0), (0.0, 0.75, 0.0), (0.0, 0.0, 0.75),
              (0.75, 0.75, 0.0), (0.0, 0.75, 0.75), (0.75, 0.0, 0.75)]
    names = ["赤", "緑", "青", "黄", "シアン", "マゼンタ"]
    import colorsys
    want = [colorsys.rgb_to_hsv(*c)[0] * 360.0 for c in colors]

    rows = make_lines(colors)
    phi, mag, a = ntsc.burst_phase(rows, SPS)
    ok.append(check("バーストが全ラインで検出できる", (mag > 100).all(),
                    "mag min=%.0f" % mag.min()))
    d = np.abs((np.diff(np.degrees(phi)) + 540) % 360 - 180)
    ok.append(check("隣接ラインの位相差が180°", abs(np.median(d) - 180) < 2,
                    "中央値 %.1f°" % np.median(d)))

    rgb, info = ntsc.decode(rows, SPS)
    ok.append(check("1 IRE あたりのコード数を復元できる",
                    abs(info["code_per_ire"] - CPI) < 0.05,
                    "%.3f (期待 %.2f)" % (info["code_per_ire"], CPI)))

    # 色帯の中央でそれぞれの色相を見る。端はクロマLPFの過渡が乗るので避ける
    w = rgb.shape[1]
    per = w // len(colors)
    errs = []
    print("\n%-8s %8s %8s %9s" % ("色", "復調", "真値", "誤差"))
    for i, nm in enumerate(names):
        x0 = i * per + per // 4
        x1 = i * per + per * 3 // 4
        h, s = hue_of(rgb, x0, x1, 6, 18)
        e = ang(h, want[i])
        errs.append(abs(e))
        print("%-8s %7.1f° %7.1f° %+8.1f°" % (nm, h, want[i], e))
    ok.append(check("\n6色すべての色相が真値に一致する(誤差<8°)",
                    max(errs) < 8.0, "最大誤差 %.1f°" % max(errs)))

    # ★**この試験が本当に符号の誤りを捕まえるか**を確かめる。
    #   V の符号を逆にした信号を作って通し、「どう色相を回しても6色が同時には
    #   合わない」ことを見る。実機で踏んだ誤りをそのまま再現している。
    #   これが無いと「常にPASSする試験」になっていても気づけない。
    bad_rows = make_lines(colors, v_sign=-1.0)
    bad_rgb, _ = ntsc.decode(bad_rows, SPS)
    best = None
    for rot in range(0, 360, 5):
        es = []
        for i in range(len(colors)):
            x0 = i * per + per // 4
            x1 = i * per + per * 3 // 4
            h, _ = hue_of(bad_rgb, x0, x1, 6, 18)
            es.append(abs(ang(h + rot, want[i])))
        m = max(es)
        if best is None or m < best:
            best = m
    ok.append(check("V符号が逆だと、どう色相を回しても6色は合わない",
                    best > 20.0,
                    "最良の回転でも最大誤差 %.1f° しか残らない = 試験が無意味" % best))
    print("   (符号を逆にした信号: 最良の回転でも最大誤差 %.1f° 残る)" % best)

    print("\n%d/%d PASS" % (sum(ok), len(ok)))
    sys.exit(0 if all(ok) else 1)


if __name__ == "__main__":
    main()
