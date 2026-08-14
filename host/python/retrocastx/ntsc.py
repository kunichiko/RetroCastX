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
    隣接ラインの位相差  177.9°(180°)  → 2次元コムが成立する
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


def chroma_comb(rows, step=1, adapt_ire=0.0):
    """クロマ C を取り出す。`adapt_ire > 0` なら **2次元適応コム**(既定は使わない)。

    ★**輝度をここで作らない。** Y は「帯域制限した U,V を再変調して引いた残り」
    として `decode` で作る。理由は下記。

    ## 2次元コム(基本)

    隣接ラインの副搬送波は180°反転しているので、

        C = (x  -  (up + dn) / 2) / 2     輝度が打ち消える
        Y = x - C                        (= (x + (up+dn)/2)/2 と同値)

    上下の2本を平均してから使うのは、片側だけだと**垂直方向に半ラインずれる**
    ため(前の行だけを引くとYの重心が上へ寄る)。

    ## 適応を試した動機(と、それが外れだったこと)

    2次元コムは「上下のラインの色が同じ」を前提にしているので、原理的には小さく
    孤立した図形や水平エッジで崩れ、輝度差が色に化ける。実機で △本体設定 の
    色相が +25.5° ずれていたのはこれだろうと考えて適応コムを書いた。

    **が、実測では △ の誤差は変わらなかった**(25.5° → 25.9°)。つまりあの
    ずれの原因はコムではない。**期待値の120°の方が推測だった**可能性が高い
    (PS2の△の緑が本当に色相120°かは確かめていない。○=赤の0°は安全な仮定で、
    そちらは誤差 -1.0° に収まっている)。仮説を実測で否定できた形。

    ## 切り替えの判定に何を使うか

    **`up - dn` を使う。** upとdnはどちらもxから180°ずれているので、
    **互いには同位相** → 差を取るとクロマが消え、**純粋な垂直輝度差**が残る。
    実測(2026-08-14)で副搬送波成分が 412 → 49 になることを確認した。
    「クロマを含まない垂直detail検出器」がタダで手に入るのがNTSCの都合の良い所。

    ## 崩れているときの代替

    ライン内だけで副搬送波を抜くノッチに逃げる:

        C_notch = (2·x[n] - x[n-4] - x[n+4]) / 4

    8fsc では4サンプル = 副搬送波の半周期 = 180°なので、x[n±4] は fsc 成分が
    反転している。よって fsc は利得1で通り、DC は (2-1-1)/4 = 0 で消える。
    左右対称なので位相もずれない。垂直方向を一切見ないので図形で崩れない
    (代わりに輝度と色の混ざりが残る = ドットクロールが出る)。

    `adapt_ire` は「この輝度差[IRE]でノッチへ全振り」の閾値。

    ## ★既定はオフ。実測で**悪化した**(2026-08-14)

    PS2起動画面で測ったところ、期待に反して良くならなかった:

        白文字「システム設定」に出る偽色(無彩色なので出たら全部偽色)
            2次元コム        平均彩度 0.0260   上位1% 0.0681   ← 最良
            適応 4 IRE            0.0428          0.1065
            適応 8 IRE            0.0400          0.1025
            適応 64 IRE           0.0279          0.0705   ← 閾値を上げて1Dへ戻る

        既知の色の色相誤差:  2次元コム 最大25.5° / 適応8 最大25.9°(変わらない)

    ノッチ自体は正しい(合成6色で2次元コムと同じ誤差2.1°になることを確認済み)。
    悪化の理由は**素材側**: 白い文字は細い縦棒=水平方向の高い周波数を持つので、
    輝度エネルギーが副搬送波の近くにある。ノッチは垂直方向を一切見ないので
    それをクロマへ通してしまう。2次元コムは上下の行が似ているので弾ける。
    つまり垂直detail検出器が**文字の水平エッジで反応して、より悪いフィルタへ
    切り替えていた**。

    まともな2D適応にするには「垂直detailが大きく**かつ**水平方向のfscエネルギーが
    小さい」ときだけノッチへ行く必要がある。本筋は時間方向を見る3Dコム。
    このコードは検証済みの出発点として残すが、**既定では使わない**。
    """
    x = rows.astype(np.float64)

    def shift(a, k):
        """行方向に k 行ずらす(端は複製)。"""
        if k == 0:
            return a
        if k > 0:
            return np.vstack([a[k:], np.repeat(a[-1:], k, axis=0)])
        return np.vstack([np.repeat(a[:1], -k, axis=0), a[:k]])

    up, dn = shift(x, -step), shift(x, step)
    c_comb = (x - (up + dn) / 2.0) / 2.0
    if adapt_ire <= 0.0:
        return c_comb
    # 以下は「クロマ側を適応にする」実験用の経路。**既定では通らない**
    # (実測で白文字の偽色が悪化した。理由は上の docstring)。
    # 垂直輝度差。up と dn は互いに同位相なのでクロマが消え、純粋な垂直detailになる
    xm4 = np.roll(x, 4, axis=1)
    xp4 = np.roll(x, -4, axis=1)
    c_notch = (2.0 * x - xm4 - xp4) / 4.0
    vd = _boxcar(np.abs(up - dn), 8)
    a = np.clip(vd / adapt_ire, 0.0, 1.0)
    return (1.0 - a) * c_comb + a * c_notch


