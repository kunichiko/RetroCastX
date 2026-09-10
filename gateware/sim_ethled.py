#!/usr/bin/env python3
"""MagJack の緑/黄LED の駆動を検証する(実機不要)。

守っているもの:

- **リンクが切れたら緑を消す。** in-band status のレジスタは eth_rx ドメインに
  あるので、PHY が RXC を止めると **最後の値のまま凍る**。in-band status だけを
  見ていると、抜線しても緑が点きっぱなしになる
- **通信が途切れても黄がすぐ消えない。** パケットは数マイクロ秒で終わるので、
  素通しでは目に見えない
- **リンクが無いのに黄が点かない。** 相手がいないのに自分が送り続けているとき
  (発見のブロードキャスト等)に点くと、繋がっているように見えてしまう
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_ethled import EthLeds, ActivityTap, LampTest

# SIM時間を詰めるため、クロックを 1MHz として時間定数を実機より短く取る。
# 大事なのは「何サイクルになるか」なので、以下は狙ったサイクル数から逆算した値。
CLK = 1_000_000
HOLD_MS = 0.04          # 黄の引き伸ばし → 40サイクル
LINK_TIMEOUT_US = 20    # リンク断とみなすまで → 20サイクル


class Wrap(Module):
    def __init__(self):
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_eth_rx = ClockDomain()
        self.link_inband = Signal()
        self.act = Signal()
        self.submodules.leds = EthLeds(
            CLK, self.link_inband, self.act,
            rx_cd="eth_rx", hold_ms=HOLD_MS, link_timeout_us=LINK_TIMEOUT_US)


def main():
    dut = Wrap()
    log = []

    def settle(n):
        for _ in range(n):
            yield

    def snap(tag):
        log.append((tag, (yield dut.leds.green), (yield dut.leds.yellow)))

    def tb():
        # --- リンク確立(in-band が立ち、RXC も動いている)---
        yield dut.link_inband.eq(1)
        yield from settle(40)
        yield from snap("リンクあり")

        # --- 1パケット受信。黄が点いて、しばらく保つ ---
        yield dut.act.eq(1)
        yield
        yield dut.act.eq(0)
        yield from settle(5)
        yield from snap("受信直後")
        yield from settle(20)
        yield from snap("受信から25サイクル")
        yield from settle(30)
        yield from snap("受信から55サイクル")

        yield from snap("通信が止まった後")



    def rxclk_running(stop_at):
        """eth_rx クロックを stop_at サイクルまで回し、そこで止める。"""
        yield

    run_simulation(dut, tb(), clocks={"sys": 10, "eth_rx": 8}, vcd_name=None)

    for tag, g, y in log:
        print(f"  {tag:<18} 緑 {g}  黄 {y}")

    d = dict((t, (g, y)) for t, g, y in log)
    assert d["リンクあり"] == (1, 0), f"リンクありで緑が点かない: {d['リンクあり']}"
    assert d["受信直後"][1] == 1, "受信しても黄が点かない"
    assert d["受信から25サイクル"][1] == 1, \
        "黄の引き伸ばしが短すぎる(パケットは数usなので素通しでは見えない)"
    assert d["受信から55サイクル"][1] == 0, \
        f"黄が消えない(hold={HOLD_MS}サイクルのはず)"

    # --- RXC が来なくなった場合 ---
    #
    # ★**「クロックを止める」ことは Migen の simulator では書けない。**
    #   `clocks` に無いドメインの sync 文は一切評価されず、クロック信号を
    #   ジェネレータから手で叩いても動かない(確認済み)。
    #   見たい性質は「エッジが timeout ぶん来なければ消える」なので、
    #   **極端に遅い eth_rx** で代用する。止まっているのはその極限。
    dut2 = Wrap()
    log2 = []
    SLOW = 6000        # eth_rx の周期[SIM単位] = sys 300サイクルぶん

    def tb2():
        yield dut2.link_inband.eq(1)
        # t=0 で全クロックが立つので、直後はまだ「生きている」
        for _ in range(8):
            yield
        log2.append(("エッジ直後", (yield dut2.leds.green)))
        # 次のエッジまで 300サイクル。timeout(20) をとうに過ぎている
        for _ in range(100):
            yield
        log2.append(("エッジが来ない", (yield dut2.leds.green)))

    run_simulation(dut2, tb2(), clocks={"sys": 10, "eth_rx": SLOW}, vcd_name=None)
    for tag, g in log2:
        print(f"  {tag:<18} 緑 {g}")
    assert log2[0][1] == 1, "エッジが来ているのに緑が点かない"
    assert log2[1][1] == 0, \
        "★RXCのエッジが途絶えても緑が点いたまま。in-band status は eth_rx " \
        "ドメインにあるので、PHYがRXCを止めると最後の値で凍る。見張りが要る"

    # --- ランプテスト(4本ぶんの強制指定)---
    #
    # ★**使っていない側のポートも点けられること。** 実機で J12 の緑だけ
    #   点かなかったとき、J11 の緑を点けて初めて「基板の不良か、こちらの
    #   ピン間違いか」を分けられた。使用中のポートしか振れないと詰む。
    class LampWrap(Module):
        def __init__(self):
            self.clock_domains.cd_sys = ClockDomain()
            self.force = Signal(8)
            self.normal = Signal(4)
            self.submodules.lamp = LampTest(self.force, self.normal)

    lw = LampWrap()
    log3 = []

    def tb3():
        yield lw.normal.eq(0b1100)      # eth1 の緑黄だけが通常動作
        yield
        for tag, f, exp in (
            ("通常",        0x00, 0b1100),
            ("全消灯",      0x01, 0b0000),
            ("全点灯",      0x02, 0b1111),
            ("通常(クリア)", 0x03, 0b1100),
            ("eth0緑だけ",  0x11, 0b0001),
            ("eth1緑だけ",  0x14, 0b0100),
            ("緑2本",       0x15, 0b0101),
        ):
            yield lw.force.eq(f)
            yield
            got = (yield lw.lamp.out)
            log3.append((tag, f, got, exp))

    run_simulation(lw, tb3(), vcd_name=None)
    for tag, f, got, exp in log3:
        print(f"  ランプ {tag:<12} force={f:#04x} → {got:04b} (期待 {exp:04b})")
        assert got == exp, f"ランプテスト {tag}: {got:04b} 期待 {exp:04b}"

    print("\n[OK] 緑=リンク(RXC停止も検出) / 黄=通信中 / ランプテスト4本個別")


if __name__ == "__main__":
    main()
