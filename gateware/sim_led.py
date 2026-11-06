#!/usr/bin/env python3
"""retrocastx_led.py の検証(Migenシミュレーション)。

LED のデータ線の波形を**受け側と同じやり方で**復号する(H 幅で 0/1 を判定し、
Low が続いたらフレームの区切り)。色を直接覗くのではなく線の上で確かめるので、
GRB の並び・ビット順・リセット幅の取り違えもここで落ちる。

実行: .venv/bin/python sim_led.py
"""
from migen import *

from retrocastx_led import (StatusLed, KeyframeLeds, Ws2812Chain, rainbow_table,
                            STATE_COLORS)

CLK = 8e6   # 1ビット10サイクル。実機(45MHz)より粗いがビット幅の比は同じ
MS = 500    # シミュレーション上の「1ms」。実時間だと遅すぎるので縮める
            # (10ms ごとの送出 = 5000 サイクル > 4LED の1フレーム 3360 サイクル)


def decode(pad, frames, n_cycles):
    """線を見て、1フレームごとに [0x00RRGGBB, ...] を frames に積む。"""
    t_bit = int(round(CLK * 1.25e-6))
    bits, high, low = [], 0, 0
    for _ in range(n_cycles):
        v = yield pad
        if v:
            if low and high:                  # 直前のパルスが確定
                bits.append(1 if high > t_bit // 2 else 0)
                high = 0
            low = 0
            high += 1
        else:
            low += 1
            if low == int(CLK * 50e-6):       # 50µs Low = フレーム区切り
                if high:
                    bits.append(1 if high > t_bit // 2 else 0)
                    high = 0
                if bits:
                    assert len(bits) % 24 == 0, f"ビット数が24の倍数でない: {len(bits)}"
                    out = []
                    for k in range(0, len(bits), 24):
                        w = int("".join(map(str, bits[k:k + 24])), 2)
                        g, r, b = w >> 16, (w >> 8) & 0xFF, w & 0xFF
                        out.append((r << 16) | (g << 8) | b)
                    frames.append(out)
                bits = []
        yield


def scale(c, k):
    return sum(((((c >> s) & 0xFF) * (k + 1)) >> 8) << s for s in (0, 8, 16))


def test_status():
    pad = Signal()
    dut = StatusLed(pad, CLK, period_cycles=5000)
    frames = []
    seen = []

    def drive():
        for state in (0, 1, 2):
            yield dut.host_seen.eq(state >= 1)
            yield dut.streaming.eq(state == 2)
            for _ in range(5000 * 4):
                yield
            seen.append(len(frames))

    n = 5000 * 4 * 3 + 10
    run_simulation(dut, [drive(), decode(pad, frames, n)])
    # 各状態の最後に受けたフレームの色
    got = [frames[k - 1][0] for k in seen]
    want = [scale(STATE_COLORS[s], 32) for s in (0, 1, 2)]
    assert got == want, f"状態LEDの色が違う: {[hex(x) for x in got]} != {[hex(x) for x in want]}"
    print("status: OK —", ", ".join(hex(x) for x in got))


def test_keyframes():
    pad = Signal()
    dut = KeyframeLeds(pad, CLK, ms_cycles=MS)
    frames = []
    marks = {}

    def drive():
        # 既定のレインボー: 起動直後は先頭フレームの色(t=0)
        yield dut.bright.eq(255)
        for _ in range(15 * MS):
            yield
        marks["boot"] = len(frames)
        # バンク1へ2LED・2フレームの表を書いて切り替える
        #   f0: 赤/緑 を 20ms 保持 → 40ms で f1 へフェード
        #   f1: 青/白 を 20ms 保持 → 切り替え(フェードなし)で f0 へ
        tbl = [(20 | ((65536 // 40) << 16)), 0xFF0000, 0x00FF00,
               20, 0x0000FF, 0xFFFFFF]
        for k, w in enumerate(tbl):
            yield dut.tbl_adr.eq(256 + k)
            yield dut.tbl_dat_w.eq(w)
            yield dut.tbl_we.eq(1)
            yield
        yield dut.tbl_we.eq(0)
        yield dut.n_led.eq(2)
        yield dut.frames.eq(2)
        yield dut.bank.eq(1)
        for _ in range(12 * MS):
            yield
        marks["hold0"] = len(frames)
        # 送出は10msごとなので、区間の真ん中あたりで最後に受けた色を見る
        for _ in range(23 * MS):          # 35ms: フェード中(20〜60ms)
            yield
        marks["fade"] = len(frames)
        for _ in range(35 * MS):          # 70ms: f1 の保持中(60〜80ms)
            yield
        marks["hold1"] = len(frames)
        for _ in range(20 * MS):          # 90ms: f0 に戻って保持中
            yield
        marks["loop"] = len(frames)

    n = 110 * MS
    run_simulation(dut, [drive(), decode(pad, frames, n)])
    words, _ = rainbow_table(4)
    boot = frames[marks["boot"] - 1]
    # 既定は保持0ms ですぐフェードを始めるので、先頭フレームの色から少しだけ進んでいる
    near = lambda x, y: all(abs(((x >> s) & 0xFF) - ((y >> s) & 0xFF)) <= 16
                            for s in (0, 8, 16))
    assert len(boot) == 4 and all(near(x, y) for x, y in zip(boot, words[1:5])), \
        f"既定パターンの先頭が違う: {[hex(x) for x in boot]}"
    assert frames[marks["hold0"] - 1] == [0xFF0000, 0x00FF00], \
        f"f0 の保持: {[hex(x) for x in frames[marks['hold0'] - 1]]}"
    mid = frames[marks["fade"] - 1]
    # フェード途中: 赤→青、緑→白 の中間のどこか(端点ではない)
    r, g = mid
    assert 0 < (r >> 16) < 0xFF and 0 < (r & 0xFF) < 0xFF and (r >> 8) & 0xFF == 0, \
        f"フェード中の LED0 が補間されていない: {r:06x}"
    assert (g >> 8) & 0xFF == 0xFF and 0 < (g >> 16) < 0xFF, \
        f"フェード中の LED1 が補間されていない: {g:06x}"
    assert frames[marks["hold1"] - 1] == [0x0000FF, 0xFFFFFF], \
        f"f1 の保持: {[hex(x) for x in frames[marks['hold1'] - 1]]}"
    assert frames[marks["loop"] - 1] == [0xFF0000, 0x00FF00], \
        f"ループして f0 に戻っていない: {[hex(x) for x in frames[marks['loop'] - 1]]}"
    print("keyframes: OK — boot", " ".join(f"{x:06x}" for x in boot),
          "/ fade", f"{r:06x} {g:06x}")


def test_bright_and_off():
    pad = Signal()
    dut = KeyframeLeds(pad, CLK, ms_cycles=MS)
    frames = []
    marks = {}

    def drive():
        yield dut.bright.eq(63)
        for _ in range(15 * MS):
            yield
        marks["dim"] = len(frames)
        yield dut.frames.eq(0)
        for _ in range(25 * MS):
            yield
        marks["off"] = len(frames)

    run_simulation(dut, [drive(), decode(pad, frames, 42 * MS)])
    words, _ = rainbow_table(4)
    dim = frames[marks["dim"] - 1]
    assert all(abs(((x >> s) & 0xFF) - ((scale(y, 63) >> s) & 0xFF)) <= 4
               for x, y in zip(dim, words[1:5]) for s in (0, 8, 16)), \
        f"明るさが掛かっていない: {[hex(x) for x in dim]}"
    assert frames[marks["off"] - 1] == [0, 0, 0, 0], "frames=0 で消灯しない"
    print("bright/off: OK")


if __name__ == "__main__":
    test_status()
    test_keyframes()
    test_bright_and_off()
    print("all led sims passed")
