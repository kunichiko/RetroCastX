#!/usr/bin/env python3
"""retrocastx_net.py(受信からのARP学習)のMigenシミュレーション。

実機で確かめにくい「相手がARPに応答しない」状況を、応答しないARPテーブルを
差し込んで再現する。検証する性質:

A) ArpLearner
   - 未学習のIPは本物のARPテーブルへ委譲し、その応答(成功/失敗)をIP TXへ中継する
   - 引けたIPは表に入り、次からは委譲せずに即答する(ARPの往復が消える)
   - 受信からの学習(learnポート)だけで、ARPを一度も引かずに解決できる
   - 同じIPが再学習されたらMACを上書きし、エントリは増やさない
   - 表が溢れたら古い順に使い回す(追い出された相手は委譲へ戻る)
   - 本物のARP応答を待っている間も学習を取りこぼさない
B) ArpSniffer
   - 自IP宛のIPパケット / 自UDPポート宛のUDPパケット / 自IP宛のARP から学習する
   - 別ポート宛・自IP宛でないパケット(マルチキャスト等)からは学習しない
C) 結線(Sniffer → Learner)
   - SUBSCRIBE相当のブロードキャストUDPを1つ受けるだけで、送信元へ即答できる

実行: .venv/bin/python sim_arp.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from migen import *                                          # noqa: E402
from migen.sim import run_simulation                         # noqa: E402

from litex.soc.interconnect import stream                    # noqa: E402
from liteeth.common import (arp_table_request_layout,         # noqa: E402
                           arp_table_response_layout,
                           convert_ip, eth_ipv4_description,
                           eth_ipv4_user_description,
                           eth_mac_description,
                           eth_udp_user_description)
from liteeth.core.arp import _arp_table_layout               # noqa: E402

from retrocastx_net import ArpLearner, ArpSniffer            # noqa: E402

BOARD_IP  = convert_ip("192.168.10.50")
UDP_PORT  = 34600
BCAST_IP  = convert_ip("255.255.255.255")

WIN_IP,  WIN_MAC  = convert_ip("192.168.11.35"), 0x001122334455
MAC_IP,  MAC_MAC  = convert_ip("192.168.11.21"), 0x0066778899AA
HOST_IP, HOST_MAC = convert_ip("192.168.10.1"),  0x00AABBCCDDEE
DEAD_IP           = convert_ip("192.168.10.99")   # 誰も応答しないIP
MCAST_IP          = convert_ip("224.0.0.251")     # mDNS(学習してはいけない)


class _FakeArpTable:
    """本物の LiteEthARPTable の代わり。応答はテストベンチが作る(応答しない相手も
    表現できるようにするため)。ロジックを持たないので submodule にはしない。"""
    def __init__(self):
        self.request  = stream.Endpoint(arp_table_request_layout)
        self.response = stream.Endpoint(arp_table_response_layout)


class _IpRxStub:
    """LiteEthIPRX のうち ArpSniffer が覗く部分だけ。"""
    class _Depacketizer:
        def __init__(self, dw):
            self.source = stream.Endpoint(eth_ipv4_description(dw))

    def __init__(self, dw=32):
        self.sink         = stream.Endpoint(eth_mac_description(dw))
        self.source       = stream.Endpoint(eth_ipv4_user_description(dw))
        self.depacketizer = self._Depacketizer(dw)


class _UdpRxStub:
    def __init__(self, dw=32):
        self.source = stream.Endpoint(eth_udp_user_description(dw))


class _ArpRxStub:
    def __init__(self, dw=32):
        self.source = stream.Endpoint(_arp_table_layout)


class LearnerDUT(Module):
    def __init__(self, entries=4):
        self.arp = _FakeArpTable()
        self.submodules.learner = ArpLearner(self.arp, entries=entries)


class SnifferDUT(Module):
    def __init__(self, with_learner=False, entries=4):
        self.ip_rx  = _IpRxStub()
        self.udp_rx = _UdpRxStub()
        self.arp_rx = _ArpRxStub()
        self.submodules.sniffer = ArpSniffer(
            ip_rx=self.ip_rx, udp_rx=self.udp_rx, arp_rx=self.arp_rx,
            ip_address=BOARD_IP, udp_port_nr=UDP_PORT)
        self.learner = None
        if with_learner:
            self.arp = _FakeArpTable()
            self.submodules.learner = ArpLearner(self.arp, entries=entries)
            self.comb += self.sniffer.source.connect(self.learner.learn)
        else:
            self.comb += self.sniffer.source.ready.eq(1)


# --- テストベンチの部品 ---------------------------------------------------------

def _resolve(learner, ip, timeout=200):
    """IP TX がやること: 宛先IPを渡してMACを引く。(mac, failed) を返す。"""
    yield learner.request.ip_address.eq(ip)
    yield learner.request.valid.eq(1)
    yield
    while not (yield learner.request.ready):
        yield
    yield learner.request.valid.eq(0)
    yield learner.response.ready.eq(1)
    yield
    for _ in range(timeout):
        if (yield learner.response.valid):
            mac    = yield learner.response.mac_address
            failed = yield learner.response.failed
            yield learner.response.ready.eq(0)
            yield
            return mac, failed
        yield
    raise AssertionError("解決の応答が返ってこない (ip=%08x)" % ip)


def _learn(learner, ip, mac):
    """受信パケットからの学習1件(ArpSniffer の出力相当)。"""
    yield learner.learn.ip_address.eq(ip)
    yield learner.learn.mac_address.eq(mac)
    yield learner.learn.valid.eq(1)
    yield
    yield learner.learn.valid.eq(0)
    yield


def _arp_responder(dut, db, asked, done, latency=5):
    """委譲された分に応答する。db に無いIPは failed(=ARPに応答しない相手)。

    run_simulation は渡した全ジェネレータが終わるまで回すので、テストベンチが
    終わったら止まるようにしておく(無限ループにすると帰ってこない)。"""
    while not done:
        if (yield dut.arp.request.valid):
            ip = yield dut.arp.request.ip_address
            asked.append(ip)
            yield dut.arp.request.ready.eq(1)
            yield
            yield dut.arp.request.ready.eq(0)
            for _ in range(latency):
                yield
            mac = db.get(ip)
            yield dut.arp.response.mac_address.eq(mac or 0)
            yield dut.arp.response.failed.eq(0 if mac else 1)
            yield dut.arp.response.valid.eq(1)
            yield
            while not (yield dut.arp.response.ready):
                yield
            yield dut.arp.response.valid.eq(0)
        yield


def _ip_beats(ip_rx, sender_mac, sender_ip, target_ip, nwords=4):
    """IP RX を1パケット分流す。ヘッダ由来の値はパケット中ずっと同じ(実装と同じ)。"""
    yield ip_rx.sink.sender_mac.eq(sender_mac)
    yield ip_rx.source.ip_address.eq(sender_ip)
    yield ip_rx.depacketizer.source.target_ip.eq(target_ip)
    yield ip_rx.source.ready.eq(1)
    for i in range(nwords):
        yield ip_rx.source.valid.eq(1)
        yield ip_rx.source.last.eq(i == nwords - 1)
        yield
    yield ip_rx.source.valid.eq(0)
    yield ip_rx.source.last.eq(0)
    yield


def _udp_beats(udp_rx, sender_ip, dst_port, nwords=4):
    yield udp_rx.source.ip_address.eq(sender_ip)
    yield udp_rx.source.dst_port.eq(dst_port)
    yield udp_rx.source.ready.eq(1)
    for i in range(nwords):
        yield udp_rx.source.valid.eq(1)
        yield udp_rx.source.last.eq(i == nwords - 1)
        yield
    yield udp_rx.source.valid.eq(0)
    yield udp_rx.source.last.eq(0)
    yield


def _arp_beat(arp_rx, sender_ip, sender_mac, request=1):
    yield arp_rx.source.ip_address.eq(sender_ip)
    yield arp_rx.source.mac_address.eq(sender_mac)
    yield arp_rx.source.request.eq(request)
    yield arp_rx.source.reply.eq(0 if request else 1)
    yield arp_rx.source.valid.eq(1)
    yield
    yield arp_rx.source.valid.eq(0)
    yield arp_rx.source.request.eq(0)
    yield arp_rx.source.reply.eq(0)
    yield


def _udp_packet(dut, sender_mac, sender_ip, target_ip, dst_port):
    """実機と同じ順序(MAC/IPヘッダが見えてからUDPヘッダが見える)で1パケット。"""
    yield from _ip_beats(dut.ip_rx, sender_mac, sender_ip, target_ip)
    yield from _udp_beats(dut.udp_rx, sender_ip, dst_port)


def _collect_learns(dut, out, cycles):
    for _ in range(cycles):
        if (yield dut.sniffer.source.valid):
            out.append(((yield dut.sniffer.source.ip_address),
                        (yield dut.sniffer.source.mac_address)))
        yield


# --- A) ArpLearner --------------------------------------------------------------

def scenario_a():
    dut = LearnerDUT(entries=2)
    asked, done = [], []
    # Mac は普通にARPに応答する。Windows(WIN_IP)と DEAD_IP は応答しない
    db = {MAC_IP: MAC_MAC, HOST_IP: HOST_MAC}

    def tb():
        L = dut.learner

        # 1) 未学習 + 相手がARPに応答する → 委譲して解決、そのまま表に入る
        mac, failed = yield from _resolve(L, MAC_IP)
        assert (mac, failed) == (MAC_MAC, 0), (hex(mac), failed)
        assert asked == [MAC_IP], asked
        mac, failed = yield from _resolve(L, MAC_IP)
        assert (mac, failed) == (MAC_MAC, 0)
        assert asked == [MAC_IP], "2回目は委譲してはいけない: %s" % asked
        assert (yield L.hit_count) == 1
        assert (yield L.miss_count) == 1
        assert (yield L.learn_count) == 1      # ARP応答からの学習

        # 2) 未学習 + 相手がARPに応答しない → 失敗がIP TXへ伝わる(従来の挙動)
        mac, failed = yield from _resolve(L, WIN_IP)
        assert failed == 1, "応答しない相手は failed になるはず"
        assert asked[-1] == WIN_IP
        assert (yield L.learn_count) == 1, "失敗したら学習してはいけない"

        # 3) 受信パケットから学習 → ARPを引かずに解決できる(本命)
        n_asked = len(asked)
        yield from _learn(L, WIN_IP, WIN_MAC)
        mac, failed = yield from _resolve(L, WIN_IP)
        assert (mac, failed) == (WIN_MAC, 0), (hex(mac), failed)
        assert len(asked) == n_asked, "学習済みなのに委譲した: %s" % asked

        # 4) 同じIPで別MAC(相手のNICが変わった)→ 上書きし、エントリは増やさない
        #    entries=2 なので、増えていたら MAC_IP が追い出されて委譲に戻る
        yield from _learn(L, WIN_IP, MAC_MAC)
        mac, failed = yield from _resolve(L, WIN_IP)
        assert (mac, failed) == (MAC_MAC, 0), "MACが上書きされていない"
        n_asked = len(asked)
        mac, failed = yield from _resolve(L, MAC_IP)
        assert (mac, failed) == (MAC_MAC, 0)
        assert len(asked) == n_asked, "同じIPの再学習でエントリが増えている"

        # 5) 溢れたら古い順に使い回す(entries=2 に3つ目)
        yield from _learn(L, HOST_IP, HOST_MAC)
        mac, failed = yield from _resolve(L, HOST_IP)
        assert (mac, failed) == (HOST_MAC, 0)
        n_asked = len(asked)
        mac, failed = yield from _resolve(L, MAC_IP)     # 追い出された側
        assert len(asked) == n_asked + 1, "追い出されたIPは委譲されるはず"
        assert (mac, failed) == (MAC_MAC, 0), "委譲すれば引き直せる"
        done.append(True)

    run_simulation(dut, [tb(), _arp_responder(dut, db, asked, done)])
    print("A(ArpLearner): OK — 委譲/中継・学習による即答・IP畳み込み・追い出しを確認")


def scenario_a2():
    """本物のARPの応答を待っている間(最悪800ms)も学習を取りこぼさないこと。"""
    dut = LearnerDUT(entries=2)
    asked, done = [], []

    def tb():
        L = dut.learner
        # 応答しないIPへの解決を始める(委譲して待ちに入る)
        yield L.request.ip_address.eq(DEAD_IP)
        yield L.request.valid.eq(1)
        yield
        while not (yield L.request.ready):
            yield
        yield L.request.valid.eq(0)
        yield
        # 待っている最中に SUBSCRIBE が来た(learnポートは常に受け付ける)
        yield from _learn(L, WIN_IP, WIN_MAC)
        assert (yield L.learn_count) == 1, "応答待ちの間に学習を落とした"
        # 待ちを終わらせる(failed)
        yield L.response.ready.eq(1)
        for _ in range(300):
            if (yield L.response.valid):
                break
            yield
        assert (yield L.response.failed) == 1
        yield L.response.ready.eq(0)
        yield
        # 学習した相手は委譲なしで解決できる
        n_asked = len(asked)
        mac, failed = yield from _resolve(L, WIN_IP)
        assert (mac, failed) == (WIN_MAC, 0)
        assert len(asked) == n_asked
        done.append(True)

    run_simulation(dut, [tb(), _arp_responder(dut, {}, asked, done, latency=30)])
    print("A2(応答待ち中の学習): OK — 委譲中でも learn を取りこぼさない")


# --- B) ArpSniffer -------------------------------------------------------------

def scenario_b():
    dut = SnifferDUT()
    got = []

    def tb():
        # 1) 自IP宛のIPパケット(ping等)→ 学習する
        yield from _ip_beats(dut.ip_rx, MAC_MAC, MAC_IP, BOARD_IP)
        # 2) 自IP宛でないIPパケット(mDNS)→ 学習しない
        yield from _ip_beats(dut.ip_rx, WIN_MAC, WIN_IP, MCAST_IP)
        # 3) ブロードキャスト宛・自UDPポート(=既定のSUBSCRIBE)→ 学習する
        yield from _udp_packet(dut, WIN_MAC, WIN_IP, BCAST_IP, UDP_PORT)
        # 4) ブロードキャスト宛・別ポート(LANの雑談)→ 学習しない
        yield from _udp_packet(dut, HOST_MAC, HOST_IP, BCAST_IP, 137)
        # 5) 自IP宛のARP要求 → 学習する
        yield from _arp_beat(dut.arp_rx, HOST_IP, HOST_MAC)
        for _ in range(10):
            yield

    run_simulation(dut, [tb(), _collect_learns(dut, got, 400)])
    assert got == [(MAC_IP, MAC_MAC), (WIN_IP, WIN_MAC), (HOST_IP, HOST_MAC)], \
        [(hex(i), hex(m)) for i, m in got]
    print("B(ArpSniffer): OK — 自分宛のIP/UDP/ARPだけから学習(3件)、"
          "マルチキャストと別ポートは無視")


# --- C) 結線 -------------------------------------------------------------------

def scenario_c():
    dut = SnifferDUT(with_learner=True, entries=4)
    asked, done = [], []

    def tb():
        # ARPに応答しない相手(Windows)からブロードキャストのSUBSCRIBEが1つ来る
        yield from _udp_packet(dut, WIN_MAC, WIN_IP, BCAST_IP, UDP_PORT)
        for _ in range(5):
            yield
        assert (yield dut.learner.learn_count) == 1
        # そのままユニキャストで返せる(ARPを一度も引かない)
        mac, failed = yield from _resolve(dut.learner, WIN_IP)
        assert (mac, failed) == (WIN_MAC, 0), (hex(mac), failed)
        assert asked == [], "ARPを引いてしまっている: %s" % asked
        assert (yield dut.learner.last_ip) == WIN_IP
        assert (yield dut.learner.last_mac) == WIN_MAC
        done.append(True)

    run_simulation(dut, [tb(), _arp_responder(dut, {}, asked, done)])
    print("C(Sniffer→Learner): OK — SUBSCRIBE 1つでARP無しにユニキャスト解決")


def main():
    scenario_a()
    scenario_a2()
    scenario_b()
    scenario_c()
    print("all scenarios passed")


if __name__ == "__main__":
    main()
