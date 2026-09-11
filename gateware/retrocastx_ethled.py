#!/usr/bin/env python3
"""MagJack の緑/黄LEDを駆動する(RetroCastX v0.9.0 基板)。

v0.9.0 では RJ45 の LED を **FPGA の GPIO から駆動する**。
(以前はi5モジュール底面のPHY LEDパッドをポゴピンで拾う案もあったが、
 基板面積と配線の都合で GPIO 駆動に一本化した。hardware/adc-frontend/main.ato)

    カソードは GND、アノードへ 220Ω。**アクティブHigh。**

    ETH2 = J11 = litex eth index 0 : 緑 R1 / 黄 T1
    ETH1 = J12 = litex eth index 1 : 緑 U1 / 黄 Y2

    ★index とコネクタ表記は入れ替わっている(retrocastx_stream.py の
      LiteEthPHYRGMII 生成部のコメント参照)。

割り当ては普通のNICに合わせる:

    緑 = リンク確立    点灯しっぱなし
    黄 = 通信中        パケットが流れている間だけ。短すぎて見えないので引き伸ばす

リンクの判定
------------
**RGMII の in-band status を使う。** MDIO を叩く必要はない。RGMII では
フレームの合間(RX_CTL=0)に RXD がリンク/速度/全二重を運んでおり、
LiteEth の `LiteEthPHYRGMIIRX` が既に復号して CSR に載せている
(`with_inband_status=True` が既定)。

★**それだけでは足りない。** リンクが切れると PHY は RXC を止めることがあり、
  そうなると in-band status のレジスタは **最後の値のまま凍る**。
  リンクありのまま抜線すると緑が点きっぱなしになる。
  そこで「eth_rx クロックが動いているか」の見張りと **AND** を取る。
  クロックが止まればリンク断、というのは PHY の実装に依らず正しい。
"""

from migen import *
from migen.genlib.cdc import MultiReg


class _Toggler(Module):
    """自分のドメインでビットを反転させ続ける(en を渡せばそのときだけ)。

    ドメイン名を変数で受けたいので、`self.sync.<名前>` と書く代わりに
    このモジュールを `ClockDomainsRenamer` で包む。
    """
    def __init__(self, en=None):
        self.tog = Signal()
        if en is None:
            self.sync += self.tog.eq(~self.tog)
        else:
            self.sync += If(en, self.tog.eq(~self.tog))


class ClockAlive(Module):
    """別ドメインのクロックが動いているかを sys から見る。

    向こう側でトグルさせたビットを2段FFで受け、`timeout` サイクルのあいだ
    変化が無ければ「止まっている」とみなす。周波数は問わないので、
    1000/100/10BASE-T で RXC が 125/25/2.5MHz と変わっても効く。
    """
    def __init__(self, cd_name, timeout):
        self.alive = Signal()
        self.tgl_sync = Signal()   # 向こう側のトグルを sys で受けたもの(診断用)

        self.submodules.tgl = tgl = ClockDomainsRenamer(cd_name)(_Toggler())
        tog_s = self.tgl_sync
        self.specials += MultiReg(tgl.tog, tog_s, "sys")
        tog_p = Signal()
        cnt = Signal(max=timeout + 1)
        self.sync += [
            tog_p.eq(tog_s),
            If(tog_s != tog_p,
                cnt.eq(0),
            ).Elif(cnt != timeout,
                cnt.eq(cnt + 1),
            ),
        ]
        self.comb += self.alive.eq(cnt != timeout)


class Stretch(Module):
    """短いパルスを目に見える長さへ引き伸ばす。

    入力が来ているあいだは点きっぱなしにしたいので、パルスのたびに
    タイマを張り直す(単発の one-shot にすると連続通信で消えてしまう)。
    """
    def __init__(self, hold):
        self.i = Signal()
        self.o = Signal()

        cnt = Signal(max=hold + 1)
        self.sync += If(self.i,
            cnt.eq(hold),
        ).Elif(cnt != 0,
            cnt.eq(cnt - 1),
        )
        self.comb += self.o.eq(cnt != 0)


