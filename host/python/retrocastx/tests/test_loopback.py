"""End-to-end loopback test: Sender -> real UDP socket -> FrameAssembler.

Run:  python3 -m retrocastx.tests.test_loopback
Exits non-zero on failure.
"""
import socket
import sys

import numpy as np

from .. import pattern, protocol as proto
from ..receiver import FrameAssembler
from ..sender_sim import Sender

N_FRAMES = 5


def _drain(sock: socket.socket, asm: FrameAssembler, out: dict):
    sock.settimeout(0.2)
    while True:
        try:
            datagram, _ = sock.recvfrom(65535)
        except socket.timeout:
            return
        for frame_idx, img, fill in asm.feed(datagram):
            out[frame_idx] = (img, fill)


def run_case(name: str, width: int, height: int, pixfmt: int, mtu: int) -> bool:
    rx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rx.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
    rx.bind(("127.0.0.1", 0))
    tx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sender = Sender(tx, rx.getsockname(), width, height, 60.0, pixfmt, mtu)
    n_frags = len(sender.fragments)

    asm = FrameAssembler()
    got = {}
    sender.send_mode(0)
    for i in range(N_FRAMES):
        sender.send_frame(pattern.make_frame(width, height, i), i)
        _drain(rx, asm, got)  # drain per frame so the socket buffer never overflows
    for frame_idx, img, fill in asm.flush():
        got[frame_idx] = (img, fill)
    rx.close()
    tx.close()

    ok = True
    if set(got) != set(range(N_FRAMES)):
        print("  missing frames: got %s" % sorted(got))
        ok = False
    for i in sorted(got):
        img, fill = got[i]
        expected = pattern.make_frame(width, height, i)
        if pixfmt == proto.PIXFMT_RGB555:
            expected = pattern.rgb555_to_rgb888(pattern.rgb888_to_rgb555(expected))
        if fill != 1.0:
            print("  frame %d fill=%.3f (expected 1.0)" % (i, fill))
            ok = False
        if not np.array_equal(img, expected):
            print("  frame %d pixel mismatch (%d differing px)" % (
                i, int(np.any(img != expected, axis=2).sum())))
            ok = False
    if asm.lost_packets or asm.orphan_lines:
        print("  lost_packets=%d orphan_lines=%d (expected 0 on loopback)"
              % (asm.lost_packets, asm.orphan_lines))
        ok = False
    print("%s %s (%d fragments/line)" % ("PASS" if ok else "FAIL", name, n_frags))
    return ok


def run_announce_case() -> bool:
    """ANNOUNCE/SUBSCRIBE round-trip: pack -> UDP -> parse."""
    rx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rx.bind(("127.0.0.1", 0))
    rx.settimeout(1.0)
    tx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sender = Sender(tx, rx.getsockname(), 320, 240, 60.0, proto.PIXFMT_RGB888, 1500)
    board_mac = bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01])
    sender.send_announce()
    tx.sendto(proto.pack_subscribe(1, mac=board_mac), rx.getsockname())

    d1, addr = rx.recvfrom(2048)
    t1, ann = proto.parse(d1)
    d2, _ = rx.recvfrom(2048)
    t2, sub = proto.parse(d2)
    rx.close()
    tx.close()

    ok = (t1 == proto.TYPE_INFO
          and ann.mac == board_mac
          and ann.name == "retrocastx-sim"
          and ann.udp_port == proto.DEFAULT_PORT
          and t2 == proto.TYPE_SUBSCRIBE
          and sub.mac == board_mac
          and addr[0] == "127.0.0.1")
    if not ok:
        print("  announce=%r type2=%r" % (ann, t2))
    print("%s announce/subscribe round-trip" % ("PASS" if ok else "FAIL"))
    return ok


def run_audio_config_case() -> bool:
    """AUDIO/CONFIG round-trip: pack -> UDP -> parse."""
    rx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rx.bind(("127.0.0.1", 0))
    rx.settimeout(1.0)
    tx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

    samples = np.arange(480, dtype="<i2").tobytes()  # 240サンプルフレーム(L/R)
    tx.sendto(proto.pack_audio(3, 7, proto.AUDIO_SRC_SPDIF, 44100, 0x12345678,
                               samples), rx.getsockname())
    board_mac = bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01])
    tx.sendto(proto.pack_config(8, proto.CFG_TARGET_BOARD, proto.CFG_OP_SET,
                                proto.CFG_KEY_AUDIO_ENABLE, 0b101, mac=board_mac),
              rx.getsockname())
    tx.sendto(proto.pack_config(8, proto.CFG_TARGET_ARGUSX, proto.CFG_OP_GET,
                                proto.CFG_KEY_ARGUSX_INPUT, 2, reply=True,
                                mac=board_mac), rx.getsockname())

    t1, aud = proto.parse(rx.recvfrom(4096)[0])
    t2, cfg_set = proto.parse(rx.recvfrom(2048)[0])
    t3, cfg_rep = proto.parse(rx.recvfrom(2048)[0])
    rx.close()
    tx.close()

    ok = (t1 == proto.TYPE_AUDIO
          and (aud.source, aud.format, aud.nsamples) == (proto.AUDIO_SRC_SPDIF,
                                                         proto.AUDIO_FMT_PCM16, 240)
          and (aud.rate_hz, aud.timestamp) == (44100, 0x12345678)
          and aud.samples == samples
          and t2 == proto.TYPE_CONFIG and not cfg_set.is_reply
          and cfg_set.mac == board_mac
          and (cfg_set.target, cfg_set.op, cfg_set.key, cfg_set.value)
              == (proto.CFG_TARGET_BOARD, proto.CFG_OP_SET,
                  proto.CFG_KEY_AUDIO_ENABLE, 0b101)
          and t3 == proto.TYPE_CONFIG and cfg_rep.is_reply and cfg_rep.value == 2
          and cfg_rep.mac == board_mac)
    if not ok:
        print("  audio=%r cfg_set=%r cfg_rep=%r" % (aud, cfg_set, cfg_rep))
    print("%s audio/config round-trip" % ("PASS" if ok else "FAIL"))
    return ok


def main():
    results = [
        run_announce_case(),
        run_audio_config_case(),
        run_case("rgb888 320x240 mtu1500 (single fragment)",
                 320, 240, proto.PIXFMT_RGB888, 1500),
        run_case("rgb888 768x512 mtu1500 (fragmented lines)",
                 768, 512, proto.PIXFMT_RGB888, 1500),
        run_case("rgb888 768x512 mtu9000 (jumbo, single fragment)",
                 768, 512, proto.PIXFMT_RGB888, 9000),
        run_case("rgb555 768x512 mtu1500",
                 768, 512, proto.PIXFMT_RGB555, 1500),
    ]
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
