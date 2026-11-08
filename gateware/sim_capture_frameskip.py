#!/usr/bin/env python3
"""フレーム間引き(CONFIG 0x35)の対モードを検証(実機不要)。

インターレースでフィールドごとに VSYNC が来る信号(480i など)は、フィールドを
1枚ずつ数えて間引くと、1/2 で**片方の極性のフィールドしか届かない**。もう片方は
受信側に古いまま残り、織り込みが崩れる。対モード(bit4)では2フィールドを1単位に
して、連続する2枚を送ってから N 単位飛ばす。

確認すること(フィールドを20枚流し、送られたラインのフレーム番号を集める):
  - 間引き 0                      → 全フィールドが届く
  - インターレース・1/2・対なし    → 1枚おき(従来どおり)
  - インターレース・1/2・対あり    → 連続する2枚を送り、次の2枚を飛ばす
  - インターレース・1/3・対あり    → 2枚送って4枚飛ばす
  - プログレッシブ・1/2・対あり    → 対モードは無視して1枚おき
  - 1回のVSYNCに2フィールド入る信号(il_frame)・1/2・対あり → 1枚おき

実行: .venv/bin/python sim_capture_frameskip.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *                      # noqa: E402
from migen.sim import run_simulation     # noqa: E402
from retrocastx_capture import TvpCapture  # noqa: E402

W, NL, NF = 8, 4, 8
H = 2 * NL
VOFF = 2
NFIELDS = 20


class _Pads:
    def __init__(self):
        self.r = Signal(8); self.g = Signal(8); self.b = Signal(8)
        self.hs = Signal(reset=1); self.vs = Signal(reset=1)


class Wrap(Module):
    def __init__(self):
        self.pads = _Pads()
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.submodules.cap = TvpCapture(
            self.pads, width=W, height=H, nface=NF,
            hs_active_low=True, vs_active_low=True,
            hs_offset=1, vs_offset=VOFF)


def run(skip, pair, interlace, il_frame=False):
    """送られたラインのフレーム番号(フィールドごとに1つ)を昇順で返す"""
    dut = Wrap()
    p, cap = dut.pads, dut.cap
    frames = set()
    done = [False]

    def drive():
        # 対モードの判定に使う2つをSIMで与える:
        #   il_det   ← cfg_il_detect(TVPのP/I検出)
        #   il_frame ← 生同期が無いSIMでは cfg_lpf_tvp との比較で決まる
        #              (cfg_vtotal*1.5 > LPF ならフレーム単位)。vtotal=0 の
        #              ままなら LPF=1 でフィールド単位、LPF=0 ならフレーム単位
        yield cap.cfg_frame_skip.eq(skip | (0x10 if pair else 0))
        yield cap.cfg_il_detect.eq(1 if interlace else 0)
        yield cap.cfg_lpf_tvp.eq(0 if il_frame else 1)
        for _ in range(8):
            yield
        for f in range(NFIELDS):
            yield p.vs.eq(0); yield
            yield p.vs.eq(1)
            for _ in range(4):
                yield
            first = VOFF - 1
            for L in range(first + NL + 1):
                yield p.hs.eq(0); yield
                yield p.hs.eq(1); yield
                yield
                if 0 <= L - first < NL:
                    for xp in range(W):
                        yield p.r.eq((xp << 3) & 0xFF)
                        yield p.g.eq(0x40)
                        yield
                    yield p.r.eq(0); yield p.g.eq(0)
                    for _ in range(3):
                        yield
                else:
                    for _ in range(W + 4):
                        yield
        for _ in range(40):
            yield
        done[0] = True

    def consume():
        while not done[0]:
            if (yield cap.line_valid):
                frames.add((yield cap.line_frame))
                yield cap.line_ack.eq(1); yield
                yield cap.line_ack.eq(0)
            yield

    run_simulation(dut, {"pix": drive(), "sys": consume()},
                   clocks={"sys": 10, "pix": 10}, vcd_name=None)
    return sorted(frames)


def check(name, got, want):
    ok = got == want
    print(f"[{'OK' if ok else 'NG'}] {name}: {got}")
    assert ok, f"{name}: 期待 {want}"


def main():
    allf = list(range(1, NFIELDS + 1))
    # 間引きの判定は frame_start で「数え始め」から行う。skip_cnt は 0 から
    # 始まるので、最初の単位は飛ばされる(1/2 なら 2,4,6… が届く)
    check("間引き0", run(0, 0, True), allf)
    check("インターレース 1/2 対なし", run(1, 0, True),
          [f for f in allf if f % 2 == 0])
    # 対は (1,2) (3,4) … 。最初の対を飛ばし、次の対を送る
    check("インターレース 1/2 対あり", run(1, 1, True),
          [f for f in allf if (f - 1) // 2 % 2 == 1])
    check("インターレース 1/3 対あり", run(2, 1, True),
          [f for f in allf if (f - 1) // 2 % 3 == 2])
    check("プログレッシブ 1/2 対あり(対は無視)", run(1, 1, False),
          [f for f in allf if f % 2 == 0])
    check("2フィールド入りのVSYNC 1/2 対あり(対は無視)", run(1, 1, True, il_frame=True),
          [f for f in allf if f % 2 == 0])

    # 対モードで届くフィールドは必ず連続する2枚の組になっている
    got = run(1, 1, True)
    pairs = [got[i:i + 2] for i in range(0, len(got), 2)]
    assert all(len(pr) == 2 and pr[1] == pr[0] + 1 for pr in pairs), pairs
    print(f"\n[OK] 対モード: 連続する2フィールドの組で届く {pairs}")


if __name__ == "__main__":
    main()
