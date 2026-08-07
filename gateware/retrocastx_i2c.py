#!/usr/bin/env python3
"""共有I2Cバイトマスタ + ステータス表示シーケンサ(CPU不介在)。

I2CByteMaster: START/WRITE/READ/STOP を op 単位で実行するビットバングマスタ。
  open-drain(scl_low/sda_low=1でLow駆動, 0で解放=プルアップHigh)。sda_in=線の実値。
  op: 0=START, 1=WRITE(wdata送出→ackr), 2=READ(rd_nack指定→rdata), 3=STOP。
  go=1パルスで開始、done=1パルスで完了。

StatusDisplay: 1本のI2Cバスを時分割し、
  - TVP7002(0x5C): RESETB解除 → 初期化数レジスタ書込 → レジスタ読出(ACK/値取得)
  - SSD1306 OLED(0x3C): 初期化 → テキストRAMを常時再描画(ライブ値をhex整形)
"""
from migen import *

try:
    from .oled_font import FONT8
except ImportError:
    import sys, os
    sys.path.insert(0, os.path.dirname(__file__))
    from oled_font import FONT8

OP_START, OP_WRITE, OP_READ, OP_STOP = 0, 1, 2, 3


class I2CByteMaster(Module):
    def __init__(self, sys_clk_freq=45e6, i2c_freq=400e3):
        self.op       = Signal(2)
        self.wdata    = Signal(8)
        self.rd_nack  = Signal()      # READ後に master が返すACKビット(1=NACK)
        self.go       = Signal()
        self.done     = Signal()
        self.rdata    = Signal(8)
        self.ackr     = Signal()      # WRITE後の slave ACK (0=ACK)
        self.busy     = Signal()
        # open-drain 線
        self.scl_low  = Signal()
        self.sda_low  = Signal()
        self.sda_in   = Signal(reset=1)

        Q = max(2, int(round(sys_clk_freq / (4 * i2c_freq))))
        div   = Signal(max=Q)
        run   = Signal()
        en    = Signal()
        phase = Signal(2)
        self.sync += [
            If(run,
                If(div == Q - 1, div.eq(0)).Else(div.eq(div + 1)),
            ).Else(div.eq(0)),
        ]
        self.comb += en.eq(run & (div == Q - 1))
        self.sync += If(en, phase.eq(phase + 1))
        cell = Signal(); self.comb += cell.eq(en & (phase == 3))

        sh    = Signal(8)
        bit   = Signal(4)   # 0..8
        opr   = Signal(2)
        bus_free = Signal(reset=1)   # STOP後=1(解放), トランザクション中=0(IDLEでSCL Low保持)

        self.submodules.fsm = fsm = FSM(reset_state="IDLE")
        fsm.act("IDLE",
            # トランザクション途中(bus_free=0)はSCLをLow保持し疑似START/STOPを防ぐ
            self.scl_low.eq(~bus_free), self.sda_low.eq(0),
            If(self.go,
                NextValue(opr, self.op),
                NextValue(sh, self.wdata),
                NextValue(bit, 0),
                NextValue(run, 1),
                NextValue(phase, 0),
                Case(self.op, {
                    OP_START: NextState("START"),
                    OP_WRITE: NextState("BYTE"),
                    OP_READ:  NextState("BYTE"),
                    OP_STOP:  NextState("STOP"),
                }),
            ).Else(NextValue(run, 0)),
        )
        # START: SCL high(0,1)->low(2,3), SDA high(0)->low(1..)
        fsm.act("START",
            self.scl_low.eq(phase >= 2),
            self.sda_low.eq(phase >= 1),
            If(cell, NextValue(run, 0), NextValue(bus_free, 0),
               self.done.eq(1), NextState("IDLE")),
        )
        # STOP: SCL low(0)->high(1..), SDA low(0,1)->high(2,3)
        fsm.act("STOP",
            self.scl_low.eq(phase == 0),
            self.sda_low.eq(phase <= 1),
            If(cell, NextValue(run, 0), NextValue(bus_free, 1),
               self.done.eq(1), NextState("IDLE")),
        )
        # BYTE: 8 data + 1 ack。SCL: high in phase 1,2。
        is_wr = (opr == OP_WRITE)
        fsm.act("BYTE",
            self.scl_low.eq((phase == 0) | (phase == 3)),
            # SDA駆動
            If(bit < 8,
                If(is_wr,
                    self.sda_low.eq(~sh[7]),      # WRITE: MSB送出
                ).Else(
                    self.sda_low.eq(0),           # READ: 解放して受信
                ),
            ).Else(
                # ACKビット
                If(is_wr,
                    self.sda_low.eq(0),           # WRITE: 解放(slaveがACK)
                ).Else(
                    self.sda_low.eq(~self.rd_nack),  # READ: masterがACK(0)/NACK(1)
                ),
            ),
            # サンプル(SCL立上り = phase==1)
            If(en & (phase == 1),
                If(bit < 8,
                    If(~is_wr, NextValue(self.rdata, Cat(self.sda_in, self.rdata[0:7]))),
                ).Else(
                    If(is_wr, NextValue(self.ackr, self.sda_in)),
                ),
            ),
            # ビット送り
            If(cell,
                NextValue(sh, Cat(Signal(1), sh[0:7])),   # <<1
                If(bit == 8,
                    NextValue(run, 0), self.done.eq(1), NextState("IDLE"),
                ).Else(
                    NextValue(bit, bit + 1),
                ),
            ),
        )
        self.comb += self.busy.eq(~fsm.ongoing("IDLE"))


