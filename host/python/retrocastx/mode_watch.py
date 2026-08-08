"""ボードが実測したタイミングを監視し、変化したら1行出す(モード表を作るための調査用)。

Usage:
    python3 -m retrocastx.mode_watch [--board IP] [--seconds 0]   # 0=Ctrl-Cまで

X68000側でモードを切り替えながら実行すると、モードごとの実測値が並ぶ。
その結果からゲートウェアのモード表(fH/vtotal → pll_divide/位置)を作る。

MODEパケットの諸元はボードがFPGA内で実測した値:
  htotal   1ライン当たりDATACLK数(=現在のH-PLL分周比。設定値がそのまま出る)
  vtotal   1フレーム当たりライン数(実測)
  dotclk   DATACLK周波数[Hz](実測)
  hfreq    水平周波数[Hz](実測)
  vfreq    垂直周波数[mHz](8秒積算)
"""
import argparse
import socket
import time

from . import protocol as proto


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--board", default="192.168.10.50")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--seconds", type=float, default=0.0, help="0でCtrl-Cまで")
    args = ap.parse_args()

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
    s.bind(("0.0.0.0", args.port))
    s.settimeout(0.5)
    sub = proto.pack_subscribe(1, announce_only=False)
    s.sendto(sub, (args.board, args.port))
    last_sub = t0 = time.time()

    print("時刻     htotal vtotal   dotclk[MHz]  fH[kHz]  fV[Hz]   (変化時のみ表示)")
    prev = None
    try:
        while args.seconds <= 0 or time.time() - t0 < args.seconds:
            if time.time() - last_sub > 2.0:
                s.sendto(sub, (args.board, args.port))
                last_sub = time.time()
            try:
                data, _ = s.recvfrom(2048)
            except socket.timeout:
                continue
            # 映像は数万パケット/秒来るので、typeバイトだけ見て安価に捨てる
            if len(data) < 3 or data[2] != proto.TYPE_MODE:
                continue
            try:
                ptype, m = proto.parse(data)
            except Exception:
                continue
            key = (m.htotal, m.vtotal, m.dotclk_hz // 10000,
                   m.hfreq_mhz_x1000 // 10000, m.vfreq_mhz_x1000 // 100)
            if key == prev:
                continue
            prev = key
            print(f"{time.strftime('%H:%M:%S')}  {m.htotal:6d} {m.vtotal:6d}  "
                  f"{m.dotclk_hz/1e6:10.4f}  {m.hfreq_mhz_x1000/1e6:7.3f}  "
                  f"{m.vfreq_mhz_x1000/1e3:6.2f}   "
                  f"active {m.hactive}x{m.vactive}")
    except KeyboardInterrupt:
        print("\n(終了)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
