#!/usr/bin/env python3
"""デジタルRGB: 生成器 → 測定器 をシミュレーションで直結して確かめる。

★**実機の前に回す。** ループバックには6本のジャンパ(デバッグ端子 J4 → J13)が要る。
  線を作る前に論理を確定させておかないと、映らなかったときに「線か、極性か、
  論理か」の三択になる。

★**小さい寸法で回す。** 実寸(953×262×3 = 74万サイクル/フレーム)を Migen で
  回すと1フレームに数分かかり、試験として使い物にならない。幾何は引数なので、
  1/10 の寸法で**同じ論理**を回す。実寸の方は算術だけで確かめる。

検証項目:
1. 実寸のタイミングが狙いどおり(fH が実機の 15,734Hz に十分近い)
2. 測定器が**極性を測れる**(決め打ちしていない)。正極性でも負極性でも同じ結果
3. 1ドット幅の縞が潰れずに数えられる(=ドット位置合わせが効いている)
4. 16色バーの色が正しい位置で読める
5. vtotal と1ラインのクロック数が測れる
"""
import sys, os
sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_drgb import (DigitalRgbGen, DigitalRgbProbe, DOT_HZ, HTOTAL,
                             VTOTAL, REAL_DOT_HZ, REAL_HTOTAL)

SYS = 45_000_000

# --- 試験用の小さい幾何。比率は実寸に近づける ---
G = dict(htotal=96, vtotal=26, hactive=64, vactive=20,
         hstart=12, vstart=4, hs_width=6, vs_width=1, dot_div=1)

# ★**2つのクロックをわざと割り切れない比にする。** 実機は
#   sys 45MHz / dot 12.288MHz = 3.662 サンプル/ドットで、**分数**になる。
#   ここを整数(例えば3.000)にすると、ドット境界が毎ライン同じ位相に来て
#   **受け側の分数アキュムレータが一度も効かないまま通ってしまう**。
#   ★**周期は偶数にする。** Migen の simulator は半周期で刻むので、奇数を渡すと
#     切り捨てられる(37 → 36)。気付かないと「期待 355 に対して 345」という、
#     原因の見えないずれになる。
#   sys 100 / aud 366 = 3.66 サンプル/ドット。実機の 3.662 とほぼ同じ。
SYS_PERIOD = 100
AUD_PERIOD = 366
CLOCKS = {"sys": SYS_PERIOD, "aud": AUD_PERIOD}
SAMPLES_PER_DOT = AUD_PERIOD / SYS_PERIOD

# 小さい幾何の1フレーム = 96*26 aud サイクル = 2496*3.7 = 9235 sys サイクル
FRAME_SYS = int(G["htotal"] * G["vtotal"] * SAMPLES_PER_DOT)
MEAS = FRAME_SYS * 4
POL = FRAME_SYS * 2


class Pads(Module):
    def __init__(self):
        for n in ("r", "g", "b", "i", "hs", "vs"):
            setattr(self, n, Signal())


class Loop(Module):
    """生成器の出力を測定器へそのまま繋ぐ(実機のジャンパ線の代わり)。"""

    def __init__(self, neg_sync, row, dot):
        # 生成器は aud、測定器は sys。**実機と同じ配置**
        self.submodules.gen = gen = ClockDomainsRenamer("aud")(DigitalRgbGen(**G))
        self.submodules.pads = pads = Pads()
        self.submodules.probe = probe = DigitalRgbProbe(
            pads, SYS, meas_clks=MEAS, pol_clks=POL)
        self.comb += [
            gen.neg_sync.eq(neg_sync),
            pads.r.eq(gen.r), pads.g.eq(gen.g), pads.b.eq(gen.b),
            pads.i.eq(gen.i), pads.hs.eq(gen.hs), pads.vs.eq(gen.vs),
            probe.cfg_htotal.eq(G["htotal"]),
            probe.cfg_hstart.eq(G["hstart"]),
            probe.cfg_hactive.eq(G["hactive"]),
            probe.cfg_row.eq(row),
            probe.cfg_dot.eq(dot),
        ]
        self.bar_w = gen.bar_w