# ---- SSD1306 128x64 初期化列 + 表示範囲 ----
OLED_INIT = [
    0xAE, 0xD5,0x80, 0xA8,0x3F, 0xD3,0x00, 0x40, 0x8D,0x14, 0x20,0x00,
    0xA1, 0xC8, 0xDA,0x12, 0x81,0xCF, 0xD9,0xF1, 0xDB,0x40, 0xA4, 0xA6,
    0x2E, 0xAF, 0x21,0x00,0x7F, 0x22,0x00,0x07,
]
COLS, ROWS = 16, 8


def _banner():
    lines = [
        "RetroCastX  i5",
        "----------------",
        "TVP : ---",       # ACK/NAK @ col6..8
        "REV : 0x--",      # reg0x00 hex @ col8,9 (正常02)
        "WRB : 0x--",      # reg0x01 書戻し hex @ col8,9 (正常A5)
        "UP  : 0x--------", # hex @ col8..15
        "",
        "i2c 0x5C / 0x3C",
    ]
    buf = []
    for r in range(ROWS):
        s = (lines[r] if r < len(lines) else "")
        s = (s + " " * COLS)[:COLS]
        buf += [ord(c) & 0x7F for c in s]
    return buf


class StatusDisplay(Module):
    """共有I2C(SDA/SCL) + RESETB を使い、TVP7002の応答/レジスタを読み OLEDに表示。"""
    def __init__(self, pads=None, sys_clk_freq=45e6, i2c_freq=400e3,
                 tvp_addr=0x5C, oled_addr=0x3C):
        self.submodules.m = m = I2CByteMaster(sys_clk_freq, i2c_freq)
        # open-drain 線(sim用に公開)
        self.scl_low = m.scl_low
        self.sda_low = m.sda_low
        self.resetb  = Signal(reset=0)      # TVP RESETB (0=reset, 1=解除)
        # 観測用
        self.tvp_ack = Signal()
        self.rev = Signal(8)     # reg0x00 Chip Revision (既定0x02)
        self.wrb = Signal(8)     # reg0x01 に0xA5書込→読戻し (正常0xA5)

        TVP_W = (tvp_addr << 1) & 0xFE       # 0xB8
        TVP_R = TVP_W | 1                    # 0xB9
        OLED_W = (oled_addr << 1) & 0xFE     # 0x78

        # ROM/RAM
        self.specials.font = Memory(8, 1024, init=FONT8)
        frd = self.font.get_port(); self.specials += frd
        self.specials.text = Memory(8, COLS*ROWS, init=_banner())
        trd = self.text.get_port(); twr = self.text.get_port(write_capable=True)
        self.specials += trd, twr

        init_rom = Array(Constant(v, 8) for v in OLED_INIT)
        NINIT = len(OLED_INIT)

        # uptime 秒カウンタ(hex表示用)
        subsec = Signal(max=int(sys_clk_freq)); sec = Signal(32)
        self.sync += [
            If(subsec == int(sys_clk_freq)-1, subsec.eq(0), sec.eq(sec+1)
              ).Else(subsec.eq(subsec+1)),
        ]

        # --- I2C op 発行ヘルパ(pumpパターン) ---
        sent = Signal()
        def issue(op, data=0, nack=0):
            return [m.op.eq(op), m.wdata.eq(data), m.rd_nack.eq(nack),
                    If(~sent & ~m.busy, m.go.eq(1))]
        # sentは done で解除、go後にセット
        self.sync += [
            If(m.go, sent.eq(1)),
            If(m.done, sent.eq(0)),
        ]

        # フレーム用インデックス
        idx = Signal(11)
        # OLED data 座標
        k = Signal(11); page = Signal(3); xcol = Signal(7)
        self.comb += [k.eq(idx-2), page.eq(k[7:10]), xcol.eq(k[0:7])]
        self.comb += [trd.adr.eq((page<<4) + xcol[3:7]),
                      frd.adr.eq((trd.dat_r<<3) + xcol[0:3])]
        prefetch = Signal(2)
        databyte = Signal(8)

        # 起動後リセット解除タイマ
        rst_cnt = Signal(max=int(sys_clk_freq//10)+1)

        # TVP read: 2レジスタ(0x01,0x02)
        # TVP手順ステップ: 0=read0x00(rev), 1=write0x01<-0xA5, 2=read0x01(wrb)
        step = Signal(2)
        WRVAL = 0xA5
        reg_b = Signal(8); self.comb += reg_b.eq(Mux(step == 0, 0x00, 0x01))
        is_wstep = Signal(); self.comb += is_wstep.eq(step == 1)

        # FORMAT
        fi = Signal(4)
        def hexch(nib):
            return Mux(nib < 10, ord('0') + nib, ord('A') - 10 + nib)

        self.submodules.fsm = fsm = FSM(reset_state="POR0")

        # 1) RESET: resetb=0 保持 -> 1 解除 -> 少し待つ
        fsm.act("POR0",
            self.resetb.eq(0),
            NextValue(rst_cnt, rst_cnt+1),
            If(rst_cnt == int(sys_clk_freq//100), NextValue(rst_cnt,0), NextState("POR1")),
        )
        fsm.act("POR1",
            self.resetb.eq(1),
            NextValue(rst_cnt, rst_cnt+1),
            If(rst_cnt == int(sys_clk_freq//100), NextState("OI_START")),
        )

        # 2) OLED 初期化フレーム: START,0x78,0x00,<INIT>,STOP
        fsm.act("OI_START", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextValue(idx,0), NextState("OI_BODY")))
        fsm.act("OI_BODY", self.resetb.eq(1),
            *issue(OP_WRITE, Mux(idx==0, OLED_W, Mux(idx==1, 0x00, init_rom[idx-2]))),
            If(m.done,
                If(idx == 2+NINIT-1, NextState("OI_STOP")).Else(NextValue(idx, idx+1))))
        fsm.act("OI_STOP", self.resetb.eq(1), *issue(OP_STOP),
            If(m.done, NextState("TP_START")))

        # 3) TVP read (reg -> r01/r02): S,0xB8,reg,Sr,0xB9,READ(nack),P
        fsm.act("TP_START", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextState("TP_ADDRW")))
        fsm.act("TP_ADDRW", self.resetb.eq(1), *issue(OP_WRITE, TVP_W),
            If(m.done, NextValue(self.tvp_ack, ~m.ackr), NextState("TP_REG")))
        fsm.act("TP_REG", self.resetb.eq(1), *issue(OP_WRITE, reg_b),
            If(m.done, If(is_wstep, NextState("TP_WVAL")).Else(NextState("TP_RSTART"))))
        # 書込ステップ: 値を書いてSTOP
        fsm.act("TP_WVAL", self.resetb.eq(1), *issue(OP_WRITE, WRVAL),
            If(m.done, NextState("TP_STOP")))
        # 読出ステップ: repeated START, 読出アドレス, READ
        fsm.act("TP_RSTART", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextState("TP_ADDRR")))
        fsm.act("TP_ADDRR", self.resetb.eq(1), *issue(OP_WRITE, TVP_R),
            If(m.done, NextState("TP_READ")))
        fsm.act("TP_READ", self.resetb.eq(1), *issue(OP_READ, nack=1),
            If(m.done,
                If(step == 0, NextValue(self.rev, m.rdata)),
                If(step == 2, NextValue(self.wrb, m.rdata)),
                NextState("TP_STOP")))
        fsm.act("TP_STOP", self.resetb.eq(1), *issue(OP_STOP),
            If(m.done,
                If(step == 2,
                    NextValue(step, 0), NextValue(fi, 0), NextState("FMT"),
                ).Else(
                    NextValue(step, step + 1), NextState("TP_START"),
                )))

        # 4) FORMAT: 動的15文字をテキストRAMへ書込
        faddr = Signal(7); fchar = Signal(8)
        up = sec
        self.comb += [
            Case(fi, {
                0: [faddr.eq(38), fchar.eq(Mux(self.tvp_ack, ord('A'), ord('N')))],
                1: [faddr.eq(39), fchar.eq(Mux(self.tvp_ack, ord('C'), ord('A')))],
                2: [faddr.eq(40), fchar.eq(ord('K'))],
                3: [faddr.eq(56), fchar.eq(hexch(self.rev[4:8]))],
                4: [faddr.eq(57), fchar.eq(hexch(self.rev[0:4]))],
                5: [faddr.eq(72), fchar.eq(hexch(self.wrb[4:8]))],
                6: [faddr.eq(73), fchar.eq(hexch(self.wrb[0:4]))],
                7:  [faddr.eq(88), fchar.eq(hexch(up[28:32]))],
                8:  [faddr.eq(89), fchar.eq(hexch(up[24:28]))],
                9:  [faddr.eq(90), fchar.eq(hexch(up[20:24]))],
                10: [faddr.eq(91), fchar.eq(hexch(up[16:20]))],
                11: [faddr.eq(92), fchar.eq(hexch(up[12:16]))],
                12: [faddr.eq(93), fchar.eq(hexch(up[8:12]))],
                13: [faddr.eq(94), fchar.eq(hexch(up[4:8]))],
                14: [faddr.eq(95), fchar.eq(hexch(up[0:4]))],
            }),
        ]
        fsm.act("FMT", self.resetb.eq(1),
            twr.adr.eq(faddr), twr.dat_w.eq(fchar), twr.we.eq(1),
            If(fi == 14, NextState("OD_START")).Else(NextValue(fi, fi+1)))

        # 5) OLED data フレーム: START,0x78,0x40,<1024>,STOP
        fsm.act("OD_START", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextValue(idx,0), NextValue(prefetch,0), NextState("OD_BODY")))
        fsm.act("OD_BODY", self.resetb.eq(1),
            # idx0,1 は 0x78,0x40。idx>=2 はフォントバイト(2サイクルprefetch)
            If(idx < 2,
                *issue(OP_WRITE, Mux(idx==0, OLED_W, 0x40)),
                If(m.done, NextValue(idx, idx+1)),
            ).Else(
                # prefetch: 0->1->2 でfrd.dat_r確定
                If(prefetch != 2,
                    NextValue(prefetch, prefetch+1),
                ).Else(
                    *issue(OP_WRITE, frd.dat_r),
                    If(m.done,
                        NextValue(prefetch, 0),
                        If(idx == 2+1024-1, NextState("OD_STOP")).Else(NextValue(idx, idx+1))),
                ),
            ),
        )
        fsm.act("OD_STOP", self.resetb.eq(1), *issue(OP_STOP),
            If(m.done, NextValue(rst_cnt,0), NextState("DWELL")))
        # 6) 次周まで少し待つ(~30fps)
        dwell = Signal(24)
        fsm.act("DWELL", self.resetb.eq(1),
            NextValue(dwell, dwell+1),
            If(dwell == int(sys_clk_freq/30), NextValue(dwell,0), NextState("TP_START")))

        # --- 実機pad(open-drain SDA/SCL + push-pull RESETB) ---
        if pads is not None:
            for sig, low in ((pads.scl, m.scl_low), (pads.sda, m.sda_low)):
                t = TSTriple(); self.specials += t.get_tristate(sig)
                self.comb += [t.o.eq(0), t.oe.eq(low)]
                if sig is pads.sda:
                    self.comb += m.sda_in.eq(t.i)
            if hasattr(pads, "resetb"):
                self.comb += pads.resetb.eq(self.resetb)
