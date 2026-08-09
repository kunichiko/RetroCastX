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


def _png(path: str, rgb: np.ndarray):
    """PILに依存せずRGB888のndarrayをPNGで書く。"""
    import struct, zlib
    h, w, _ = rgb.shape
    raw = b"".join(b"\x00" + rgb[y].tobytes() for y in range(h))

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 6)))
        f.write(chunk(b"IEND", b""))


def cmd_dump(board: Board, args):
    """フレームを保存し、縦方向の構造(繰り返し周期)を測る。

    「測定したvtotal」と「絵が実際に繰り返す周期」が食い違っていないかを見る。
    食い違っていれば、VSYNCを1つおきにしか受理できていない=インターレース。
    """
    imgs = board.frames(args.frames)
    board.drain(3.0)
    show_mode(board)
    if not imgs:
        print("フレームを受信できません")
        return
    img = imgs[-1]
    _png(args.out, img)
    print(f"{args.out} に保存 ({img.shape[1]}x{img.shape[0]})")

    y = luma(img)
    prof = y.mean(axis=1)
    lit = np.flatnonzero(prof > 0.5)
    if len(lit):
        print(f"内容のある行: {lit[0]} .. {lit[-1]}")
    # 自己相関で縦の繰り返し周期を探す。絵が1枚だけなら山は出ない
    d = prof - prof.mean()
    ac = np.correlate(d, d, mode="full")[len(d) - 1:]
    ac = ac / (ac[0] if ac[0] else 1.0)
    lo = 32
    k = int(np.argmax(ac[lo:len(ac) // 1])) + lo
    print(f"縦方向の自己相関ピーク: 周期 {k} 行 (相関 {ac[k]:.3f})")
    print("行平均輝度(16行ごと):")
    for i in range(0, len(prof), 16 * 8):
        print("  %4d: " % i + " ".join("%4.0f" % v for v in prof[i:i + 16 * 8:16]))


def set_reliable(board: Board, key: int, value: int, tries: int = 5) -> bool:
    """CONFIGは単発だと落ちることがあるので、応答で確認できるまで再送する。"""
    for _ in range(tries):
        board.set_cfg(key, value)
        end = time.monotonic() + 0.5
        while time.monotonic() < end:
            try:
                d, _ = board.sock.recvfrom(65535)
            except socket.timeout:
                continue
            try:
                t, p = proto.parse(d)
            except ValueError:
                continue
            if t == proto.TYPE_CONFIG and getattr(p, "key", None) == key \
                    and p.value == value:
                return True
    return False


def weave_error(y: np.ndarray, agree: float = 8.0, active: float = 20.0) -> float:
    """織り込みのずれ具合。小さいほど正しい。

    同一フィールドの上下(y-1, y+1)がよく一致している画素だけを見て、その間に
    挟まれた行(=別フィールド)が中間値からどれだけ外れるかを測る。上下が一致
    している場所は縦方向に滑らかなので、正しく織り込めていれば間の行も中間値
    付近に来るはず。フィールドの割当を間違えると絵の別の位置が入るので外れる。

    単純な「隣接行の差÷1行飛ばしの差」も試したが、黒地に細い線という絵では
    ノイズに埋もれて4通りの差が1%も出ず判別できなかった。上下が一致する場所に
    限ると差が1.7倍つく(実測)。絵の内容には依存しない。
    """
    up, dn, mid = y[:-2], y[2:], y[1:-1]
    m = (np.abs(up - dn) < agree) & (np.maximum(up, dn) > active)
    if m.sum() < 500:
        return float("nan")
    return float(np.abs(mid[m] - (up[m] + dn[m]) / 2).mean())


def cmd_weave(board: Board, args):
    """f2_row と field_swap の4通りを実測して、櫛が最小の組み合わせを選ぶ。

    vtotalが奇数なので2枚のフィールドの行数は1違う。どちらが長いかも、どちらが
    偶数ラインを描いているかも信号からは決まらないので、4通り試すしかない。
    """
    set_reliable(board, proto.CFG_KEY_INTERLACE, 1)
    board.drain(1.0)
    m = board.asm.mode
    if not m or not m.vtotal:
        print("MODEが取れません")
        return
    half = m.vtotal // 2
    print(f"vtotal={m.vtotal} → f2_row の候補 {half} / {half + 1}")
    # f2_row を1増やすのと swap を反転するのは、フィールドの相対位置に対しては
    # 打ち消し合う(実測でも (465,1) と (466,0) は同値だった)。両方振るのは、
    # 相対位置として -929 / -931 / -933 行の3通りを確かめるため。
    results = []
    for f2 in (half, half + 1):
        for sw in (0, 1):
            ok1 = set_reliable(board, proto.CFG_KEY_F2_ROW, f2)
            ok2 = set_reliable(board, proto.CFG_KEY_FIELD_SWAP, sw)
            board.drain(args.settle)
            imgs = board.frames(max(args.frames, 12), timeout=12)
            if not imgs or not (ok1 and ok2):
                print(f"  f2_row={f2} swap={sw}: 測定できず")
                continue
            y = np.mean([luma(i) for i in imgs], axis=0)
            c = weave_error(y)
            results.append((c, f2, sw))
            print(f"  f2_row={f2} swap={sw}: ずれ {c:.3f}")
    if not results:
        return
    results.sort()
    c, f2, sw = results[0]
    print(f"\n最良: f2_row={f2} swap={sw} (ずれ {c:.3f})")
    if len(results) > 1:
        print(f"  最大との比: {results[-1][0] / c:.2f}倍"
              " (1.3倍以上あれば判別できている)")
    set_reliable(board, proto.CFG_KEY_F2_ROW, f2)
    set_reliable(board, proto.CFG_KEY_FIELD_SWAP, sw)
    board.drain(1.0)
    imgs = board.frames(args.frames)
    if imgs and args.out:
        _png(args.out, imgs[-1])
        print(f"{args.out} に保存")


def _runs(mask):
    out = []
    start = None
    for i, v in enumerate(mask):
        if v and start is None:
            start = i
        elif not v and start is not None:
            out.append((start, i))
            start = None
    if start is not None:
        out.append((start, len(mask)))
    return out


def cmd_crosstalk(board: Board, args):
    """白ベタ領域の先頭が暗くなる量をチャンネル別に測る。

    暗→白の境目に入った直後、しばらく本来のレベルより低いままになることがある。
    原因はチャンネル間のクロストークで、TVPのCMオフセット(レジスタ2Ah bit7)や、
    基板側の配線長・GNDの引き回しで変わる。配線を直した前後の比較に使う。

    領域の末尾を100%として、先頭側(4〜18サンプル)と後半(26〜62サンプル)の
    平均を出す。差が大きいほど症状が重い。
    """
    imgs = board.frames(args.frames, timeout=30)
    if not imgs:
        print("フレームを受信できません")
        return
    a = np.mean([i.astype(np.float32) for i in imgs], axis=0)
    h, w, _ = a.shape
    show_mode(board)
    print(f"{len(imgs)}枚平均  白ベタ領域の先頭の落ち込み(領域末尾=100%)")
    for ci, nm in ((0, "R"), (1, "G"), (2, "B")):
        prof = []
        for r in range(h):
            mk = (a[r, :, 0] > 70) & (a[r, :, 1] > 70) & (a[r, :, 2] > 70)
            for s, e in _runs(mk):
                # 十分長い白ベタで、直前が暗いところだけ使う
                if e - s < 70 or s < 8 or a[r, s - 6:s - 1, :].max() > 40:
                    continue
                seg = a[r, s - 4:s + 66, ci]
                if len(seg) == 70:
                    prof.append(seg / max(a[r, e - 20:e, ci].mean(), 1e-6) * 100)
        if len(prof) < 10:
            print(f"  {nm}: 該当領域 {len(prof)} — 少なすぎ(絵に大きな白ベタが要る)")
            continue
        p = np.mean(prof, axis=0)
        head, tail = p[8:22].mean(), p[30:66].mean()
        print(f"  {nm}: {len(prof):3d}本  先頭 {head:5.1f}%  後半 {tail:5.1f}%  "
              f"差 {tail - head:+4.1f}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", required=True, help="ボードのIP")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--frames", type=int, default=4, help="1点あたり平均するフレーム数")
    ap.add_argument("--settle", type=float, default=0.6,
                    help="設定変更後にPLLが落ち着くのを待つ秒数")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("probe", help="今の映像を測る(外接矩形が飽和していないか等)")
    sub.add_parser("crosstalk", help="白ベタ先頭の落ち込みをチャンネル別に測る(配線改善の前後比較)")
    dp = sub.add_parser("dump", help="フレームをPNG保存し縦の繰り返し周期を測る")
    dp.add_argument("--out", default="frame.png")
    wv = sub.add_parser("weave", help="インターレースのフィールド割当を実測で決める")
    wv.add_argument("--out", default="weave.png")
    sw = sub.add_parser("sweep", help="pll_divideを振って鋭さが最大の点を探す")
    sw.add_argument("--center", type=int, required=True, help="探索の中心")
    sw.add_argument("--span", type=int, default=12, help="中心から±いくつ振るか")
    args = ap.parse_args()

    board = Board(args.board, args.port)
    print(f"ボード {args.board}:{args.port} に購読中...")
    board.drain(1.0)
    if args.cmd == "probe":
        cmd_probe(board, args)
    elif args.cmd == "crosstalk":
        cmd_crosstalk(board, args)
    elif args.cmd == "dump":
        cmd_dump(board, args)
    elif args.cmd == "weave":
        cmd_weave(board, args)
    else:
        cmd_sweep(board, args)


if __name__ == "__main__":
    main()
