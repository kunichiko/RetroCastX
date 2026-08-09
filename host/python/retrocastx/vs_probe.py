#!/usr/bin/env python3
"""VSYNCの水平位相を実測して、インターレースの信号形を確定させる。

知りたいこと: 24kHz 1024x848 のようなモードで、VSYNCは1フレームに1回なのか、
フィールドごとに(半ラインずれて)2回来るのか。

これが分かると実装が変わる。フィールドごとに来るなら、行位置を「VSYNCから
何半ライン後か」で決めるだけで織り込みが自動的に成立し、il/f2_row/swap/
field_src という設定が全部不要になる。1回しか来ないなら、その方式のために
別の扱いが必要になる。

判定の材料:
  vs_x  VSYNCエッジ時の行内カウンタ(=水平位相)。ゲートで捨てられたエッジも
        含めて生のエッジで更新されるので、2回来ているなら 0付近と htotal/2 付近を
        交互に取る。1回しか来ないなら1つの値に落ち着く。
  vtotal MODEが報告する値。VSYNC間のライン数。

使い方:
    python3 -m retrocastx.vs_probe --board 192.168.10.50
"""
import argparse
import socket
import time
from collections import Counter

from . import protocol as proto
from .receiver import FrameAssembler

KEY_VS_X = 0x0020
KEY_FID = 0x0021


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", required=True)
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--samples", type=int, default=400)
    args = ap.parse_args()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
    sock.bind(("0.0.0.0", args.port))
    sock.settimeout(0.2)
    asm = FrameAssembler()
    seq = 0
    dst = (args.board, args.port)

    def sub():
        nonlocal seq
        sock.sendto(proto.pack_subscribe(seq), dst)
        seq = (seq + 1) & 0xFFFF

    def get(key):
        nonlocal seq
        sock.sendto(proto.pack_config(seq, proto.CFG_TARGET_BOARD,
                                      proto.CFG_OP_GET, key, 0), dst)
        seq = (seq + 1) & 0xFFFF
        end = time.monotonic() + 0.25
        while time.monotonic() < end:
            try:
                d, _ = sock.recvfrom(65535)
            except socket.timeout:
                continue
            kind, pkt = proto.parse(d)
            if kind == proto.TYPE_CONFIG and pkt.key == key:
                return pkt.value
            asm.feed(d)
        return None

    sub()
    end = time.monotonic() + 3.0
    while time.monotonic() < end:
        try:
            d, _ = sock.recvfrom(65535)
        except socket.timeout:
            continue
        asm.feed(d)
    m = asm.mode
    if m is None:
        print("MODEを受信できません(ボード/入力信号を確認)")
        return
    print(f"MODE {m.hactive}x{m.vactive}  htotal={m.htotal} vtotal={m.vtotal}  "
          f"fH={m.hfreq_mhz_x1000/1e6:.3f}kHz  fV={m.vfreq_mhz_x1000/1e6:.3f}Hz")
    half = m.htotal // 2

    vals = Counter()
    fids = Counter()
    last = 0.0
    for i in range(args.samples):
        if time.monotonic() - last > 1.5:
            sub()
            last = time.monotonic()
        v = get(KEY_VS_X)
        if v is not None:
            vals[v] += 1
        if i % 4 == 0:
            f = get(KEY_FID)
            if f is not None:
                fids[f] += 1

    if not vals:
        print("vs_x を読めません(このファームは key 0x20 を持っていない可能性)")
        return
    print(f"\nvs_x の分布({sum(vals.values())}回読み, htotal={m.htotal}, "
          f"htotal/2={half}):")
    for v, n in vals.most_common(10):
        # 位相をラインに対する割合でも出す
        frac = v / m.htotal if m.htotal else 0.0
        print(f"  {v:6d} ({frac:5.2f}ライン)  {n:4d}回 {n/sum(vals.values())*100:5.1f}%")
    print(f"FIDOUT の分布: {dict(fids)}")

    # 判定
    top = vals.most_common(2)
    print()
    if len(top) >= 2 and top[1][1] >= sum(vals.values()) * 0.2:
        d = abs(top[0][0] - top[1][0])
        print(f"2つの位相が交互に出ています(差 {d} = {d/m.htotal:.2f}ライン)")
        if abs(d - half) < m.htotal * 0.15:
            print("  → 差がほぼ半ライン。**VSYNCはフィールドごとに来ている**")
            print("     行位置を半ライン単位で決めれば織り込みは自動で成立する")
        else:
            print("  → 差が半ラインではない。別の要因(ジッタ等)の可能性")
    else:
        print("位相はほぼ1つの値に固定されています")
        print("  → このモードでは **VSYNCは1周期に1回しか来ていない**")
        print("     半ライン位相では第2フィールドを分離できないので、別の扱いが要る")


if __name__ == "__main__":
    main()
