#!/usr/bin/env python3
"""MDIO の読みフレームが規格どおりか、偽PHYを相手に確かめる(実機不要)。

**なぜ模擬でやるか。** MDIO は片方向ずつ線を持ち合うので、ビットの並びや
TA(線を渡す2ビット)を間違えると、実機では「いつも 0xFFFF」や「いつも 0」と
いう**もっともらしい値**が返る。それを見て「PHYが応答しない」「アドレスが
違う」と誤診するのが一番ありがちな外し方なので、先に模擬で潰しておく。

偽PHYは、受け取ったフレームを規格どおりに読み解いて、
指定された PHYAD/REGAD が自分のものだったときだけ値を返す。
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_mdio import MdioReader, MdioPoller

CLK = 1_000_000       # SIM時間を詰めるため 1MHz とする
# ★**半周期を詰めすぎないこと。** 偽PHYはジェネレータなのでMDCの縁を
#   1サイクル遅れて認識し、`yield sig.eq()` の反映にもう1サイクルかかる。
#   半周期2サイクルだと、PHYが置いた値が立ち上がりに間に合わず、読めた値が
#   まるごと 0x0000 になった(実機の配線ミスと同じ見え方なので紛らわしい)。
MDC_HZ = 62_500       # → 半周期8サイクル
PHYAD = 0b00101
REGAD = 0b00001       # BMSR
HALF = max(int(CLK / MDC_HZ / 2), 2)
VALUE = 0x796D        # 適当だが 0/0xFFFF ではない値(取り違えに気付けるように)


class Wrap(Module):
    def __init__(self):
        self.clock_domains.cd_sys = ClockDomain()
        self.mdc = Signal()
        self.o = Signal(); self.oe = Signal(); self.i = Signal()
        self.submodules.rd = MdioReader(
            self.mdc, self.o, self.oe, self.i, CLK, mdc_hz=MDC_HZ)


def main():
    dut = Wrap()
    seen = {}

    def fake_phy():
        """MDCの立ち上がりを数えながら、規格どおりにフレームを解く。"""
        bits = []
        prev = 0
        drive = None          # PHYが出す番になったら (残りビット列)
        # ★**無限ループにしないこと。** Migen の run_simulation は全ての
        #   ジェネレータが終わるまで回り続けるので、`while True` を書くと
        #   シミュレーションが終わらない(実際に固まった)。
        #   64ビット × 半周期2サイクル × 2 + 余裕。
        for _ in range(64 * 2 * HALF + 400):
            clk = (yield dut.mdc)
            if clk and not prev:
                # 立ち上がり: マスタが出しているビットを取り込む
                if drive is None:
                    bits.append((yield dut.o) if (yield dut.oe) else None)
                    if len(bits) == 46:
                        # ここまでで解ける。プリアンブル32 + ST + OP + AD
                        pre = bits[0:32]
                        st  = bits[32:34]
                        op  = bits[34:36]
                        pa  = bits[36:41]
                        ra  = bits[41:46]
                        seen["preamble_ok"] = all(b == 1 for b in pre)
                        seen["st"] = (st[0] << 1) | st[1]
                        seen["op"] = (op[0] << 1) | op[1]
                        seen["phyad"] = int("".join(str(b) for b in pa), 2)
                        seen["regad"] = int("".join(str(b) for b in ra), 2)
                        if seen["phyad"] == PHYAD and seen["regad"] == REGAD:
                            # ★TAは**2ビット**。bit46 はマスタが線を離すだけで
                            #   誰も駆動せず、bit47 で PHY が 0 を出す。データは
                            #   bit48 から。ここを1ビット詰めると読めた値が
                            #   まるごとずれる(最初にこれで 0x0000 が出た)。
                            drive = [0, 0]          # bit46(誰も駆動しない), bit47(PHYのTA)
                            drive += [(VALUE >> (15 - k)) & 1 for k in range(16)]
                        else:
                            drive = [1] * 18      # 応答しないPHYは線が浮く
            if not clk and prev:
                # 立ち下がり: PHYが出す番なら次のビットを置く
                if drive is not None and drive:
                    yield dut.i.eq(drive.pop(0))
            prev = clk
            yield

    def tb():
        yield dut.rd.phyad.eq(PHYAD)
        yield dut.rd.regad.eq(REGAD)
        yield
        yield dut.rd.start.eq(1)
        yield
        yield dut.rd.start.eq(0)
        # ★**まず busy が立つのを待つ。** いきなり「busy が0なら終わり」と
        #   見ると、start がまだ反映されていない最初の1回で抜けてしまい、
        #   読めた値が 0x0000 になる(配線ミスと同じ見え方で紛らわしい)。
        for _ in range(50):
            if (yield dut.rd.busy):
                break
            yield
        assert (yield dut.rd.busy), "start を出してもフレームが始まらない"
        for _ in range(64 * 2 * HALF + 200):
            if not (yield dut.rd.busy):
                break
            yield
        seen["data"] = (yield dut.rd.data)
        seen["busy_end"] = (yield dut.rd.busy)

    run_simulation(dut, [tb(), fake_phy()], vcd_name=None)

    print(f"  プリアンブル32個  {seen.get('preamble_ok')}")
    print(f"  ST                {seen.get('st'):#04b} (期待 0b01)")
    print(f"  OP                {seen.get('op'):#04b} (期待 0b10 = 読み)")
    print(f"  PHYAD             {seen.get('phyad')} (期待 {PHYAD})")
    print(f"  REGAD             {seen.get('regad')} (期待 {REGAD})")
    print(f"  読めた値          {seen.get('data'):#06x} (期待 {VALUE:#06x})")

    assert seen["preamble_ok"], "プリアンブルが32ビット揃っていない"
    assert seen["st"] == 0b01, f"ST が違う: {seen['st']:#04b}"
    assert seen["op"] == 0b10, f"OP が違う: {seen['op']:#04b}"
    assert seen["phyad"] == PHYAD, f"PHYAD が違う: {seen['phyad']}"
    assert seen["regad"] == REGAD, f"REGAD が違う: {seen['regad']}"
    assert seen["busy_end"] == 0, "フレームが終わっていない"
    assert seen["data"] == VALUE, \
        f"読めた値が違う: {seen['data']:#06x} 期待 {VALUE:#06x}"
    assert seen["data"] not in (0x0000, 0xFFFF), \
        "0/0xFFFF は配線ミスでも出る値。試験値としては使えない"

    poller_check()
    print("\n[OK] Clause 22 の読みフレームが規格どおり / 巡回読みが番地とずれない")


REGS = (1, 0x19)
REG_VALUES = {1: 0x796D, 0x19: 0x871C}


class PollWrap(Module):
    def __init__(self):
        self.clock_domains.cd_sys = ClockDomain()
        self.mdc = Signal()
        self.o = Signal(); self.oe = Signal(); self.i = Signal()
        self.submodules.rd = MdioReader(
            self.mdc, self.o, self.oe, self.i, CLK, mdc_hz=MDC_HZ)
        self.submodules.poll = MdioPoller(self.rd, list(REGS), gap_cycles=8)


def poller_check():
    """★**巡回読みで値が番地とずれないこと。**

    読み終えた値を「いま指している番地」に仕舞うと全部1つずれる。ずれても
    値としてはもっともらしいので、実機では「リンクが1周遅れで反応する」
    という分かりにくい壊れ方になる。ここで押さえる。
    """
    dut = PollWrap()
    got = {}

    def fake_phy():
        prev = 0
        bits = []
        drive = None
        for _ in range(64 * 2 * HALF * 8 + 2000):
            clk = (yield dut.mdc)
            if clk and not prev:
                if drive is None:
                    bits.append((yield dut.o) if (yield dut.oe) else None)
                    if len(bits) == 46:
                        ra = int("".join(str(b) for b in bits[41:46]), 2)
                        val = REG_VALUES.get(ra, 0xFFFF)
                        drive = [0, 0] + [(val >> (15 - k)) & 1 for k in range(16)]
            if not clk and prev:
                if drive is not None:
                    if drive:
                        yield dut.i.eq(drive.pop(0))
                    else:
                        drive = None
                        bits = []
            prev = clk
            yield

    def tb():
        # 4周ぶん回して落ち着かせる
        for _ in range(64 * 2 * HALF * 6 + 500):
            yield
        for n, r in enumerate(REGS):
            got[r] = (yield dut.poll.data[n])

    run_simulation(dut, [tb(), fake_phy()], vcd_name=None)
    for r in REGS:
        print(f"  巡回読み reg {r:#04x} → {got[r]:#06x} (期待 {REG_VALUES[r]:#06x})")
        assert got[r] == REG_VALUES[r], \
            f"reg {r:#04x} の値が {got[r]:#06x}。番地と結果がずれている"


if __name__ == "__main__":
    main()
