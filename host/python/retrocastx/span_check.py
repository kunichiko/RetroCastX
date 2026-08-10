#!/usr/bin/env python3
"""ボードが報告する「非黒範囲」が、その行の実際の内容と一致しているか測る。

やり方は実機でのA/B。key 0x30(全ライン送信)を切り替えて2回取り、

  B: 1ラインまるごと送らせる → 受けた画素から「本来の範囲」を計算する
  A: 通常モード              → ボードが報告した範囲をそのまま読む

を行ごとに突き合わせる。範囲判定だけを見る道具で、画素そのものの正しさは見ない。

注意(この道具で何度も踏んだ落とし穴):

  - 範囲の算術。offset_px と count_px[画素] から entry(=2画素)へ直すとき、
    末尾は (offset + count - 1)//2 である。ここを間違えると全行が不一致に見える
  - A と B は別の時刻のキャプチャなので、画面が動いていると比較にならない。
    同条件を2回取った自己一致率を先に出して、ばらつきの下限を示す
  - ボードの購読先は1つ。Viewer が開いていると奪い合いになるので別ポートを使う

使い方:
    python3 -m retrocastx.span_check --board 192.168.10.50
"""
import argparse
import collections
import socket
import time

import numpy as np

from . import protocol as proto

# 黒判定のしきい値(RGB555の各成分 0..31)。gateware の cfg_black_th の既定値と
# 合わせる。ここがずれると「本来の範囲」の計算がボードと食い違う
BLACK_TH = 2


def entries_from_report(offset_px: int, count_px: int):
    """報告された (offset_px, count_px) を entry 範囲へ直す。count_px=0 なら None。

    entry は 2画素で1つ。範囲は [offset, offset+count) なので、
    末尾の画素は offset+count-1、その entry は (offset+count-1)//2。
    """
    if count_px <= 0:
        return None
    return (offset_px // 2, (offset_px + count_px - 1) // 2)


def entries_from_pixels(line: np.ndarray):
    """1ラインの画素(RGB555)から非黒範囲を entry 単位で求める。全黒なら None。"""
    nz = (((line & 0x1F) > BLACK_TH)
          | (((line >> 5) & 0x1F) > BLACK_TH)
          | (((line >> 10) & 0x1F) > BLACK_TH))
    # entry は隣り合う2画素のどちらかが非黒なら非黒
    ent = nz[0::2] | nz[1::2]
    idx = np.flatnonzero(ent)
    if not len(idx):
        return None
    return (int(idx[0]), int(idx[-1]))


def _self_test():
    """算術の検算。ここが通らないと以降の数字は全部信用できない。"""
    assert entries_from_report(258, 8) == (129, 132), entries_from_report(258, 8)
    assert entries_from_report(258, 16) == (129, 136)
    assert entries_from_report(0, 0) is None
    assert entries_from_report(0, 2) == (0, 0)
    L = np.zeros(64, dtype=np.uint16)
    assert entries_from_pixels(L) is None
    L[10] = 3 << 5          # 緑 3 > BLACK_TH
    L[21] = 3 << 5
    assert entries_from_pixels(L) == (5, 10), entries_from_pixels(L)
    L2 = np.zeros(64, dtype=np.uint16)
    L2[4] = BLACK_TH << 5   # しきい値ちょうどは黒扱い
    assert entries_from_pixels(L2) is None


class Board:
    def __init__(self, ip, port, local_port):
        self.dst = (ip, port)
        self.seq = 0
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 32 << 20)
        self.sock.bind(("0.0.0.0", local_port))
        self.sock.settimeout(0.2)

    def _send(self, payload):
        self.sock.sendto(payload, self.dst)
        self.seq = (self.seq + 1) & 0xFFFF

    def set_cfg(self, key, value):
        self._send(proto.pack_config(self.seq, proto.CFG_TARGET_BOARD,
                                     proto.CFG_OP_SET, key, value))

    def grab(self, seconds, width=2400):
        """frame → slot → (report, 画素ライン) を集める。"""
        out = collections.defaultdict(dict)
        mode = None
        last = 0.0
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if time.monotonic() - last >= 1.5:
                self._send(proto.pack_subscribe(self.seq))
                last = time.monotonic()
            try:
                data, _ = self.sock.recvfrom(65535)
            except socket.timeout:
                continue
            try:
                ptype, pkt = proto.parse(data)
            except ValueError:
                continue
            if ptype == proto.TYPE_MODE:
                mode = pkt
            elif ptype == proto.TYPE_LINE:
                cur = out[pkt.frame].get(pkt.line)
                if cur is None:
                    cur = [None, 0, np.zeros(width, dtype=np.uint16)]
                    out[pkt.frame][pkt.line] = cur
                cur[0] = pkt.offset_px if cur[0] is None else min(cur[0], pkt.offset_px)
                cur[1] += pkt.count_px
                px = np.frombuffer(pkt.pixels, dtype="<u2")
                n = min(len(px), width - pkt.offset_px)
                if n > 0:
                    cur[2][pkt.offset_px:pkt.offset_px + n] = px[:n]
        return mode, out


