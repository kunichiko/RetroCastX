#!/usr/bin/env python3
"""流れているLINEパケットをそのまま見て、行が欠ける原因を切り分ける。

実機で「特定の行だけ内容が横にずれる/行が欠ける」現象を追うための道具。
絵を組み立てる前の生のパケットを見るので、ボードが送っていないのか、空で
送っているのか、送っているのに受信側で落ちているのかを区別できる。

見るもの:

  スロットの網羅   そのフレームで届かなかった行(=ボードが送っていない)
  count_px == 0    中身が空のまま送られた行(=範囲の計算が壊れている)
  offset_px        行ごとの送出開始位置。隣の行と大きく違えば範囲が動いている
  ts の差          ライン先頭のDATACLK自走カウンタ。隣り合う行の差は本来ちょうど
                   htotal になる。ここが動いていれば HSOUT が DATACLK に対して
                   動いており、動いていないのに絵がずれるなら DATACLK ごと
                   アナログ入力に対して動いている(PLL/電源側)

注意: ボードの購読先は1つなので、これを動かすと Viewer への配信が止まる。
Viewer は閉じてから使うこと。

使い方:
    python3 -m retrocastx.line_probe --board 192.168.10.50
    python3 -m retrocastx.line_probe --board 192.168.10.50 --rows 100,102,104
"""
import argparse
import collections
import socket
import time

from . import netutil
from . import protocol as proto


def collect(sock, dsts, port, seconds, seq0=0):
    """LINEパケットを frame ごとにまとめて返す。"""
    seq = seq0
    by_frame = collections.defaultdict(list)
    mode = None
    last_sub = 0.0
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        now = time.monotonic()
        if now - last_sub >= 2.0:
            netutil.send_all(sock, dsts, port, proto.pack_subscribe(seq))
            seq = (seq + 1) & 0xFFFF
            last_sub = now
        try:
            data, _ = sock.recvfrom(65535)
        except socket.timeout:
            continue
        try:
            ptype, pkt = proto.parse(data)
        except ValueError as e:
            print(f"  !! パースできないパケット: {e} ({len(data)}バイト)")
            continue
        if ptype == proto.TYPE_MODE:
            mode = pkt
        elif ptype == proto.TYPE_LINE:
            by_frame[pkt.frame].append(pkt)
    return mode, by_frame


