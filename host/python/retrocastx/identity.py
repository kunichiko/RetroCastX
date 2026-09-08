"""ボードの個体設定(名前 / 静的IP)を見る・変える・EEPROMへ焼く。

    python3 -m retrocastx.identity                       # いまの設定を見る
    python3 -m retrocastx.identity --name x68k-desk      # 名前を変える(揮発)
    python3 -m retrocastx.identity --name x68k-desk --save
    python3 -m retrocastx.identity --ip 192.168.10.50 --save
    python3 -m retrocastx.identity --ip auto --save      # リンクローカルに戻す

**--save するまで電源で消える。** 試してから決められるようにしてある。
逆に言えば、焼いた設定を戻したいのに手が無い状況は作らない。

**設定を間違えても基板は行方不明にならない。** 受信は宛先IPを見ずに通し
(LiteEth with_broadcast)、応答は受信パケットから学習したMACへ返すので、
サブネットが食い違うPCへ直結してもブロードキャストで見つかる。
その場で `--ip auto --save` すれば戻せる。
"""
import argparse
import socket
import sys
import time

from . import discover as disc
from . import netutil
from . import protocol as proto
from .cfg import Cfg

NAME_KEYS = (proto.CFG_KEY_NAME0, proto.CFG_KEY_NAME1,
             proto.CFG_KEY_NAME2, proto.CFG_KEY_NAME3)

EE_ERR = {0: "OK", 1: "未設定", 2: "壊れている(チェックサム不一致)",
          3: "EEPROMが応答しない"}
SAVE_STATE = {0: "未実行", 1: "実行中", 2: "成功", 3: "失敗"}


def name_to_words(name: str):
    """16バイトNUL詰めを4つの u32(リトルエンディアン)にする。"""
    b = name.encode("utf-8")
    if len(b) > 16:
        raise SystemExit(
            "名前が長すぎます: %r は UTF-8 で %d バイト(上限16)。\n"
            "  ANNOUNCE の name フィールドが16バイト固定です。"
            "日本語なら1文字3バイトなので5文字までになります。" % (name, len(b)))
    b = b.ljust(16, b"\0")
    return [int.from_bytes(b[i:i + 4], "little") for i in range(0, 16, 4)]


def words_to_name(words) -> str:
    b = b"".join(w.to_bytes(4, "little") for w in words)
    return b.split(b"\0")[0].decode("utf-8", "replace")


def read_identity(c: Cfg):
    """いまの設定を読む。1つでも欠けたら None。"""
    out = {}
    for tag, key in (("flags", proto.CFG_KEY_NET_MODE),
                     ("ip", proto.CFG_KEY_STATIC_IP),
                     ("save", proto.CFG_KEY_IDENT_SAVE),
                     ("ee", proto.CFG_KEY_IDENT_EE)):
        v = c.get(key)
        if v is None:
            return None
        out[tag] = v
    words = []
    for key in NAME_KEYS:
        v = c.get(key)
        if v is None:
            return None
        words.append(v)
    out["name"] = words_to_name(words)
    return out


def show(info, src_ip=None):
    ee = info["ee"]
    err = (ee >> 1) & 0x3
    print("  名前       : %r" % info["name"])
    if info["flags"] & 0x01:
        ip = socket.inet_ntoa(info["ip"].to_bytes(4, "big"))
        print("  IP         : 静的 %s" % ip)
        if info["ip"] == 0:
            print("               ★0.0.0.0 は静的扱いしないので、実際は"
                  "リンクローカルで動いています")
    else:
        print("  IP         : 自動(MAC由来のリンクローカル 169.254.x.y)")
    if src_ip:
        print("  実アドレス : %s" % src_ip)
    print("  EEPROM     : %s%s" % (
        "設定ページ有効" if ee & 1 else "設定ページ無し",
        "" if err == 0 else " (%s)" % EE_ERR.get(err, "?")))
    print("  最後の保存 : %s" % SAVE_STATE.get(info["save"], "?"))


