"""キャプチャ画のノイズ量を数値化する(ビットストリーム/配線のA/B比較用)。

Usage:
    python3 -m retrocastx.noise_meter [--board IP] [--frames 20] [--label NAME]

黒(または暗部)に現れる点状ノイズを「孤立した低レベル画素」として数える。
RGB555の1LSBは8bit展開で 8 なので、値が 8/16/24 付近の画素が点ノイズの主成分。

出力する指標:
  dark_frac    : 全画素のうち完全な黒(0,0,0)の割合
  speckle      : 「黒地の中で1〜3LSBだけ光っている」画素数(=点ノイズの量)
  speckle_ppm  : 上記を全画素比 ppm で
  mean_dark    : 暗部(最大値≤3LSB)の平均レベル
目視ではなくこの数値で比較すれば、ビルドや配線の変更の効果が判定できる。
"""
import argparse
import socket
import time

import numpy as np

from . import protocol as proto
from .receiver import FrameAssembler

LSB = 8  # RGB555の1LSBを8bitに展開した値


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--board", default="192.168.10.50")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--frames", type=int, default=20)
    ap.add_argument("--label", default="", help="比較用の見出し")
    args = ap.parse_args()

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 32 << 20)
    s.bind(("0.0.0.0", args.port))
    s.settimeout(0.5)
    sub = proto.pack_subscribe(1, announce_only=False)
    s.sendto(sub, (args.board, args.port))
    last_sub = t0 = time.time()

    asm = FrameAssembler()
    frames = []
    while len(frames) < args.frames and time.time() - t0 < 20.0:
        if time.time() - last_sub > 2.0:
            s.sendto(sub, (args.board, args.port))
            last_sub = time.time()
        try:
            data, _ = s.recvfrom(2048)
        except socket.timeout:
            continue
        for idx, img, fill in asm.feed(data):
            frames.append(img)

    if not frames:
        print("*** フレームを受信できませんでした ***")
        return 1

    speckle = []
    dark_frac = []
    mean_dark = []
    for img in frames:
        a = img.astype(np.int16)
        mx = a.max(axis=2)                       # 画素ごとのRGB最大値
        black = mx == 0
        low = (mx > 0) & (mx <= 3 * LSB)         # 1〜3LSBだけ光っている画素
        speckle.append(int(low.sum()))
        dark_frac.append(float(black.mean()))
        m = mx[mx <= 3 * LSB]
        mean_dark.append(float(m.mean()) if m.size else 0.0)

    # --- 時間差分(内容に依存しない指標) ---
    # 静止した絵はフレーム間で一致するので差分に出ない。ランダムノイズだけが残る。
    # 動く絵(レベルメーター等)がある画面では動体も混ざるので、比較は同じ画面で行う。
    stack = np.stack([f.astype(np.int16) for f in frames])   # (N,H,W,3)
    med = np.median(stack, axis=0)                # 各画素の中央値=「本来の絵」
    dev = np.abs(stack - med).max(axis=3)         # 中央値からのずれ(RGB最大)
    dark_mask = med.max(axis=2) <= 3 * LSB        # 暗部だけを見る
    dark_dev = dev[:, dark_mask] if dark_mask.any() else dev.reshape(len(frames), -1)
    flick = float((dark_dev > 0).mean())          # 暗部でちらつく画素の割合
    flick_lvl = float(dark_dev[dark_dev > 0].mean()) if (dark_dev > 0).any() else 0.0
    temporal_std = float(stack.std(axis=0).mean())

    npx = frames[0].shape[0] * frames[0].shape[1]
    head = f"[{args.label}] " if args.label else ""
    print(f"{head}{len(frames)} フレーム  {frames[0].shape[1]}x{frames[0].shape[0]}")
    print("  --- 内容に依存する指標(同じ静止画面同士でのみ比較可)---")
    print(f"  dark_frac   {np.mean(dark_frac):.4f}  (完全な黒の割合)")
    print(f"  speckle     {np.mean(speckle):.0f} 画素/フレーム "
          f"(1〜3LSBだけ光る点ノイズ)")
    print(f"  speckle_ppm {np.mean(speckle) / npx * 1e6:.0f} ppm")
    print(f"  mean_dark   {np.mean(mean_dark):.2f} / 255")
    print("  --- 時間差分(内容にほぼ依存しない。これで比較する)---")
    print(f"  flicker     {flick*100:.2f} %  (暗部でフレーム毎に値が変わる画素の割合)")
    print(f"  flick_lvl   {flick_lvl:.2f} / 255  (そのずれの平均振幅)")
    print(f"  temporal_std {temporal_std:.3f} / 255  (全画素の時間標準偏差)")
    # チャネル別(1本だけ外して比較する切り分けに使う。同期は繋いだままにすること)
    print("  --- チャネル別の時間標準偏差 ---")
    for ci, cname in enumerate("RGB"):
        cstd = float(stack[:, :, :, ci].std(axis=0).mean())
        # 暗部限定(明部の動きを除く)
        dmask = med[:, :, ci] <= 3 * LSB
        cdark = (float(stack[:, :, :, ci].std(axis=0)[dmask].mean())
                 if dmask.any() else 0.0)
        print(f"  {cname}  全体 {cstd:6.3f}   暗部 {cdark:6.3f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
