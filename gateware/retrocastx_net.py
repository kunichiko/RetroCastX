"""RetroCastX のネットワーク層: LiteEth の UDP/IP コアに「受信からのARP学習」を足す。

## なぜ必要か

ボードは映像もCONFIG応答もユニキャストで返すので、宛先のMACをARPで引く。ところが
**Windows は自分と別サブネットの送信元から来たARP要求に応答しない**(strong host
model)。macOS/BSD は応答するので Mac だけの検証では見落とす。結果として
「Viewer の SUBSCRIBE はボードに届いているのに、ボードから何も返ってこない」
という形で詰まる(docs/design-notes.md「実機を動かすときの準備」)。

対策は **こちらへ向けて来たパケットの「イーサネット送信元MAC + IPヘッダの送信元
アドレス」を覚え、宛先MACの解決にそれを使う**こと。SUBSCRIBE を受けた瞬間に相手の
MACが分かるのでARPの往復自体が不要になり、相手がARPに応答しない環境でも返せる。
利用者に `New-NetIPAddress` の追加を要求しなくなる。

## どこに挟むか

LiteEth のARPキャッシュ(`LiteEthARPCache.update`)へ流し込むのが素直に見えるが、
この口は `LiteEthARPTable` のFSMが既に駆動しているので外から足せない(migenの
多重ドライバになる)。キャッシュ自体も学習の置き場所には向かない:

- 書き込みがラウンドロビンで、同じIPを畳まない(同一IPの重複エントリができ、
  検索は先に見つかった側=古いMACを返しうる)
- 1秒ごとに全消去する。ただし消去タイマは検索でもリセットされるので、映像が
  流れている間は実質消えない ─ つまり「消えるときだけ消える」読みにくい挙動

そこで **IP TX から見た「ARPテーブル」の位置に自前の表(`ArpLearner`)を差し込む**。
学習済みなら即答し、未知なら本物のARPテーブルへ委譲して応答を中継する。LiteEth
本体は無改造(`gateware/litex-src` は litex_setup.py で取得するもので、リポジトリ
には入っていない。パッチを当てても残らない)。

そのため `LiteEthUDPIPCore` をそのまま使えない ─ IP TX の解決先がコア内部で本物の
ARPテーブルに直結されているので、構成(MAC + ARP + IP + ICMP + UDP)だけをこちらに
持ってきたのが `RetroCastXUDPIPCore`。
"""
from migen import *

from litex.gen import LiteXModule
from litex.soc.interconnect import stream

from liteeth.common import (arp_table_request_layout, arp_table_response_layout,
                            convert_ip, eth_mtu_default)
from liteeth.mac import LiteEthMAC
from liteeth.core.arp import LiteEthARP
from liteeth.core.icmp import LiteEthICMP
from liteeth.core.ip import LiteEthIP
from liteeth.core.udp import LiteEthUDP


# 学習した1組。ArpSniffer → ArpLearner
ARP_LEARN_LAYOUT = [("ip_address", 32), ("mac_address", 48)]


