#!/usr/bin/env python3
"""SSD1306 (128x64 I2C OLED) テキスト表示ドライバ。CPU不介在の純HDL。

- I2Cビットバングマスタ(書き込み専用、ACKは無視=表示用途で十分)
- 電源投入後にSSD1306初期化列を送出 → 以後 1024バイトのGDDRAMを連続再描画
- 16文字 × 8行(8x8フォント)= 128文字のテキストRAM。外部から書き換え可
  (text_we/text_addr/text_data)。初期値はバナー文字列。
- フォントは oled_font.FONT8(dhepper/font8x8, Public Domain を列優先転置)

I2Cは open-drain: scl_low/sda_low=1 で線をLowに駆動、0で解放(プルアップでHigh)。
実機は pads(sda,scl)を渡すと TSTriple で接続。sim は scl_low/sda_low を直接観測。
"""
from migen import *

try:
    from .oled_font import FONT8
except ImportError:
    import sys, os
    sys.path.insert(0, os.path.dirname(__file__))
    from oled_font import FONT8

# SSD1306 128x64 初期化列 + 表示範囲設定(列0-127, ページ0-7)
INIT_SEQ = [
    0xAE,             # display off
    0xD5, 0x80,       # clock divide
    0xA8, 0x3F,       # multiplex = 63 (64行)
    0xD3, 0x00,       # display offset 0
    0x40,             # start line 0
    0x8D, 0x14,       # charge pump on
    0x20, 0x00,       # memory addressing = horizontal
    0xA1,             # segment remap (col127->SEG0)
    0xC8,             # COM scan remapped
    0xDA, 0x12,       # COM pins config
    0x81, 0xCF,       # contrast
    0xD9, 0xF1,       # pre-charge
    0xDB, 0x40,       # VCOMH
    0xA4,             # resume to RAM content
    0xA6,             # normal (not inverted)
    0x2E,             # deactivate scroll
    0xAF,             # display on
    0x21, 0x00, 0x7F, # column address range 0..127
    0x22, 0x00, 0x07, # page address range 0..7
]

COLS = 16   # 文字/行 (8x8)
ROWS = 8    # 行 (= 8ページ)


def _banner():
    """初期表示テキスト(16x8=128文字)。"""
    lines = [
        "RetroCastX",
        "----------------",
        "OLED  : ready",
        "I2C   : 0x3C",
        "FPGA  : i5 ECP5",
        "TVP7002: bring-up",
        "",
        "status ->",
    ]
    buf = []
    for r in range(ROWS):
        s = lines[r] if r < len(lines) else ""
        s = (s + " " * COLS)[:COLS]
        buf += [ord(c) & 0x7F for c in s]
    return buf


