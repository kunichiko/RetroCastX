#!/usr/bin/env python3
"""videoin.py の波形解析を、合成したNTSC 1ラインで検証する(実機不要)。

実機が繋がる前に確かめておきたいのは、

  1. 区間の窓(同期チップ/バースト/バックポーチ)が µs で正しく置けているか
  2. 同期チップ〜ブランキングの40 IREから 1 IRE あたりのコード数が出るか
  3. バーストの周期推定が 8fsc で 8.00 サンプル/周期を返すか
  4. **失敗の形を区別できるか** — ボトムレベルクランプ(バーストの下半分が消える)と
     ゲイン過大(白が飽和)を、それぞれの症状として指摘できるか

4 が本題。実機では両方とも「絵は出ているのに復調できない」形で失敗するので、
数値で切り分けられないと延々ゲインを振ることになる。

Run:  python3 -m retrocastx.tests.test_videoin
"""
import math
import sys

from .. import videoin as comp

SPS = 8 * comp.FSC_NTSC          # 8fsc = 28.636363 MHz
H_US = 1e6 / 15734.264           # NTSC の1ライン = 63.5556 µs


def make_line(code_per_ire, blank_code, luma_ire=50.0, burst_ire=40.0,
              chroma_ire=0.0, phase=0.0):
    """NTSC 1ラインを 8bit コードで合成する。0〜255 でクリップする。

    ここでクリップさせるのが要点。実機の飽和(ADCの上下端に張り付く)と同じ形に
    なるので、解析側がそれを検出できるかを試験できる。
    """
    n = int(round(H_US * 1e-6 * SPS))
    out = []
    for i in range(n):
        t = i / SPS * 1e6                                  # [µs]
        if t < 4.7:
            ire = -40.0                                    # 同期チップ
        elif t < 5.3:
            ire = 0.0                                      # ブリーズウェイ
        elif t < 7.8:
            # カラーバースト。8fsc なら 1周期 = 8サンプル
            ire = burst_ire / 2.0 * math.sin(
                2 * math.pi * comp.FSC_NTSC * (t * 1e-6) + math.pi)
        elif t < 9.4:
            ire = 0.0                                      # バックポーチ
        else:
            ire = luma_ire + chroma_ire / 2.0 * math.sin(
                2 * math.pi * comp.FSC_NTSC * (t * 1e-6) + phase)
        out.append(max(0, min(255, int(round(blank_code + ire * code_per_ire)))))
    return out


def _reg(mode, key):
    """モード表からキーの値を引く(最後に書かれた値が実際に効く)。"""
    got = None
    for k, v, _why in mode["regs"]:
        if k == key:
            got = v
    return got


def check(name, cond, detail=""):
    print("%s %s%s" % ("PASS" if cond else "FAIL", name,
                       "" if cond else "  <- " + detail))
    return cond


