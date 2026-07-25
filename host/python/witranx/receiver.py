"""Receiver: reassembles WiTranX v0 UDP packets into frames.

Usage:
    python3 -m witranx.receiver [--bind 0.0.0.0] [--port 34600]
        [--dump DIR] [--every 30]

Prints per-second stats; with --dump writes every Nth completed frame to
DIR/frame_NNNNNN.ppm (dependency-free PPM, viewable with Preview/ffplay).
"""
import argparse
import os
import socket
import time
from typing import List, Optional, Tuple

import numpy as np

from . import pattern, protocol as proto


class FrameAssembler:
    """Feed datagrams in, get completed frames out.

    A frame is considered complete when a LINE for a newer frame counter arrives
    (the stream has no explicit frame-end marker in v0). Missing lines keep the
    previous frame's content, which is exactly the "fill from last frame" loss
    policy from the protocol doc.
    """

    def __init__(self):
        self.mode: Optional[proto.Mode] = None
        self.fb: Optional[np.ndarray] = None
        self.cur_frame: Optional[int] = None
        self.px_filled = 0
        self.last_seq: Optional[int] = None
        self.lost_packets = 0
        self.orphan_lines = 0  # LINE whose mode_id we haven't seen

    def _track_seq(self, seq: int):
        if self.last_seq is not None:
            gap = (seq - self.last_seq) & 0xFFFF
            if gap == 0 or gap > 0x8000:  # duplicate or reordered: ignore
                return
            self.lost_packets += gap - 1
        self.last_seq = seq

    def feed(self, datagram: bytes) -> List[Tuple[int, np.ndarray, float]]:
        """Returns [] or [(frame_idx, rgb888_frame, fill_ratio)] completed by this packet."""
        try:
            ptype, pkt = proto.parse(datagram)
        except ValueError:
            return []
        completed = []
        if ptype == proto.TYPE_MODE:
            self._track_seq(pkt.seq)
            if self.mode is None or pkt.mode_id != self.mode.mode_id:
                self.mode = pkt
                self.fb = np.zeros((pkt.vactive, pkt.hactive, 3), dtype=np.uint8)
                self.cur_frame = None
                self.px_filled = 0
            return completed
        # LINE
        self._track_seq(pkt.seq)
        if self.mode is None or self.fb is None or pkt.mode_id != self.mode.mode_id:
            self.orphan_lines += 1
            return completed
        if self.cur_frame is not None and pkt.frame != self.cur_frame:
            completed.append(self._emit())
        if self.cur_frame is None:
            self.cur_frame = pkt.frame
        if pkt.line >= self.mode.vactive or pkt.offset_px + pkt.count_px > self.mode.hactive:
            return completed  # out of range for the current mode; drop
        sl = self.fb[pkt.line, pkt.offset_px:pkt.offset_px + pkt.count_px]
        if pkt.pixfmt == proto.PIXFMT_RGB888:
            sl[:] = np.frombuffer(pkt.pixels, dtype=np.uint8).reshape(-1, 3)
        elif pkt.pixfmt == proto.PIXFMT_RGB555:
            packed = np.frombuffer(pkt.pixels, dtype="<u2").reshape(1, -1)
            sl[:] = pattern.rgb555_to_rgb888(packed)[0]
        else:
            return completed
        self.px_filled += pkt.count_px
        return completed

    def _emit(self) -> Tuple[int, np.ndarray, float]:
        total = self.mode.hactive * self.mode.vactive
        result = (self.cur_frame, self.fb.copy(), self.px_filled / total)
        self.cur_frame = None
        self.px_filled = 0
        return result

    def flush(self) -> List[Tuple[int, np.ndarray, float]]:
        """Emit the in-progress frame (e.g. at end of stream)."""
        if self.cur_frame is None or self.fb is None:
            return []
        return [self._emit()]


def save_ppm(path: str, img: np.ndarray):
    with open(path, "wb") as f:
        f.write(b"P6\n%d %d\n255\n" % (img.shape[1], img.shape[0]))
        f.write(img.tobytes())


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--bind", default="0.0.0.0")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--dump", metavar="DIR", help="save completed frames as PPM")
    ap.add_argument("--every", type=int, default=30, help="dump every Nth frame")
    args = ap.parse_args()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
    sock.bind((args.bind, args.port))
    sock.settimeout(1.0)
    if args.dump:
        os.makedirs(args.dump, exist_ok=True)

    asm = FrameAssembler()
    n_frames = 0
    last_report = time.monotonic()
    frames_since = 0
    print("listening on %s:%d" % (args.bind, args.port))
    try:
        while True:
            try:
                datagram, _ = sock.recvfrom(65535)
            except socket.timeout:
                continue
            for frame_idx, img, fill in asm.feed(datagram):
                n_frames += 1
                frames_since += 1
                if args.dump and n_frames % args.every == 0:
                    save_ppm(os.path.join(args.dump, "frame_%06d.ppm" % n_frames), img)
            now = time.monotonic()
            if now - last_report >= 1.0:
                mode = asm.mode
                dims = "%dx%d" % (mode.hactive, mode.vactive) if mode else "no mode yet"
                print("%s  %.1f fps  frames=%d  lost_pkts=%d  orphan_lines=%d" % (
                    dims, frames_since / (now - last_report), n_frames,
                    asm.lost_packets, asm.orphan_lines))
                last_report = now
                frames_since = 0
    except KeyboardInterrupt:
        print("\n%d frames received" % n_frames)


if __name__ == "__main__":
    main()
