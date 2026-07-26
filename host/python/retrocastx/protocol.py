"""RetroCastX protocol v0: packet pack/unpack.

See docs/protocol-v0.md. All fields little-endian.
"""
import struct
from dataclasses import dataclass
from typing import Optional, Tuple

MAGIC = 0x52
VERSION = 0
DEFAULT_PORT = 34600

TYPE_LINE = 0
TYPE_MODE = 1
TYPE_AUDIO = 2
TYPE_INFO = 3       # ANNOUNCE: ボード→ブロードキャスト(発見用)
TYPE_SUBSCRIBE = 4  # アプリ→ボード: 映像の送り先を自分に向ける
TYPE_CONFIG = 5     # アプリ→ボード: 設定読み書き(ボードはREPLYを返す)

# AUDIO sources / formats
AUDIO_SRC_RGB = 0    # D-SUB15(RGB端子)の音声
AUDIO_SRC_LINE = 1   # 基板上のLINE入力
AUDIO_SRC_SPDIF = 2  # 光デジタル
AUDIO_FMT_PCM16 = 0  # s16le 2ch インターリーブ

# CONFIG
CFG_TARGET_BOARD = 0
CFG_TARGET_ARGUSX = 1
CFG_OP_SET = 0
CFG_OP_GET = 1
CFG_FLAG_REPLY = 0x01
CFG_KEY_AUDIO_ENABLE = 0x0001   # board: bit0=RGB音声, bit1=LINE, bit2=S/PDIF
CFG_KEY_ARGUSX_INPUT = 0x0001   # ArgusX: 映像入力選択

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
_INFO = struct.Struct("<6s4sHHH16s")      # mac, ip, udp_port, fw_version, caps, name
_AUDIO = struct.Struct("<BBHII")          # source, format, nsamples, rate_hz, timestamp
_CONFIG = struct.Struct("<BBHI")          # target, op, key, value

COMMON_SIZE = _COMMON.size               # 8
LINE_HDR_SIZE = _LINE.size               # 12
MODE_HDR_SIZE = _MODE.size               # 24
INFO_SIZE = _INFO.size                   # 32
AUDIO_HDR_SIZE = _AUDIO.size             # 12
CONFIG_SIZE = _CONFIG.size               # 8


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


@dataclass
class Announce:
    """ボードが毎秒ブロードキャストする自己紹介(発見用)。

    受信側はデータグラムの送信元アドレスを正とすべき(ip フィールドは参考値。
    ボードが自IPを知らない構成では 0.0.0.0 になり得る)。
    """
    mac: bytes           # 6 bytes
    ip: str              # dotted quad
    udp_port: int
    fw_version: int
    caps: int
    name: str
    seq: int = 0

    def pack(self, seq: int) -> bytes:
        import socket as _s
        return _COMMON.pack(MAGIC, VERSION, TYPE_INFO, 0, 0, seq & 0xFFFF) + _INFO.pack(
            self.mac, _s.inet_aton(self.ip), self.udp_port,
            self.fw_version, self.caps, self.name.encode("ascii")[:16].ljust(16, b"\0"))


@dataclass
class Audio:
    """音声サンプル。timestamp は映像LINEと同一のドットクロックカウンタ(A/V同期用)。"""
    frame: int
    seq: int
    source: int          # AUDIO_SRC_*
    format: int          # AUDIO_FMT_*
    nsamples: int        # サンプルフレーム数(L+Rで1)
    rate_hz: int
    timestamp: int
    samples: bytes       # s16le L/R interleaved


@dataclass
class Config:
    """設定読み書き(app→board)/応答(board→app、flags bit0=REPLY)。"""
    seq: int
    flags: int
    target: int          # CFG_TARGET_*
    op: int              # CFG_OP_*
    key: int
    value: int

    @property
    def is_reply(self) -> bool:
        return bool(self.flags & CFG_FLAG_REPLY)


def pack_audio(frame: int, seq: int, source: int, rate_hz: int,
               timestamp: int, samples: bytes, fmt: int = AUDIO_FMT_PCM16) -> bytes:
    if fmt == AUDIO_FMT_PCM16:
        nsamples, rem = divmod(len(samples), 4)  # s16le * 2ch
        if rem:
            raise ValueError("sample buffer not a multiple of 4 bytes")
    else:
        raise ValueError("unknown audio format %d" % fmt)
    return (_COMMON.pack(MAGIC, VERSION, TYPE_AUDIO, 0, frame & 0xFFFF, seq & 0xFFFF)
            + _AUDIO.pack(source, fmt, nsamples, rate_hz, timestamp & 0xFFFFFFFF)
            + samples)


def pack_config(seq: int, target: int, op: int, key: int, value: int = 0,
                reply: bool = False) -> bytes:
    flags = CFG_FLAG_REPLY if reply else 0
    return (_COMMON.pack(MAGIC, VERSION, TYPE_CONFIG, flags, 0, seq & 0xFFFF)
            + _CONFIG.pack(target, op, key, value & 0xFFFFFFFF))


# SUBSCRIBE flags
SUB_FLAG_ANNOUNCE_ONLY = 0x01  # 発見のみ(ストリーム送り先は変更しない)


def pack_subscribe(seq: int, announce_only: bool = False) -> bytes:
    """アプリ→ボード。ブロードキャスト可。

    ボードはANNOUNCEを送信元へユニキャストで返す。announce_only=False なら
    以後の映像ストリームの送り先もこのパケットの送信元に切り替える。
    """
    flags = SUB_FLAG_ANNOUNCE_ONLY if announce_only else 0
    return _COMMON.pack(MAGIC, VERSION, TYPE_SUBSCRIBE, flags, 0, seq & 0xFFFF)


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
    if ptype == TYPE_INFO:
        if len(body) < INFO_SIZE:
            raise ValueError("short INFO packet")
        import socket as _s
        mac, ip4, port, fw, caps, name = _INFO.unpack_from(body, 0)
        return ptype, Announce(mac, _s.inet_ntoa(ip4), port, fw, caps,
                               name.rstrip(b"\0").decode("ascii", "replace"), seq=seq)
    if ptype == TYPE_AUDIO:
        if len(body) < AUDIO_HDR_SIZE:
            raise ValueError("short AUDIO packet")
        source, fmt, nsamples, rate_hz, timestamp = _AUDIO.unpack_from(body, 0)
        samples = body[AUDIO_HDR_SIZE:]
        if fmt == AUDIO_FMT_PCM16 and len(samples) != nsamples * 4:
            raise ValueError("AUDIO payload size mismatch")
        return ptype, Audio(frame, seq, source, fmt, nsamples, rate_hz,
                            timestamp, samples)
    if ptype == TYPE_CONFIG:
        if len(body) < CONFIG_SIZE:
            raise ValueError("short CONFIG packet")
        target, op, key, value = _CONFIG.unpack_from(body, 0)
        return ptype, Config(seq, flags, target, op, key, value)
    if ptype == TYPE_SUBSCRIBE:
        return ptype, None
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