class ArpLearner(LiteXModule):
    """IP TX から見える「ARPテーブル」。学習済みなら即答し、未知なら本物へ委譲する。

    `request`/`response` は `LiteEthARPTable` と同じインターフェースなので、
    `LiteEthIP(arp_table=...)` にそのまま渡せる。

    表は小さく(既定4)、**IPで畳む**(同じIPが来たらMACだけ更新)。溢れたら
    古いスロットから順に使い回す。エージングは持たない: 同じIPの相手が別のMACに
    なっても上書きで直り、消えたIPのエントリは誰も引かないので害が無い。
    """
    def __init__(self, arp_table, entries=4):
        assert entries >= 1
        self.request  = stream.Endpoint(arp_table_request_layout)   # IP TX から
        self.response = stream.Endpoint(arp_table_response_layout)  # IP TX へ
        self.learn    = stream.Endpoint(ARP_LEARN_LAYOUT)           # ArpSniffer から

        # 診断(CONFIG key 0x40..0x45 で読む)。実機で「学習できているのか」
        # 「学習した表で答えているのか」を切り分けるために出す
        self.learn_count = Signal(32)   # 学習した回数
        self.hit_count   = Signal(32)   # 学習済みの表で即答した回数
        self.miss_count  = Signal(32)   # 本物のARPへ委譲した回数
        self.last_ip     = Signal(32)   # 最後に学習した相手
        self.last_mac    = Signal(48)

        # # #

        valids = [Signal(name=f"valid{i}")     for i in range(entries)]
        ips    = [Signal(32, name=f"ip{i}")    for i in range(entries)]
        macs   = [Signal(48, name=f"mac{i}")   for i in range(entries)]

        # --- 学習(常に受け付ける) ---
        #
        # 引き当てFSMの状態に依らず受け付けるのが肝。委譲した本物のARPの応答待ちは
        # 最悪800ms(8回×100ms)かかるので、その間に来たSUBSCRIBEを取りこぼすと
        # 学習が2秒(keepalive周期)遅れる。
        lrn_valid = Signal()
        lrn_ip    = Signal(32)
        lrn_mac   = Signal(48)
        arp_learn = Signal()            # 本物のARPが解決できた分も覚える
        arp_mac   = arp_table.response.mac_address
        req_ip    = Signal(32)
        self.comb += [
            self.learn.ready.eq(1),
            If(self.learn.valid,
                lrn_valid.eq(1),
                lrn_ip.eq(self.learn.ip_address),
                lrn_mac.eq(self.learn.mac_address),
            ).Elif(arp_learn,
                lrn_valid.eq(1),
                lrn_ip.eq(req_ip),
                lrn_mac.eq(arp_mac),
            ),
        ]

        match = Signal(entries)         # 学習しようとしているIPが既に表にあるか
        wr    = Signal(max=entries)     # 未知のIPを入れる次のスロット
        for i in range(entries):
            self.comb += match[i].eq(valids[i] & (ips[i] == lrn_ip))
            self.sync += If(lrn_valid,
                # 既知のIP: MACだけ更新(重複エントリを作らない)
                If(match[i], macs[i].eq(lrn_mac)),
                # 未知のIP: 次のスロットへ
                If((match == 0) & (wr == i),
                    valids[i].eq(1),
                    ips[i].eq(lrn_ip),
                    macs[i].eq(lrn_mac),
                ),
            )
        self.sync += If(lrn_valid,
            self.learn_count.eq(self.learn_count + 1),
            self.last_ip.eq(lrn_ip),
            self.last_mac.eq(lrn_mac),
            If(match == 0,
                If(wr == entries - 1, wr.eq(0)).Else(wr.eq(wr + 1)),
            ),
        )

        # --- 引き当て ---
        look     = Signal(entries)
        look_mac = Signal(48)
        for i in range(entries):
            self.comb += look[i].eq(valids[i] &
                                    (ips[i] == self.request.ip_address))
            # 同じIPは重複登録しないので、立つのは高々1ビット
            self.comb += If(look[i], look_mac.eq(macs[i]))

        resp_mac    = Signal(48)
        resp_failed = Signal()
        self.comb += [
            self.response.mac_address.eq(resp_mac),
            self.response.failed.eq(resp_failed),
            # 本物のARPへ渡す宛先(委譲するときだけ valid を立てる)
            arp_table.request.ip_address.eq(req_ip),
        ]
        self.fsm = fsm = FSM(reset_state="IDLE")
        fsm.act("IDLE",
            If(self.request.valid,
                self.request.ready.eq(1),
                NextValue(req_ip, self.request.ip_address),
                If(look != 0,
                    NextValue(resp_mac, look_mac),
                    NextValue(resp_failed, 0),
                    NextValue(self.hit_count, self.hit_count + 1),
                    NextState("RESPOND"),
                ).Else(
                    NextValue(self.miss_count, self.miss_count + 1),
                    NextState("ASK_ARP"),
                ),
            ),
        )
        fsm.act("ASK_ARP",
            arp_table.request.valid.eq(1),
            If(arp_table.request.ready,
                NextState("WAIT_ARP"),
            ),
        )
        fsm.act("WAIT_ARP",
            arp_table.response.ready.eq(1),
            If(arp_table.response.valid,
                NextValue(resp_mac, arp_mac),
                NextValue(resp_failed, arp_table.response.failed),
                # 引けた相手は表に入れる(以後ARPの往復が消える)
                arp_learn.eq(~arp_table.response.failed),
                NextState("RESPOND"),
            ),
        )
        fsm.act("RESPOND",
            self.response.valid.eq(1),
            If(self.response.ready,
                NextState("IDLE"),
            ),
        )


