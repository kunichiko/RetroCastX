"""Deterministic test pattern generator (shared by sender simulator and tests)."""
import numpy as np

# カラーバー横スクロール量[px/frame]。gateware retrocastx_stream.py の SCROLL_PX と一致必須。
# 幅が2のべき乗のとき gateware のビットスライスと同一結果になる。
SCROLL_PX = 4


def make_frame(width: int, height: int, frame_idx: int) -> np.ndarray:
    """RGB888 frame (height, width, 3) uint8: scrolling color bars + moving sweep line + border."""
    img = np.zeros((height, width, 3), dtype=np.uint8)
    bars = np.array([
        [255, 255, 255], [255, 255, 0], [0, 255, 255], [0, 255, 0],
        [255, 0, 255], [255, 0, 0], [0, 0, 255], [32, 32, 32],
    ], dtype=np.uint8)
    # 毎フレーム SCROLL_PX ずつ横スクロール(width幅でラップ)
    xs = (((np.arange(width) + frame_idx * SCROLL_PX) % width) * len(bars)) // width
    img[:, :, :] = bars[xs]
    # moving horizontal sweep line (1 px per frame, wraps)
    y = frame_idx % height
    img[y, :, :] = (frame_idx * 7 % 256, 255 - frame_idx * 5 % 256, frame_idx % 256)
    # 1px white border for geometry checks
    img[0, :, :] = 255
    img[-1, :, :] = 255
    img[:, 0, :] = 255
    img[:, -1, :] = 255
    return img


def rgb888_to_rgb555(img: np.ndarray) -> np.ndarray:
    """(h, w, 3) uint8 -> (h, w) uint16 packed 0RRRRRGGGGGBBBBB."""
    r = (img[:, :, 0].astype(np.uint16) >> 3) << 10
    g = (img[:, :, 1].astype(np.uint16) >> 3) << 5
    b = img[:, :, 2].astype(np.uint16) >> 3
    return r | g | b


def rgb555_to_rgb888(packed: np.ndarray) -> np.ndarray:
    """(h, w) uint16 -> (h, w, 3) uint8, 5bit expanded by bit replication."""
    r5 = ((packed >> 10) & 0x1F).astype(np.uint8)
    g5 = ((packed >> 5) & 0x1F).astype(np.uint8)
    b5 = (packed & 0x1F).astype(np.uint8)
    out = np.empty(packed.shape + (3,), dtype=np.uint8)
    out[:, :, 0] = (r5 << 3) | (r5 >> 2)
    out[:, :, 1] = (g5 << 3) | (g5 >> 2)
    out[:, :, 2] = (b5 << 3) | (b5 >> 2)
    return out
