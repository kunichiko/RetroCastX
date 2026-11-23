#!/usr/bin/env python3
"""retrocastx_led.py の検証(Migenシミュレーション)。

LED のデータ線の波形を**受け側と同じやり方で**復号する(H 幅で 0/1 を判定し、
Low が続いたらフレームの区切り)。色を直接覗くのではなく線の上で確かめるので、
GRB の並び・ビット順・リセット幅の取り違えもここで落ちる。

実行: .venv/bin/python sim_led.py
"""
from migen import *

from retrocastx_led import (StatusLed, KokameLeds, Ws2812Chain,
                            STATE_COLORS, IDLE_KEEP, IDLE_BREATHE, IDLE_OFF,
                            EFF_OFF, EFF_SOLID, EFF_BLINK, EFF_BREATHE, EFF_RAINBOW)

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


def hue_ref(h):
    """KokameLeds と同じ式の色相 → 0x00RRGGBB(16bit の色相)"""
    h6 = h * 6
    seg, f = (h6 >> 16) & 7, (h6 >> 8) & 0xFF
    d = 255 - f
    r, g, b = {0: (255, f, 0), 1: (d, 255, 0), 2: (0, 255, f),
               3: (0, d, 255), 4: (f, 0, 255)}.get(seg, (255, 0, d))
    return (r << 16) | (g << 8) | b


def run_kokame(setup, total_ms, marks_at, **kw):
    """setup(dut) を最初に流し、marks_at の各時刻[ms]に受けていた最後のフレームを返す"""
    pad = Signal()
    dut = KokameLeds(pad, CLK, ms_cycles=MS, **kw)
    frames = []
    marks = {}

    def drive():
        yield from setup(dut)
        t = 0
        for name, at in sorted(marks_at.items(), key=lambda x: x[1]):
            for _ in range((at - t) * MS):
                yield
            t = at
            marks[name] = len(frames)

    run_simulation(dut, [drive(), decode(pad, frames, total_ms * MS)])
    return dut, frames, marks


