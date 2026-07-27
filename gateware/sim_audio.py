#!/usr/bin/env python3
"""音声キャプチャモジュールのユニットシミュレーション。

- I2sCapture: PCM1808のI2S出力(24bit, MSBファースト, LRCKエッジ+1BCK遅れ)を
  忠実にモデル化した波形を食わせ、上位16bitのL/Rペアが取れることを確認
- SpdifDecoder: 48kHz相当(UI≈8.14サイクル、非整数)のBMC波形を生成して食わせ、
  デコードされたL/Rペアがビット一致することを確認

実行: .venv/bin/python sim_audio.py
"""
import random
import sys

from migen import *
from migen.sim import run_simulation

from retrocastx_audio import I2sCapture, SpdifDecoder


# --- I2S ----------------------------------------------------------------------

class I2sDut(Module):
    def __init__(self):
        self.bck = Signal()
        self.lrck = Signal()
        self.dout = Signal()
        self.submodules.cap = I2sCapture(self.bck, self.lrck, [self.dout])


def i2s_adc_model(dut, samples):
    """PCM1808モデル: DUTのフレームカウンタを鏡映しにしてDOUTを駆動する。

    24bit出力(上位16bit=テスト値、下位8bit=0xA5でゴミを模擬)。
    スロットsのビットはBCK立ち下がり後に変化する実デバイスに合わせ、
    「次サイクルのcnt」に対応するビットを常時出力する。
    """
    idx = 0
    while idx < len(samples):
        # 生成器の書き込みは「現サイクル終端のクロックエッジ」で取り込まれるため、
        # 現在のcnt値のスロットに対応するビットをそのまま出す
        c = (yield dut.cap.cnt) & 0xFF
        half = (c >> 7) & 1
        slot = (c >> 2) & 31
        l, r = samples[idx]
        word24 = ((r if half else l) << 8) | 0xA5
        if 1 <= slot <= 24:
            bit = (word24 >> (24 - slot)) & 1
        else:
            bit = 0
        yield dut.dout.eq(bit)
        if c == 255:
            idx += 1
        yield


def i2s_collector(dut, out, n):
    src = dut.cap.sources[0]
    yield src.ready.eq(1)
    cycles = 0
    while len(out) < n and cycles < 300 * 256:
        if (yield src.valid):
            d = yield src.data
            out.append((d & 0xFFFF, d >> 16))
        yield
        cycles += 1


def test_i2s():
    dut = I2sDut()
    samples = [(0x1234, 0xABCD), (0x0000, 0xFFFF), (0x8001, 0x7FFE),
               (0x5555, 0xAAAA), (0xDEAD, 0xBEEF)] * 3
    got = []
    run_simulation(dut, {"aud": [i2s_adc_model(dut, samples + samples[:2])],
                         "sys": [i2s_collector(dut, got, len(samples))]},
                   clocks={"sys": 10, "aud": 20})
    # 先頭は起動時の不完全フレームがあり得るので、既知列がどこかに現れることを確認
    flat = got
    target = samples[1:-1]
    ok = any(flat[i:i + len(target)] == target
             for i in range(max(1, len(flat) - len(target) + 1)))
    assert ok, "I2S sequence not found: got %s" % [
        ("%04x" % l, "%04x" % r) for l, r in flat[:8]]
    print("I2S: OK — %d frames captured, sequence bit-exact" % len(flat))


# --- S/PDIF -------------------------------------------------------------------

PRE_B = [3, 1, 1, 3]   # 左ch(ブロック先頭)
PRE_M = [3, 3, 1, 1]   # 左ch
PRE_W = [3, 2, 1, 2]   # 右ch


def bmc_subframe(preamble, audio16):
    """1サブフレームのパルス幅列(UI単位)を返す。スロット4..31、音声はスロット12..27。"""
    bits = [0] * 8                      # スロット4..11(aux+下位4bit)=0
    bits += [(audio16 >> i) & 1 for i in range(16)]  # LSBファースト
    bits += [0] * 4                      # V,U,C,P=0 (パリティ無視のv0デコーダ)
    runs = list(preamble)
    for b in bits:
        if b:
            runs += [1, 1]
        else:
            runs += [2]
    # 連続する同レベル区間の結合: BMCはセル境界で必ず遷移するので、
    # ビット列から作ったrunsはそのままパルス幅列になる(結合は起きない)
    return runs


def spdif_waveform(frames, ui_cycles=8.14):
    """(L,R)列 → sysサイクル単位の(レベル, 幅)列。非整数UIは誤差蓄積で再現。"""
    runs_ui = []
    for i, (l, r) in enumerate(frames):
        pre = PRE_B if i == 0 else PRE_M
        runs_ui += bmc_subframe(pre, l)
        runs_ui += bmc_subframe(PRE_W, r)
    level = 1
    acc = 0.0
    out = []
    pos = 0
    for ui in runs_ui:
        acc += ui * ui_cycles
        width = round(acc) - pos
        pos += width
        out.append((level, width))
        level ^= 1
    return out


def spdif_driver(dut, waveform):
    for level, width in waveform:
        yield dut.pad.eq(level)
        for _ in range(width):
            yield
    yield dut.pad.eq(0)
    yield


def spdif_collector(dut, out, n, max_cycles):
    yield dut.dec.source.ready.eq(1)
    cycles = 0
    while len(out) < n and cycles < max_cycles:
        if (yield dut.dec.source.valid):
            d = yield dut.dec.source.data
            out.append((d & 0xFFFF, d >> 16))
        yield
        cycles += 1


class SpdifDut(Module):
    def __init__(self):
        self.pad = Signal()
        self.submodules.dec = SpdifDecoder(self.pad, sys_clk_freq=int(50e6))


def test_spdif():
    rnd = random.Random(7)
    frames = [(0x1234, 0xABCD), (0x0001, 0x8000), (0xFFFF, 0x0000)] + \
             [(rnd.randrange(65536), rnd.randrange(65536)) for _ in range(12)]
    dut = SpdifDut()
    got = []
    wave = spdif_waveform(frames)
    total = sum(w for _, w in wave) + 1000
    run_simulation(dut, [spdif_driver(dut, wave),
                         spdif_collector(dut, got, len(frames), total)])
    target = frames[1:-1]
    ok = any(got[i:i + len(target)] == target
             for i in range(max(1, len(got) - len(target) + 1)))
    assert ok, "S/PDIF sequence not found: got %s expect %s" % (
        [("%04x" % l, "%04x" % r) for l, r in got[:6]],
        [("%04x" % l, "%04x" % r) for l, r in target[:6]])
    print("S/PDIF: OK — %d/%d frames decoded, sequence bit-exact"
          % (len(got), len(frames)))


if __name__ == "__main__":
    test_i2s()
    test_spdif()
    print("all audio unit sims passed")