def pick_board(sock, port, bind, mac_arg, timeout=2.0):
    """対象のボードを1枚に決める。複数居たら --mac を要求する。

    ★**既定で全台に撃たない。** 名前や静的IPは個体ごとの設定なので、
      ブロードキャストのまま SET すると同じLANの全ボードが同名・同IPになる
      (静的IPの重複は、症状が「たまに繋がらない」になって原因が見えにくい)。
    """
    boards = disc.probe(port=port, bind=bind, timeout=timeout, sock=sock)
    if mac_arg:
        want = bytes(int(x, 16) for x in mac_arg.replace("-", ":").split(":"))
        for b in boards:
            if b.mac == want:
                return b
        # 見つからなくても指名して撃つことはできる(ANNOUNCEを取りこぼした場合)
        print("警告: 指定したMAC %s のボードは見つかりませんでしたが、"
              "指名して送ります。" % mac_arg, file=sys.stderr)
        return disc.Found(mac=want, ip=None, name="?", fw=0, caps=0, port=port)
    if not boards:
        raise SystemExit(
            "ボードが見つかりません。\n"
            "  VPN接続中は限定ブロードキャストが落ちることがあります"
            "(--bind で使うNICを指定してください)。")
    if len(boards) > 1:
        print("ボードが %d 枚見つかりました。--mac で1枚を指名してください:"
              % len(boards), file=sys.stderr)
        for b in boards:
            print("  %s  %s  name=%r" % (
                ":".join("%02x" % x for x in b.mac), b.ip, b.name), file=sys.stderr)
        raise SystemExit(1)
    return boards[0]


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter,
                                 epilog=__doc__)
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--bind", default="0.0.0.0")
    ap.add_argument("--mac", help="対象ボードのMAC(複数台のときは必須)")
    ap.add_argument("--name", help="ボード名(UTF-8で16バイトまで)")
    ap.add_argument("--ip", help="静的IP(A.B.C.D)、または auto でリンクローカル")
    ap.add_argument("--save", action="store_true",
                    help="EEPROMへ焼く。これを付けないと電源で消える")
    args = ap.parse_args()

    # 先に socket を握ってから探す(ボードは固定ポートへ返すので1つで回す)
    c = Cfg("auto", args.port, bind=args.bind)
    board = pick_board(c.sock, args.port, args.bind, args.mac)
    c.mac = board.mac

    before = read_identity(c)
    if before is None:
        raise SystemExit(
            "設定キー(0x0046〜0x004D)に応答がありません。\n"
            "  このゲートウェアが個体設定に対応していない可能性があります"
            "(gw-v0.9.3 以前)。`python3 -m retrocastx.discover` で"
            "fw を確認してください。")

    print("現在:")
    show(before, board.ip)

    changed = False
    if args.name is not None:
        for key, w in zip(NAME_KEYS, name_to_words(args.name)):
            if c.set(key, w) is None:
                raise SystemExit("名前の書き込みに応答がありません (key %#06x)" % key)
        changed = True
    if args.ip is not None:
        if args.ip.lower() in ("auto", "link-local", "linklocal", "0"):
            if c.set(proto.CFG_KEY_NET_MODE, 0) is None:
                raise SystemExit("net_mode の書き込みに応答がありません")
        else:
            try:
                v = int.from_bytes(socket.inet_aton(args.ip), "big")
            except OSError:
                raise SystemExit("IPアドレスとして読めません: %r" % args.ip)
            if v == 0:
                raise SystemExit("0.0.0.0 は静的IPにできません(auto を使ってください)")
            # ★**IPを先、モードを後。** 逆にすると、新しいIPが入る前に静的モードへ
            #   切り替わり、一瞬だけ古い(あるいは 0.0.0.0 の)静的IPで名乗る。
            if c.set(proto.CFG_KEY_STATIC_IP, v) is None:
                raise SystemExit("静的IPの書き込みに応答がありません")
            if c.set(proto.CFG_KEY_NET_MODE, 1) is None:
                raise SystemExit("net_mode の書き込みに応答がありません")
        changed = True

    if changed:
        # IPを変えると応答元アドレスが変わる。ブロードキャストで送っているので
        # 追いかける必要は無いが、少し待ってから読み直す
        time.sleep(0.3)
        after = read_identity(c)
        print("\n変更後:")
        if after is None:
            print("  読み返せませんでした(IPが変わった直後は数秒かかることがあります)")
        else:
            show(after)

    if args.save:
        if c.set(proto.CFG_KEY_IDENT_SAVE, 1) is None:
            raise SystemExit("保存要求に応答がありません")
        # 焼くのは表示ループの合間(~33ms周期)+ 4ページ×書込み待ち
        state = None
        for _ in range(20):
            time.sleep(0.1)
            state = c.get(proto.CFG_KEY_IDENT_SAVE)
            if state in (2, 3):
                break
        if state == 2:
            print("\nEEPROMへ焼きました。次の電源投入からこの設定で上がります。")
        elif state == 3:
            raise SystemExit(
                "\nEEPROMへの書き込みに失敗しました。\n"
                "  EEPROM が応答していないか、書込み保護がかかっています。\n"
                "  `python3 -m retrocastx.cfg get 0x004D` で読み出し側の状態を"
                "確認してください。")
        else:
            raise SystemExit("\n保存の結果が取れませんでした(state=%s)" % state)
    elif changed:
        print("\n★まだEEPROMに焼いていません。電源を切ると元に戻ります。"
              "確定するなら --save を付けて実行してください。")


if __name__ == "__main__":
    main()