class Ssd1306Display(Module):
    def __init__(self, pads=None, sys_clk_freq=45e6, i2c_freq=400e3, addr=0x3C):
        # --- 外部I/F ---
        self.scl_low = Signal()      # 1=SCLをLow駆動
        self.sda_low = Signal()      # 1=SDAをLow駆動
        self.sda_in  = Signal(reset=1)  # 実機はpadの入力(ACK)。未使用
        # テキストRAM書き込みポート(任意)
        self.text_we   = Signal()
        self.text_addr = Signal(7)
        self.text_data = Signal(8)
        self.frame_done = Signal()   # DATAフレーム完了で1パルス

        # --- ROM/RAM ---
        self.specials.font = Memory(8, 1024, init=FONT8)
        font_rd = self.font.get_port()
        self.specials += font_rd

        self.specials.text = Memory(8, COLS * ROWS, init=_banner())
        text_rd = self.text.get_port()
        text_wr = self.text.get_port(write_capable=True)
        self.specials += text_rd, text_wr
        self.comb += [
            text_wr.adr.eq(self.text_addr),
            text_wr.dat_w.eq(self.text_data),
            text_wr.we.eq(self.text_we),
        ]

        init_rom = Array(Constant(v, bits_sign=8) for v in INIT_SEQ)
        NINIT = len(INIT_SEQ)
        NDATA = 1024
        ADDR_W = (addr << 1) & 0xFE   # 書き込みアドレスバイト (0x78)

        # --- I2C ビット位相生成 ---
        Q = max(2, int(round(sys_clk_freq / (4 * i2c_freq))))  # 1位相=Qサイクル
        div = Signal(max=Q)
        en = Signal()                 # 位相前進パルス
        self.sync += [
            If(div == Q - 1, div.eq(0)).Else(div.eq(div + 1)),
        ]
        self.comb += en.eq(div == Q - 1)
        phase = Signal(2)             # 0..3(1ビットセル内)
        cell_end = Signal()
        self.comb += cell_end.eq(en & (phase == 3))
        self.sync += If(en, phase.eq(phase + 1))

        # --- バイト送信シフタ ---
        shifter = Signal(8)
        bitpos  = Signal(4)           # 0..8 (8=ACK)
        txbyte  = Signal(8)

        # フレーム管理
        is_data = Signal()            # 0=INIT(cmd), 1=DATA
        idx     = Signal(11)          # フレーム内バイト位置(0=addr,1=ctrl,2..=payload)
        framelen = Signal(12)

        # 現在バイト先頭で参照する payload index
        pay = Signal(11)
        self.comb += pay.eq(idx - 2)
        # DATA payload の座標
        page    = Signal(3)
        xcol    = Signal(7)
        self.comb += [page.eq(pay[7:10]), xcol.eq(pay[0:7])]
        charcol = Signal(4)
        pix     = Signal(3)
        self.comb += [charcol.eq(xcol[3:7]), pix.eq(xcol[0:3])]

        # --- FSM ---
        self.submodules.fsm = fsm = FSM(reset_state="INIT_FRAME")

        # 共通: SCL/SDA を state と phase から駆動するのは各stateで指定
        def scl_pattern(low_phases):
            return reduce(lambda a, b: a | b, [phase == p for p in low_phases])

        fsm.act("INIT_FRAME",
            NextValue(is_data, 0),
            NextValue(framelen, 2 + NINIT),
            NextState("START"),
        )
        fsm.act("DATA_FRAME",
            NextValue(is_data, 1),
            NextValue(framelen, 2 + NDATA),
            NextState("START"),
        )
        # START: SCL high(phase0,1)->low(2,3), SDA high(0)->low(1..)
        fsm.act("START",
            self.scl_low.eq((phase == 2) | (phase == 3)),
            self.sda_low.eq(phase != 0),
            If(cell_end,
                NextValue(idx, 0),
                NextState("LOAD"),
            ),
        )
        # LOAD: idx に応じて txbyte を用意(SCL Low中)。DATAはROM2段読み。
        ld = Signal(2)
        fsm.act("LOAD",
            self.scl_low.eq(1),          # SCL Low 保持
            self.sda_low.eq(~shifter[7]),  # 直前値保持(害なし, SCL Low中)
            # メモリアドレスを常時提示
            text_rd.adr.eq((page << 4) + charcol),
            font_rd.adr.eq((text_rd.dat_r << 3) + pix),
            If(idx == 0, NextValue(txbyte, ADDR_W)),
            If(idx == 1, NextValue(txbyte, Mux(is_data, 0x40, 0x00))),
            # payload
            If(idx >= 2,
                If(is_data,
                    # 2サイクル待ってfont読出し確定
                    NextValue(ld, ld + 1),
                    If(ld == 2,
                        NextValue(txbyte, font_rd.dat_r),
                        NextValue(ld, 0),
                        NextValue(shifter, font_rd.dat_r),
                        NextValue(bitpos, 0),
                        NextState("SEND"),
                    ),
                ).Else(
                    NextValue(txbyte, init_rom[pay]),
                    NextValue(shifter, init_rom[pay]),
                    NextValue(bitpos, 0),
                    NextState("SEND"),
                ),
            ),
            If(idx < 2,
                NextValue(shifter, Mux(idx == 0, ADDR_W, Mux(is_data, 0x40, 0x00))),
                NextValue(bitpos, 0),
                NextState("SEND"),
            ),
        )
        # SEND: 1バイト = 8データビット + 1ACK。SCL: phase1,2=High。
        fsm.act("SEND",
            self.scl_low.eq((phase == 0) | (phase == 3)),
            If(bitpos < 8,
                self.sda_low.eq(~shifter[7]),
            ).Else(
                self.sda_low.eq(0),      # ACK: SDA解放
            ),
            If(cell_end,
                NextValue(shifter, Cat(Signal(1), shifter[0:7])),  # <<1
                If(bitpos == 8,
                    # バイト完了
                    If(idx == framelen - 1,
                        NextState("STOP"),
                    ).Else(
                        NextValue(idx, idx + 1),
                        NextState("LOAD"),
                    ),
                ).Else(
                    NextValue(bitpos, bitpos + 1),
                ),
            ),
        )
        # STOP: SCL Low(0)->High(1..), SDA Low(0,1)->High(2,3)
        refresh = Signal(24)
        fsm.act("STOP",
            self.scl_low.eq(phase == 0),
            self.sda_low.eq((phase == 0) | (phase == 1)),
            If(cell_end,
                If(is_data,
                    self.frame_done.eq(1),
                    NextValue(refresh, 0),
                    NextState("REFRESH"),
                ).Else(
                    NextState("DATA_FRAME"),
                ),
            ),
        )
        # REFRESH: 次のDATAフレームまで少し待つ(バス占有を緩める)
        fsm.act("REFRESH",
            self.scl_low.eq(0),
            self.sda_low.eq(0),
            NextValue(refresh, refresh + 1),
            If(refresh == int(sys_clk_freq / 30),   # ~30fps
                NextState("DATA_FRAME"),
            ),
        )

        # --- 実機pad接続(open-drain) ---
        if pads is not None:
            for sig, low in ((pads.scl, self.scl_low), (pads.sda, self.sda_low)):
                t = TSTriple()
                self.specials += t.get_tristate(sig)
                self.comb += [t.o.eq(0), t.oe.eq(low)]
                if sig is pads.sda:
                    self.comb += self.sda_in.eq(t.i)
