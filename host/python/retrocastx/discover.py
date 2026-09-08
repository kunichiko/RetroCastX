"""Board discovery: listen for RetroCastX ANNOUNCE broadcasts and list boards.

Usage:
    python3 -m retrocastx.discover [--port 34600] [--timeout 0] [--bind ADDR]

The board broadcasts a TYPE_INFO packet every second. This tool prints each
board (source IP is authoritative; the payload ip is advisory) and can also
send a SUBSCRIBE back with --subscribe to direct the video stream to this host.
"""
import argparse
import socket
import time

from . import netutil
from . import protocol as proto


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--timeout", type=float, default=0, help="seconds; 0 = run forever")
    ap.add_argument("--subscribe", action="store_true",
                    help="send SUBSCRIBE back to each discovered board")
    ap.add_argument("--bind", default="0.0.0.0",
                    help="受信ソケットを縛るローカルアドレス。**通常は不要**: "
                         "probe はNICごとのサブネット宛へ出すので既定経路を "
                         "参照しない。NICを1つに絞りたいときだけ指定する")
    args = ap.parse_args()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    # bind はポートを掴むだけ。宛先の決め方は netutil に寄せてある。
    try:
        sock.bind((args.bind, args.port))
    except OSError as e:
        raise SystemExit(
            "UDP %d を %s で bind できません (%s)\n"
            "  Viewerが起動中なら終了してください。"
            % (args.port, args.bind, e))
    sock.settimeout(1.0)

    seen = {}
    seq = 0
    last_probe = 0.0
    warned = False
    targets = netutil.broadcast_targets(args.bind)
    deadline = time.monotonic() + args.timeout if args.timeout > 0 else None
    print("probing for RetroCastX boards on UDP %d (DISCOVER → %s) ..."
          % (args.port, " ".join(targets)))
    try:
        while deadline is None or time.monotonic() < deadline:
            now = time.monotonic()
            if now - last_probe >= 1.0:
                # 全ボードへ問いかける(ワイルドカードMAC+ブロードキャスト、発見のみ)。
                # ★**送信の失敗でプログラムを終わらせない。** ボードはANNOUNCEを
                #   毎秒自発送出するので、probe が出せなくても受動リッスンで
                #   見つかる。以前は sendto の例外がそのまま終了させていたため、
                #   VPN接続中は「ボードが見つからない」という形で失敗していた。
                pkt = proto.pack_subscribe(seq, announce_only=True,
                                           mac=proto.WILDCARD_MAC)
                if netutil.send_all(sock, targets, args.port, pkt) == 0 and not warned:
                    warned = True
                    print("  ※ " + netutil.explain_failure(targets))
                    print("  (ANNOUNCEの受動リッスンは続けます)")
                seq += 1
                last_probe = now
            try:
                datagram, addr = sock.recvfrom(2048)
            except socket.timeout:
                continue
            try:
                ptype, pkt = proto.parse(datagram)
            except ValueError:
                continue
            if ptype != proto.TYPE_INFO:
                continue
            key = (addr[0], pkt.mac)
            if key not in seen:
                mac = ":".join("%02x" % b for b in pkt.mac)
                # fw_version は gw-vX.Y.Z タグ由来。bit15:12=major 11:6=minor 5:0=patch
                fw = pkt.fw_version
                fwtxt = ("不明" if fw == 0 else
                         "%d.%d.%d" % ((fw >> 12) & 0xF, (fw >> 6) & 0x3F, fw & 0x3F))
                print("FOUND %-15s  mac=%s  name=%r  port=%d  fw=%s (0x%04x)  caps=0x%04x"
                      % (addr[0], mac, pkt.name, pkt.udp_port, fwtxt, fw, pkt.caps))
                # ★**MACがフォールバックのままの基板は出荷してはいけない。**
                #   全基板共通のアドレスなので、同じLANに2枚繋ぐとスイッチの
                #   学習テーブルが壊れて両方通信できなくなる。
                if not (pkt.caps & 0x0001):
                    print("  ★警告: MACをEEPROMから読めていません"
                          "(全基板共通のフォールバック値で動作中)。")
                    print("    原因は CONFIG key 0x06 (mac_info) で切り分けられます:")
                    print("      python3 -m retrocastx.cfg get 0x0006")
                if args.subscribe:
                    # 発見したボードのMACを指名して購読(複数ボードLANでも安全)
                    sock.sendto(proto.pack_subscribe(seq, mac=pkt.mac),
                                (addr[0], pkt.udp_port))
                    seq += 1
                    print("  -> SUBSCRIBE sent (stream will be directed here)")
            seen[key] = time.monotonic()
    except KeyboardInterrupt:
        pass
    print("%d board(s) seen" % len(seen))


if __name__ == "__main__":
    main()
