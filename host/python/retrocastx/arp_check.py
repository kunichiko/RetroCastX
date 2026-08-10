#!/usr/bin/env python3
"""ボードが「こちらのMACを学習して返せているか」を見る。

Windows は別サブネットの送信元から来たARP要求に応答しないので、ボードが宛先MACを
ARPで引こうとすると返信できない(docs/design-notes.md「実機を動かすときの準備」)。
対策としてボードは受信パケットから送信元MACを学習する(gateware/retrocastx_net.py)。
この道具はその学習が効いているかを確かめる。

やること: SUBSCRIBE(発見のみ)をブロードキャスト → ANNOUNCE が返るか見る →
ARP学習の診断カウンタ(key 0x40..0x45)を GET で読む。

    python3 -m retrocastx.arp_check                        # 探して読む
    python3 -m retrocastx.arp_check --board 192.168.10.50  # 宛先を指定
    python3 -m retrocastx.arp_check --watch                # 1秒ごとに読み続ける

**そもそも応答が1つも返らなければ、それが症状そのもの**(ボードはSUBSCRIBEを
受けているのに返せていない)。ボード側の受信は生きているか(OLEDのリンク表示、
Wiresharkで自分のSUBSCRIBEが出ているか)を先に確かめること。
"""
import argparse
import socket
import struct
import time

from . import protocol as proto

KEYS = [
    (proto.CFG_KEY_ARP_LEARNS, "学習した回数            "),
    (proto.CFG_KEY_ARP_HITS, "学習した表で即答        "),
    (proto.CFG_KEY_ARP_MISSES, "本物のARPへ委譲        "),
    (proto.CFG_KEY_ARP_LAST_IP, "最後に学習した相手のIP  "),
    (proto.CFG_KEY_ARP_LAST_MAC_LO, "同 MAC下位4B            "),
    (proto.CFG_KEY_ARP_LAST_MAC_HI, "同 MAC上位2B            "),
]


def _ip_str(v):
    return socket.inet_ntoa(struct.pack(">I", v & 0xFFFFFFFF))


def _mac_str(lo, hi):
    return ":".join("%02x" % b for b in struct.pack(">Q", ((hi & 0xFFFF) << 32)
                                                    | (lo & 0xFFFFFFFF))[2:])


def _local_ip(dst, port):
    """ボードから見えるこちらの送信元アドレス(経路選択はOSに任せる)。"""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        s.connect((dst, port))
        return s.getsockname()[0]
    except OSError:
        return None
    finally:
        s.close()


def _drain(sock, values, until):
    """届いているものを拾う。ANNOUNCE(発見)を見つけたら返す。"""
    board = None
    while True:
        left = until - time.monotonic()
        if left <= 0:
            return board
        sock.settimeout(left)
        try:
            data, src = sock.recvfrom(65535)
        except (socket.timeout, OSError):
            return board
        try:
            ptype, pkt = proto.parse(data)
        except ValueError:
            continue
        if ptype == proto.TYPE_INFO:
            board = (src[0], ":".join("%02x" % b for b in pkt.mac), pkt.name)
        elif ptype == proto.TYPE_CONFIG and pkt.is_reply:
            values[pkt.key] = pkt.value


def poll(sock, dst, port, seq, tries=3, wait=0.25):
    """SUBSCRIBE(発見のみ)+ 各キーのGET。**1キーずつ送って応答を待つ。**

    ボードのCONFIG応答は「1つ分の保留」しか持たない(`cfg_pending` と
    `cfg_key`/`cfg_reply_val` は1組)。まとめて投げると応答が上書きされて
    落ちる ─ 実機で6キー中1つが返らない形で出た。
    """
    values, board = {}, None
    sock.sendto(proto.pack_subscribe(seq, announce_only=True), (dst, port))
    board = _drain(sock, values, time.monotonic() + wait) or board
    for attempt in range(tries):
        missing = [k for k, _ in KEYS if k not in values]
        if not missing:
            break
        for i, key in enumerate(missing):
            sock.sendto(proto.pack_config(seq + 1 + attempt * 8 + i,
                                          proto.CFG_TARGET_BOARD,
                                          proto.CFG_OP_GET, key), (dst, port))
            board = _drain(sock, values, time.monotonic() + wait) or board
    return board, values


def report(board, values, me):
    if board is None and not values:
        print("  応答なし。ボードはこちらへ返せていない(または受信できていない)")
        return False
    if board:
        print(f"  ボード: {board[0]}  mac {board[1]}  ({board[2]})")
    for key, label in KEYS:
        v = values.get(key)
        if v is None:
            print(f"  {label}: (応答なし)")
        elif key == proto.CFG_KEY_ARP_LAST_IP:
            print(f"  {label}: {_ip_str(v)}")
        elif key == proto.CFG_KEY_ARP_LAST_MAC_LO:
            hi = values.get(proto.CFG_KEY_ARP_LAST_MAC_HI)
            if hi is not None:
                print(f"  最後に学習した相手のMAC : {_mac_str(v, hi)}")
        elif key == proto.CFG_KEY_ARP_LAST_MAC_HI:
            pass
        else:
            print(f"  {label}: {v}")
    last_ip = values.get(proto.CFG_KEY_ARP_LAST_IP)
    if last_ip is not None and me:
        if _ip_str(last_ip) == me:
            print(f"  → こちら({me})を学習できている")
        else:
            print(f"  → 最後に学習したのは別の相手。こちらは {me}")
    return True


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", default="255.255.255.255",
                    help="ボードのIP(既定: ブロードキャストで探す)")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--local-port", type=int, default=34697,
                    help="こちらの受信ポート(Viewerと食い合わないように別にする)")
    ap.add_argument("--watch", action="store_true", help="1秒ごとに読み続ける")
    ap.add_argument("--count", type=int, default=0,
                    help="読む回数(--watch と併用。0=無限)")
    args = ap.parse_args()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    sock.bind(("0.0.0.0", args.local_port))

    me = _local_ip(args.board, args.port)
    dst = "ブロードキャスト" if args.board == "255.255.255.255" else args.board
    print(f"宛先 {dst}:{args.port} / こちら {me or '?'}:{args.local_port}")

    seq, n = 0, 0
    while True:
        print(time.strftime("[%H:%M:%S]"))
        board, values = poll(sock, args.board, args.port, seq)
        seq = (seq + 32) & 0xFFFF
        ok = report(board, values, me)
        n += 1
        if not args.watch or (args.count and n >= args.count):
            raise SystemExit(0 if ok else 1)
        time.sleep(1.0)


if __name__ == "__main__":
    main()
