#!/usr/bin/env python3
"""送出される「非黒範囲」が、その行の内容と一致しているかを検証する(実機不要)。

実機で出た現象:

  全黒の行が3行続いた直後の数行が、内容があるのに count_px=0(空)で送られる。
  Viewerでは「行が消える」「行がずれる」形で見える。

実機で切り分けた事実(key 0x30 = 1ラインまるごと送るモードとのA/B):

  - 画素データはバッファに正しく入っている(全ライン送信にすると内容が出る)
  - 壊れているのは非黒範囲の判定(ln_first/ln_last)だけ
  - 報告された範囲は、実際の内容の範囲と約30%の行で食い違う
  - 食い違いには「4ライン前の内容の範囲と一致する」という系統的な成分がある
    (4 = メタFIFOの段数 fifo_depth)
  - 測定の再現性は99.2〜99.4%あるので、この食い違いはばらつきではない

ここで再現させる条件(実機の画面から抜き出したもの):

  - 全黒の行を3行以上つづけて挟む
  - 内容の右端が行ごとに変わる(">"のような斜線)。右端が動かないと範囲が
    別の行のものとすり替わっても値が同じになり、検出できない
  - 送信側に背圧をかけてメタFIFOを溜める

検査はひとつだけ: 各行について「報告された範囲」と「駆動した内容から計算した
本来の範囲」が一致すること。実機でやった測定と同じ形にしてある。

実行: .venv/bin/python sim_capture_black.py
"""
import os
import random
import sys

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "host", "python"))

from migen import *                                        # noqa: E402
from litex.soc.interconnect import stream                  # noqa: E402
from liteeth.common import eth_udp_user_description        # noqa: E402

from retrocastx import protocol as proto                   # noqa: E402
from retrocastx_capture import TvpCapture                  # noqa: E402
from retrocastx_stream import RetroCastXStreamer, convert_ip  # noqa: E402

SYS = 1_000_000
W, NL, NF, VOFF = 64, 16, 8, 2
H = 2 * NL                       # 行位置は半ラインスロット(2のべき乗であること)
BOARD_MAC = bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01])
SUBSCRIBER = (convert_ip("192.168.10.3"), proto.DEFAULT_PORT)

# 各行の非黒範囲 [lo, hi)。None は全黒の行。
#
# 実機の画面(文字セル16ライン、"U>" の斜線、セル間に全黒3行)を写している。
# 右端が1行ずつ動くのが要点。
ROW_SPAN = [
    (10, 22),   # 0
    (10, 23),   # 1
    (10, 24),   # 2
    (10, 25),   # 3
    (10, 26),   # 4
    None,       # 5  全黒
    None,       # 6  全黒
    None,       # 7  全黒
    (10, 22),   # 8  ← 実機ではこの直後の3行が壊れた
    (10, 23),   # 9
    (10, 24),   # 10
    (10, 25),   # 11
    (10, 26),   # 12
    None,       # 13 全黒
    None,       # 14 全黒
    (10, 22),   # 15
]
assert len(ROW_SPAN) == NL


def want_pix(x, row):
    """行 row の位置 x にあるべき RGB555。範囲外/全黒行は 0(黒)。"""
    span = ROW_SPAN[row]
    if span is None or not (span[0] <= x < span[1]):
        return 0
    # 緑を黒判定しきい値(既定2)より必ず大きくする
    return ((x + 1) << 10) | ((row + 3) << 5)


def drive_rgb(x, row):
    v = want_pix(x, row)
    return ((v >> 10) & 0x1F) << 3, ((v >> 5) & 0x1F) << 3, (v & 0x1F) << 3


# 送出範囲の整列単位(画素)。retrocastx_stream の PX_ALIGN と一致させること。
# RGB888 が4画素=3語なので4。RGB555 でも4の倍数は2の倍数を満たすので共用できる。
PX_ALIGN = 4


