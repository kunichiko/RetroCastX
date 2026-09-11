#!/usr/bin/env python3
"""MDIO(IEEE 802.3 Clause 22)で PHY のレジスタを読み続ける。

**なぜ要るか**

MagJack の緑LED(リンク表示)を正しく出すには、本当のリンク状態が要る。
実機(v0.9.0 / Broadcom B50612D)で、簡単な方から順に潰した結果:

  1. **RGMII の in-band status** … PHYが出していない。フレームの合間の
     RXD[3:0] が常に 0(link/speed/duplex 全部0)。タイミングのずれなら
     化けた値が出るはずで、きれいに0が続くのは無効ということ
  2. **RXC が動いているか**   … ケーブルを抜いても止まらない
  3. **RXC の周波数**         … 抜いても 125MHz のまま
     (窓4096sysあたり リンク時 11377 / 抜線時 11375。差は窓の境目の揺れ)

残るのは MDIO で PHY のレジスタを直接読むことだけ。

**作り**

読み出し専用。書き込みは要らない(リンク状態を読むだけ)。

PHYアドレスとレジスタ番号は外から差し替えられるようにしてある。**基板の
立ち上げでこれが効く。** アドレスは基板の PHYA[0] ストラップで決まり、
2つのPHYがバスを共有しているので、どちらが自分のポートかは実測でしか
分からない。ホストから番地を振れば、走査も切り分けもゲートウェアを
焼き直さずにできる。

★**LiteEth も同じピンに Tristate を張っている**(`LiteEthPHYMDIO`)。
  `LiteEthPHYRGMII` は pads に mdc があると勝手に生やすので、**mdc/mdio を
  持たない覆いを渡して**生やさせないこと(`PadsWithoutMdio`)。
"""

from migen import *


class PadsWithoutMdio:
    """LiteEth に mdc/mdio を見せないための覆い。

    `LiteEthPHYRGMII` は `hasattr(pads, "mdc")` で `LiteEthPHYMDIO` を
    生やし、pads.mdc を駆動して pads.mdio に Tristate を張る。こちらで
    MDIO を持つならピンの二重駆動になるので、その属性だけ隠す。
    """
    def __init__(self, pads):
        for name in ("rst_n", "tx_ctl", "tx_data", "rx_ctl", "rx_data"):
            if hasattr(pads, name):
                setattr(self, name, getattr(pads, name))


