#!/usr/bin/env python3
"""TvpCapture の座標復元 + ラインバッファ + CDC を検証(実機不要)。

cd_pix を sys に別名接続(=同一クロックでロジック検証)。小フレーム
(width=8,height=4,nface=4, hs_offset=0,vs_offset=2)を pad レベルで駆動し:
- アクティブ4ラインが face 0..3 に (xpix,row_eff) 符号化どおり格納されるか
- メタFIFO が {face,row,frame} を順に返すか
を確認する。RGB符号化: r=xpix<<3, g=row<<3, b=0 → pix555 = xpix<<10 | row<<5。
"""
import sys, os
sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_capture import TvpCapture

W, H, NF = 8, 4, 8
VOFF = 2                     # 先頭2行は blanking(row_eff<0)


class _Pads:
    def __init__(self):
        self.r = Signal(8); self.g = Signal(8); self.b = Signal(8)
        self.hs = Signal(reset=1); self.vs = Signal(reset=1)


class Wrap(Module):
    def __init__(self):
        self.pads = _Pads()
        # sys/pix を実クロックとして定義(simが両方を駆動; comb接続だと刻まれない)
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.submodules.cap = TvpCapture(
            self.pads, width=W, height=H, nface=NF,
            hs_active_low=True, vs_active_low=True,
            hs_offset=0, vs_offset=VOFF)


def expect_pix(x, row):
    return ((x & 0x1F) << 10) | ((row & 0x1F) << 5)


def main():
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    res = {"meta": [], "faces": {}}

    def tb():
        # 初期アイドル
        for _ in range(5):
            yield
        # VSYNC(active-low): 1cyc low
        yield p.vs.eq(0); yield
        yield p.vs.eq(1)
        for _ in range(4):      # 2FF伝播 + row リセット待ち
            yield
        # row は hs_edge で先にインクリメントされるので、最初のアクティブ行は
        # L=VOFF-1(その区間で module row=VOFF → row_eff=0)。
        first = VOFF - 1
        total_lines = first + H + 1    # +1: 最終アクティブ行を flush する hs
        for L in range(total_lines):
            # HSYNC(active-low): 1cyc low
            yield p.hs.eq(0); yield
            yield p.hs.eq(1); yield
            yield                       # 2FF伝播 → この後 x=0
            row_eff = L - first
            if 0 <= row_eff < H:
                for xp in range(W):
                    yield p.r.eq((xp << 3) & 0xFF)
                    yield p.g.eq((row_eff << 3) & 0xFF)
                    yield p.b.eq(0)
                    yield
                yield p.r.eq(0); yield p.g.eq(0)
                for _ in range(3):      # front porch
                    yield
            else:
                for _ in range(W + 4):  # blanking 行(書き込み無し)
                    yield

        # --- sys 側: メタを pop しつつ各 face を読み出し ---
        for _ in range(4):
            yield
        for _ in range(H):
            assert (yield cap.line_valid) == 1, f"line_valid=0 @line{_}"
            face = (yield cap.line_face)
            row  = (yield cap.line_row)
            frm  = (yield cap.line_frame)
            yield cap.rd_face.eq(face)   # 読み出す面を固定
            # face 読み出し(1cyc latency)
            words = []
            for e in range(W // 2):
                yield cap.rd_word.eq(e)
                yield
                yield              # latency
                words.append((yield cap.rd_data))
            res["meta"].append((face, row, frm))
            res["faces"][row] = words
            # pop
            yield cap.line_ack.eq(1); yield
            yield cap.line_ack.eq(0); yield
        res["drops"] = (yield cap.cap_drops)

    run_simulation(dut, tb(), clocks={"sys": 10, "pix": 10}, vcd_name=None)

    print("meta (face,row,frame):", res["meta"])
    print("drops:", res["drops"])
    for row in sorted(res["faces"]):
        words = res["faces"][row]
        decoded = []
        for e, w in enumerate(words):
            lo, hi = w & 0xFFFF, (w >> 16) & 0xFFFF
            decoded.append((lo, hi))
        print(f"  row{row}: " + " ".join(f"{w:08X}" for w in words))

    # 検証
    rows_seen = [m[1] for m in res["meta"]]
    assert rows_seen == list(range(H)), f"row順不一致: {rows_seen}"
    assert all(m[2] == 1 for m in res["meta"]), f"frame!=1: {res['meta']}"
    assert res["drops"] == 0, f"drops={res['drops']}"
    for row in range(H):
        words = res["faces"][row]
        for e in range(W // 2):
            lo = words[e] & 0xFFFF
            hi = (words[e] >> 16) & 0xFFFF
            exp_lo = expect_pix(2 * e, row)
            exp_hi = expect_pix(2 * e + 1, row)
            assert lo == exp_lo, f"row{row} e{e} lo={lo:#06x} exp={exp_lo:#06x}"
            assert hi == exp_hi, f"row{row} e{e} hi={hi:#06x} exp={exp_hi:#06x}"

    print("\n[OK] TvpCapture: 4アクティブ行を座標どおり face 0..3 に格納 + "
          "メタCDC(row 0..3, frame 1)を確認")


if __name__ == "__main__":
    main()
