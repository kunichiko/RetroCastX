#!/usr/bin/env python3
"""NTSCコンポジットの復調(Y/C分離 + 直交復調)。オフライン開発用の参照実装。

TVP7002は digitizer であって decoder ではないので、復調はここから先の仕事。
`videoin capture` が落とした .npz を読んで RGB にする。

    python3 -m retrocastx.videoin --board 255.255.255.255 capture --out cvbs.npz
    python3 -m retrocastx.ntsc --in cvbs.npz --out out.png

**Viewer(Rust)に載せる前にここで作る。** 復調は係数と位相の詰めが要るので、
実機を占有したまま1回ずつ確認していると回転が遅い。一度録ればオフラインで
何通りでも試せる(この方針は docs/composite-video-plan.md §4 の理由1〜3と同じ)。

## 成立の前提(すべて実測で確認済み、2026-08-14)

    サンプルレート  8fsc = 1820サンプル/ライン  → 副搬送波が**きっちり8サンプル/周期**
    バースト        40 IRE p-p / 31コード
    隣接ラインの位相差  177.9°(180°)  → 1Dコムが成立する
    2ライン差の位相差    3.7°(0°)

ライン番号は1フィールドぶんが飛び飛び(奇数だけ)に入っているが、**配列上の
隣接行は時間的に隣接**していて180°反転している。だから配列の隣同士でコムを組む。

## なぜ 8fsc だと楽か

1周期8サンプルなので、cos/sin が 1, 0.707, 0, -0.707, -1, ... の8点しか出てこない。
位相基準がバーストから決まれば、直交復調は実質**加減算と定数倍**で書ける。
Viewer(Rust)へ移すときも、この性質がそのまま効く。
"""
import argparse
import math

import numpy as np

FSC_NTSC = 3_579_545.0

# ライン内の区間 [µs]。同期立ち下がりを0とする(キャプチャは HSOUT の
# 立ち下がりから始まっているので、そのまま使える)
BURST_US = (5.4, 7.7)
ACTIVE_US = (9.6, 62.0)


def _win(us, sps):
    return int(us[0] * 1e-6 * sps), int(us[1] * 1e-6 * sps)


def burst_phase(rows, sps):
    """各ラインのバースト位相[rad]と振幅を測る。

    位相はバースト区間の先頭サンプル(a)を基準に返す。すなわちバーストは
    `A·cos(2π(n-a)/8 - φ)` と表せる。

    **ライン毎に測るのが要点。** ライン番号のパリティから予測すると、行が
    1本落ちただけで以降の色が全部反転する。実時間で測れば落ちても復帰する
    (「ドット数・ライン数に頼らず実時間で位置を決める」という方針と同じ)。
    """
    a, b = _win(BURST_US, sps)
    k = np.arange(b - a)
    cw = np.cos(2 * np.pi * k / 8.0)
    sw = np.sin(2 * np.pi * k / 8.0)
    seg = rows[:, a:b].astype(np.float64)
    seg -= seg.mean(axis=1, keepdims=True)
    # macOS の Accelerate BLAS は matmul で浮動小数点例外フラグを立てることが
    # あり、「overflow / invalid / divide by zero encountered in matmul」という
    # 警告が出る。**計算結果は正しい**: 同じ積和をスカラーのループで求めた値と
    # 5.7e-14 まで一致し、NaN も Inf も出ていないことを確認済み(2026-08-14)。
    # 実害の無い警告で実害のある警告が埋もれる方が困るので、ここだけ黙らせる。
    with np.errstate(all="ignore"):
        ci = seg @ cw
        si = seg @ sw
    return np.arctan2(si, ci), np.hypot(ci, si), a


