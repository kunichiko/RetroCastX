#!/usr/bin/env python3
"""ブロードキャストの宛先を「NICごとのサブネット宛」で決める。

★**限定ブロードキャスト(255.255.255.255)を宛先にしてはいけない。**

限定ブロードキャストはサブネット経路を持たないので既定経路に載る。既定経路が
point-to-point の VPN トンネル(utun*)だと送信元アドレスを割り当てられず、

    OSError: [Errno 49] Can't assign requested address

で **sendto そのものが落ちる**。`discover` はこの例外で終了してしまうため受動
リッスンまで到達せず、症状が「ボードが見つからない」になる。ボードの故障や
ケーブル断と見分けがつかない形で失敗するので、何度も時間を溶かした
(2026-09-02 / 2026-09-04)。

サブネット宛(例 192.168.11.255)なら経路が「そのNICのサブネット経路」に決まる
ので、既定経路を一切参照しない。実測(VPN接続中、bind は 0.0.0.0 のまま):

    255.255.255.255 → EADDRNOTAVAIL
    192.168.11.255  → 通る

**ボードと別サブネットでも届く。** ボード 192.168.10.50 / PC 192.168.11.24 で
確認済み。同じL2にいればL2ブロードキャストで届き、返りはボード側の
`retrocastx_net.py`(受信パケットから相手のMACを学習する層)が処理する。

Viewer(client/src/receiver.rs の `broadcast_targets`)と同じ判定をしている。
あちらは `if-addrs` クレートでNICを列挙する。

## psutil が無いときは今までどおりに落ちる

NICの列挙には netmask/broadcast が要るが、Python の標準ライブラリには
取る手段が無い(getifaddrs 相当が無い)。psutil があればそれを使い、無ければ
限定ブロードキャストに落ちる = VPN下では従来と同じく失敗する。そのときは
`--bind` を指定すれば迂回できる。psutil を入れるなら:

    pip install psutil
"""
import socket

LIMITED = "255.255.255.255"


def pick_targets(ifs, pinned=None):
    """NICの列挙結果から宛先を選ぶ。**純関数**(試験用に切り出してある)。

    ifs   … (ip, broadcast, is_ptp) のイテラブル。broadcast は None 可
    pinned… 指定があればそのアドレスを持つNICだけに絞る(--bind の尊重)

    ループバックと point-to-point(VPNトンネル)は除く。候補が1つも作れなかった
    ときだけ、最後の手段として限定ブロードキャストを返す。
    """
    out = []
    for ip, bcast, is_ptp in ifs:
        if not bcast or is_ptp:
            continue
        try:
            if socket.inet_aton(ip)[0] == 127:      # ループバック
                continue
        except OSError:
            continue
        if pinned and pinned != ip:
            continue
        if bcast not in out:
            out.append(bcast)
    return out or [LIMITED]


def _enumerate():
    """psutil でNICを列挙する。使えなければ空を返す。"""
    try:
        import psutil
    except ImportError:
        return []
    out = []
    for _name, addrs in psutil.net_if_addrs().items():
        for a in addrs:
            if a.family != socket.AF_INET:
                continue
            out.append((a.address, a.broadcast, bool(a.ptp)))
    return out


def broadcast_targets(bind="0.0.0.0"):
    """ブロードキャストの宛先を列挙する。少なくとも1つ返す。"""
    pinned = None
    if bind and bind not in ("0.0.0.0", ""):
        pinned = bind
    return pick_targets(_enumerate(), pinned)


def targets_for(board, bind="0.0.0.0"):
    """`--board` の指定を宛先リストに直す。

    既定(限定ブロードキャスト)や "auto" のときはNICごとのサブネット宛に展開し、
    具体的なIPが指定されていればそれ1つにする。各ツールがこれを呼ぶだけで
    VPN下でも通るようになる。
    """
    if board in (LIMITED, "auto", "", None):
        return broadcast_targets(bind)
    return [board]


def add_args(ap, default_board=None):
    """`--board` / `--bind` を argparse へ足す。全ツールで文言を揃えるため。"""
    ap.add_argument("--board", default=default_board or LIMITED,
                    help="ボードのIP。既定はブロードキャスト(NICごとのサブネット宛へ"
                         "自動展開するのでVPN下でも通る)")
    ap.add_argument("--bind", default="0.0.0.0",
                    help="ソケットを縛るローカルアドレス。通常は不要"
                         "(NICを1つに絞りたいときだけ)")


def prep_socket(sock):
    """ブロードキャストを出せるようにする。各ツールで付け忘れないため。"""
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    return sock


def send_all(sock, targets, port, pkt):
    """宛先すべてへ送り、成功した数を返す。0 なら1つも出せていない。

    ★**例外で止めない。** 1つのNICで送れなくても他で届く可能性がある。
      呼び出し側は戻り値が 0 のときだけ「送れていない」と判断すればよい。
      以前は sendto の例外がそのままツールを終了させていた。
    """
    ok = 0
    for t in targets:
        try:
            sock.sendto(pkt, (t, port))
            ok += 1
        except OSError:
            pass
    return ok


def explain_failure(targets):
    """1つも送れなかったときの説明文。"""
    if targets == [LIMITED]:
        return ("ブロードキャストを送信できません(宛先 %s)。\n"
                "  NICを列挙できていません(psutil が無いと列挙できません: "
                "pip install psutil)。\n"
                "  VPN接続中は限定ブロードキャストが既定経路に載って失敗するので、\n"
                "  --bind にボードと同じL2にいるIFのアドレスを指定してください。"
                % LIMITED)
    return ("ブロードキャストを送信できません(宛先 %s)。\n"
            "  そのNICが有効か、ボードと同じL2にいるかを確認してください。"
            % " ".join(targets))