def report_frame(frame, pkts, mode, want_rows):
    """1フレーム分のLINEパケットを行ごとに整理して出す。"""
    by_row = collections.defaultdict(list)
    for p in pkts:
        by_row[p.line].append(p)
    rows = sorted(by_row)
    print(f"\n=== frame {frame}: {len(pkts)}パケット / {len(rows)}行 ===")
    if not rows:
        return
    print(f"スロット範囲 {rows[0]}..{rows[-1]}")

    # 届いた行の間隔。プログレッシブは1つ飛び(0,2,4,...)になるはず
    step = collections.Counter(b - a for a, b in zip(rows, rows[1:]))
    print("行間隔:", dict(step.most_common(5)))
    common = step.most_common(1)[0][0] if step else 2
    gaps = [(a, b) for a, b in zip(rows, rows[1:]) if b - a != common]
    if gaps:
        print(f"  ★ 間隔が {common} でない箇所 {len(gaps)}件(=行が届いていない):")
        for a, b in gaps[:12]:
            print(f"      スロット {a} の次が {b}(間に {(b-a)//common - 1}行ぶんの欠け)")

    # 中身が空のまま送られた行
    empty = [r for r in rows if sum(p.count_px for p in by_row[r]) == 0]
    if empty:
        print(f"  ★ count_px==0 の行 {len(empty)}件: {empty[:16]}")

    # offset_px が隣とかけ離れている行(範囲が動いている兆候)
    offs = {r: min(p.offset_px for p in by_row[r]) for r in rows}
    cnts = {r: sum(p.count_px for p in by_row[r]) for r in rows}
    jumps = []
    for a, b in zip(rows, rows[1:]):
        if abs(offs[b] - offs[a]) > 64:
            jumps.append((a, offs[a], cnts[a], b, offs[b], cnts[b]))
    if jumps:
        print(f"  offset_px が隣と64px以上違う箇所 {len(jumps)}件(先頭8件):")
        for a, oa, ca, b, ob, cb in jumps[:8]:
            print(f"      {a}: off={oa} cnt={ca}  →  {b}: off={ob} cnt={cb}")

    # ts の差。
    #
    # ts は「ライン先頭 + offset_px」であって、ライン先頭の時刻ではない
    # (プロトコルの定義。断片ごとに正しいサンプル時刻を持たせるため)。
    # だから隣接行の ts 差は htotal ではなく htotal + (offsetの差)になる。
    # ここを htotal と比べると、offset が動いた行が全部「異常」に見えて
    # しまう(実際それで ±258 の偽陽性を出した)。offset を引いてから比べる。
    ts = {r: min(p.timestamp for p in by_row[r]) for r in rows}
    d = collections.Counter()
    odd = []
    for a, b in zip(rows, rows[1:]):
        if b - a != common:
            continue
        delta = ((ts[b] - offs[b]) - (ts[a] - offs[a])) & 0xFFFFFFFF
        d[delta] += 1
        odd.append((a, b, delta))
    print("ライン先頭の ts 差(offsetを引いた値):", dict(d.most_common(6)))
    if mode is not None:
        ht = mode.htotal
        bad = [(a, b, x) for a, b, x in odd if x != ht]
        if bad:
            # ここが動いていれば HSOUT が DATACLK に対して動いている。
            # 動いていないのに絵がずれるなら DATACLK ごとアナログ入力に対して
            # 動いている(PLL/電源側)ことになり、切り分けの要になる。
            print(f"  ★ ライン先頭の間隔が htotal({ht}) でない箇所 {len(bad)}件:")
            for a, b, x in bad[:8]:
                print(f"      スロット {a}→{b}: {x}(差 {x - ht:+d})")
        else:
            print(f"  ライン先頭の間隔は全行 htotal({ht}) と一致"
                  f" → HSOUTとDATACLKの関係は安定している")

    for r in want_rows:
        if r in by_row:
            ps = sorted(by_row[r], key=lambda p: p.offset_px)
            print(f"  行 {r}: " + ", ".join(
                f"off={p.offset_px} cnt={p.count_px} ts={p.timestamp}" for p in ps))
        else:
            print(f"  行 {r}: 届いていない")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    netutil.add_args(ap)
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--seconds", type=float, default=3.0)
    ap.add_argument("--frames", type=int, default=2, help="詳しく出すフレーム数")
    ap.add_argument("--rows", default="",
                    help="個別に中身を出すスロット番号(カンマ区切り)")
    args = ap.parse_args()

    want_rows = [int(x) for x in args.rows.split(",") if x.strip()]
    sock = netutil.prep_socket(socket.socket(socket.AF_INET, socket.SOCK_DGRAM))
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 16 << 20)
    sock.bind((args.bind, args.port))
    sock.settimeout(0.2)

    print(f"ボード {args.board}:{args.port} を {args.seconds}秒ぶん観測します")
    print("(Viewer が開いていると購読先が奪い合いになります。閉じてください)")
    dsts = netutil.targets_for(args.board, args.bind)
    mode, by_frame = collect(sock, dsts, args.port, args.seconds)
    if mode is not None:
        print(f"\nMODE {mode.hactive}x{mode.vactive} htotal={mode.htotal} "
              f"vtotal={mode.vtotal} fH={mode.hfreq_mhz_x1000/1e6:.3f}kHz "
              f"mflags={mode.mflags:#x}")
    else:
        print("\nMODE 未受信")
    if not by_frame:
        print("LINEパケットが1つも来ません(購読/配線を確認)")
        return
    # 端のフレームは取りこぼしがあるので、真ん中あたりを見る
    frames = sorted(by_frame)
    print(f"受信フレーム {len(frames)}個: {frames[0]}..{frames[-1]}")
    mid = frames[len(frames) // 2:]
    for f in mid[:args.frames]:
        report_frame(f, by_frame[f], mode, want_rows)


if __name__ == "__main__":
    main()
