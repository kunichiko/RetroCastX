#!/usr/bin/env python3
"""pll_divide(サンプリング周波数)を実測で決めるための調査/自動調整ツール。

pll_divide は「1ラインを何サンプルで取り込むか」なので、入力の水平トータル
[ドット]に一致させると1サンプル=1ドットになる。ずれていると隣り合うドットが
混ざったり同じドットを2回取ったりして、細かい模様にビート(モアレ)が出る。

合わせ方の指標は2つある。

probe: 有効映像の外接矩形を測る
    「有効映像の幅=そのモードのドット数」で合わせる考え方。ただし外接矩形は
    絵の内容に依存するうえ、ノイズがあるとしきい値次第で簡単に飽和する。
    どのしきい値でどこまで飽和するかを見るために、複数のしきい値で同時に測る。

sweep: 水平方向の鋭さ(隣接サンプル差の二乗和)が最大になる点を探す
    サンプリングが正確に1ドット=1サンプルのとき、ドットの境目はちょうど1
    サンプル分の段差として現れ、二乗和が最大になる。ずれると段差が2サンプルに
    分かれて二乗和が落ちる(段差Aを2つに割ると 2*(A/2)^2 = A^2/2)。
    モニタの「自動調整」と同じ原理で、絵の内容にほとんど依存しない。

使い方:
    python3 -m retrocastx.pll_tune --board 192.168.10.50 probe
    python3 -m retrocastx.pll_tune --board 192.168.10.50 sweep --center 1104
"""
import argparse
import socket
import time

import numpy as np

from . import protocol as proto
from .receiver import FrameAssembler


class Board:
    """SUBSCRIBEを維持しつつフレームを受け、CONFIGを送る。"""

    def __init__(self, board_ip: str, port: int):
        self.ip = board_ip
        self.port = port
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
        self.sock.bind(("0.0.0.0", port))
        self.sock.settimeout(0.2)
        self.asm = FrameAssembler()
        self.seq = 0
        self.last_sub = 0.0

    def _keepalive(self):
        now = time.monotonic()
        if now - self.last_sub >= 2.0:
            self.sock.sendto(proto.pack_subscribe(self.seq), (self.ip, self.port))
            self.seq = (self.seq + 1) & 0xFFFF
            self.last_sub = now

    def set_cfg(self, key: int, value: int):
        self.sock.sendto(
            proto.pack_config(self.seq, proto.CFG_TARGET_BOARD, proto.CFG_OP_SET,
                              key, value),
            (self.ip, self.port))
        self.seq = (self.seq + 1) & 0xFFFF

    def frames(self, count: int, timeout: float = 5.0):
        """完成フレームを count 枚集めて返す。"""
        out = []
        deadline = time.monotonic() + timeout
        while len(out) < count and time.monotonic() < deadline:
            self._keepalive()
            try:
                datagram, _ = self.sock.recvfrom(65535)
            except socket.timeout:
                continue
            for _idx, img, _fill in self.asm.feed(datagram):
                out.append(img)
        return out

    def drain(self, seconds: float):
        """指定時間ぶん受信だけして捨てる(設定変更の反映待ち)。"""
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            self._keepalive()
            try:
                datagram, _ = self.sock.recvfrom(65535)
            except socket.timeout:
                continue
            self.asm.feed(datagram)


def luma(img: np.ndarray) -> np.ndarray:
    """RGB888 HxWx3 → 輝度 float32 HxW。"""
    f = img.astype(np.float32)
    return 0.299 * f[:, :, 0] + 0.587 * f[:, :, 1] + 0.114 * f[:, :, 2]


def sharpness(img: np.ndarray) -> float:
    """水平方向の隣接差の二乗和。1サンプル=1ドットのとき最大になる。"""
    y = luma(img)
    d = np.diff(y, axis=1)
    return float(np.sum(d * d))


def bbox_at(y: np.ndarray, th: float, min_frac: float):
    """しきい値thを超える画素が、行/列の min_frac 以上ある範囲の外接矩形。"""
    h, w = y.shape
    m = y > th
    rows = m.sum(axis=1) >= max(1, int(w * min_frac))
    cols = m.sum(axis=0) >= max(1, int(h * min_frac))
    if not rows.any() or not cols.any():
        return None
    r = np.flatnonzero(rows)
    c = np.flatnonzero(cols)
    return int(c[0]), int(r[0]), int(c[-1] - c[0] + 1), int(r[-1] - r[0] + 1)


def show_mode(board: Board):
    m = board.asm.mode
    if not m:
        print("MODE 未受信")
        return
    print(f"MODE {m.hactive}x{m.vactive}  htotal={m.htotal} vtotal={m.vtotal}  "
          f"dotclk={m.dotclk_hz/1e6:.3f}MHz  fH={m.hfreq_mhz_x1000/1e6:.3f}kHz  "
          f"fV={m.vfreq_mhz_x1000/1e6:.3f}Hz")