class EthLeds(Module):
    """緑(リンク)/黄(通信)を作る。すべて sys ドメイン。

    link_inband : in-band status のリンクビット(sys へ同期済みのもの)
    act         : 送受信いずれかが動いた印(sysドメインのパルスまたはレベル)
    speed_ok : リンク速度が期待どおりか。**0 だと緑をゆっくり点滅させる。**
               100BASE-T で繋がると帯域が足りずパケットを落とすが、
               「リンクはある」ので原因に辿り着きにくい。目で分かるようにする。
               省略すると常に点灯。

    ランプテスト(強制点灯)は `LampTest` の方でまとめて掛ける。
    """
    def __init__(self, sys_clk_freq, link_inband, act,
                 rx_cd="eth_rx", hold_ms=40, link_timeout_us=200,
                 speed_ok=None, blink_ms=250):
        self.green = Signal()
        self.yellow = Signal()
        self.link = Signal()         # 判定結果(診断用に外へ出す)
        self.rxclk_alive = Signal()  # RXCが動いているか(診断用)

        # RXC が止まっていないか。1000BASE-T なら 125MHz なので 200us もあれば
        # 十分すぎるが、10BASE-T の 2.5MHz でもトグルは 400ns 周期なので通る。
        timeout = max(int(sys_clk_freq * link_timeout_us / 1e6), 4)
        self.submodules.rxclk = rxclk = ClockAlive(rx_cd, timeout)

        hold = max(int(sys_clk_freq * hold_ms / 1000), 2)
        self.submodules.blip = blip = Stretch(hold)
        self.comb += blip.i.eq(act)

        # 速度が期待どおりでないときの点滅
        blink = Signal(reset=1)
        if speed_ok is None:
            speed_ok = 1
        else:
            period = max(int(sys_clk_freq * blink_ms / 1000), 2)
            bc = Signal(max=period)
            self.sync += If(bc == period - 1,
                bc.eq(0), blink.eq(~blink),
            ).Else(
                bc.eq(bc + 1),
            )

        link = self.link
        g = Signal(); y = Signal()
        self.comb += [
            self.rxclk_alive.eq(rxclk.alive),
            link.eq(link_inband & rxclk.alive),
            g.eq(link & (speed_ok | blink)),
            # ★**リンクが無いのに黄が点くのはおかしい。** 相手がいないのに
            #   自分が送り続けている(発見のブロードキャスト等)ときに点くと、
            #   「繋がっているのに通信できない」ように見えてしまう。
            y.eq(link & blip.o),
        ]
        self.comb += [self.green.eq(g), self.yellow.eq(y)]


class ActivityTap(Module):
    """別ドメインのストリームが動いた印を sys へ渡す。

    valid が立つたびに向こう側でトグルさせ、sys 側で変化を見る。
    **eth_rx / eth_tx に足すのは FF 1個だけ**にしてある。この2つのドメインは
    LiteEth の CDC FIFO が配線律速でクリティカルパスになっており、論理を
    足すと配置の当たり外れが変わる(retrocastx_stream.py の SEEDS のコメント)。
    """
    def __init__(self, cd_name, valid):
        self.pulse = Signal()      # sysドメイン。1サイクルのパルス

        self.submodules.tgl = tgl = ClockDomainsRenamer(cd_name)(_Toggler(valid))
        tog_s = Signal()
        self.specials += MultiReg(tgl.tog, tog_s, "sys")
        tog_p = Signal()
        self.sync += tog_p.eq(tog_s)
        self.comb += self.pulse.eq(tog_s != tog_p)


class LampTest(Module):
    """4本のLED(両ポートの緑/黄)に強制指定を掛ける。

    ★**立ち上げでこれが無いと詰む。** 光らないときに、原因がピン割り当て
      なのか論理が0なのかを分けられない。実際に両方消えて切り分けに
      1ビルド余計にかかり、さらに「片方だけ光る」段になって、
      **使っていない側のポートも点けられないと基板の不良と切り分けられない**
      ことが分かった。だから最初から4本ぜんぶ個別に振れるようにしておく。

    force の意味:

        0            通常(リンク/通信に従う。未使用ポートは消灯)
        1            全消灯
        2            全点灯(両ポート4本)
        3            通常 + 診断のスティッキーをクリア
        0x10 | mask  個別。mask の bit で1本ずつ指定する

    ビットの並びは `normal` / `out` と同じで、下から

        bit0 = eth0 緑 / bit1 = eth0 黄 / bit2 = eth1 緑 / bit3 = eth1 黄

    (eth0 = コネクタ ETH2 = J11、eth1 = コネクタ ETH1 = J12)
    """
    def __init__(self, force, normal):
        self.out = Signal(4)
        self.comb += If(force == 1,
            self.out.eq(0),
        ).Elif(force == 2,
            self.out.eq(0b1111),
        ).Elif(force[4],
            self.out.eq(force[0:4]),
        ).Else(
            self.out.eq(normal),
        )
