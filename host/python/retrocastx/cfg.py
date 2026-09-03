"""CONFIG パケットを任意のキーに対して送る/読む汎用ツール。

既存のツール(pll_tune / vs_probe / span_check)は用途ごとにキーを決め打ちして
いるので、「レジスタを1個だけ振って様子を見る」ができなかった。TVP7002 の
レジスタ調整は実機を見ながらのA/Bが基本なので、その足回りとして用意した。

使い方:

    # 現在値を読む(GET)
    python -m retrocastx.cfg --board 192.168.10.10 get 0x22

    # 設定する(SET)。反映値が返るので、そのまま確認になる
    python -m retrocastx.cfg --board 192.168.10.10 set 0x22 0x5B

    # まとめて読む
    python -m retrocastx.cfg --board 192.168.10.10 dump

    # 値を振る(1点ごとに待つ。何が起きたかは絵/オシロ/別ツールで見る)
    python -m retrocastx.cfg --board 192.168.10.10 sweep 0x22 0x52 0x53 0x5B

応答が来ないとき、まず疑うのは**キー未対応ではなくネットワーク**。実際に踏んだ順に:

1. **PCとボードでサブネットが違うと、ユニキャストは届かない。** ボードのIPが
   192.168.10.x でPCが 192.168.11.x だと、`--board 192.168.10.50` はデフォルト
   ゲートウェイに流れてボードに着かない。`--board 255.255.255.255` を使う。
   `discover` だけは動くので勘違いしやすい(ANNOUNCE はL2ブロードキャストで、
   経路と無関係に届く)。
2. **ブロードキャストは自分自身にもループバックする。** 受信側で flags の
   REPLY ビット(`Config.is_reply`)を見ないと、自分が送った要求を応答と誤認して
   「SETは常に成功、GETは常に送った値」に見える。

そのうえでキー未対応も起こる(古いファームは未知のキーを黙って捨てる)ので、
SET のあとは必ず**別コマンドで**読み返して一致を確かめる。同じコマンド内の
読み返しでは上記2を取り逃す。
"""
import argparse
import socket
import time

from . import protocol as proto

# 実行時に振れるキー(gateware/retrocastx_stream.py の cfg_* と対応)。
# ここに無いキーも --board 指定で送れる(数値を直接渡せばよい)。
KNOWN = [
    (0x0001, "audio_enable_mask", "bit0=RGB音声 bit1=LINE bit2=S/PDIF"),
    (0x0010, "vbp",               "キャプチャ窓の先頭をVSYNCの何行後にするか"),
    (0x0011, "hs_offset",         "水平バックポーチ[DATACLK]"),
    (0x0012, "pll_divide",        "H-PLL帰還分周比=1ライン当たりDATACLK数"),
    (0x0013, "interlace",         "インターレース(ウィーブ)"),
    (0x0014, "f2_row",            "第2フィールドが始まる row"),
    (0x0015, "field_swap",        "フィールドの偶奇入替"),
    (0x0017, "video_bw",          "reg 3Fh アナログ映像帯域 0=最大〜15=最小"),
    (0x0018, "fine_clamp",        "reg 2Ah ファインクランプ"),
    (0x0019, "pll_ctl",           "reg 03h H-PLL(VCOレンジ+チャージポンプ)"),
    (0x001A, "clamp_start",       "reg 05h クランプ開始位置"),
    (0x001B, "clamp_width",       "reg 06h クランプ幅"),
    (0x001C, "gain_b",            "reg 08h 青ファインゲイン"),
    (0x001D, "gain_g",            "reg 09h 緑ファインゲイン"),
    (0x001E, "gain_r",            "reg 0Ah 赤ファインゲイン"),
    (0x001F, "phase",             "reg 04h サンプル位相"),
    (0x0022, "sync_ctl",          "reg 0Eh 同期制御 0x52=5線 / 0x53=4線CSYNC / 0x5B=SOG"),
    (0x0030, "full_line",         "1ラインまるごと送る(診断)"),
    (0x0036, "pixfmt",            "伝送形式 1=RGB555(既定) 3=YC8(生8bit、CVBS用)"),
    (0x0050, "sog_thresh",        "reg 10h bit[7:3] SOGスライス閾値(5bit)"),
    (0x0051, "sep_thresh",        "reg 11h 同期セパレータ閾値"),
    (0x0052, "precoast",          "reg 12h プリコースト[ライン] 最低1が必要"),
    (0x0053, "postcoast",         "reg 13h ポストコースト[ライン] 最低1が必要"),
    (0x0054, "syncdet",           "reg 14h 同期検出ステータス(読専)"),
    (0x0055, "lines_per_frame",   "reg 37h:38h TVPが測ったライン数/フレーム(読専)"),
    (0x0056, "clocks_per_line",   "reg 39h:3Ah TVPが測ったDATACLK数/ライン(読専)"),
    (0x0057, "lpf_msbs",          "reg 38h 生値 bit5=P/I detect 0=インターレース(読専)"),
    (0x0058, "fh_tvp",            "TVP HSOUT周波数[Hz] sysクロック基準の絶対値(読専)"),
    (0x0059, "fv_tvp",            "TVP VSOUT周波数[mHz] 同(読専)"),
    (0x005A, "lines_tvp",         "VSOUT間のHSOUT数=vtotal 同(読専)"),
    (0x005B, "sync_ctl2",         "reg 22h bit0=VS Bypass bit1=VS Select 既定0x08"),
    (0x005C, "sync_bypass",       "reg 36h bit0=HS BP bit1=VS BP 生同期素通し(診断)"),
    (0x005D, "in_mux2",           "reg 1Ah SOG LPF[7:6]/クランプLPF[5:4] 15kHzは0x12"),
    (0x005E, "clamp_sel",         "reg 10h[2:0] クランプ基準 bit2=B bit1=G bit0=R 1=ミッド"),
    (0x005F, "coarse_gain_gb",    "reg 1Bh 粗ゲイン G[7:4]/B[3:0] Gain=0.5+N/10 既定0x77"),
    (0x0065, "coarse_off_g",      "reg 1Fh[5:0] 粗オフセットG 10h=+64c 1Fh=+124c"),
    (0x0067, "coarse_gain_r",     "reg 1Ch 粗ゲインR [3:0]のみ Gain=0.5+N/10 既定0x07"),
    (0x0068, "coarse_off_r",      "reg 20h[5:0] 粗オフセットR 既定0x10"),
    (0x0069, "in_mux1",           "reg 19h 入力MUX SOG[7:6]/R[5:4]/G[3:2]/B[1:0] 10=_3"),
    (0x0060, "sog_hlen",          "SOGOUTの水平周期[pixクロック](読専)"),
    (0x0061, "sog_lowmax",        "SOGOUTの最長Low期間[pixクロック](読専)"),
    (0x0062, "sog_vphase",        "垂直区間開始の半ライン位相[pixクロック](読専)"),
    (0x0063, "sog_vlines",        "垂直区間の間隔[水平エッジ数](読専)"),
    (0x0064, "sog_vth",           "垂直とみなすLow期間の閾値[pixクロック] 既定400"),
    (0x0066, "field_invert",      "インターレースのフィールド極性を入れ替える(0/1)"),
]