class ArpSniffer(LiteXModule):
    """受信パケットから (送信元MAC, 送信元IP) を取り出して `ArpLearner` へ渡す。

    学習するのは **こちらへ向けられたパケットだけ** に絞る:

    - 自IP宛のIPパケット(ping等も含む。ブロードキャスト/マルチキャストは除く)
    - 自UDPポート宛のUDPパケット(SUBSCRIBE/CONFIG。既定ではブロードキャストで来る)
    - 自IP宛のARP要求(`LiteEthARPRX` が自IP宛だけを通す)

    LANの雑多なブロードキャスト/マルチキャスト(mDNS/SSDP/NetBIOS等)で学習させると、
    狭い表が無関係な相手で埋まって肝心の購読者が追い出される。`LiteEthIPRX` は
    `with_broadcast=True` で宛先IPを問わず通すので、絞り込みはここでやる必要がある。

    MACヘッダの送信元MACはIP層より上には運ばれない(`eth_ipv4_user_description` に
    無い)ので、IP RX の入口(=MAC層の出口)を覗く。Depacketizer はヘッダを1パケット
    分保持するので、パケットが流れている間ずっと同じ値が見える。

    出力は1サイクル遅れの単発パルス(`ready` を見ない)。受け手の `ArpLearner` は
    常に受け付けるので詰まらない。取りこぼしても keepalive で次が来る。
    """
    def __init__(self, ip_rx, udp_rx, arp_rx, ip_address, udp_port_nr):
        self.source = source = stream.Endpoint(ARP_LEARN_LAYOUT)

        # # #

        ip_address = convert_ip(ip_address)

        # IPパケットの先頭ビート。ここで送信元MAC/送信元IP/宛先IPが揃って見える
        ip_beat  = Signal()
        ip_first = Signal(reset=1)
        self.comb += ip_beat.eq(ip_rx.source.valid & ip_rx.source.ready)
        self.sync += If(ip_beat, ip_first.eq(ip_rx.source.last))

        # UDPポートの判定はUDPヘッダが要る。IP層で控えておいて、UDP層の先頭ビートで
        # 使う(両者は同じパケットで数サイクルしか離れない。念のため送信元IPの一致も
        # 見て、取り違えたら学習しない ─ keepaliveで次があるので取りこぼしは無害)。
        cand_mac = Signal(48)
        cand_ip  = Signal(32)
        self.sync += If(ip_beat & ip_first,
            cand_mac.eq(ip_rx.sink.sender_mac),
            cand_ip.eq(ip_rx.source.ip_address),
        )

        udp_beat  = Signal()
        udp_first = Signal(reset=1)
        self.comb += udp_beat.eq(udp_rx.source.valid & udp_rx.source.ready)
        self.sync += If(udp_beat, udp_first.eq(udp_rx.source.last))

        ip_learn  = Signal()            # 自IP宛のIPパケット
        udp_learn = Signal()            # 自UDPポート宛のUDPパケット
        arp_learn = Signal()            # 自IP宛のARP
        self.comb += [
            ip_learn.eq(ip_beat & ip_first &
                        (ip_rx.depacketizer.source.target_ip == ip_address)),
            udp_learn.eq(udp_beat & udp_first &
                         (udp_rx.source.dst_port == udp_port_nr) &
                         (udp_rx.source.ip_address == cand_ip)),
            arp_learn.eq(arp_rx.source.valid &
                         (arp_rx.source.request | arp_rx.source.reply)),
        ]
        # 同時に立つことはほぼ無い(別パケット由来)。唯一あり得るのは自IP宛の
        # UDPパケットで ip_learn と udp_learn が数サイクル差で立つ場合だが、
        # 同じ組なので後から来た方は上書きで消える。
        #
        # **出力は1段レジスタで出す。** ここを組合せで出すと
        #   IPヘッダのレジスタ → ICMP/UDPのデコード → UDP RXのFSM →
        #   ArpLearner の表の比較 → 書き込みイネーブル
        # が1サイクルの経路になり、sys 45MHz(22.2ns)に対して実測22.9nsまで伸びた
        # (nextpnr 43.6MHz で未達)。学習が1サイクル遅れても誰も困らない。
        self.sync += [
            source.valid.eq(0),
            If(arp_learn,
                source.valid.eq(1),
                source.ip_address.eq(arp_rx.source.ip_address),
                source.mac_address.eq(arp_rx.source.mac_address),
            ).Elif(ip_learn,
                source.valid.eq(1),
                source.ip_address.eq(ip_rx.source.ip_address),
                source.mac_address.eq(ip_rx.sink.sender_mac),
            ).Elif(udp_learn,
                source.valid.eq(1),
                source.ip_address.eq(cand_ip),
                source.mac_address.eq(cand_mac),
            ),
        ]