def decode(rows, sps, hue_deg=0.0, sat=1.0, chroma_lpf=16, adapt_ire=0.0):
    """1フィールドぶんの生サンプルを RGB(float, 0..1)にする。

    `adapt_ire` は2次元適応コムの閾値[IRE]。**既定0(=2次元コム)。実測で悪化した**
    ので使わない。理由は comb_separate のコメント参照。

    戻り値は (rgb[n_lines, n_active, 3], info)。
    """
    phi, mag, a = burst_phase(rows, sps)
    # 閾値はIREで受けてコードへ直す。レベル校正は下でやるので先に軽く求める
    ta, tb = _win((0.7, 4.2), sps)
    ba2, bb2 = _win((7.9, 9.3), sps)
    tip0 = np.median(rows[:, ta:tb])
    blank0 = np.median(rows[:, ba2:bb2])
    cpi0 = max(1e-6, (blank0 - tip0) / 40.0)
    c_full = chroma_comb(rows, 1, adapt_ire * cpi0)

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
    cth, sth = np.cos(th), np.sin(th)
    u = _boxcar(-2.0 * c_full * cth, chroma_lpf)
    v = _boxcar(+2.0 * c_full * sth, chroma_lpf)

    # --- 輝度は「帯域制限した U,V を再変調して引いた残り」にする ---
    #
    # ★**コムでもノッチでも駄目だった。** どちらも1本の線を3本に広げる:
    #
    #     C の作り方        縦線への水平応答     横棒への垂直応答
    #     Y = x - C_comb    100% の1本  ○       25%/50%/25%  ×
    #     Y = x - C_notch   25%/50%/25% ×       100% の1本   ○
    #     Y = x - Ĉ (これ)  100% の1本  ○       100% の1本   ○
    #
    #   実機で最初に横棒の二重化が出て、ノッチにしたら今度は**縦線が二重**になった
    #   (鼻の縦線が二重に見えた)。artefact を垂直から水平へ付け替えただけだった。
    #
    # C = a·cos(th) + b·sin(th) と書けるとき、復調とLPFで a = -u, b = v が出る。
    # そこから **同じ基底で再変調** すれば、実際に色として使う帯域制限された
    # クロマだけが引かれる:
    #
    #     Ĉ = a·cos(th) + b·sin(th) = -u·cos(th) + v·sin(th)
    #     Y = x - Ĉ
    #
    # これが素通しになる理由:
    #   - 垂直detailの無い縦線は C_comb = 0 なので Ĉ = 0 → Y = x
    #   - fsc成分の無い横棒は 復調+LPF で u,v ≈ 0 → Ĉ ≈ 0 → Y = x
    #   つまり**「色として取り出した分だけ」を引く**ので、余計な広がりが出ない。
    c_hat = -u * cth + v * sth
    y_full = rows.astype(np.float64) - c_hat

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
    ap.add_argument("--adapt-ire", type=float, default=0.0,
                    help="2次元適応コムの閾値[IRE]。0=2次元コム(既定)。"
                         "実測では偽色が悪化したので通常は使わない")
    args = ap.parse_args()

    rows, ln, meta, frame = load_field(args.inp, args.frame)
    sps = meta["dotclk_hz"]
    print("frame %d: %d行 × %dサンプル  dotclk=%.4fMHz  fH=%.1fHz"
          % (frame, rows.shape[0], rows.shape[1], sps / 1e6, meta["fh_hz"]))
    print("副搬送波 %.2f サンプル/周期" % (sps / FSC_NTSC))

    rgb, info = decode(rows, sps, args.hue, args.sat, args.lpf, args.adapt_ire)
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