def _num(s: str) -> int:
    """0x22 / 34 / 0b1010 のいずれも受ける。"""
    return int(s, 0)


class Cfg:
    def __init__(self, ip: str, port: int, timeout: float = 0.3,
                 bind: str = "0.0.0.0"):
        self.dst = (ip, port)
        self.seq = 1
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.settimeout(timeout)
        # ★ボードは応答を「固定ポート(既定34600)」へ返す。送信元ポートへは返さない。
        #   エフェメラルポートにbindすると応答が受け取れず「キー未対応」に見えるので、
        #   受信ポートに合わせてbindする(他のツールも同じ流儀)。
        #   Viewerが起動中は同じポートを掴んでいるので bind に失敗する。
        #
        # ★**VPN を張っていると 0.0.0.0 では 255.255.255.255 へ送れない**
        #   (2026-09-02)。VPN が既定経路を握ると限定ブロードキャストが
        #   point-to-point の utun に載ろうとして sendto が
        #   `OSError: [Errno 49] Can't assign requested address` で落ちる。
        #   `--bind 192.168.11.24` のようにボードと同じL2にいるIFのアドレスを
        #   指定すれば、既定経路を迂回して出ていく。
        try:
            self.sock.bind((bind, port))
        except OSError as e:
            raise SystemExit(
                "UDP %d を %s で bind できません (%s)\n"
                "Viewer など同じポートを使うアプリを閉じてから実行してください。"
                % (port, bind, e))

    def _xfer(self, op: int, key: int, value: int = 0, retries: int = 4):
        """SET/GET を送って REPLY の value を返す。来なければ None。"""
        for _ in range(retries):
            self.sock.sendto(
                proto.pack_config(self.seq, proto.CFG_TARGET_BOARD, op, key, value),
                self.dst)
            self.seq = (self.seq + 1) & 0xFFFF
            end = time.monotonic() + 0.3
            while time.monotonic() < end:
                try:
                    d, _ = self.sock.recvfrom(65535)
                except socket.timeout:
                    break
                try:
                    kind, pkt = proto.parse(d)
                except ValueError:
                    continue
                # ★is_reply を必ず見る。ブロードキャストで送ると自分の送信パケットが
                #   同じポートにループバックしてくるので、flags を見ないと「要求の
                #   value をそのまま読んだ」ことになり、SETが常に成功に見える。
                if kind != proto.TYPE_CONFIG or not pkt.is_reply:
                    continue
                if pkt.key == key:
                    return pkt.value
        return None

    def get(self, key: int):
        return self._xfer(proto.CFG_OP_GET, key)

    def set(self, key: int, value: int):
        return self._xfer(proto.CFG_OP_SET, key, value)