def run(neg_sync, row, dot, frames=6):
    dut = Loop(neg_sync, row, dot)
    res = {"bar_w": dut.bar_w}
    steps = FRAME_SYS * frames

    def tb():
        for _ in range(steps):
            yield
        for k in ("lines", "hlen", "pol", "pixel", "edges", "fh", "fv"):
            res[k] = (yield getattr(dut.probe, "stat_" + k))

    run_simulation(dut, tb(), clocks=CLOCKS)
    return res


def main():
    print(f"サンプル/ドット: 試験 {SAMPLES_PER_DOT:.3f} / 実機 {SYS/DOT_HZ:.3f}"
          f"  ★どちらも分数であること(整数だと分数アキュムレータを試験できない)")
    assert SAMPLES_PER_DOT != int(SAMPLES_PER_DOT), \
        "サンプル/ドットが整数。これでは受け側の位置合わせを試験できない"

    # --- 1) 実寸のタイミング(算術) ---
    gen_fh = DOT_HZ / HTOTAL
    real_fh = REAL_DOT_HZ / REAL_HTOTAL
    print(f"実寸: 生成 fH {gen_fh:.1f}Hz / 実機 fH {real_fh:.1f}Hz "
          f"(差 {abs(gen_fh-real_fh)/real_fh*100:.3f}%)  fV {gen_fh/VTOTAL:.2f}Hz")
    assert abs(gen_fh - real_fh) / real_fh < 0.001, "実機の fH から 0.1% 以上ずれている"

    # 1ラインの sys クロック数。aud で htotal ドット進む時間
    hs_per_line = int(round(G["htotal"] * SAMPLES_PER_DOT))
    for neg in (1, 0):
        name = "負極性" if neg else "正極性"
        # 上半分のカラーバー。bar_w=4 なので dot=4..7 が色1(R)
        r = run(neg, row=G["vstart"] + 2, dot=5)
        print(f"{name}: vtotal={r['lines']} hlen={r['hlen']} pol={r['pol']:#04b} "
              f"pixel={r['pixel']:#07b} fh={r['fh']} fv={r['fv']} bar_w={r['bar_w']}")
        assert r['lines'] == G["vtotal"], f"{name}: vtotal={r['lines']}"
        # ★**ぴったり一致しない。** クロックが割り切れないので、ライン長は
        #   毎ライン ±1 サンプル揺れる ─ これが実機で起きることそのもの。
        assert abs(r['hlen'] - hs_per_line) <= 1, \
            f"{name}: hlen={r['hlen']} (期待 {hs_per_line}±1)"
        # ★極性を測れていること
        assert (r['pol'] & 0b11) == (0b11 if neg else 0b00), \
            f"{name}: 極性を測れていない pol={r['pol']:#04b}"
        assert r['pixel'] & 0x10, f"{name}: 覗き窓が発火していない"
        assert (r['pixel'] & 0xF) == 1, f"{name}: 色={r['pixel'] & 0xF} (期待 1 = R)"
        # publish 周期に入る HS の本数
        want_fh = MEAS // hs_per_line
        assert abs(r['fh'] - want_fh) <= 2, f"{name}: fh={r['fh']} (期待 {want_fh})"

    # --- 色を一通り読む ---
    bar_w = run(1, 0, 0)["bar_w"]
    for want in (0, 1, 2, 4, 8, 15):
        d = want * bar_w + bar_w // 2
        r = run(1, row=G["vstart"] + 2, dot=d)
        got = r['pixel'] & 0xF
        assert got == want, f"色 {want} を dot={d} で読んだら {got}"
    print(f"16色バー: 0/1/2/4/8/15 を正しく読めた(バー幅 {bar_w} ドット)")

    # --- 1ドット縞 ---
    #
    # ★**ここが本題。** 色が読めるだけならクロックが2倍ずれていても通る。
    #   1ドットごとに変わる縞を数え切れて初めて、位置合わせが効いている。
    r = run(1, row=G["vstart"] + G["vactive"] - 2, dot=0)
    print(f"1ドット縞: edges={r['edges']} (期待 {G['hactive']-1} 付近)")
    assert r['edges'] >= G["hactive"] - 2, f"縞が潰れている: {r['edges']}"
    assert r['edges'] <= G["hactive"] + 1, f"数えすぎ: {r['edges']}"

    print("\n[OK] デジタルRGB: 実寸タイミング / 極性の測定 / 16色の読み出し / "
          "1ドット縞の解像 / vtotal・ライン長の測定 を確認")


if __name__ == "__main__":
    main()