def cmd_probe(board: Board, args):
    imgs = board.frames(args.frames)
    board.drain(3.0)   # MODEは数秒に1回しか来ないので待つ
    show_mode(board)
    if not imgs:
        print("フレームを受信できません(ボード/入力信号を確認)")
        return
    y = np.mean([luma(i) for i in imgs], axis=0)
    h, w = y.shape
    print(f"フレーム {len(imgs)}枚 平均  バッファ {w}x{h}")
    print(f"輝度: 最小 {y.min():.1f}  中央 {np.median(y):.1f}  "
          f"平均 {y.mean():.1f}  最大 {y.max():.1f}")
    print()
    print("しきい値ごとの外接矩形(飽和していないか確認する):")
    print("  th  frac       x    y      w    h")
    for th in (8, 16, 24, 48, 96):
        for frac in (0.008, 0.05, 0.20, 0.50):
            b = bbox_at(y, th, frac)
            s = "なし" if b is None else "%5d %4d  %5d %4d" % b
            print(f"  {th:3d} {frac:5.3f}   {s}")
    print()
    # 列ごとの明るさプロファイル。有効映像の右端がどこで切れているかを見る
    prof = y.mean(axis=0)
    nz = np.flatnonzero(prof > 1.0)
    if len(nz):
        print(f"列平均輝度が1.0を超える範囲: {nz[0]} .. {nz[-1]} (幅 {nz[-1]-nz[0]+1})")
    print("列平均輝度(32列ごと):")
    print("  " + " ".join("%4.0f" % v for v in prof[::32]))
    print(f"\nsharpness = {sharpness(imgs[0]) / 1e6:.2f} M")


def measure(board: Board, pll: int, args) -> dict:
    board.set_cfg(proto.CFG_KEY_PLL_DIVIDE, pll)
    board.drain(args.settle)
    imgs = board.frames(args.frames)
    if not imgs:
        return {"pll": pll, "sharp": 0.0, "box": None}
    y = np.mean([luma(i) for i in imgs], axis=0)
    return {
        "pll": pll,
        "sharp": float(np.mean([sharpness(i) for i in imgs])) / 1e6,
        "box": bbox_at(y, 24, 0.20),
    }


def cmd_sweep(board: Board, args):
    """1刻みで振り、なだらかな背景を除いた「局所ピーク」を探す。

    生の二乗和はpll_divideが小さいほど大きく出る強い偏りがある(オーバー
    サンプリングになるほど、有限帯域の立ち上がりが多くのサンプルに分散して
    1サンプルあたりの段差が小さくなるため)。さらにpllを下げすぎると今度は
    ドットを飛ばす(アンダーサンプリング)ので、値は大きいが絵は正しくない。
    したがって「最大値」ではなく、近傍の中央値からどれだけ突出しているかで
    判定する。1ドット=1サンプルが成立する点だけが局所的に跳ね上がる。
    """
    plls = list(range(args.center - args.span, args.center + args.span + 1))
    print(f"探索 {plls[0]}..{plls[-1]} step 1 ({len(plls)}点, "
          f"約{len(plls) * (args.settle + 0.3):.0f}秒)")
    res = []
    for pll in plls:
        r = measure(board, pll, args)
        res.append(r)
        box = r["box"]
        print("  pll %4d  sharp %8.2f M  box %s" % (
            r["pll"], r["sharp"],
            "なし" if box is None else "%dx%d at (%d,%d)" % (box[2], box[3], box[0], box[1])))

    sharp = np.array([r["sharp"] for r in res])
    # 近傍中央値との比。両端は窓が片側になるので評価対象から外す
    k = max(2, args.span // 4)
    ratio = np.zeros(len(res))
    for i in range(len(res)):
        lo, hi = max(0, i - k), min(len(res), i + k + 1)
        base = np.median(np.delete(sharp[lo:hi], i - lo))
        ratio[i] = sharp[i] / base if base > 0 else 0.0
    inner = slice(k, len(res) - k) if len(res) > 2 * k else slice(0, len(res))

    print("\n突出度(近傍中央値との比)の上位5点:")
    order = np.argsort(-ratio)
    shown = [i for i in order if inner.start <= i < inner.stop][:5]
    for i in shown:
        print("  pll %4d  sharp %8.2f M  突出度 %.3f" % (
            res[i]["pll"], res[i]["sharp"], ratio[i]))
    if not shown:
        print("  (点数が足りません。--span を大きくしてください)")
        return
    best = res[shown[0]]
    peak = ratio[shown[0]]
    print(f"\n最良: pll_divide = {best['pll']}  突出度 {peak:.3f}")
    if peak < 1.15:
        print("  ※突出が弱い。絵に細かい模様(文字など)が出ている状態で測ると出やすい")
    board.set_cfg(proto.CFG_KEY_PLL_DIVIDE, best["pll"])
    board.drain(0.5)
    print("この値をボードに設定した(電源を切ると既定値に戻る)")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", required=True, help="ボードのIP")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--frames", type=int, default=4, help="1点あたり平均するフレーム数")
    ap.add_argument("--settle", type=float, default=0.6,
                    help="設定変更後にPLLが落ち着くのを待つ秒数")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("probe", help="今の映像を測る(外接矩形が飽和していないか等)")
    sw = sub.add_parser("sweep", help="pll_divideを振って鋭さが最大の点を探す")
    sw.add_argument("--center", type=int, required=True, help="探索の中心")
    sw.add_argument("--span", type=int, default=12, help="中心から±いくつ振るか")
    args = ap.parse_args()

    board = Board(args.board, args.port)
    print(f"ボード {args.board}:{args.port} に購読中...")
    board.drain(1.0)
    if args.cmd == "probe":
        cmd_probe(board, args)
    else:
        cmd_sweep(board, args)


if __name__ == "__main__":
    main()