def _boxcar(x, n):
    """移動平均。**長さを副搬送波1周期(8サンプル)の倍数にする。**

    直交復調の積には 2fsc の成分が必ず出る。8の倍数で平均すると、その成分が
    ちょうど整数周期ぶん入って完全に消える。窓長を適当に選ぶと 2fsc が残り、
    色に細かい縞が乗る。
    """
    if n <= 1:
        return x
    k = np.ones(n) / n
    pad = n // 2
    xp = np.pad(x, ((0, 0), (pad, pad)), mode="edge")
    return np.apply_along_axis(lambda r: np.convolve(r, k, mode="same"), 1, xp)[
        :, pad:pad + x.shape[1]]


def comb_separate(rows):
    """1Dコムで Y と C に分ける。

    隣接ラインの副搬送波は180°反転しているので、

        Y = (x[i]  +  (x[i-1] + x[i+1]) / 2) / 2     クロマが打ち消える
        C = (x[i]  -  (x[i-1] + x[i+1]) / 2) / 2     輝度が打ち消える

    上下の2本を平均してから使うのは、片側だけだと**垂直方向に半ラインずれる**
    ため(前の行だけを引くとYの重心が上へ寄る)。

    端の行は相手がいないので、そのままY=x・C=0にする(色が出ないだけで壊れない)。
    """
    x = rows.astype(np.float64)
    up = np.vstack([x[:1], x[:-1]])
    dn = np.vstack([x[1:], x[-1:]])
    avg = (up + dn) / 2.0
    y = (x + avg) / 2.0
    c = (x - avg) / 2.0
    y[0] = x[0]; y[-1] = x[-1]
    c[0] = 0.0;  c[-1] = 0.0
    return y, c


def decode(rows, sps, hue_deg=0.0, sat=1.0, chroma_lpf=16):
    """1フィールドぶんの生サンプルを RGB(float, 0..1)にする。

    戻り値は (rgb[n_lines, n_active, 3], info)。
    """
    phi, mag, a = burst_phase(rows, sps)
    y_full, c_full = comb_separate(rows)

    n = np.arange(rows.shape[1])
    # ψ(n) = 2π(n-a)/8 - φ   … この位相でバーストが cos の山になる
    psi = (2 * np.pi * (n[None, :] - a) / 8.0) - phi[:, None]
    # NTSCのバーストは -(B-Y) 軸(位相180°)なので、U軸は ψ+180°。
    #   U = 2·C·cos(ψ+180°) = -2·C·cos(ψ)
    #   V = U軸から90°回した軸。**回す向きは符号の規約で決まる。**
    #
    # ★V の符号は実測で決めた(2026-08-14)。PS2起動画面の ○=赤 / △=緑 という
    #   既知の2色に対し、色相をどう回しても両方は合わなかった(2色の開きが
    #   149.8°、本来は240°)。V を反転すると開きが 200.4° になり、○の誤差も
    #   -85.7° → -7° に落ちた。**1色だけで合わせていたら気づけない誤り。**
    #   既知の色を2つ使うと、回転では消せない食い違いとして出る。
    th = psi + math.radians(hue_deg)
    u = -2.0 * c_full * np.cos(th)
    v = +2.0 * c_full * np.sin(th)
    u = _boxcar(u, chroma_lpf)
    v = _boxcar(v, chroma_lpf)

    # --- レベルをIREに直す。基準は同期チップとブランキング(絵の内容に依存しない)
    ta, tb = _win((0.7, 4.2), sps)
    ba, bb = _win((7.9, 9.3), sps)
    tip = np.median(rows[:, ta:tb])
    blank = np.median(rows[:, ba:bb])
    code_per_ire = max(1e-6, (blank - tip) / 40.0)

    aa, ab = _win(ACTIVE_US, sps)
    Y = (y_full[:, aa:ab] - blank) / code_per_ire / 100.0      # 0..1 (100 IRE=1.0)
    U = u[:, aa:ab] / code_per_ire / 100.0 * sat
    V = v[:, aa:ab] / code_per_ire / 100.0 * sat

    # U=0.493(B-Y) / V=0.877(R-Y)
    b_y = U / 0.493
    r_y = V / 0.877
    R = Y + r_y
    B = Y + b_y
    G = Y - 0.5094 * r_y - 0.1942 * b_y
    rgb = np.clip(np.stack([R, G, B], axis=-1), 0.0, 1.0)
    info = {
        "burst_phase_deg": np.degrees(phi),
        "burst_mag": mag,
        "tip": tip, "blank": blank, "code_per_ire": code_per_ire,
        "active": (aa, ab),
    }
    return rgb, info