class RetroCastXUDPIPCore(LiteXModule):
    """`LiteEthUDPIPCore` と同じ構成(MAC + ARP + IP + ICMP + UDP)に、受信からの
    ARP学習(`ArpLearner` + `ArpSniffer`)を挟んだもの。

    構成を持ってきているのは、IP TX の宛先MAC解決先がコア内部で本物のARPテーブルに
    直結されていて、外から差し替えられないため(モジュール冒頭の説明を参照)。
    LiteEth側は無改造なので、`litex-src` を更新しても追従は要らない。
    """
    def __init__(self, phy, mac_address, ip_address, clk_freq, udp_port_nr,
                 dw=8, arp_entries=4, learn_entries=4, with_icmp=True,
                 with_sys_datapath=False, eth_mtu=eth_mtu_default,
                 tx_cdc_depth=32, tx_cdc_buffered=True,
                 rx_cdc_depth=32, rx_cdc_buffered=True):
        ip_address = convert_ip(ip_address)

        # cdc_buffered は **LiteEthMAC の既定(False)ではなく LiteEthIPCore が渡す値
        # (True)に合わせる**。ここを既定任せにすると、コアを自前にした副作用で
        # CDCの挙動が静かに変わる。LiteEth を更新したときはこの引数の並びを
        # liteeth/core/__init__.py の LiteEthIPCore と突き合わせること。
        self.mac = LiteEthMAC(
            phy               = phy,
            dw                = dw,
            interface         = "crossbar",
            endianness        = "big",
            hw_mac            = mac_address,
            with_preamble_crc = True,
            with_sys_datapath = with_sys_datapath,
            tx_cdc_depth      = tx_cdc_depth,
            tx_cdc_buffered   = tx_cdc_buffered,
            rx_cdc_depth      = rx_cdc_depth,
            rx_cdc_buffered   = rx_cdc_buffered,
            eth_mtu           = eth_mtu,
        )
        # arp_entries: 本物のARPキャッシュは「こちらから先に送る相手」用にだけ残る
        # (学習済みの相手は ArpLearner が答えるのでここまで来ない)。既定の1
        # (=実質2エントリ)では ANNOUNCE の初期宛先などで追い出しが起きるので少し増やす
        self.arp = LiteEthARP(
            mac         = self.mac,
            mac_address = mac_address,
            ip_address  = ip_address,
            clk_freq    = clk_freq,
            entries     = arp_entries,
            dw          = dw,
        )
        self.arp_learner = ArpLearner(self.arp.table, entries=learn_entries)
        self.ip = LiteEthIP(
            mac            = self.mac,
            mac_address    = mac_address,
            ip_address     = ip_address,
            arp_table      = self.arp_learner,   # ← 本物のARPテーブルの代わり
            with_broadcast = True,
            dw             = dw,
        )
        if with_icmp:
            self.icmp = LiteEthICMP(ip=self.ip, ip_address=ip_address, dw=dw)
        self.udp = LiteEthUDP(ip=self.ip, ip_address=ip_address, dw=dw)

        self.arp_sniffer = ArpSniffer(
            ip_rx       = self.ip.rx,
            udp_rx      = self.udp.rx,
            arp_rx      = self.arp.rx,
            ip_address  = ip_address,
            udp_port_nr = udp_port_nr,
        )
        self.comb += self.arp_sniffer.source.connect(self.arp_learner.learn)
