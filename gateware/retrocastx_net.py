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
from functools import reduce
from operator import or_

from migen import *

from litex.gen import LiteXModule
from litex.soc.interconnect import stream
from litex.soc.interconnect.stream import BufferizeEndpoints, DIR_SINK

from liteeth.common import (arp_table_request_layout, arp_table_response_layout,
                            convert_ip, eth_mtu_default, eth_phy_description,
                            eth_min_frame_length, eth_fcs_length)
from liteeth.mac import LiteEthMAC
from liteeth.mac import crc, padding, preamble
from liteeth.mac.common import (LiteEthMACCrossbar, LiteEthMACPacketizer,
                                LiteEthMACDepacketizer)
from liteeth.mac.core import LiteEthMACCore
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
        if isinstance(phy, (list, tuple)):
            # 2ポート(LAN0 / LAN1)。下の DualPortMAC の説明を参照
            assert with_sys_datapath
            self.mac = DualPortMAC(
                phys            = phy,
                mac_address     = mac_address,
                ip_address      = ip_address,
                udp_port_nr     = udp_port_nr,
                dw              = dw,
                tx_cdc_depth    = tx_cdc_depth,
                tx_cdc_buffered = tx_cdc_buffered,
                rx_cdc_depth    = rx_cdc_depth,
                rx_cdc_buffered = rx_cdc_buffered,
                eth_mtu         = eth_mtu,
            )
        else:
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


# --- 2ポート(LAN0 / LAN1)-------------------------------------------------------
#
# ★**コアを2組置くと FPGA に入らない**(LUT 107%。1組で LUT4 約5,000)。1組のうち
#   約1,900 は 32bit 幅の CRC/プリアンブル/パディングで、ポートごとに要るのは
#   8bit↔32bit の変換とクロック乗り換え(約270)だけだった。そこで
#
#     PHY0 ─ 前段0 ─ プリアンブル検査 ─┐
#                                       ├ 束ねる ─ CRC/パディング ─ MAC層 ─ ARP/IP/UDP
#     PHY1 ─ 前段1 ─ プリアンブル検査 ─┘                                    (1組)
#
#   と、ポートごとに持つのは前段だけにして、残りは1組を共有する。
#
# ★**返事をどちらへ出すかは「相手の MAC がどちらに居たか」で決める**(スイッチの
#   学習表と同じ)。共有した層はどのポートから来たかを運ばないので、束ねる位置で
#   受信フレームの送信元 MAC とポートを覚え(`PortTable`)、送信フレームの宛先 MAC
#   で引く。ARP 応答・ping の返事・映像のどれも、相手が見えたポートへ出る。
#
# ★**電線上の MAC は両ポートで同じ**(ボードの名乗り = 先に読めた EUI-48)。ポート
#   ごとに変えるには送信の CRC をポートごとに持つ必要があり、さらに約1,000 LUT
#   かかる。2ポートへ振り分けて 1Gbps を超えるとき(共有の送信路は 1.44Gbps が
#   上限なので、どのみちポートごとの送信路が要る)に回す。


class PortTable(LiteXModule):
    """相手の MAC → それが居るポート の小さな表。

    `ArpLearner` と同じく MAC で畳み、溢れたら古いスロットから使い回す。
    エージングは持たない(相手がポートを移っても、次に見えたときに上書きで直る)。
    """
    def __init__(self, entries=4):
        self.learn_valid = Signal()
        self.learn_mac   = Signal(48)
        self.learn_port  = Signal()
        self.look_mac    = Signal(48)
        self.look_hit    = Signal()
        self.look_port   = Signal()
        self.last_port   = Signal()     # 最後に学習したポート

        # # #

        valids = [Signal(name=f"valid{i}")   for i in range(entries)]
        macs   = [Signal(48, name=f"mac{i}") for i in range(entries)]
        ports  = [Signal(name=f"port{i}")    for i in range(entries)]

        match = Signal(entries)
        wr    = Signal(max=max(entries, 2))
        for i in range(entries):
            self.comb += match[i].eq(valids[i] & (macs[i] == self.learn_mac))
            self.sync += If(self.learn_valid,
                If(match[i], ports[i].eq(self.learn_port)),
                If((match == 0) & (wr == i),
                    valids[i].eq(1),
                    macs[i].eq(self.learn_mac),
                    ports[i].eq(self.learn_port),
                ),
            )
        self.sync += If(self.learn_valid,
            self.last_port.eq(self.learn_port),
            If(match == 0,
                If(wr == entries - 1, wr.eq(0)).Else(wr.eq(wr + 1)),
            ),
        )

        look = Signal(entries)
        for i in range(entries):
            self.comb += look[i].eq(valids[i] & (macs[i] == self.look_mac))
        self.comb += [
            self.look_hit.eq(look != 0),
            # 同じ MAC は重複登録しないので、立つのは高々1ビット
            self.look_port.eq(reduce(or_, [look[i] & ports[i] for i in range(entries)])),
        ]


