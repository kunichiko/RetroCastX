"""End-to-end loopback test: Sender -> real UDP socket -> FrameAssembler.

Run:  python3 -m witranx.tests.test_loopback
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


def main():
    results = [
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
