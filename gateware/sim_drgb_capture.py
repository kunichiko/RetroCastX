#!/usr/bin/env python3
"""デジタルRGB の取り込み(retrocastx_drgb_capture.py)をシミュレーションで確かめる。

★**実機が無いので、これが唯一の確認。** 生成器(DigitalRgbGen)を音声の水晶の
  代わりの別クロック(aud)で回し、取り込み(sys)へ直結する。2つのクロックは
  **割り切れない比**(3.66 サンプル/ドット)にする。整数比だとドット境界が
  毎ライン同じ位相に来て、分数の足し込みを試験できない。
  ★Migen の simulator は半周期で刻むので、周期は偶数にする(奇数は切り捨てられる)。

★**小さい寸法で回す。** 実寸を Migen で回すと1フレームに数分かかる。
  ライン長の平均も 256 本ではなく 16 本(avg_log2=4)にする。

確かめること:
1. 取り込み単体: 16色バーと1ドット縞が、HS から数えたドット位置どおりに
   ラインバッファへ入る(全有効ラインの全ドット)。行は VS の後の HS から数えた
   ライン × 2(半ライン単位のスロット)
2. 測定: 極性(負極性と測れる)、vtotal、2フレームの組(クロック数とライン数)
3. 送出部まで通す: video_src = 1 で LINE が pixfmt 6(RGBI4)になり、ライン全体
   (htotal を8の倍数へ切り上げた画素数)が1パケットで届き、中身が 1 と同じ。
   MODE も pixfmt 6
"""
import os, sys
sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "host", "python"))
from migen import *
from litex.soc.interconnect import stream
from liteeth.common import eth_udp_user_description
from retrocastx import protocol as proto
from retrocastx_drgb_capture import DigitalRgbCapture, CaptureSel
from retrocastx_capture import TvpCapture
from retrocastx_stream import RetroCastXStreamer, convert_ip

# ---------------- 試験信号の生成器 ----------------
#
# ★以前は retrocastx_drgb.py にあり、実機でもデバッグ端子から出して J13 へ
#   戻すループバック試験に使っていた。v1.0 基板には端子が無いので
#   ゲートウェアからは外し(2026-11-23、ロジック使用率を下げるため)、
#   シミュレーションの信号源としてここに残した。