class DualPortMACCore(LiteXModule):
    """`LiteEthMACCore` の2ポート版。sink/source は同じ(32bit、sys ドメイン)。

    phys       : ドメインを eth0_rx/eth0_tx・eth1_rx/eth1_tx に改名済みの PHY 2つ
    mac_address: 自MAC(Signal 可)。自分宛ユニキャストの判定に使う
    ip_address : 自IP(Signal 可)。自分宛 ARP の判定に使う
    udp_port_nr: 自UDPポート。ブロードキャストの SUBSCRIBE の判定に使う

    tx_ok は呼び側が繋ぐ(ポートごとに「RXC が動いている」)。**送信クロックは
    PHY の RXC から作っている**ので、RXC が止まったポートへ送ると前段の CDC FIFO が
    埋まって共有の送信路ごと止まる。そのポート宛のフレームは捨てる。

    link_ok も呼び側が繋ぐ(ポートごとに「リンクがある」)。送り先を選ぶのに使う。
    **B50612D はケーブルが抜けていても RXC を出し続ける**ので、tx_ok だけでは
    両ポートとも生きて見え、起動直後の ANNOUNCE がリンクの無い側へ出ていた。
    """
    def __init__(self, phys, mac_address, ip_address, udp_port_nr, dw=32,
                 tx_cdc_depth=32, tx_cdc_buffered=True,
                 rx_cdc_depth=32, rx_cdc_buffered=True,
                 eth_mtu=eth_mtu_default, table_entries=4):
        assert len(phys) == 2 and dw == 32
        self.sink   = stream.Endpoint(eth_phy_description(dw))
        self.source = stream.Endpoint(eth_phy_description(dw))
        self.tx_ok  = Signal(2, reset=0b11)
        self.link_ok = Signal(2, reset=0b11)
        # 診断: [7:0] LAN0 受信 / [15:8] LAN1 受信 / [23:16] LAN0 送信 / [31:24] LAN1 送信
        #       (フレーム数。8bit で一周する)
        self.frame_counts = Signal(32)

        # # #

        self.table = table = PortTable(table_entries)
        rx_cnt = [Signal(8, name=f"rx_cnt{i}") for i in range(2)]
        tx_cnt = [Signal(8, name=f"tx_cnt{i}") for i in range(2)]
        self.comb += self.frame_counts.eq(Cat(*rx_cnt, *tx_cnt))

        # --- ポートごとの前段 ---
        # CRC/プリアンブル/パディングを外した LiteEthMACCore = 8bit↔32bit の変換と
        # CDC(と送信の IFG)だけ。プリアンブル検査はここで済ませておく(32bit 版は
        # 比較1つの小さな FSM)。束ねる位置でフレームの先頭が宛先 MAC に揃うので、
        # 中身を決まった位置で読める。
        fronts, rx_srcs = [], []
        for i, phy in enumerate(phys):
            front = ClockDomainsRenamer({"eth_rx": f"eth{i}_rx", "eth_tx": f"eth{i}_tx"})(
                LiteEthMACCore(phy, dw,
                    with_sys_datapath = True,
                    with_preamble_crc = False,
                    with_padding      = False,
                    tx_cdc_depth      = tx_cdc_depth,
                    tx_cdc_buffered   = tx_cdc_buffered,
                    rx_cdc_depth      = rx_cdc_depth,
                    rx_cdc_buffered   = rx_cdc_buffered))
            pre = preamble.LiteEthMACPreambleChecker(dw)
            setattr(self, f"front{i}", front)
            setattr(self, f"rx_preamble{i}", pre)
            self.comb += front.source.connect(pre.sink)
            fronts.append(front)
            rx_srcs.append(pre.source)

        # --- 受信: 2本をフレーム単位で束ねる ---
        # ★**片方のフレームを通している間、もう片方は前段の CDC FIFO(32語=128B)で
        #   待つ。** 長いフレームと重なると後ろの方は溢れて CRC で落ちる。受信は
        #   制御パケットだけなので許容する(両ポートを同じ LAN に挿したときの
        #   ブロードキャストは、もう片方の写しが届くので失っても困らない)。
        busy     = Signal()
        sel      = Signal()
        last_sel = Signal()
        grant    = Signal()
        self.comb += If(busy,
            grant.eq(sel),
        ).Elif(rx_srcs[0].valid & rx_srcs[1].valid,
            grant.eq(~last_sel),
        ).Else(
            grant.eq(rx_srcs[1].valid),
        )
        merged = stream.Endpoint(eth_phy_description(dw))
        self.comb += Case(grant, {i: rx_srcs[i].connect(merged) for i in range(2)})
        beat = Signal()
        self.comb += beat.eq(merged.valid & merged.ready)
        self.sync += If(beat,
            If(merged.last,
                busy.eq(0),
                last_sel.eq(grant),
            ).Else(
                busy.eq(1),
                sel.eq(grant),
            ),
        )
        for i in range(2):
            self.sync += If(beat & merged.last & (grant == i), rx_cnt[i].eq(rx_cnt[i] + 1))

        # --- 受信: 送信元 MAC とポートを覚える ---
        # ★**自分宛のフレームだけで覚える。** LAN の雑多なブロードキャスト(他人宛の
        #   ARP 等)で覚えると、狭い表が無関係な相手で埋まって Viewer が追い出される
        #   (`ArpSniffer` と同じ理由)。覚えるのは
        #     ・宛先が自MAC のユニキャスト
        #     ・自IP宛の ARP(ブロードキャスト)
        #     ・自UDPポート宛の UDP(ブロードキャスト。発見の SUBSCRIBE)
        #   IPv4 はオプション無し(IHL=5)を仮定して位置で読む。外れても覚えない
        #   だけで、返事は既定のポートへ出る。
        #
        #   バイト n は語 n//4 の (n%4)*8 ビット目から(先に届いたバイトが下位)。
        #     0..5 宛先MAC / 6..11 送信元MAC / 12..13 EtherType / 23 IPプロトコル /
        #     36..37 UDP宛先ポート / 38..41 ARPの目標IP
        d = merged.data
        def b(k):
            return d[8 * k:8 * k + 8]
        widx    = Signal(4)              # いまの語の番号(15 で止める)
        w0      = Signal(32)
        w1_src  = Signal(16)             # 送信元MAC の先頭2バイト
        src_mac = Signal(48)
        is_uni  = Signal()
        is_bc   = Signal()
        is_arp  = Signal()
        is_ip   = Signal()
        is_udp  = Signal()
        port_ok = Signal()
        tpa_hi  = Signal(16)
        my_ip = (C(convert_ip(ip_address), 32) if isinstance(ip_address, (str, int))
                 else ip_address)
        dst_now = Signal(48)
        self.comb += dst_now.eq(Cat(b(1), b(0), w0[24:32], w0[16:24], w0[8:16], w0[0:8]))
        self.sync += If(beat,
            If(merged.last, widx.eq(0)).Elif(widx != 15, widx.eq(widx + 1)),
            Case(widx, {
                0:  w0.eq(d),
                1:  [w1_src.eq(d[16:32]),
                     is_uni.eq(dst_now == mac_address),
                     is_bc.eq(dst_now == 0xFFFFFFFFFFFF)],
                2:  src_mac.eq(Cat(b(3), b(2), b(1), b(0), w1_src[8:16], w1_src[0:8])),
                3:  [is_arp.eq(Cat(b(1), b(0)) == 0x0806),
                     is_ip.eq(Cat(b(1), b(0)) == 0x0800)],
                5:  is_udp.eq(b(3) == 17),
                9:  [port_ok.eq(Cat(b(1), b(0)) == udp_port_nr),
                     tpa_hi.eq(d[16:32])],
            }),
        )
        tpa_ok = Signal()
        self.comb += tpa_ok.eq(Cat(b(1), b(0), tpa_hi[8:16], tpa_hi[0:8]) == my_ip)
        # 1段レジスタで表へ渡す(比較の組合せ路を表の書き込みに繋げない)
        self.sync += [
            table.learn_valid.eq(beat & (widx == 10) &
                (is_uni | (is_bc & ((is_arp & tpa_ok) | (is_ip & is_udp & port_ok))))),
            table.learn_mac.eq(src_mac),
            table.learn_port.eq(grant),
        ]

        # --- 受信: 共有の CRC/パディング検査 ---
        # ★**ready もレジスタで受ける(入口と出口の2か所)。** 受信の ready は
        #   UDP/ICMP の depacketizer → IP → crossbar → CRC 検査 → 調停 → 前段の
        #   CDC FIFO まで組合せで一続きになり、sys のクリティカルパスだった
        #   (seed 12 で 45.29MHz、22.1ns のうち配線 16.2ns)。
        rx_crc = BufferizeEndpoints({"sink": DIR_SINK}, pipe_ready=True)(
            crc.LiteEthMACCRC32Checker(eth_phy_description(dw)))
        rx_pad = padding.LiteEthMACPaddingChecker(
            dw, eth_min_frame_length - eth_fcs_length, eth_mtu=eth_mtu)
        rx_out = stream.Buffer(eth_phy_description(dw), pipe_valid=True, pipe_ready=True)
        self.rx_crc, self.rx_pad, self.rx_out = rx_crc, rx_pad, rx_out
        self.comb += [
            merged.connect(rx_crc.sink),
            rx_crc.source.connect(rx_pad.sink),
            rx_pad.source.connect(rx_out.sink),
            rx_out.source.connect(self.source),
        ]

        # --- 送信: 宛先 MAC でポートを決める ---
        # 2語目で宛先 MAC が揃う。表を引いて決めたポートを小さな FIFO に積み、
        # 共有の送信路(パディング/CRC/プリアンブル。フレームを落とさず順番も
        # 変えない)を出たところで取り出す。
        #
        # ★**2語目は「受け取ったとき」ではなく「出されたとき」(valid)に読む。**
        #   出口はポートが決まるまで1語目を流さずに待つので、受け取りを条件に
        #   すると、途中の段(CRC の入口の1段)が1語目で埋まって2語目を受け取れず、
        #   ポートも決まらないまま送信路ごと止まる(sim_dualport.py で捕まえた)。
        #   valid の間はデータが有効なので、受け取りを待たずに読んでよい。
        sink = self.sink
        tbeat = Signal()
        self.comb += tbeat.eq(sink.valid & sink.ready)
        tidx  = Signal(2)
        tw0   = Signal(32)
        tdst  = Signal(48)
        tpend = Signal()
        tdone = Signal()                 # このフレームのポートは積んだ
        self.sync += [
            tpend.eq(0),
            If(tbeat,
                If(sink.last, tidx.eq(0)).Elif(tidx != 3, tidx.eq(tidx + 1)),
                If(tidx == 0, tw0.eq(sink.data)),
            ),
            If(sink.valid & (tidx == 1) & ~tdone,
                tdst.eq(Cat(sink.data[8:16], sink.data[0:8], tw0[24:32],
                            tw0[16:24], tw0[8:16], tw0[0:8])),
                tpend.eq(1),
                tdone.eq(1),
            ),
            If(tbeat & sink.last, tdone.eq(0)),
        ]
        # 既定のポート(ブロードキャスト/マルチキャスト/表に無い相手):
        # 片方しか繋がっていなければそちら、両方なら最後に自分宛が来たポート
        up = Signal(2)
        dflt = Signal()
        self.comb += up.eq(self.tx_ok & self.link_ok)
        self.comb += If(up == 0b01, dflt.eq(0)
            ).Elif(up == 0b10, dflt.eq(1)
            ).Else(dflt.eq(table.last_port))
        want = Signal()
        pick = Signal()
        self.comb += [
            table.look_mac.eq(tdst),
            # tdst[40] = 先頭バイトの I/G ビット(ブロードキャスト/マルチキャスト)
            want.eq(Mux(table.look_hit & ~tdst[40], table.look_port, dflt)),
            # 覚えたポートが繋がっていなくて、もう片方が繋がっていればそちらへ
            pick.eq(Mux(~Mux(want, up[1], up[0]) &
                        Mux(want, up[0], up[1]), ~want, want)),
        ]
        self.port_fifo = port_fifo = stream.SyncFIFO([("port", 1)], 4)
        self.comb += [
            port_fifo.sink.valid.eq(tpend),
            port_fifo.sink.port.eq(pick),
        ]

        tx_pad = padding.LiteEthMACPaddingInserter(dw, eth_min_frame_length - eth_fcs_length)
        tx_crc = BufferizeEndpoints({"sink": DIR_SINK})(
            crc.LiteEthMACCRC32Inserter(eth_phy_description(dw)))
        tx_pre = preamble.LiteEthMACPreambleInserter(dw)
        self.tx_pad, self.tx_crc, self.tx_pre = tx_pad, tx_crc, tx_pre
        self.comb += [
            sink.connect(tx_pad.sink),
            tx_pad.source.connect(tx_crc.sink),
            tx_crc.source.connect(tx_pre.sink),
        ]

        # --- 送信: 決めたポートの前段へ ---
        out   = tx_pre.source
        tbusy = Signal()
        tsel  = Signal()
        cur   = Signal()
        drop  = Signal()
        self.comb += [
            cur.eq(Mux(tbusy, tsel, port_fifo.source.port)),
            # 止まったポートへは出さずに捨てる(途中で止まったら残りも捨てる)
            drop.eq(~Mux(cur, self.tx_ok[1], self.tx_ok[0])),
        ]
        # フレームの先頭ではポートが決まるまで待つ
        can_go = Signal()
        self.comb += can_go.eq(tbusy | port_fifo.source.valid)
        for i in range(2):
            self.comb += [
                fronts[i].sink.data.eq(out.data),
                fronts[i].sink.last_be.eq(out.last_be),
                fronts[i].sink.error.eq(out.error),
                fronts[i].sink.last.eq(out.last),
                fronts[i].sink.first.eq(out.first),
                fronts[i].sink.valid.eq(out.valid & can_go & ~drop & (cur == i)),
            ]
        obeat = Signal()
        self.comb += [
            out.ready.eq(can_go & (drop | Mux(cur, fronts[1].sink.ready,
                                              fronts[0].sink.ready))),
            obeat.eq(out.valid & out.ready),
            port_fifo.source.ready.eq(obeat & ~tbusy),
        ]
        self.sync += If(obeat,
            tbusy.eq(~out.last),
            If(~tbusy, tsel.eq(port_fifo.source.port)),
        )
        for i in range(2):
            self.sync += If(obeat & out.last & ~drop & (cur == i),
                            tx_cnt[i].eq(tx_cnt[i] + 1))


class DualPortMAC(LiteXModule):
    """`LiteEthMAC(interface="crossbar")` の2ポート版。上に載る ARP/IP から見れば同じ。"""
    def __init__(self, phys, mac_address, ip_address, udp_port_nr, dw=32, **kwargs):
        self.core = DualPortMACCore(phys, mac_address, ip_address, udp_port_nr,
                                    dw=dw, **kwargs)
        self.crossbar     = LiteEthMACCrossbar(dw)
        self.packetizer   = LiteEthMACPacketizer(dw)
        self.depacketizer = LiteEthMACDepacketizer(dw)
        self.comb += [
            self.crossbar.master.source.connect(self.packetizer.sink),
            self.packetizer.source.connect(self.core.sink),
            self.core.source.connect(self.depacketizer.sink),
            self.depacketizer.source.connect(self.crossbar.master.sink),
        ]