def _label(key: int) -> str:
    for k, name, desc in KNOWN:
        if k == key:
            return name
    return "?"


def _no_reply(args):
    """応答が無いときの案内。

    「キー未対応」だけを出していると、実際には**経路の問題**なのに
    ファームの機能不足だと誤読する(2026-09-03 に実際にやった)。

    いちばん多いのは **VPN接続中にボードのIPをユニキャストで指定した**場合。
    既定経路がVPNトンネルなので、ボードが同じL2にいてもパケットは
    トンネルへ吸われて届かない:

        route -n get 192.168.10.50
          gateway: 172.23.60.199   interface: utun4    ← ここ

    このとき ANNOUNCE はブロードキャストなので discover では見つかってしまい、
    「ボードは居るのに CONFIG だけ通らない」という紛らわしい形になる。
    --board 255.255.255.255 にすれば discover と同じ経路で届く
    (ボードは ArpLearner が受信パケットから相手のMACを学習して返す)。
    """
    print("応答なし: key 0x%04X" % args.key)
    if args.board != "255.255.255.255":
        print("  ★まず --board 255.255.255.255 を試してください。")
        print("    VPN接続中はボードのIPを直接指定すると、同じL2にいても")
        print("    既定経路のトンネルへ吸われて届きません。確認:")
        print("      route -n get %s   # interface が utun* ならこれ" % args.board)
        print("    ブロードキャストなら discover と同じ経路で届きます。")
    else:
        print("  このファームでは未対応のキーかもしれません。")
        print("  Viewerが同じポートを掴んでいないかも確認してください。")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--board", required=True,
                    help="ボードのIP(255.255.255.255 でブロードキャストも可)")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--bind", default="0.0.0.0",
                    help="受信ソケットを縛るローカルアドレス。**VPN接続中は必須**: "
                         "既定経路がVPNだと 255.255.255.255 への送信が "
                         "EADDRNOTAVAIL で落ちる。ボードと同じL2にいるIFの "
                         "アドレスを指定する(例 192.168.11.24)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    g = sub.add_parser("get", help="現在値を読む")
    g.add_argument("key", type=_num)

    s = sub.add_parser("set", help="設定して反映値を読み返す")
    s.add_argument("key", type=_num)
    s.add_argument("value", type=_num)

    sub.add_parser("dump", help="既知のキーをまとめて読む")

    sw = sub.add_parser("sweep", help="値を順に設定して都度待つ")
    sw.add_argument("key", type=_num)
    sw.add_argument("values", nargs="+", type=_num)
    sw.add_argument("--settle", type=float, default=2.0, help="1点あたり待つ秒数")

    sub.add_parser("keys", help="既知のキー一覧を表示")

    args = ap.parse_args()

    if args.cmd == "keys":
        print("%-8s %-20s %s" % ("key", "name", "説明"))
        for k, name, desc in KNOWN:
            print("0x%04X   %-20s %s" % (k, name, desc))
        return

    c = Cfg(args.board, args.port, bind=args.bind)

    if args.cmd == "get":
        v = c.get(args.key)
        if v is None:
            _no_reply(args)
            raise SystemExit(1)
        print("key 0x%04X (%s) = 0x%X (%d)" % (args.key, _label(args.key), v, v))

    elif args.cmd == "set":
        v = c.set(args.key, args.value)
        if v is None:
            _no_reply(args)
            raise SystemExit(1)
        ok = "OK" if v == args.value else "★不一致(値が丸められた/未対応の可能性)"
        print("key 0x%04X (%s): 要求 0x%X → 反映 0x%X  %s"
              % (args.key, _label(args.key), args.value, v, ok))
        if v != args.value:
            raise SystemExit(1)

    elif args.cmd == "dump":
        print("%-8s %-20s %-12s %s" % ("key", "name", "value", "説明"))
        for k, name, desc in KNOWN:
            v = c.get(k)
            shown = "-" if v is None else "0x%X (%d)" % (v, v)
            print("0x%04X   %-20s %-12s %s" % (k, name, shown, desc))

    elif args.cmd == "sweep":
        for val in args.values:
            v = c.set(args.key, val)
            shown = "応答なし" if v is None else "0x%X" % v
            print("key 0x%04X = 0x%X → 反映 %s   (%.1fs 待機)"
                  % (args.key, val, shown, args.settle))
            time.sleep(args.settle)


if __name__ == "__main__":
    main()