def test_rainbow():
    """電源投入時の既定(レインボー、4個、明るさ64、4個で半周ずらす)。
    各フレームの4色が「ある色相 h から spread ずつずらした色相」を明るさで
    縮めたものと一致すること"""
    spread = 65536 // 8

    def setup(dut):
        yield dut.rate.eq(65536 // 40)      # 40ms で1周(試験を短くする)
        yield

    _, frames, marks = run_kokame(setup, 60, {"a": 25, "b": 55})
    for name in ("a", "b"):
        got = frames[marks[name] - 1]
        assert len(got) == 4, got
        hit = None
        for h in range(65536):
            if all(scale(hue_ref((h + i * spread) & 0xFFFF), 64) == got[i] for i in range(4)):
                hit = h
                break
        assert hit is not None, f"レインボーの色が合わない: {[hex(x) for x in got]}"
    a, b = frames[marks["a"] - 1], frames[marks["b"] - 1]
    assert a != b, "レインボーの色が動いていない"
    print("rainbow: OK —", " ".join(f"{x:06x}" for x in a), "→", " ".join(f"{x:06x}" for x in b))


def test_effects():
    """固定色・明るさ・点滅・呼吸・消灯"""
    def setup(dut):
        yield dut.n_led.eq(2)
        yield dut.effect.eq(EFF_SOLID)
        yield dut.color.eq(0xFF8040)
        yield dut.bright.eq(255)
        yield

    _, frames, marks = run_kokame(setup, 25, {"solid": 22})
    assert frames[marks["solid"] - 1] == [scale(0xFF8040, 255)] * 2, \
        f"固定色: {[hex(x) for x in frames[marks['solid'] - 1]]}"

    def setup(dut):
        yield dut.n_led.eq(2)
        yield dut.effect.eq(EFF_SOLID)
        yield dut.color.eq(0xFF8040)
        yield dut.bright.eq(63)
        yield

    _, frames, marks = run_kokame(setup, 25, {"dim": 22})
    assert frames[marks["dim"] - 1] == [scale(0xFF8040, 63)] * 2, \
        f"明るさ: {[hex(x) for x in frames[marks['dim'] - 1]]}"

    # 点滅: 80ms 周期。前半は色、後半は消える
    def setup(dut):
        yield dut.n_led.eq(1)
        yield dut.effect.eq(EFF_BLINK)
        yield dut.color.eq(0x00FF00)
        yield dut.bright.eq(255)
        yield dut.rate.eq(65536 // 80)
        yield

    _, frames, marks = run_kokame(setup, 170, {"end": 165})
    seen = {f[0] for f in frames[:marks["end"]]}
    assert seen == {0, scale(0x00FF00, 255)}, f"点滅: {[hex(x) for x in seen]}"

    # 呼吸: 200ms 周期。明るさが 0 から山まで上下する(色は変わらない)
    def setup(dut):
        yield dut.n_led.eq(1)
        yield dut.effect.eq(EFF_BREATHE)
        yield dut.color.eq(0xFF0000)
        yield dut.bright.eq(255)
        yield dut.rate.eq(65536 // 200)
        yield

    _, frames, marks = run_kokame(setup, 420, {"end": 415})
    br = frames[2:marks["end"]]
    assert all(f[0] & 0x00FFFF == 0 for f in br), "呼吸で色が変わった"
    lv = [f[0] >> 16 for f in br]
    assert min(lv) <= 2 and max(lv) >= 240, f"呼吸の幅: {min(lv)}〜{max(lv)}"

    def setup(dut):
        yield dut.n_led.eq(2)
        yield dut.effect.eq(EFF_OFF)
        yield

    _, frames, marks = run_kokame(setup, 25, {"off": 22})
    assert frames[marks["off"] - 1] == [0, 0], "消灯しない"
    print(f"effects: OK — 固定色 / 明るさ / 点滅 / 呼吸 {min(lv)}〜{max(lv)} / 消灯")


def test_idle():
    """Viewer が居ないあいだの動作。固定色(赤)で確かめる。

    - 送信中はモードに関係なく通常の明るさ
    - 呼吸: 色は赤のまま、明るさが上下する。山は通常の1/4、谷は消える
    - 消灯: 全部 0。送信が戻れば通常の明るさに戻る
    呼吸の1段を 1ms にして、1周(510段)を 510ms に縮める
    """
    pad = Signal()
    dut = KokameLeds(pad, CLK, ms_cycles=MS, breathe_step_ms=1)
    frames = []
    marks = {}

    def wait(ms):
        for _ in range(ms * MS):
            yield

    def drive():
        yield dut.n_led.eq(2)
        yield dut.effect.eq(EFF_SOLID)
        yield dut.color.eq(0xFF0000)
        yield dut.bright.eq(255)
        yield dut.idle_mode.eq(IDLE_BREATHE)
        yield dut.streaming.eq(1)
        yield from wait(30)
        marks["stream"] = len(frames)
        yield dut.streaming.eq(0)
        yield from wait(600)                # 呼吸を1周より長く見る
        marks["breathe"] = len(frames)
        yield dut.idle_mode.eq(IDLE_OFF)
        yield from wait(30)
        marks["off"] = len(frames)
        yield dut.idle_mode.eq(IDLE_KEEP)
        yield from wait(30)
        marks["keep"] = len(frames)
        yield dut.idle_mode.eq(IDLE_OFF)
        yield dut.streaming.eq(1)
        yield from wait(30)
        marks["back"] = len(frames)

    run_simulation(dut, [drive(), decode(pad, frames, 725 * MS)])
    red = scale(0xFF0000, 255)
    assert frames[marks["stream"] - 1] == [red, red], \
        f"送信中に通常の明るさでない: {[hex(x) for x in frames[marks['stream'] - 1]]}"
    br = frames[marks["stream"] + 3:marks["breathe"]]
    assert all(x[0] == x[1] and x[0] & 0x00FFFF == 0 for x in br), "呼吸で色が変わった"
    lv = [x[0] >> 16 for x in br]
    # 山は 255 × 1/4 ≒ 64(ガンマと切り捨てで少し下がる)。谷は 0
    assert 48 <= max(lv) <= 64, f"呼吸の山: {max(lv)}"
    assert min(lv) == 0, f"呼吸の谷: {min(lv)}"
    # 上りも下りもあること(1周している)
    i_min = lv.index(0)
    assert max(lv[:i_min]) >= 48 and max(lv[i_min:]) >= 48, "呼吸が1周していない"
    assert frames[marks["off"] - 1] == [0, 0], "消灯しない"
    assert frames[marks["keep"] - 1] == [red, red], "「そのまま」で通常の明るさに戻らない"
    assert frames[marks["back"] - 1] == [red, red], "送信が戻っても通常の明るさに戻らない"
    print(f"idle: OK — 呼吸 {min(lv)}〜{max(lv)} / {len(br)} 回送出")


if __name__ == "__main__":
    test_status()
    test_rainbow()
    test_effects()
    test_idle()
    print("all led sims passed")
