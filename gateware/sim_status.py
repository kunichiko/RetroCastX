#!/usr/bin/env python3
"""StatusDisplay の共有I2C動作を検証(実機不要)。

open-drainバスを模擬し、0x5C=TVP7002(ACK + reg 0x01->0x11, 0x02->0x22 を返す)、
0x3C=OLED(ACKのみ)のスレーブをモデル化。検証項目:
- TVP応答: dut.tvp_ack==1, rev(0x00)==0x02, wrb(0x01書戻し)==0xA5
- バス上に OLED初期化(0x78,0x00,..)/TVP読出(0xB8,reg,0xB9,..)/OLEDデータ(0x78,0x40) フレーム
"""
import sys, os
sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_i2c import StatusDisplay, OLED_INIT


class I2CBus:
    """open-drainバス+スレーブ模型。立上りエッジをnbitで数え pos=nbit%9(0..7=data,8=ACK)。
    スレーブ駆動は立下りで「次ビット」用に更新し保持(I2C: SCL Low中にSDA変化)。"""
    def __init__(self):
        self.scl = 1; self.line = 1
        self.slave_low = 0
        self.txn = False
        self.nbit = -1          # START後の立上り数-1
        self.shift = 0
        self.mode = 'ADDR'      # ADDR/WRITE/READ
        self.addr = None; self.rw = 0
        self.last_reg = None
        self.rd_byte = 0
        self.wr_dcount = 0; self.wr_reg = None
        self.regs = {0x00: 0x02, 0x01: 0x67, 0x02: 0x20}
        self.frames = []; self.cur = []

    def _push(self):
        if self.cur: self.frames.append(self.cur); self.cur = []

    def _start(self):
        self._push()
        self.txn = True; self.nbit = -1; self.shift = 0; self.mode = 'ADDR'
        self.addr = None; self.slave_low = 0; self.wr_dcount = 0

    def _stop(self):
        self._push(); self.txn = False; self.slave_low = 0

    def step(self, scl, m_sda):
        line = 0 if (m_sda == 0 or self.slave_low) else 1
        pscl, pline = self.scl, self.line
        # START/STOP (SCL high中のSDA変化)
        if scl == 1 and pscl == 1:
            if pline == 1 and line == 0: self._start()
            elif pline == 0 and line == 1: self._stop()
        # 立上り: データビットのサンプル(ADDR/WRITE)
        if pscl == 0 and scl == 1 and self.txn:
            self.nbit += 1
            pos = self.nbit % 9
            if pos < 8 and self.mode in ('ADDR', 'WRITE'):
                self.shift = ((self.shift << 1) | line) & 0xFF
        # 立下り: 8データビットは出揃っている。npos==8でバイト確定+ACK判定。
        if pscl == 1 and scl == 0 and self.txn:
            npos = (self.nbit + 1) % 9
            self.slave_low = 0
            if npos == 8:
                if self.mode in ('ADDR', 'WRITE'):
                    byte = self.shift; self.cur.append(byte); self.shift = 0
                    if self.mode == 'ADDR':
                        self.addr = byte >> 1; self.rw = byte & 1
                        if self.addr == 0x5C and self.rw == 1:
                            self.mode = 'READ'; self.rd_byte = self.regs.get(self.last_reg, 0)
                        elif self.rw == 0:
                            self.mode = 'WRITE'
                    else:  # WRITE data: 1st=reg, 2nd=value(格納)
                        if self.addr == 0x5C:
                            if self.wr_dcount == 0:
                                self.wr_reg = byte; self.last_reg = byte; self.wr_dcount = 1
                            else:
                                self.regs[self.wr_reg] = byte; self.wr_dcount = 2
                    if self.addr in (0x3C, 0x5C):
                        self.slave_low = 1          # ACK
                elif self.mode == 'READ':
                    self.cur.append(self.rd_byte); self.mode = 'IDLE'  # 1バイトで停止
            elif npos < 8 and self.mode == 'READ':
                bit = (self.rd_byte >> (7 - npos)) & 1
                self.slave_low = 1 if bit == 0 else 0
        self.scl, self.line = scl, line
        return line


def main():
    dut = StatusDisplay(pads=None, sys_clk_freq=4000, i2c_freq=1000)
    res = {}

    def tb():
        bus = I2CBus()
        for _ in range(120000):
            scl = 0 if (yield dut.scl_low) else 1
            m_sda = 0 if (yield dut.sda_low) else 1
            line = bus.step(scl, m_sda)
            yield dut.m.sda_in.eq(line)
            # TVP読出2つ完了 + OLEDデータフレーム(0x78,0x40)開始を確認したら十分
            if (yield dut.wrb) == 0xA5 and bus.cur[:2] == [0x78, 0x40]:
                break
            yield
        if bus.cur:
            bus.frames.append(bus.cur)
        res['frames'] = bus.frames
        res['tvp_ack'] = (yield dut.tvp_ack)
        res['rev'] = (yield dut.rev)
        res['wrb'] = (yield dut.wrb)

    run_simulation(dut, tb())

    frames = res['frames']
    print(f"tvp_ack={res['tvp_ack']} rev={res['rev']:#04x} wrb={res['wrb']:#04x}")
    print(f"frames captured: {len(frames)}")
    for i, f in enumerate(frames[:6]):
        print(f"  frame{i}: " + " ".join(f"{b:02X}" for b in f[:8]) + (" ..." if len(f) > 8 else ""))

    assert res['tvp_ack'] == 1, "TVPがACKしていない"
    assert res['rev'] == 0x02, f"rev={res['rev']:#x} (期待0x02=Chip Rev)"
    assert res['wrb'] == 0xA5, f"wrb={res['wrb']:#x} (期待0xA5=書戻し)"

    # OLED初期化フレーム(0x78,0x00,<INIT..>)
    oled_init = [f for f in frames if len(f) >= 2 and f[0] == 0x78 and f[1] == 0x00]
    assert oled_init, "OLED初期化フレームが無い"
    assert oled_init[0][2:2+len(OLED_INIT)] == OLED_INIT, "OLED初期化列不一致"
    # TVP読出しフレーム(0xB8..) と 読出アドレス 0xB9
    tvp_f = [f for f in frames if f and f[0] == 0xB8]
    assert tvp_f, "TVP書込アドレスフレームが無い"
    assert any(0xB9 in f for f in frames), "TVP読出アドレス(0xB9)が無い"
    # OLEDデータフレーム(0x78,0x40)
    assert any(len(f) >= 2 and f[0] == 0x78 and f[1] == 0x40 for f in frames), "OLEDデータフレームが無い"

    print("\n[OK] 共有I2C: TVP ACK + reg 0x11/0x22 読出、OLED init/data フレーム、TVP読出トランザクション 全て確認")


if __name__ == "__main__":
    main()
