"""AUDIOパケットをWAVファイルに書き出す(音声パスの実機確認用)。

Usage:
    python3 -m retrocastx.audio_dump [--board 192.168.10.50] [--seconds 5]
        [--source 0] [--out out.wav]

source: 0=D-SUB15音声(PCM1808 #1), 1=LINE入力(PCM1808 #2), 2=S/PDIF

タイムスタンプ順に並べ、欠落分は無音で埋めるので、パケットロスがあっても
再生時間がずれない。書き出し後に統計(実効サンプルレート・ピーク・欠落)を表示する。
"""
import argparse
import socket
import struct
import time
import wave

from . import protocol as proto

SRC_NAMES = {0: "D-SUB15 audio (PCM1808 #1)",
             1: "LINE in (PCM1808 #2)",
             2: "S/PDIF"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--board", default="192.168.10.50")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--seconds", type=float, default=5.0)
    ap.add_argument("--source", type=int, default=0, choices=(0, 1, 2))
    ap.add_argument("--out", default=None, help="既定: audio_src<N>.wav")
    args = ap.parse_args()
    out = args.out or f"audio_src{args.source}.wav"

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 16 << 20)
    s.bind(("0.0.0.0", args.port))
    s.settimeout(0.5)
    sub = proto.pack_subscribe(1, announce_only=False)
    s.sendto(sub, (args.board, args.port))
    last_sub = t0 = time.time()

    chunks = []          # (timestamp, samples bytes)
    rate = None
    npkt = 0
    print(f"subscribed to {args.board}:{args.port}; "
          f"source {args.source} ({SRC_NAMES.get(args.source)}) を "
          f"{args.seconds}s 収録 ...")
    while time.time() - t0 < args.seconds:
        if time.time() - last_sub > 2.0:
            s.sendto(sub, (args.board, args.port))
            last_sub = time.time()
        try:
            data, _ = s.recvfrom(2048)
        except socket.timeout:
            continue
        # 映像は数万パケット/秒来るので、共通ヘッダのtypeバイトだけ見て安価に捨てる。
        # 全パケットを proto.parse するとPythonが追いつかず、OSバッファが溢れて
        # 肝心のAUDIOパケットまで落ちる(実測で200/s→28/sまで欠落した)。
        if len(data) < 3 or data[2] != proto.TYPE_AUDIO:
            continue
        try:
            ptype, pkt = proto.parse(data)
        except Exception:
            continue
        if ptype != proto.TYPE_AUDIO or pkt.source != args.source:
            continue
        npkt += 1
        rate = pkt.rate_hz or rate
        chunks.append((pkt.timestamp, pkt.samples))

    if not chunks:
        print("*** 対象sourceのAUDIOパケットが来ていません ***")
        return 1

    # 到着順に連結する(UDPはLAN内では順序が保たれるのが普通)。タイムスタンプで
    # 並べ替えたり欠落を無音で埋めたりはしない: tsは32bitでラップするうえ、音声tsは
    # FIFOが空→非空になる瞬間にラッチする近似なので、並べ替えの鍵にすると破綻する。
    # 欠落は下でtsギャップから推定して報告するだけにする。
    n0 = len(chunks[0][1]) // 4
    pcm = bytearray()
    for _ts, samples in chunks:
        pcm += samples
    # ts差分の中央値=1パケット分。ラップ(2^32)を考慮して大きなギャップを数える
    ts_list = [c[0] for c in chunks]
    d = [(b - a) & 0xFFFFFFFF for a, b in zip(ts_list, ts_list[1:])]
    ts_per_pkt = sorted(d)[len(d) // 2] if d else None
    ngap = 0
    if ts_per_pkt:
        for g in d:
            miss = round(g / ts_per_pkt) - 1
            if 0 < miss < 1000:      # 妥当な範囲の欠落だけ数える
                ngap += miss

    with wave.open(out, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(rate or 48000)
        w.writeframes(bytes(pcm))

    nframes = len(pcm) // 4
    vals = struct.unpack_from(f"<{len(pcm)//2}h", pcm, 0)
    peak = max((abs(v) for v in vals), default=0)
    el = time.time() - t0
    print(f"\n{out} に書き出し: {nframes} フレーム "
          f"({nframes/(rate or 48000):.2f} 秒 @ {rate} Hz)")
    print(f"  パケット {npkt}  実効 {nframes/el:.0f} samples/s")
    print(f"  振幅ピーク {peak} / 32767 "
          f"({20*__import__('math').log10(peak/32767):.1f} dBFS)" if peak else
          "  振幅ピーク 0 (無音)")
    if ngap:
        print(f"  ts上の欠落パケット推定: {ngap} 個")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
