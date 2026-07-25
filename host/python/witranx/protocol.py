"""WiTranX protocol v0: packet pack/unpack.

See docs/protocol-v0.md. All fields little-endian.
"""
import struct
from dataclasses import dataclass
from typing import Optional, Tuple

MAGIC = 0x57
VERSION = 0
DEFAULT_PORT = 34600

TYPE_LINE = 0
TYPE_MODE = 1
TYPE_AUDIO = 2
TYPE_INFO = 3

# LINE flags
FLAG_LAST_FRAGMENT = 0x01
FLAG_FIELD_ODD = 0x02

# MODE mflags
MFLAG_INTERLACE = 0x0001
MFLAG_HSYNC_NEG = 0x0002
MFLAG_VSYNC_NEG = 0x0004

PIXFMT_RGB888 = 0
PIXFMT_RGB555 = 1
PIXFMT_RGB565 = 2

BYTES_PER_PX = {PIXFMT_RGB888: 3, PIXFMT_RGB555: 2, PIXFMT_RGB565: 2}

_COMMON = struct.Struct("<BBBBHH")        # magic, version, type, flags, frame, seq
_LINE = struct.Struct("<HHHBBI")          # line, offset_px, count_px, pixfmt, mode_id, timestamp
_MODE = struct.Struct("<BBHHHHHIII")      # mode_id, pixfmt, mflags, hactive, htotal,
                                          # vactive, vtotal, dotclk_hz, hfreq_mhz, vfreq_mhz

COMMON_SIZE = _COMMON.size               # 8
LINE_HDR_SIZE = _LINE.size               # 12
MODE_HDR_SIZE = _MODE.size               # 22


@dataclass
class Mode:
    mode_id: int
    pixfmt: int
    mflags: int
    hactive: int
    htotal: int
    vactive: int
    vtotal: int
    dotclk_hz: int
    hfreq_mhz_x1000: int
    vfreq_mhz_x1000: int
    # From the common header; filled in by parse(), unused by pack().
    frame: int = 0
    seq: int = 0

    def pack(self, frame: int, seq: int) -> bytes:
        return _COMMON.pack(MAGIC, VERSION, TYPE_MODE, 0, frame & 0xFFFF, seq & 0xFFFF) + _MODE.pack(
            self.mode_id & 0xFF, self.pixfmt, self.mflags,
            self.hactive, self.htotal, self.vactive, self.vtotal,
            self.dotclk_hz, self.hfreq_mhz_x1000, self.vfreq_mhz_x1000)


@dataclass
class Line:
    frame: int
    seq: int
    flags: int
    line: int
    offset_px: int
    count_px: int
    pixfmt: int
    mode_id: int
    timestamp: int
    pixels: bytes

    @property
    def last_fragment(self) -> bool:
        return bool(self.flags & FLAG_LAST_FRAGMENT)


def pack_line(frame: int, seq: int, line: int, offset_px: int, pixfmt: int,
              mode_id: int, timestamp: int, pixels: bytes,
              last_fragment: bool = True, field_odd: bool = False) -> bytes:
    bpp = BYTES_PER_PX[pixfmt]
    count_px, rem = divmod(len(pixels), bpp)
    if rem:
        raise ValueError("pixel buffer not a multiple of bytes-per-pixel")
    flags = (FLAG_LAST_FRAGMENT if last_fragment else 0) | (FLAG_FIELD_ODD if field_odd else 0)
    return (_COMMON.pack(MAGIC, VERSION, TYPE_LINE, flags, frame & 0xFFFF, seq & 0xFFFF)
            + _LINE.pack(line, offset_px, count_px, pixfmt, mode_id & 0xFF, timestamp & 0xFFFFFFFF)
            + pixels)


def parse(datagram: bytes) -> Tuple[int, object]:
    """Parse a datagram. Returns (type, Line|Mode). Raises ValueError on malformed input."""
    if len(datagram) < COMMON_SIZE:
        raise ValueError("short datagram")
    magic, version, ptype, flags, frame, seq = _COMMON.unpack_from(datagram, 0)
    if magic != MAGIC or version != VERSION:
        raise ValueError("bad magic/version")
    body = datagram[COMMON_SIZE:]
    if ptype == TYPE_LINE:
        if len(body) < LINE_HDR_SIZE:
            raise ValueError("short LINE packet")
        line, offset_px, count_px, pixfmt, mode_id, timestamp = _LINE.unpack_from(body, 0)
        pixels = body[LINE_HDR_SIZE:]
        if pixfmt not in BYTES_PER_PX or len(pixels) != count_px * BYTES_PER_PX[pixfmt]:
            raise ValueError("LINE payload size mismatch")
        return ptype, Line(frame, seq, flags, line, offset_px, count_px,
                           pixfmt, mode_id, timestamp, pixels)
    if ptype == TYPE_MODE:
        if len(body) < MODE_HDR_SIZE:
            raise ValueError("short MODE packet")
        return ptype, Mode(*_MODE.unpack_from(body, 0), frame=frame, seq=seq)
    raise ValueError("unknown packet type %d" % ptype)


def fragment_line(hactive: int, pixfmt: int, mtu_payload: int) -> list:
    """Split a line of hactive pixels into (offset_px, count_px) fragments that fit
    in mtu_payload bytes of UDP payload (header included)."""
    bpp = BYTES_PER_PX[pixfmt]
    max_px = (mtu_payload - COMMON_SIZE - LINE_HDR_SIZE) // bpp
    if max_px <= 0:
        raise ValueError("MTU too small")
    frags = []
    off = 0
    while off < hactive:
        n = min(max_px, hactive - off)
        frags.append((off, n))
        off += n
    return frags
