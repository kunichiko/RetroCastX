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
KEY_SYNC_CTL = 0x0022      # TVPレジスタ0x0E(同期制御)
KEY_MEAS_VTOTAL = 0x0023   # 実測vtotal(VSYNC間のライン数)


def sweep_sync(get, setv, sub, mode):
    """TVPの同期制御(0x0E)を振って、VSOUTに半ライン位相が残る設定を探す。

    24kHz 1024x848 の実測では、生信号はフィールドごとにVSYNCが来ている(オシロで
    VSYNCトリガごとにHSYNCが半ラインずれる)のに、TVPのVSOUTは931ラインに1パルスしか
    出さず、水平位相も完全固定だった。VSOUTを再生成ではなく素通しにできる設定が
    あれば、vtotal が半分(約465)になり、位相が2値を交互に取るはずである。

    判定は次の2つ:
      meas_vtotal が約半分になる  → 2本目のVSYNCが見えるようになった
      vs_x が2値を交互に取る      → 半ライン位相が残っている
    """
    base_vt = mode.vtotal
    print(f"\n同期制御(0x0E)を振る。いまの vtotal={base_vt}")
    print("期待: 2本目のVSYNCが見えるようになれば vtotal が約半分になる")
    print("  0x0E  vtotal  vs_x(数回読み)")
    hits = []
    for v in range(0, 256):
        if get(KEY_SYNC_CTL) is None:
            print("  key 0x22 に応答なし。焼き込みを確認してください")
            return
        # 設定して落ち着くのを待つ
        setv(KEY_SYNC_CTL, v)
        time.sleep(0.35)
        sub()
        vt = get(KEY_MEAS_VTOTAL)
        xs = {get(KEY_VS_X) for _ in range(4)}
        xs.discard(None)
        mark = ""
        if vt and 0.4 * base_vt < vt < 0.6 * base_vt:
            mark += " ← vtotalが半分"
        if len(xs) >= 2:
            mark += " ← 位相が複数"
        if mark:
            hits.append((v, vt, sorted(xs), mark))
        if v % 16 == 0 or mark:
            print(f"  0x{v:02X}  {vt}  {sorted(xs)}{mark}")
    print()
    if hits:
        print("見つかった設定:")
        for v, vt, xs, mark in hits:
            print(f"  0x{v:02X}  vtotal={vt}  vs_x={xs}{mark}")
    else:
        print("どの値でも半ライン位相は現れませんでした。")
        print("  → TVPのVSOUTからは取り出せない。生のHSYNC/VSYNCをFPGAへ入れる必要がある")
    setv(KEY_SYNC_CTL, 0x52)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", required=True)
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--samples", type=int, default=400)
    ap.add_argument("--sweep-sync", action="store_true",
                    help="TVPの同期制御(0x0E)を振って、VSOUTに半ライン位相が"
                         "残る設定を探す")
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

    def setv(key, value):
        nonlocal seq
        sock.sendto(proto.pack_config(seq, proto.CFG_TARGET_BOARD,
                                      proto.CFG_OP_SET, key, value), dst)
        seq = (seq + 1) & 0xFFFF

    if args.sweep_sync:
        sweep_sync(get, setv, sub, m)
        return

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