class MdioReader(Module):
    """Clause 22 の読みフレームを1本送って16bit受け取る。

    フレームは全部で64ビット:

        0..31   プリアンブル(1を32個)
        32..33  ST = 01
        34..35  OP = 10(読み)
        36..40  PHYAD(MSB先)
        41..45  REGAD(MSB先)
        46      TA。ここで出力を離す(高インピーダンス)
        47      TA。PHYが0を出す
        48..63  DATA。PHYが出す(MSB先)

    MDIO は MDC の立ち上がりでサンプルされる規約なので、こちらは
    **立ち下がりで出し、立ち上がりで読む**。
    """
    def __init__(self, mdc, mdio_o, mdio_oe, mdio_i, sys_clk_freq, mdc_hz=1.4e6):
        self.phyad = Signal(5)
        self.regad = Signal(5)
        self.start = Signal()
        self.data  = Signal(16)
        self.busy  = Signal()

        # MDC の半周期。規格上の上限は 2.5MHz なので余裕を取る
        half = max(int(sys_clk_freq / mdc_hz / 2), 2)
        cnt = Signal(max=half)
        tick = Signal()
        self.sync += If(cnt == half - 1, cnt.eq(0)).Else(cnt.eq(cnt + 1))
        self.comb += tick.eq(cnt == half - 1)

        bitno = Signal(6)
        clk = Signal()
        self.comb += mdc.eq(clk)

        # ★**開始時にアドレスをラッチする。** アドレスが線に出るのは
        #   bit41〜45 で、フレームの終盤。呼ぶ側が送信中に番地を変えると
        #   **別の番地を読みに行く**。しかも返ってくる値はもっともらしいので、
        #   巡回読みで「値が番地と1つずれる」という分かりにくい壊れ方をする
        #   (sim_mdio.py の巡回読み試験で実際に捕まえた)。
        ph = Signal(5)
        rg = Signal(5)

        # 送出するヘッダ。**bit0 が最初に出るビット**になるよう並べる
        # (Cat は先頭が LSB なので、送る順にそのまま書ける)。
        hdr = Signal(14)
        self.comb += hdr.eq(Cat(
            0, 1,                       # ST
            1, 0,                       # OP = 読み
            ph[4], ph[3], ph[2], ph[1], ph[0],
            rg[4], rg[3], rg[2], rg[1], rg[0],
        ))
        # ★シフト量は必ず0以上にしておく。`hdr >> (bitno - 32)` と書くと、
        #   Mux で選ばれない側も評価されるため bitno<32 で負のシフトになる
        #   (Migenのシミュレータが ValueError で止まる)。
        sh = Signal(6)
        self.comb += sh.eq(Mux(bitno >= 32, bitno - 32, 0))
        self.comb += [
            # 46ビット目からは線を離す(TA と DATA は PHY が出す)
            mdio_oe.eq(self.busy & (bitno < 46)),
            mdio_o.eq(Mux(bitno < 32, 1, (hdr >> sh) & 1)),
        ]

        self.sync += [
            If(~self.busy,
                If(self.start,
                    self.busy.eq(1), bitno.eq(0), clk.eq(0), self.data.eq(0),
                    ph.eq(self.phyad), rg.eq(self.regad),
                ),
            ).Elif(tick,
                If(~clk,
                    clk.eq(1),
                    # 立ち上がり: DATA区間なら1ビット取り込む(MSB先)
                    If(bitno >= 48, self.data.eq(Cat(mdio_i, self.data[0:15]))),
                ).Else(
                    clk.eq(0),
                    If(bitno == 63, self.busy.eq(0)).Else(bitno.eq(bitno + 1)),
                ),
            ),
        ]


class MdioPoller(Module):
    """複数のレジスタを順ぐりに読み続け、それぞれの最新値を出す。

    ★**BMSR(reg 1)のリンクビットはラッチロー**で、「前回読んでから一度でも
      切れたか」を返す。読みっぱなしにしておけば、切れた次の1回だけ0を返し、
      その次から本当の状態になる。ポーリングし続けるのが正しい使い方。

    regs は Signal か定数の並び。`data[i]` が regs[i] の最新値になる。
    ホストから番地を振れるようにしたいので、Signal を混ぜられる作りにした。
    """
    def __init__(self, reader, regs, gap_cycles=2048):
        self.data = [Signal(16) for _ in regs]

        idx = Signal(max=max(len(regs), 2))
        # ★**いま出ている reader.data は「1つ前に読んだ番地」の結果。**
        #   idx のところへ仕舞うと**全部1つずれる**。ずれても値としては
        #   もっともらしいので、実機では「リンクが1周遅れで反応する」
        #   ような分かりにくい壊れ方になる。読み始めた番地を覚えておく。
        reading = Signal(max=max(len(regs), 2))
        started = Signal()
        gap = Signal(max=gap_cycles)
        due = Signal()
        self.comb += [
            due.eq(~reader.busy & (gap == gap_cycles - 1)),
            reader.start.eq(due),
            reader.regad.eq(Array(regs)[idx]),
        ]
        self.sync += [
            If(reader.busy,
                gap.eq(0),
            ).Elif(gap != gap_cycles - 1,
                gap.eq(gap + 1),
            ),
            If(due,
                If(started, Array(self.data)[reading].eq(reader.data)),
                reading.eq(idx),
                started.eq(1),
                If(idx == len(regs) - 1, idx.eq(0)).Else(idx.eq(idx + 1)),
            ),
        ]