def want_entries(row):
    """その行の非黒範囲を entry(=x/2)単位で返す。全黒なら None。

    ★ゲートウェアは範囲の両端を PX_ALIGN に**外側へ**丸める(lo は切り下げ、
      hi は切り上げ、幅でクランプ)。ワード整列のために必要な処理で、
      増えるのは端の黒画素だけなので内容は欠けない。
      期待値も同じ丸めをしないと、この試験が「範囲の取り違え」ではなく
      整列そのものを不一致として報告してしまう。
      この試験が見たいのはメタFIFOによる**別の行の範囲とのすり替わり**の方。
    """
    xs = [x for x in range(W) if want_pix(x, row) != 0]
    if not xs:
        return None
    lo = min(xs) & ~(PX_ALIGN - 1)
    hi = min(W, (max(xs) + PX_ALIGN) & ~(PX_ALIGN - 1))   # 上端は排他
    return (lo // 2, (hi - 1) // 2)


class _Pads:
    def __init__(self):
        self.r = Signal(8); self.g = Signal(8); self.b = Signal(8)
        self.hs = Signal(reset=1); self.vs = Signal(reset=1)


class _FakeUDPPort:
    def __init__(self, dw=32):
        self.sink = stream.Endpoint(eth_udp_user_description(dw))
        self.source = stream.Endpoint(eth_udp_user_description(dw))


class DUT(Module):
    def __init__(self, gap, hs_off):
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.pads = _Pads()
        self.port = _FakeUDPPort()
        # 実機は hs_offset=0 だが、RGBは1段・HSOUTは2段のレジスタで受けるため
        # 駆動側が置いた画素と取り込みの x には固定で1画素のずれがある。ここは
        # 範囲の一致だけを見たいので hs_offset=1 でそれを打ち消す
        # (実測: 0だと全画素が1ずれる。範囲の不整合の有無は hs_offset に依らない)。
        self.submodules.cap = TvpCapture(self.pads, width=W, height=H, nface=NF,
                                         hs_offset=hs_off, vs_offset=VOFF)
        self.submodules.streamer = RetroCastXStreamer(
            self.port, SYS, width=W, height=H, fps=200.0,
            announce_period=0.02, mode_period=0.01, sub_timeout=0.05,
            mtu_payload=1472, capture=self.cap)
        self.gap = gap


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


def subscriber(dut):
    for _ in range(60):
        yield
    yield from _send_datagram(dut.port.source,
                              proto.pack_subscribe(1, announce_only=False,
                                                   mac=BOARD_MAC), *SUBSCRIBER)


def tvp_driver(dut, nframes):
    p = dut.pads
    for _ in range(120):
        yield
    for _f in range(nframes):
        yield p.vs.eq(0); yield
        yield p.vs.eq(1)
        for _ in range(4):
            yield
        first = VOFF - 1
        for L in range(first + NL + 1):
            yield p.hs.eq(0); yield
            yield p.hs.eq(1); yield
            yield
            row = L - first
            if 0 <= row < NL:
                for xp in range(W):
                    r8, g8, b8 = drive_rgb(xp, row)
                    yield p.r.eq(r8); yield p.g.eq(g8); yield p.b.eq(b8)
                    yield
                yield p.r.eq(0); yield p.g.eq(0); yield p.b.eq(0)
                for _ in range(dut.gap):
                    yield
            else:
                for _ in range(W + 8):
                    yield
    for _ in range(800):
        yield


def collector(dut, out, n_cycles, ready_p):
    """UDP側。ready_p<1 で背圧をかけ、メタFIFOを溜める。

    背圧が無いと送信が常に間に合ってFIFOが空のままになり、実機で起きている
    「FIFOに次が溜まった状態」を作れない。
    """
    rnd = random.Random(7)
    cycle = 0
    words = []
    while cycle < n_cycles:
        snk = dut.port.sink
        if (yield snk.valid) and (yield snk.ready):
            words.append((yield snk.data))
            if (yield snk.last):
                out.append(b"".join(w.to_bytes(4, "little") for w in words))
                words = []
        yield snk.ready.eq(int(rnd.random() < ready_p))
        yield
        cycle += 1


def run(gap, hs_off=1, nframes=4, n_cycles=80_000, ready_p=1.0):
    dut = DUT(gap, hs_off)
    grams = []
    run_simulation(dut,
                   [subscriber(dut), tvp_driver(dut, nframes),
                    collector(dut, grams, n_cycles, ready_p)],
                   clocks={"sys": 10, "pix": 10})
    lines, broken = [], []
    for payload in grams:
        try:
            ptype, pkt = proto.parse(payload)
        except ValueError as e:
            hdr = payload[proto.COMMON_SIZE:proto.COMMON_SIZE + proto.LINE_HDR_SIZE]
            line = int.from_bytes(hdr[0:2], "little")
            off = int.from_bytes(hdr[2:4], "little")
            cnt = int.from_bytes(hdr[4:6], "little")
            got = (len(payload) - proto.COMMON_SIZE - proto.LINE_HDR_SIZE) // 2
            broken.append((str(e), line, off, cnt, got))
            continue
        if ptype == proto.TYPE_LINE:
            lines.append(pkt)
    return lines, broken


def check(res, gap, rp):
    """報告された範囲が、その行の内容の範囲と一致するかだけを見る。"""
    lines, broken = res
    assert lines or broken, "LINEパケットが1つも出ていない"
    agg = {}
    for p in lines:
        k = (p.frame, p.line)
        lo, tot = agg.get(k, (None, 0))
        lo = p.offset_px if lo is None else min(lo, p.offset_px)
        agg[k] = (lo, tot + p.count_px)

    mismatch = []
    checked = 0
    for (frame, slot), (lo, tot) in sorted(agg.items()):
        row = slot // 2
        if row >= NL:
            continue
        want = want_entries(row)
        got = None if tot == 0 else (lo // 2, (lo + tot - 2) // 2)
        checked += 1
        if got != want:
            mismatch.append((frame, slot, row, want, got))

    print(f"gap={gap:2d} ready={rp:.2f}  検査 {checked:4d}行  "
          f"範囲の不一致 {len(mismatch):4d}件  長さ不整合 {len(broken):3d}件")
    for frame, slot, row, want, got in mismatch[:10]:
        print(f"    frame={frame} slot={slot} (row {row}): "
              f"本来 {want} → 報告 {got}")
    for e, line, off, cnt, got_px in broken[:4]:
        print(f"    長さ不整合 slot={line} off={off} count_px={cnt} 実画素={got_px}")
    return mismatch, broken


def main():
    print("非黒範囲が内容と一致するかを見る。行の並び(None=全黒):")
    for i, s in enumerate(ROW_SPAN):
        print("  row %2d: %s  → entry %s"
              % (i, "全黒" if s is None else "非黒 [%2d,%2d)" % s,
                 want_entries(i)))
    print()
    bad = False
    # 背圧を強くするほど送信が詰まり、メタFIFOに次のラインが溜まる
    for gap, rp in ((16, 1.0), (8, 0.7), (4, 0.5), (2, 0.35)):
        mismatch, broken = check(run(gap, ready_p=rp), gap, rp)
        if mismatch or broken:
            bad = True
    print()
    if bad:
        print("[NG] 範囲が内容と一致しない行がある(実機の現象を再現)")
        sys.exit(1)
    print("[OK] どの条件でも範囲は内容と一致")


if __name__ == "__main__":
    main()