class DigitalRgbGen(Module):
    """デバッグ端子へ擬似デジタルRGBを出す。

    パターンは**2つの領域**に分ける:

      上半分  16色の縦カラーバー  → R/G/B/I の4bitが全部通るか
      下半分  1ドット幅の白黒縦縞  → ドットクロックの再生ができるか

    ★**1ドット縞を必ず入れる。** カラーバーだけだと、ドットクロックが2倍
      ずれていても「色は合っている」ように見えてしまう。細かい縞が潰れるか
      どうかが、位相とクロックが合っている唯一の証拠になる。

    寸法を引数にしてあるのは**シミュレーションを現実的な時間で回すため**。
    実寸(953×262×3 = 74万サイクル/フレーム)を Migen で回すと1フレームに
    数分かかり、試験として使えない。
    """

    def __init__(self, htotal, vtotal, hactive, vactive, hstart, vstart,
                 hs_width, vs_width, dot_div=1):
        self.htotal, self.vtotal = htotal, vtotal
        self.hactive, self.vactive = hactive, vactive
        self.hstart, self.vstart = hstart, vstart
        self.dot_div = dot_div

        # ★**「ピンの駆動」と「内部ロジック」を別々に止められるようにする。**
        #   同じビットストリームのまま両方を切り替えられれば、**配置が変わらない
        #   まま**「ピンを振ったせいか、論理を足したせいか」を切り分けられる。
        #   ビルドを分けると配置も変わってしまい、どちらの効果か分からなくなる。
        self.enable = Signal(reset=1)       # 0 = 6本とも Low に固定(駆動を止める)
        self.run = Signal(reset=1)          # 0 = 内部カウンタを止める(論理を静める)
        self.neg_sync = Signal(reset=1)     # 1 = アイドルHigh、パルスでLow

        self.r = Signal(); self.g = Signal(); self.b = Signal(); self.i = Signal()
        self.hs = Signal(); self.vs = Signal()
        self.x = Signal(max=htotal)
        self.y = Signal(max=vtotal)

        # # #

        dot = Signal()
        if dot_div > 1:
            div = Signal(max=dot_div)
            self.sync += If(self.run,
                If(div == dot_div - 1, div.eq(0)).Else(div.eq(div + 1)))
            self.comb += dot.eq(self.run & (div == dot_div - 1))
        else:
            self.comb += dot.eq(self.run)

        self.sync += If(dot,
            If(self.x == htotal - 1,
                self.x.eq(0),
                If(self.y == vtotal - 1, self.y.eq(0)).Else(self.y.eq(self.y + 1)),
            ).Else(self.x.eq(self.x + 1)),
        )

        # ★**enable は6本すべてを止める。** 以前は色の4本しか見ておらず、
        #   HS/VS は出続けていた。そのため「生成器を切って切り分ける」が
        #   成立せず、原因究明を一度誤らせた(2026-09-10)。
        hs_pulse = Signal(); vs_pulse = Signal()
        self.comb += [
            hs_pulse.eq(self.x < hs_width),
            vs_pulse.eq(self.y < vs_width),
            self.hs.eq(self.enable & (hs_pulse ^ self.neg_sync)),
            self.vs.eq(self.enable & (vs_pulse ^ self.neg_sync)),
        ]

        active = Signal()
        px = Signal(max=htotal)
        py = Signal(max=vtotal)
        self.comb += [
            active.eq((self.x >= hstart) & (self.x < hstart + hactive)
                      & (self.y >= vstart) & (self.y < vstart + vactive)),
            px.eq(self.x - hstart),
            py.eq(self.y - vstart),
        ]

        # ★**バー幅は2のべき乗にする。** 640/16 = 40 で割ると除算器が要る。
        #   幅をビットスライスで取れる値にして、余りは黒のままにする ─ 見たいのは
        #   色が通るかどうかで、バーの幅そのものではない。
        bar_shift = max(0, (hactive // 16).bit_length() - 1)
        bar_w = 1 << bar_shift
        self.bar_w = bar_w
        bar = Signal(4)
        in_bars = Signal()
        self.comb += [
            bar.eq(px[bar_shift:bar_shift + 4]),
            in_bars.eq(px < bar_w * 16),
        ]

        rgbi = Signal(4)
        self.comb += If(~active | ~self.enable | ~self.run,
            rgbi.eq(0),
        ).Elif(py < vactive // 2,
            If(in_bars, rgbi.eq(bar)).Else(rgbi.eq(0)),
        ).Else(
            rgbi.eq(Mux(px[0], 0b1111, 0b0000)),     # 1ドット幅の白黒縞
        )
        self.comb += [
            self.r.eq(rgbi[0]), self.g.eq(rgbi[1]),
            self.b.eq(rgbi[2]), self.i.eq(rgbi[3]),
        ]


# 生成器の幾何(小さい寸法)。htotal は8の倍数でない値にして、末尾の扱いも通す
G = dict(htotal=52, vtotal=20, hactive=32, vactive=12,
         hstart=12, vstart=4, hs_width=4, vs_width=1, dot_div=1)
SYS_PERIOD = 100
AUD_PERIOD = 366          # 3.66 サンプル/ドット(実機 45/14.318 = 3.14 と同じく分数)
LINE_SYS = G["htotal"] * AUD_PERIOD // SYS_PERIOD
FRAME_SYS = LINE_SYS * G["vtotal"]
AVG_LOG2 = 4
W = 64                    # ラインバッファの幅(htotal 52 → 56 画素を送る)
H = 64


def expect(x, y):
    """生成器(DigitalRgbGen)が HS から x ドット目、VS から y ライン目に出す色"""
    px, py = x - G["hstart"], y - G["vstart"]
    if not (0 <= px < G["hactive"] and 0 <= py < G["vactive"]):
        return 0
    if py < G["vactive"] // 2:
        bar_shift = max(0, (G["hactive"] // 16).bit_length() - 1)
        return (px >> bar_shift) & 0xF if px < (1 << bar_shift) * 16 else 0
    return 0xF if px & 1 else 0


class Pads(Module):
    def __init__(self):
        for n in ("r", "g", "b", "i", "hs", "vs"):
            setattr(self, n, Signal())


def _wire_gen(m, gen, pads):
    m.comb += [
        gen.neg_sync.eq(1),
        pads.r.eq(gen.r), pads.g.eq(gen.g), pads.b.eq(gen.b),
        pads.i.eq(gen.i), pads.hs.eq(gen.hs), pads.vs.eq(gen.vs),
    ]


# ---------------- 1. 取り込み単体 ----------------

class CapDUT(Module):
    def __init__(self, phase=16):
        self.submodules.gen = gen = ClockDomainsRenamer("aud")(DigitalRgbGen(**G))
        self.submodules.pads = pads = Pads()
        _wire_gen(self, gen, pads)
        # 極性は取り込みが自分で測る(窓は1フレームより長く)
        self.submodules.cap = cap = DigitalRgbCapture(
            pads, width=W, height=H, sys_clk_freq=1e6,
            meas_clks=FRAME_SYS * 2, frame_timeout=FRAME_SYS * 4,
            avg_log2=AVG_LOG2, pol_clks=FRAME_SYS + 7)
        self.comb += [cap.cfg_htotal.eq(G["htotal"]), cap.cfg_phase.eq(phase)]


def run_capture(phase=16, frames=5):
    dut = CapDUT(phase)
    lines = {}          # (frame, row) -> [色 × 56]
    res = {}
    n_words = (G["htotal"] + 7) // 8

    def reader():
        cap = dut.cap
        for _ in range(FRAME_SYS * frames):
            if (yield cap.line_valid):
                face = (yield cap.line_face)
                row = (yield cap.line_row)
                frame = (yield cap.line_frame)
                yield cap.line_ack.eq(1)
                yield
                yield cap.line_ack.eq(0)
                px = []
                for w in range(n_words):
                    yield cap.rd_face.eq(face)
                    yield cap.rd_word.eq(w)
                    yield
                    yield
                    d = (yield cap.rd_data) & 0xFFFFFFFF
                    px += [(d >> (4 * k)) & 0xF for k in range(8)]
                lines[(frame, row)] = px
            else:
                yield
        for k in ("meas_vtotal", "meas_vtotal_stable", "meas_frame_lines",
                  "meas_frame_clocks", "meas_hfreq", "out_vactive", "stat_hsum",
                  "stat_pol"):
            res[k] = (yield getattr(cap, k))

    run_simulation(dut, reader(), clocks={"sys": SYS_PERIOD, "aud": AUD_PERIOD})
    return lines, res


def check_lines(lines, label):
    """最後の完全なフレームの全行・全ドットを期待値と比べる"""
    frames = sorted({f for f, _ in lines})
    assert len(frames) >= 3, f"{label}: フレームが足りない {frames}"
    f = frames[-2]                      # 最後は途中かもしれない
    rows = sorted(r for ff, r in lines if ff == f)
    assert rows == [2 * y for y in range(G["vtotal"])], f"{label}: 行 {rows}"
    bad = []
    for y in range(G["vtotal"]):
        got = lines[(f, 2 * y)]
        # 8画素に満たない末尾(htotal 52 → 48〜51)は書かないので比べない
        for x in range(G["htotal"] // 8 * 8):
            if got[x] != expect(x, y):
                bad.append((y, x, got[x], expect(x, y)))
    assert not bad, f"{label}: {len(bad)} 画素が違う 例 {bad[:6]}"
    print(f"{label}: フレーム {f} の {G['vtotal']} 行 × {G['htotal'] // 8 * 8} ドットが一致")


# ---------------- 3. 送出部まで ----------------

BOARD_MAC = bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01])
SUBSCRIBER = (convert_ip("192.168.10.3"), proto.DEFAULT_PORT)


class _TvpPads:
    def __init__(self):
        self.r = Signal(8); self.g = Signal(8); self.b = Signal(8)
        self.hs = Signal(reset=1); self.vs = Signal(reset=1)


class _FakeUDPPort:
    def __init__(self, dw=32):
        self.sink = stream.Endpoint(eth_udp_user_description(dw))
        self.source = stream.Endpoint(eth_udp_user_description(dw))


class StreamDUT(Module):
    def __init__(self):
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.clock_domains.cd_aud = ClockDomain()
        self.submodules.gen = gen = ClockDomainsRenamer("aud")(DigitalRgbGen(**G))
        self.submodules.pads = pads = Pads()
        _wire_gen(self, gen, pads)
        self.port = _FakeUDPPort()
        # TVP 側は何も来ない(選ばれていない側)
        self.submodules.tvp = TvpCapture(_TvpPads(), width=W, height=H)
        self.submodules.drgb = DigitalRgbCapture(
            pads, C(0b11, 2), width=W, height=H, sys_clk_freq=1e6,
            meas_clks=FRAME_SYS * 2, frame_timeout=FRAME_SYS * 4,
            avg_log2=AVG_LOG2)
        self.submodules.sel = CaptureSel(self.tvp, self.drgb, C(1, 1))
        self.submodules.streamer = s = RetroCastXStreamer(
            self.port, 1_000_000, width=W, height=H, fps=200.0,
            announce_period=0.02, mode_period=0.01, sub_timeout=0.5,
            mtu_payload=1472, capture=self.sel, cfg_pll_divide=G["htotal"])
        self.comb += [
            self.drgb.cfg_htotal.eq(s.cfg_pll_divide),
            self.drgb.cfg_phase.eq(s.cfg_phase),
            s.video_src.eq(1),
        ]


def _send_datagram(ep, payload, ip, port):
    words = [int.from_bytes(payload[i:i + 4], "little")
             for i in range(0, len(payload), 4)]
    for i, w in enumerate(words):
        yield ep.data.eq(w); yield ep.ip_address.eq(ip)
        yield ep.src_port.eq(port); yield ep.last.eq(i == len(words) - 1)
        yield ep.valid.eq(1); yield
        while not ((yield ep.valid) and (yield ep.ready)):
            yield
    yield ep.valid.eq(0); yield ep.last.eq(0); yield


def run_stream(frames=5):
    dut = StreamDUT()
    grams = []
    n = FRAME_SYS * frames

    def subscriber():
        for _ in range(60):
            yield
        yield from _send_datagram(dut.port.source,
                                  proto.pack_subscribe(1, announce_only=False,
                                                       mac=BOARD_MAC), *SUBSCRIBER)

    def collector():
        words = []
        yield dut.port.sink.ready.eq(1)
        for _ in range(n):
            snk = dut.port.sink
            if (yield snk.valid) and (yield snk.ready):
                words.append((yield snk.data))
                if (yield snk.last):
                    grams.append(b"".join(w.to_bytes(4, "little") for w in words))
                    words = []
            yield

    run_simulation(dut, [subscriber(), collector()],
                   clocks={"sys": SYS_PERIOD, "aud": AUD_PERIOD, "pix": SYS_PERIOD})
    return grams


def check_stream(grams):
    sent_px = (G["htotal"] + 7) // 8 * 8
    lines = {}
    modes = []
    for g in grams:
        ptype, pkt = proto.parse(g)
        if ptype == proto.TYPE_LINE:
            assert pkt.pixfmt == proto.PIXFMT_RGBI4, f"LINE の pixfmt {pkt.pixfmt}"
            assert pkt.offset_px == 0 and pkt.count_px == sent_px, \
                f"範囲 {pkt.offset_px}+{pkt.count_px}(期待 0+{sent_px})"
            px = [(b >> s) & 0xF for b in pkt.pixels for s in (0, 4)]
            lines[(pkt.frame, pkt.line)] = px
        elif ptype == proto.TYPE_MODE:
            modes.append(pkt)
    assert lines, "LINE が1つも来ない"
    assert modes and modes[-1].pixfmt == proto.PIXFMT_RGBI4, "MODE の pixfmt が 6 でない"
    frames = sorted({f for f, _ in lines})
    f = frames[-2]
    bad = []
    for y in range(G["vtotal"]):
        got = lines.get((f, 2 * y))
        assert got is not None, f"送出: フレーム {f} の行 {2 * y} が無い"
        for x in range(G["htotal"] // 8 * 8):
            if got[x] != expect(x, y):
                bad.append((y, x, got[x], expect(x, y)))
    assert not bad, f"送出: {len(bad)} 画素が違う 例 {bad[:6]}"
    m = modes[-1]
    print(f"送出: LINE pixfmt 6、{sent_px} 画素 = {sent_px // 2} バイトを1パケットで、"
          f"フレーム {f} の全行が一致。MODE htotal {m.htotal} vactive {m.vactive}")


def main():
    print(f"サンプル/ドット {AUD_PERIOD / SYS_PERIOD:.2f}(分数)、"
          f"htotal {G['htotal']}(8の倍数でない)")
    lines, res = run_capture()
    check_lines(lines, "取り込み(位相16)")
    assert res["stat_pol"] == 0b11, f"極性 {res['stat_pol']:#x}(期待 0x3 = HS/VS とも負極性)"
    assert res["meas_vtotal_stable"] == G["vtotal"], res
    assert res["meas_frame_lines"] == 2 * G["vtotal"], res
    fc = res["meas_frame_clocks"]
    exact = 2 * G["vtotal"] * G["htotal"] * AUD_PERIOD / SYS_PERIOD
    assert abs(fc - exact) <= 2, f"2フレームのクロック数 {fc}(期待 {exact:.1f})"
    assert res["out_vactive"] == 2 * G["vtotal"], res
    hs_exp = round(G["htotal"] * AUD_PERIOD / SYS_PERIOD * (1 << AVG_LOG2))
    assert abs(res["stat_hsum"] - hs_exp) <= (1 << AVG_LOG2), (res["stat_hsum"], hs_exp)
    print(f"測定: vtotal {res['meas_vtotal_stable']} / 2フレーム {fc} クロック "
          f"{res['meas_frame_lines']} ライン / ライン長の合計 {res['stat_hsum']}(期待 {hs_exp})")

    check_stream(run_stream())
    print("\n[OK] デジタルRGB の取り込み: ドット位置 / 行 / 測定 / RGBI4 の送出 を確認")


if __name__ == "__main__":
    main()