def main():
    ok = []

    # --- 正常系: ミッドレベルクランプ + 粗ゲイン0.5倍 ---
    # 1 IRE = 3.66 コード(10bit)/ 4 = 0.915 コード(8bit)、ブランキング 128
    cpi = 0.915
    y = make_line(cpi, 128, luma_ire=50.0, chroma_ire=40.0)
    r = comp.analyze_line(y, SPS)
    ok.append(check("同期チップとブランキングを分離して読める",
                    r is not None and abs(r["blank"] - 128) <= 1
                    and abs(r["tip"] - (128 - 40 * cpi)) <= 2,
                    "tip=%s blank=%s" % (r and r["tip"], r and r["blank"])))
    tip, blank, burst_pp, act_max, got_cpi = comp._summary([r])
    ok.append(check("40 IRE から 1 IRE あたりのコード数を復元できる",
                    got_cpi is not None and abs(got_cpi - cpi) < 0.05,
                    "got %s want %s" % (got_cpi, cpi)))
    ok.append(check("バースト振幅が 40 IRE 相当で読める",
                    abs(burst_pp - 40 * cpi) <= 3,
                    "burst_pp=%s want %.1f" % (burst_pp, 40 * cpi)))
    spc, _ = comp.burst_period(y, blank, SPS)
    ok.append(check("バーストの周期が 8.00 サンプル/周期",
                    abs(spc - 8.0) < 0.3, "got %.2f" % spc))
    ok.append(check("正常系では指摘が出ない",
                    not comp.verdict(tip, blank, burst_pp, act_max, got_cpi),
                    str(comp.verdict(tip, blank, burst_pp, act_max, got_cpi))))

    # --- 失敗1: ボトムレベルクランプ。ブランキングが低くバーストの下半分が消える ---
    # 実測のボトムレベルは粗オフセット既定の +64コード(10bit) = 16コード(8bit)。
    # バーストは ±20 IRE 振れるので下半分が0でクリップする
    y2 = make_line(cpi, 16, luma_ire=50.0, chroma_ire=40.0)
    r2 = comp.analyze_line(y2, SPS)
    s2 = comp._summary([r2])
    bad2 = comp.verdict(*s2)
    ok.append(check("ボトムレベルクランプを指摘する", bool(bad2), "指摘なし"))
    ok.append(check("  バーストが潰れていることを見抜く",
                    r2["burst_pp"] < 40 * cpi,
                    "burst_pp=%s (クリップしていないと判定している)" % r2["burst_pp"]))

    # --- 失敗2: 粗ゲイン過大。白が255に張り付く ---
    # 1 IRE = 2.2 コードだと 100 IRE で 128+220 = 348 → 255でクリップ
    y3 = make_line(2.2, 128, luma_ire=100.0, chroma_ire=0.0)
    r3 = comp.analyze_line(y3, SPS)
    s3 = comp._summary([r3])
    bad3 = comp.verdict(*s3)
    ok.append(check("ゲイン過大(白の飽和)を指摘する",
                    any("飽和" in b for b in bad3), str(bad3)))
    ok.append(check("  同時に同期チップの飽和も見る",
                    r3["act_max"] >= 254, "act_max=%s" % r3["act_max"]))

    # --- 失敗3: クランプ位置が同期チップの上(既定0x32のまま)---
    # ブランキングも同期チップも同じ値になる = 段差が消える
    y4 = make_line(0.0, 128)
    s4 = comp._summary([comp.analyze_line(y4, SPS)])
    ok.append(check("クランプ位置が同期チップの上にあることを指摘する",
                    any("クランプ位置" in b for b in comp.verdict(*s4)),
                    str(comp.verdict(*s4))))

    # --- 飽和しているときに IRE 換算を出さない ---
    # 同期チップが0に張り付くと (blank-tip)/40 が小さく出て、校正がでたらめに
    # なる(実際 1 IRE = 0.40コード、バースト85 IRE、映像160 IRE と出た)。
    # 数字が出ていると信じてしまうので、信用できないときは出さないこと。
    rep_bad = comp.report([r2], [y2], SPS)
    ok.append(check("飽和時はIRE換算を出さない",
                    "IRE  " not in rep_bad and "IRE換算は出せない" in rep_bad,
                    "IRE行が出てしまっている"))
    rep_ok = comp.report([r], [y], SPS)
    ok.append(check("正常時はIRE換算を出す", "1 IRE = 0.9" in rep_ok, rep_ok))

    # --- ASCIIプロットが区間ごとに違う形になる ---
    # 「バーストの帯」が同期チップより上にあり、かつ幅を持つこと
    plot = comp.ascii_plot(y, SPS)
    ok.append(check("ラインの先頭を図にできる", len(plot) > 10, "%d行" % len(plot)))

    # --- S端子: 役割ごとに合否が逆になることを確かめる ---
    #
    # ここが role を足した理由。**同じ波形でも役割が違えば判定が逆**になる:
    # S端子のYはボトムレベル + クランプ窓を同期チップの中に置くので、同期チップが
    # 0付近に座っているのが正常。cvbs の判定をそのまま当てると「同期チップが0に
    # 張り付いている」と誤検出する。
    #
    # 波形は**実機の実測値**(ブランク58 / 1 IRE=1.450 / 白100 IRE=203)。
    y_sv = make_line(1.45, 58, luma_ire=100.0, burst_ire=0.0, chroma_ire=0.0)
    s_y = comp._summary([comp.analyze_line(y_sv, SPS)])
    ok.append(check("S端子のY: cvbs判定なら誤検出する(role を足した理由)",
                    bool(comp.verdict(*s_y, role="cvbs")),
                    "cvbsでも通ってしまうならこの試験に意味が無い"))
    ok.append(check("S端子のY: role='y' なら指摘が出ない",
                    not comp.verdict(*s_y, role="y"),
                    str(comp.verdict(*s_y, role="y"))))
    # ★輝度が数倍になった実機の壊れ方を、そのまま試験にしておく。
    #   窓をバックポーチに置いたままボトムにすると、ブランキングが底に座って
    #   その40 IRE下の同期がクリップし、**チップとブランキングの差が消える**。
    #   「チップが0か」ではなく「差が残っているか」で捕まえるのが要点。
    y_bad = make_line(1.45, 6, luma_ire=100.0, burst_ire=0.0, chroma_ire=0.0)
    s_yb = comp._summary([comp.analyze_line(y_bad, SPS)])
    ok.append(check("S端子のY: 同期がクリップして40 IRE基準が消えたら指摘する",
                    any("クランプ窓" in b for b in comp.verdict(*s_yb, role="y")),
                    str(comp.verdict(*s_yb, role="y"))))

    # C は搬送波抑圧なので、同期区間も無彩色=ブランキングと同じ値。
    # ミッドレベル128で、バーストは 0.286Vpp ≒ 88コード出る想定。
    c_sv = make_line(2.19, 128, luma_ire=0.0, burst_ire=40.0, chroma_ire=40.0)
    s_c = comp._summary([comp.analyze_line(c_sv, SPS)])
    ok.append(check("S端子のC: role='c' なら指摘が出ない",
                    not comp.verdict(*s_c, role="c"),
                    str(comp.verdict(*s_c, role="c"))))
    # Cのゲインが高すぎて飽和した場合
    c_hot = make_line(6.0, 128, luma_ire=0.0, burst_ire=40.0, chroma_ire=90.0)
    s_ch = comp._summary([comp.analyze_line(c_hot, SPS)])
    ok.append(check("S端子のC: 飽和したら赤のゲイン(0x67)を指すこと",
                    any("0x67" in b for b in comp.verdict(*s_ch, role="c")),
                    str(comp.verdict(*s_ch, role="c"))))

    # --- モード表そのものの整合 ---
    # 設定漏れ・取り違えは実機でしか出ないので、表の性質を先に固定しておく。
    ok.append(check("msx だけ SOG が _2(19hキーを足した理由)",
                    _reg(comp.MODES["msx"], comp.proto.CFG_KEY_IN_MUX1) == comp.MUX_SOG2
                    and all(_reg(comp.MODES[m], comp.proto.CFG_KEY_IN_MUX1)
                            == comp.MUX_SOG3
                            for m in ("x68k", "composite", "svideo")),
                    "19hの割り当てが想定と違う"))
    ok.append(check("x68k だけ5線同期、他はSOG",
                    _reg(comp.MODES["x68k"], comp.proto.CFG_KEY_SYNC_CTL)
                    == comp.SYNC_5WIRE
                    and all(_reg(comp.MODES[m], comp.proto.CFG_KEY_SYNC_CTL)
                            == comp.SYNC_SOG for m in ("msx", "composite", "svideo")),
                    "0Ehの割り当てが想定と違う"))
    ok.append(check("コンポジットだけ緑をミッドレベル + 粗ゲイン0.5倍",
                    _reg(comp.MODES["composite"], comp.proto.CFG_KEY_CLAMP_SEL) == 0b010
                    and _reg(comp.MODES["composite"],
                             comp.proto.CFG_KEY_COARSE_GAIN_GB) == 0x07,
                    "ミッドレベルと粗ゲインはセットで動かすこと"))
    ok.append(check("S端子は赤をミッドレベル、緑(Y)はボトム",
                    _reg(comp.MODES["svideo"], comp.proto.CFG_KEY_CLAMP_SEL) == 0b001,
                    "clamp_selが想定と違う"))
    # pll_ctl は pll_divide と対で決まる(ICP = 40×75/N)。片方だけ変えると
    # H-PLLのループ利得がずれる。表の中で対応が崩れていないか確かめる。
    for m in comp.MODES:
        n = _reg(comp.MODES[m], comp.proto.CFG_KEY_PLL_DIVIDE)
        ctl = _reg(comp.MODES[m], comp.proto.CFG_KEY_PLL_CTL)
        want_icp = max(1, min(7, round(40 * 75 / n)))
        ok.append(check("%s: pll_ctl が pll_divide=%d と整合" % (m, n),
                        ctl is not None and ((ctl >> 3) & 7) == want_icp
                        and (ctl >> 6) == 0,
                        "ctl=0x%02X → ICP=%s / 期待 ICP=%d, VCO=00"
                        % (ctl or 0, ctl and ((ctl >> 3) & 7), want_icp)))
    ok.append(check("コンポジット/S端子は pll_divide=1820(8fsc NTSC)",
                    all(_reg(comp.MODES[m], comp.proto.CFG_KEY_PLL_DIVIDE) == 1820
                        for m in ("composite", "svideo")),
                    "8fsc NTSCは1820"))
    # 方式を切り替えたとき前の設定が残らないこと。全モードが同じキー集合を
    # 書いていれば、どの順に切り替えても状態が持ち越されない。
    keysets = {m: {k for k, _v, _w in comp.MODES[m]["regs"]} for m in comp.MODES}
    shared = set.intersection(*keysets.values())
    leftover = {m: sorted(k for k in keysets[m] - shared) for m in comp.MODES}
    ok.append(check("どのモードも同じキー集合を書く(切替で設定が残らない)",
                    not any(leftover.values()),
                    "モード固有で書き残るキー: %s" % leftover))

    # --- RGB555 で観測した場合の刻みの粗さ(なぜ YC8 が要るか) ---
    # 5bit は 8コード刻み。40 IRE のバーストが何段階に落ちるかを確認する
    steps = len({(v >> 3) for v in make_line(cpi, 128, chroma_ire=40.0)[152:223]})
    ok.append(check("RGB555の5bitではバーストが数段階しか残らない(<=8段)",
                    steps <= 8, "%d段" % steps))

    print("\n%d/%d PASS" % (sum(ok), len(ok)))
    sys.exit(0 if all(ok) else 1)


if __name__ == "__main__":
    main()
