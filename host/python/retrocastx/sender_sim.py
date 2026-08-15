"""Sender simulator: emits a test pattern as RetroCastX v0 UDP packets.

Stands in for the FPGA until real hardware exists; also documents the exact
packetization the gateware must implement.

Usage:
    python3 -m retrocastx.sender_sim [--dest 127.0.0.1] [--port 34600]
        [--width 512] [--height 512] [--fps 30] [--pixfmt rgb888|rgb555]
        [--mtu 1500] [--frames 0]
"""
import argparse
import socket
import time

import numpy as np

from . import pattern, protocol as proto

PIXFMT_BY_NAME = {"rgb888": proto.PIXFMT_RGB888, "rgb555": proto.PIXFMT_RGB555,
                  "yc8": proto.PIXFMT_YC8}


class Sender:
    """Packetizes frames exactly as the gateware will: MODE on change/periodically,
    one LINE packet per line fragment, dot-clock-counter timestamps."""

    def __init__(self, sock: socket.socket, dest, width: int, height: int,
                 fps: float, pixfmt: int, mtu: int = 1500):
        self.sock = sock
        self.dest = dest
        self.pixfmt = pixfmt
        self.seq = 0
        self.dotclk_counter = 0
        # Plausible blanking, in the spirit of retro timings (~78% / ~94% active).
        htotal = int(width * 1.28)
        vtotal = int(height * 1.06)
        dotclk = int(htotal * vtotal * fps)
        self.mode = proto.Mode(
            mode_id=1, pixfmt=pixfmt, mflags=0,
            hactive=width, htotal=htotal, vactive=height, vtotal=vtotal,
            dotclk_hz=dotclk,
            hfreq_mhz_x1000=int(dotclk / htotal * 1000),
            vfreq_mhz_x1000=int(fps * 1000))
        # UDP payload budget: MTU minus IP(20) + UDP(8) headers.
        self.fragments = proto.fragment_line(width, pixfmt, mtu - 28)
        self.frame_period_clk = htotal * vtotal

    def _send(self, payload: bytes):
        self.sock.sendto(payload, self.dest)
        self.seq = (self.seq + 1) & 0xFFFF

    def send_mode(self, frame_idx: int):
        self._send(self.mode.pack(frame_idx, self.seq))

    def send_announce(self):
        """実機ではブロードキャスト。シミュレータではdest宛のユニキャストで代用。"""
        ann = proto.Announce(
            mac=bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01]),  # locally administered "RCX"
            ip="0.0.0.0", udp_port=proto.DEFAULT_PORT,
            fw_version=0x0001, caps=0x0000, name="retrocastx-sim")
        self._send(ann.pack(self.seq))

    def send_frame(self, img: np.ndarray, frame_idx: int):
        m = self.mode
        if self.pixfmt == proto.PIXFMT_RGB555:
            packed = pattern.rgb888_to_rgb555(img)
            rows = [packed[y].astype("<u2").tobytes() for y in range(m.vactive)]
            bpp = 2
        elif self.pixfmt == proto.PIXFMT_YC8:
            # ボードと同じ詰め方: byte0 = 緑ch(CVBS/Y) / byte1 = 赤ch(C)。
            # ゲートウェアは Cat(g, r) を16bitリトルエンディアンで置くので、
            # 下位バイトが緑になる。
            yc = np.stack([img[:, :, 1], img[:, :, 0]], axis=2).astype(np.uint8)
            rows = [yc[y].tobytes() for y in range(m.vactive)]
            bpp = 2
        else:
            rows = [img[y].tobytes() for y in range(m.vactive)]
            bpp = 3
        frame_start_clk = self.dotclk_counter
        for y in range(m.vactive):
            line_ts = frame_start_clk + y * m.htotal
            row = rows[y]
            for i, (off, count) in enumerate(self.fragments):
                last = i == len(self.fragments) - 1
                self._send(proto.pack_line(
                    frame_idx, self.seq, y, off, self.pixfmt, m.mode_id,
                    line_ts + off, row[off * bpp:(off + count) * bpp],
                    last_fragment=last))
        self.dotclk_counter = (frame_start_clk + self.frame_period_clk) & 0xFFFFFFFF


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dest", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--width", type=int, default=512)
    ap.add_argument("--height", type=int, default=512)
    ap.add_argument("--fps", type=float, default=30.0)
    ap.add_argument("--pixfmt", choices=sorted(PIXFMT_BY_NAME), default="rgb888")
    ap.add_argument("--mtu", type=int, default=1500)
    ap.add_argument("--frames", type=int, default=0, help="0 = run forever")
    args = ap.parse_args()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4 << 20)
    sender = Sender(sock, (args.dest, args.port), args.width, args.height,
                    args.fps, PIXFMT_BY_NAME[args.pixfmt], args.mtu)

    period = 1.0 / args.fps
    frame_idx = 0
    next_deadline = time.monotonic()
    last_mode_at = None  # None = MODE not sent yet; send it before the first frame
    print("sending %dx%d %s @%.5gfps -> %s:%d" % (
        args.width, args.height, args.pixfmt, args.fps, args.dest, args.port))
    try:
        while args.frames <= 0 or frame_idx < args.frames:
            now = time.monotonic()
            if last_mode_at is None or now - last_mode_at >= 1.0:
                sender.send_announce()
                sender.send_mode(frame_idx)
                last_mode_at = now
            sender.send_frame(pattern.make_frame(args.width, args.height, frame_idx),
                              frame_idx)
            frame_idx += 1
            next_deadline += period
            delay = next_deadline - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            else:
                next_deadline = time.monotonic()  # can't keep up; don't spiral
    except KeyboardInterrupt:
        pass
    print("sent %d frames" % frame_idx)


if __name__ == "__main__":
    main()