def mid_frame(frames):
    ks = sorted(frames)
    return ks[len(ks) // 2] if ks else None


def compare(label, ref, got, max_show=10):
    slots = sorted(set(ref) & set(got))
    if not slots:
        print(f"  {label}: 比較できる行がない")
        return None
    bad = [(y, ref[y], got[y]) for y in slots if ref[y] != got[y]]
    print(f"  {label}: 一致 {len(slots)-len(bad)}/{len(slots)} = "
          f"{(len(slots)-len(bad))/len(slots)*100:5.1f}%")
    for y, e, g in bad[:max_show]:
        print(f"      slot {y}: 本来 {e} → 報告 {g}")
    return bad


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", required=True)
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--local-port", type=int, default=34699,
                    help="受信に使う自分側のポート(Viewerと競合しない値)")
    ap.add_argument("--seconds", type=float, default=3.0)
    args = ap.parse_args()

    _self_test()
    print("算術の自己検算: OK")
    b = Board(args.board, args.port, args.local_port)

    # まず同条件を2回。ここが低いと以降の比較に意味がない
    b.set_cfg(proto.CFG_KEY_FULL_LINE, 0)
    time.sleep(1.2)
    _, a1 = b.grab(args.seconds)
    _, a2 = b.grab(args.seconds)
    f1, f2 = mid_frame(a1), mid_frame(a2)
    if f1 is None or f2 is None:
        print("LINEが来ない(購読/配線を確認)")
        return
    r1 = {y: entries_from_report(v[0], v[1]) for y, v in a1[f1].items()}
    r2 = {y: entries_from_report(v[0], v[1]) for y, v in a2[f2].items()}
    print("\n対照実験(同条件を2回)= 測定のばらつきの下限")
    compare("通常モードどうし", r1, r2, max_show=3)

    # B: 全ライン送信で「本来の範囲」を得る
    b.set_cfg(proto.CFG_KEY_FULL_LINE, 1)
    time.sleep(1.5)
    mode, bb = b.grab(args.seconds)
    fb = mid_frame(bb)
    # A: 通常モードの報告
    b.set_cfg(proto.CFG_KEY_FULL_LINE, 0)
    time.sleep(1.5)
    _, aa = b.grab(args.seconds)
    fa = mid_frame(aa)
    b.set_cfg(proto.CFG_KEY_FULL_LINE, 0)
    if fb is None or fa is None:
        print("A/Bのどちらかが取れなかった")
        return
    if mode is not None:
        print(f"\nMODE {mode.hactive}x{mode.vactive} htotal={mode.htotal} "
              f"fH={mode.hfreq_mhz_x1000/1e6:.3f}kHz")
    ref = {y: entries_from_pixels(v[2]) for y, v in bb[fb].items()}
    rep = {y: entries_from_report(v[0], v[1]) for y, v in aa[fa].items()}
    print("\n本題: 全ライン送信から計算した本来の範囲 vs 通常モードの報告")
    bad = compare("ずらし 0ライン", ref, rep)
    # ずらして一致するなら、範囲と画素が縦にずれていることになる
    slots = sorted(set(ref) & set(rep))
    if bad:
        print("\n  縦にずらして一致するか(ずれ方の系統性を見る)")
        # 遅れはFIFOの滞留段数ぶんになり得るので、広めに探す。狭いと最良点を
        # 見落として「系統性なし」と誤読する(実際 ±8 までで一度見落とした)。
        best = []
        for k in range(-8, 65, 2):
            n = ok = 0
            for y in slots:
                if (y - k) in ref:
                    n += 1
                    ok += (rep[y] == ref[y - k])
            if n:
                best.append((ok / n, k, ok, n))
        best.sort(reverse=True)
        for r, k, ok, n in best[:5]:
            print(f"    {k:+3d}スロット({k//2:+d}ライン): "
                  f"{ok}/{n} = {r*100:5.1f}%")


if __name__ == "__main__":
    main()