def load_field(path, frame=None):
    """.npz から1フレーム(=1フィールド)ぶんをライン番号順に取り出す。"""
    d = np.load(path, allow_pickle=True)
    y, ln, fr = d["y"], d["line"], d["frame"]
    meta = d["meta"].item()
    if frame is None:
        # 行数が最も揃っているフレームを選ぶ(端のフレームは欠けやすい)
        u, cnt = np.unique(fr, return_counts=True)
        frame = int(u[np.argmax(cnt)])
    m = fr == frame
    o = np.argsort(ln[m])
    return y[m][o], ln[m][o], meta, frame


def to_png(rgb, path, width=720):
    """PNGで書き出す。外部ライブラリ無しで済ませる(zlib + 手書きチャンク)。"""
    import struct
    import zlib

    h, w, _ = rgb.shape
    if width and width != w:
        idx = (np.arange(width) * (w / width)).astype(int).clip(0, w - 1)
        rgb = rgb[:, idx]
        w = width
    img = (rgb * 255.0 + 0.5).astype(np.uint8)
    raw = b"".join(b"\x00" + img[r].tobytes() for r in range(h))

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 6))
           + chunk(b"IEND", b""))
    with open(path, "wb") as f:
        f.write(png)
    return w, h


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--in", dest="inp", required=True, help="videoin capture の .npz")
    ap.add_argument("--out", default="ntsc.png")
    ap.add_argument("--frame", type=int, default=None)
    ap.add_argument("--hue", type=float, default=0.0, help="色相の補正[度]")
    ap.add_argument("--sat", type=float, default=1.0, help="彩度の倍率")
    ap.add_argument("--lpf", type=int, default=16,
                    help="クロマの移動平均長[サンプル]。8の倍数にすること")
    ap.add_argument("--width", type=int, default=720)
    args = ap.parse_args()

    rows, ln, meta, frame = load_field(args.inp, args.frame)
    sps = meta["dotclk_hz"]
    print("frame %d: %d行 × %dサンプル  dotclk=%.4fMHz  fH=%.1fHz"
          % (frame, rows.shape[0], rows.shape[1], sps / 1e6, meta["fh_hz"]))
    print("副搬送波 %.2f サンプル/周期" % (sps / FSC_NTSC))

    rgb, info = decode(rows, sps, args.hue, args.sat, args.lpf)
    ok = info["burst_mag"] > 200
    print("同期チップ %.0f / ブランキング %.0f → 1 IRE = %.2f コード"
          % (info["tip"], info["blank"], info["code_per_ire"]))
    print("バーストが取れた行: %d / %d" % (ok.sum(), len(ok)))
    if ok.sum() >= 2:
        d = np.diff(info["burst_phase_deg"][ok])
        d = (d + 540) % 360 - 180
        print("隣接ラインの位相差 中央値 = %.1f°(180°ならコムが成立)"
              % np.median(np.abs(d)))
    w, h = to_png(rgb, args.out, args.width)
    print("書き出し: %s (%dx%d)" % (args.out, w, h))
    # 色がどれだけ載っているかの目安
    mx = rgb.max(axis=2) - rgb.min(axis=2)
    print("彩度(R,G,Bの開き) 平均 %.3f / 上位1%% %.3f"
          % (mx.mean(), np.quantile(mx, 0.99)))


if __name__ == "__main__":
    main()
