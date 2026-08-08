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
        "TVP : ---",        # ACK/NAK @ col6..8
        "SYNC: 0x--",       # 0x14 hex @ col8,9
        "LPF : 0x----",     # 0x37:0x38 hex @ col8..11
        "CPL : 0x----",     # 0x39:0x3A hex @ col8..11
        "UP  : 0x--------",  # hex @ col8..15
        "in3 RGB  0x5C/3C",
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
                 tvp_addr=0x5C, oled_addr=0x3C,
                 green_input=3, red_input=3, blue_input=3, pll_divide=0):
        # pll_divide: H-PLL帰還分周比(=1ライン当たりのDATACLK数)。0なら書かない
        #   (TVP既定1650)。入力の実水平トータル[ドット]に合わせると1サンプル=1ドットに
        #   なる。既定1650のままだと実htotal(X68000 31kHz≒1104)より細かくサンプルする
        #   ので、有効領域がキャプチャ幅1024を超えて右が切れる。
        assert pll_divide == 0 or 1 <= pll_divide <= 0xFFF
        # {green,red,blue}_input: 各チャネルに使う入力ピン番号。既定3(基板配線)。
        #   0x19 = [7:6]SOG [5:4]Red [3:2]Green [1:0]Blue、各 00=_1 01=_2 10=_3 11=_4。
        #   緑のクランプ/レベル異常の切り分け用。R/Bも切り替えられるようにして
        #   「muxの切り替えでクランプ電圧が別ピンへ移動するか」の対照実験に使う。
        assert green_input in (1, 2, 3, 4)
        assert red_input in (1, 2, 3) and blue_input in (1, 2, 3)  # _4はR/Bに無い
        self.submodules.m = m = I2CByteMaster(sys_clk_freq, i2c_freq)
        # open-drain 線(sim用に公開)
        self.scl_low = m.scl_low
        self.sda_low = m.sda_low
        self.resetb  = Signal(reset=0)      # TVP RESETB (0=reset, 1=解除)
        # 観測用
        self.tvp_ack = Signal()
        self.syncdet = Signal(8)  # reg0x14 Sync Detect Status
        self.lpf_hi = Signal(8); self.lpf_lo = Signal(8)  # 0x37/0x38 Lines/Frame
        self.cpl_hi = Signal(8); self.cpl_lo = Signal(8)  # 0x39/0x3A Clocks/Line
        # H-PLL帰還分周比(=1ライン当たりDATACLK数)。実行時に変更可。
        # このFSMは初期化書き込みを毎周(約30回/秒)繰り返すので、値を変えれば
        # 次の周で自動的にTVPへ書き込まれる(別途トリガは不要)。
        # pll_divide=0 でビルドした場合は 0x01/0x02 を書かないので効かない。
        self.cfg_pll_divide = Signal(12, reset=pll_divide)

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

        # TVPステップ: 先頭NWRITE個=初期化書込(reg<-val)、残り=ステータス読出
        #   write: 0x19<-0xAA(SOG/R/G/B 全て _3 入力を選択, OSSC互換)
        #          0x0E<-0x52(AHSS=0/AVSS=0: 外部HSYNC/VSYNC を有効HSYNC/VSYNCに)
        #          0x17<-0x02(Output En=0: RGB/DATACLK/HSOUT/VSOUT/FIDOUT 出力ON。
        #                     既定0x03はbit0=1で全出力Hi-Z=DATACLKが出ない。SOG Enは1のまま)
        #          0x18<-0x01(CLK POL=1: データをDATACLK立下りでlaunch。FPGAは立上りで
        #                     安定サンプルできる。他ビットは既定0)
        #          0x31<-0x18(ALC Placement: データシートが「PCグラフィックス/バイレベル
        #                     同期のSDTV」に指定する値。既定0x5AはHDTV三値同期用)
        #          0x10<-0x58(既定0x5DのRed CS(bit0)/Blue CS(bit2)を0にし、R/G/B全て
        #                     bottom-levelクランプへ。既定はYPbPr向けで Pr(赤)/Pb(青)が
        #                     mid-levelクランプ = blankレベルが512(中央値)にマップされ、
        #                     黒が R,B≈128 になって背景が紫がかっていた。データシート:
        #                     "Bottom-level clamping is required for Y and RGB inputs,
        #                      while mid-level clamping is required for Pb and Pr inputs"。
        #                     SOG Threshold[7:3]は既定のまま)
        #   read : 0x14(SyncDet) 0x37/0x38(Lines/Frame) 0x39/0x3A(Clocks/Line)
        # 0x19: SOGは_3固定、R/G/B は引数で選択(全て既定3 → 0xAA)。
        MUX1 = ((2 << 6) | ((red_input - 1) << 4) |
                ((green_input - 1) << 2) | (blue_input - 1))
        WR_REG = [0x19, 0x0E, 0x17, 0x18, 0x31, 0x10]
        WR_VAL = [MUX1, 0x52, 0x02, 0x01, 0x18, 0x58]
        if pll_divide:
            # 0x01=PLL divide[11:4], 0x02=[7:4]にPLL divide[3:0]。データシート指定どおり
            # MSBs(0x01)を先に書く。
            WR_REG += [0x01, 0x02]
            WR_VAL += [(pll_divide >> 4) & 0xFF, (pll_divide & 0x0F) << 4]
        RD_REG = [0x14, 0x37, 0x38, 0x39, 0x3A]
        NWRITE = len(WR_REG); NREAD = len(RD_REG); NSTEP = NWRITE + NREAD
        step = Signal(max=NSTEP + 1)
        wreg_rom = Array(Constant(v, 8) for v in WR_REG)
        wval_rom = Array(Constant(v, 8) for v in WR_VAL)
        rreg_rom = Array(Constant(v, 8) for v in RD_REG)
        rstep = Signal(max=NREAD + 1); self.comb += rstep.eq(step - NWRITE)
        is_wstep = Signal(); self.comb += is_wstep.eq(step < NWRITE)
        reg_b = Signal(8); self.comb += reg_b.eq(Mux(is_wstep, wreg_rom[step], rreg_rom[rstep]))
        wval_b = Signal(8); self.comb += wval_b.eq(wval_rom[step])
        # 分周比(0x01/0x02)の書込値は cfg_pll_divide から取る(後の代入が優先される)
        if 0x01 in WR_REG:
            self.comb += If(step == WR_REG.index(0x01),
                            wval_b.eq(self.cfg_pll_divide[4:12]))
        if 0x02 in WR_REG:
            self.comb += If(step == WR_REG.index(0x02),
                            wval_b.eq(Cat(C(0, 4), self.cfg_pll_divide[0:4])))

        # FORMAT
        fi = Signal(5)   # 0..20 (NFMT=21)
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
        fsm.act("TP_WVAL", self.resetb.eq(1), *issue(OP_WRITE, wval_b),
            If(m.done, NextState("TP_STOP")))
        # 読出ステップ: repeated START, 読出アドレス, READ
        fsm.act("TP_RSTART", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextState("TP_ADDRR")))
        fsm.act("TP_ADDRR", self.resetb.eq(1), *issue(OP_WRITE, TVP_R),
            If(m.done, NextState("TP_READ")))
        fsm.act("TP_READ", self.resetb.eq(1), *issue(OP_READ, nack=1),
            If(m.done,
                Case(rstep, {
                    0: NextValue(self.syncdet, m.rdata),
                    1: NextValue(self.lpf_hi, m.rdata),
                    2: NextValue(self.lpf_lo, m.rdata),
                    3: NextValue(self.cpl_hi, m.rdata),
                    4: NextValue(self.cpl_lo, m.rdata),
                }),
                NextState("TP_STOP")))
        fsm.act("TP_STOP", self.resetb.eq(1), *issue(OP_STOP),
            If(m.done,
                If(step == NSTEP - 1,
                    NextValue(step, 0), NextValue(fi, 0), NextState("FMT"),
                ).Else(
                    NextValue(step, step + 1), NextState("TP_START"),
                )))

        # 4) FORMAT: 動的21文字(ACK/SYNC/LPF/CPL/UP)をテキストRAMへ書込
        NFMT = 21
        faddr = Signal(7); fchar = Signal(8)
        up = sec
        self.comb += [
            Case(fi, {
                0:  [faddr.eq(38), fchar.eq(Mux(self.tvp_ack, ord('A'), ord('N')))],
                1:  [faddr.eq(39), fchar.eq(Mux(self.tvp_ack, ord('C'), ord('A')))],
                2:  [faddr.eq(40), fchar.eq(ord('K'))],
                3:  [faddr.eq(56), fchar.eq(hexch(self.syncdet[4:8]))],
                4:  [faddr.eq(57), fchar.eq(hexch(self.syncdet[0:4]))],
                # Lines/Frame[11:0]=(0x38[3:0]<<8)|0x37 → 4桁表示 0x0NNN(上位nibは常に0)
                5:  [faddr.eq(72), fchar.eq(ord('0'))],
                6:  [faddr.eq(73), fchar.eq(hexch(self.lpf_lo[0:4]))],
                7:  [faddr.eq(74), fchar.eq(hexch(self.lpf_hi[4:8]))],
                8:  [faddr.eq(75), fchar.eq(hexch(self.lpf_hi[0:4]))],
                # Clocks/Line[11:0]=(0x3A[3:0]<<8)|0x39 → 4桁表示 0x0NNN
                9:  [faddr.eq(88), fchar.eq(ord('0'))],
                10: [faddr.eq(89), fchar.eq(hexch(self.cpl_lo[0:4]))],
                11: [faddr.eq(90), fchar.eq(hexch(self.cpl_hi[4:8]))],
                12: [faddr.eq(91), fchar.eq(hexch(self.cpl_hi[0:4]))],
                13: [faddr.eq(104), fchar.eq(hexch(up[28:32]))],
                14: [faddr.eq(105), fchar.eq(hexch(up[24:28]))],
                15: [faddr.eq(106), fchar.eq(hexch(up[20:24]))],
                16: [faddr.eq(107), fchar.eq(hexch(up[16:20]))],
                17: [faddr.eq(108), fchar.eq(hexch(up[12:16]))],
                18: [faddr.eq(109), fchar.eq(hexch(up[8:12]))],
                19: [faddr.eq(110), fchar.eq(hexch(up[4:8]))],
                20: [faddr.eq(111), fchar.eq(hexch(up[0:4]))],
            }),
        ]
        fsm.act("FMT", self.resetb.eq(1),
            twr.adr.eq(faddr), twr.dat_w.eq(fchar), twr.we.eq(1),
            If(fi == NFMT - 1, NextState("OD_START")).Else(NextValue(fi, fi+1)))

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
