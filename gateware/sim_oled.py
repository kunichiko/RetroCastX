#!/usr/bin/env python3
"""Ssd1306Display のI2Cバイト列を検証(実機不要)。

scl_low/sda_low から線(open-drain=解放でHigh)を復元し、I2Cをデコード。
- INITフレーム: 0x78,0x00,<INIT_SEQ> と一致するか
- DATAフレーム: 0x78,0x40,<データ...>で先頭8バイトが 'R' のフォント列と一致するか
"""
import sys, os
sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_oled import Ssd1306Display, INIT_SEQ
from oled_font import FONT8


def decode(dut, max_bytes):
    """I2Cデコーダ(SCL立上りでサンプル)。フレーム区切り付きバイト列を返す。"""
    frames = []
    cur = []
    prev_scl = 1
    prev_sda = 1
    bits = []
    in_frame = False
    # 十分なサイクル回す
    for _ in range(4_000_000):
        scl = 0 if (yield dut.scl_low) else 1
        sda = 0 if (yield dut.sda_low) else 1
        # START
        if scl == 1 and prev_sda == 1 and sda == 0:
            in_frame = True; bits = []; cur = []
        # STOP
        elif scl == 1 and prev_sda == 0 and sda == 1:
            if cur:
                frames.append(cur)
            in_frame = False; bits = []; cur = []
            if len(frames) >= 2 and sum(len(f) for f in frames) >= max_bytes:
                break
        # bit sample on SCL rising
        elif in_frame and prev_scl == 0 and scl == 1:
            bits.append(sda)
            if len(bits) == 9:            # 8 data + ACK
                byte = 0
                for b in bits[:8]:
                    byte = (byte << 1) | b
                cur.append(byte)
                bits = []
                if len(cur) >= max_bytes:
                    frames.append(cur)
                    break
        prev_scl, prev_sda = scl, sda
        yield
    return frames


def main():
    # sim高速化のためi2c位相を短く(Q=2)
    dut = Ssd1306Display(pads=None, sys_clk_freq=8e6, i2c_freq=1e6)
    result = {}

    def tb():
        frames = yield from decode(dut, max_bytes=48)
        result['frames'] = frames

    run_simulation(dut, tb(), vcd_name=None)

    frames = result['frames']
    assert len(frames) >= 2, f"フレーム数不足: {len(frames)}"
    init_f, data_f = frames[0], frames[1]

    # INITフレーム検証
    exp_init = [0x78, 0x00] + INIT_SEQ
    got = init_f[:len(exp_init)]
    assert got == exp_init, (
        "INITフレーム不一致\n  exp="+ " ".join(f"{v:02X}" for v in exp_init)
        + "\n  got="+ " ".join(f"{v:02X}" for v in got))
    print(f"[OK] INIT frame {len(exp_init)}B 一致 (0x78,0x00,<{len(INIT_SEQ)} cmds>)")

    # DATAフレーム検証: 0x78,0x40, 先頭8B = 'R' の列
    assert data_f[0] == 0x78 and data_f[1] == 0x40, f"DATA header 不一致 {data_f[:2]}"
    R = ord('R')
    exp_R = FONT8[R*8:R*8+8]
    got_R = data_f[2:10]
    assert got_R == exp_R, (
        "先頭文字 'R' の列不一致\n  exp="+ " ".join(f"{v:02X}" for v in exp_R)
        + "\n  got="+ " ".join(f"{v:02X}" for v in got_R))
    print(f"[OK] DATA frame: 0x78,0x40, 先頭8B = 'R'({R:#04x}) のフォント列と一致")
    # 'R' を可視化
    print("     'R' glyph:")
    for row in range(8):
        print("       " + "".join('#' if (got_R[c]>>row)&1 else '.' for c in range(8)))
    print("\nALL OK")


if __name__ == "__main__":
    main()
