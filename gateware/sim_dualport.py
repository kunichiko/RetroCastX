#!/usr/bin/env python3
"""DualPortMACCore(LAN0 / LAN1 の2ポート MAC)の検証(実機不要)。

PHY を2つ模し(8bit、ポートごとに別クロック)、sys 側の 32bit ストリームとの間で
フレームを流す。受信はプリアンブル + FCS 付きで入れ、送信は出てきたフレームの
FCS まで確かめる。

検証項目:
1. 両ポートから受けたフレームが、FCS を外して sys 側へ出る
2. 両ポートに同時に届いても、両方とも出る(フレーム単位の調停)
3. 自分宛(自MAC宛ユニキャスト / 自IP宛 ARP / 自UDPポート宛 UDP)を受けた相手は、
   見えたポートとして覚える。**他人宛の ARP では覚えない**
4. 送信は宛先 MAC で引いたポートへ出る(もう片方には出ない)。FCS が正しい
5. 覚えていない相手・ブロードキャストは、最後に自分宛が来たポートへ。
   まだ何も来ていなくても、片方しか繋がっていなければそちらへ(RXC は両方動いていても)
6. 覚えたポートが止まっていれば(tx_ok=0)、もう片方へ
7. 両方止まっていれば捨てる。**共有の送信路は止まらない**(次の送信も通る)
"""
import os
import sys
import zlib

sys.path.insert(0, os.path.dirname(__file__))
from migen import *                                        # noqa: E402
from migen.sim import run_simulation                       # noqa: E402
from litex.soc.interconnect import stream                  # noqa: E402
from liteeth.common import eth_phy_description             # noqa: E402

from retrocastx_net import DualPortMACCore                 # noqa: E402

MY_MAC = 0x74C90F7C8429
MY_IP = (169, 254, 132, 41)
UDP_PORT = 34600
BCAST = b"\xff" * 6

PEER_A = bytes.fromhex("0a0000000001")    # LAN1 から自MAC宛ユニキャスト
PEER_B = bytes.fromhex("0a0000000002")    # LAN0 から自IP宛 ARP
PEER_C = bytes.fromhex("0a0000000003")    # LAN1 から他人宛 ARP(覚えない)
PEER_D = bytes.fromhex("0a0000000004")    # LAN0 から自UDPポート宛ブロードキャスト
PEER_E = bytes.fromhex("0a0000000005")    # 一度も見ていない


def mac_bytes(v):
    return v.to_bytes(6, "big")


def eth(dst, src, etype, payload):
    f = dst + src + etype.to_bytes(2, "big") + payload
    return f + bytes(max(0, 60 - len(f)))


def arp_request(src, target_ip):
    p = (b"\x00\x01\x08\x00\x06\x04\x00\x01" + src + bytes((10, 0, 0, 9)) +
         bytes(6) + bytes(target_ip))
    return eth(BCAST, src, 0x0806, p)


def udp_bcast(src, dport, payload=b"RCX!" * 4):
    ip = bytearray(20)
    ip[0], ip[9] = 0x45, 17
    udp = (40001).to_bytes(2, "big") + dport.to_bytes(2, "big") + \
        (8 + len(payload)).to_bytes(2, "big") + bytes(2)
    return eth(BCAST, src, 0x0800, bytes(ip) + udp + payload)


def wire(frame):
    """プリアンブル + フレーム + FCS(電線上の並び)"""
    return (b"\x55" * 7 + b"\xd5" + frame +
            zlib.crc32(frame).to_bytes(4, "little"))


class FakePHY(Module):
    dw = 8

    def __init__(self):
        self.sink = stream.Endpoint(eth_phy_description(8))
        self.source = stream.Endpoint(eth_phy_description(8))


class DUT(Module):
    def __init__(self):
        for i in range(2):
            for d in ("rx", "tx"):
                setattr(self.clock_domains, f"cd_eth{i}_{d}", ClockDomain(f"eth{i}_{d}"))
        self.phys = [FakePHY(), FakePHY()]
        self.submodules += self.phys
        self.submodules.core = DualPortMACCore(
            self.phys, MY_MAC, Constant(int.from_bytes(bytes(MY_IP), "big"), 32),
            UDP_PORT)


def phy_rx(dut, i, sched):
    """sched: [(このドメインの何サイクル目から, フレーム)]"""
    src = dut.phys[i].source
    t = 0
    for at, frame in sched:
        while t < at:
            yield
            t += 1
        data = wire(frame)
        for k, byte in enumerate(data):
            yield src.valid.eq(1)
            yield src.data.eq(byte)
            yield src.last.eq(k == len(data) - 1)
            yield src.last_be.eq(k == len(data) - 1)
            yield
            t += 1
        yield src.valid.eq(0)
        yield src.last.eq(0)
        # IFG
        for _ in range(12):
            yield
            t += 1


def phy_tx(dut, i, out, n):
    snk = dut.phys[i].sink
    yield snk.ready.eq(1)
    buf = bytearray()
    for _ in range(n):
        if (yield snk.valid):
            buf.append((yield snk.data))
            if (yield snk.last):
                out.append(bytes(buf))
                buf = bytearray()
        yield


def sys_rx(dut, out, n):
    src = dut.core.source
    yield src.ready.eq(1)
    buf = bytearray()
    for _ in range(n):
        if (yield src.valid):
            w = yield src.data
            if (yield src.last):
                # last_be は最後の有効バイトの位置を1ビットで示す
                lb = yield src.last_be
                nb = {1: 1, 2: 2, 4: 3, 8: 4}.get(lb, 4)
                buf += w.to_bytes(4, "little")[:nb]
                out.append((bytes(buf), (yield src.error)))
                buf = bytearray()
            else:
                buf += w.to_bytes(4, "little")
        yield


def sys_tx(dut, sched, accepted):
    """sched: [(sysの何サイクル目から, フレーム(FCS無し), tx_ok[, link_ok])]"""
    snk = dut.core.sink
    t = 0
    for at, frame, ok, *link in sched:
        while t < at:
            yield
            t += 1
        yield dut.core.tx_ok.eq(ok)
        yield dut.core.link_ok.eq(link[0] if link else 0b11)
        frame = frame + bytes((-len(frame)) % 4)
        words = [int.from_bytes(frame[k:k + 4], "little") for k in range(0, len(frame), 4)]
        for k, w in enumerate(words):
            yield snk.valid.eq(1)
            yield snk.data.eq(w)
            yield snk.last.eq(k == len(words) - 1)
            yield snk.last_be.eq(0b1000 if k == len(words) - 1 else 0)
            yield
            t += 1
            while not (yield snk.ready):
                assert t < at + 2000, "送信路が詰まった(%d サイクル目から)" % at
                yield
                t += 1
        yield snk.valid.eq(0)
        yield snk.last.eq(0)
        accepted.append(at)


def payload_of(wired):
    """PHY から出たバイト列を検査してフレーム本体を返す。"""
    assert wired[:8] == b"\x55" * 7 + b"\xd5", "プリアンブルが無い: %s" % wired[:8].hex()
    body, fcs = wired[8:-4], wired[-4:]
    assert zlib.crc32(body).to_bytes(4, "little") == fcs, "FCS が合わない"
    return body


def main():
    dut = DUT()
    my = mac_bytes(MY_MAC)
    to_me_a = eth(my, PEER_A, 0x0800, b"\x45" + bytes(40))
    arp_b = arp_request(PEER_B, MY_IP)
    arp_c = arp_request(PEER_C, (10, 0, 0, 1))          # 他人宛
    udp_d = udp_bcast(PEER_D, UDP_PORT)
    # 受信: LAN1 に A、少し後に LAN0 の B と LAN1 の C を**同時に**、最後に LAN0 の D
    rx_sched = {
        0: [(1500, arp_b), (3000, udp_d)],
        1: [(200, to_me_a), (1500, arp_c)],
    }

    def tx_frame(dst, tag):
        return eth(dst, my, 0x0800, tag * 8)

    # 送信(sysサイクル)。最初の1本以外は表が埋まった後に出す
    tx_sched = [
        # 起動直後(まだ何も受けていない)。RXC は両方動くが繋がっているのは LAN1 だけ
        (20,   tx_frame(BCAST,  b"toX0"), 0b11, 0b10), # → LAN1
        (2000, tx_frame(PEER_A, b"toA1"), 0b11),   # → LAN1
        (2300, tx_frame(PEER_B, b"toB1"), 0b11),   # → LAN0
        (2600, tx_frame(PEER_D, b"toD1"), 0b11),   # → LAN0
        (2900, tx_frame(PEER_E, b"toE1"), 0b11),   # 知らない → 最後に自分宛が来た LAN0
        (3200, tx_frame(PEER_C, b"toC1"), 0b11),   # 覚えていない → LAN0
        (3500, tx_frame(PEER_A, b"toA2"), 0b01),   # LAN1 が止まっている → LAN0
        (3800, tx_frame(PEER_A, b"toA3"), 0b00),   # 両方止まっている → 捨てる
        (4100, tx_frame(PEER_B, b"toB2"), 0b11),   # 止まらずに次も通る → LAN0
    ]
    N_SYS = 5200
    rx_out, tx_out, accepted = [], [[], []], []
    gens = {
        "sys": [sys_rx(dut, rx_out, N_SYS), sys_tx(dut, tx_sched, accepted)],
        "eth0_rx": [phy_rx(dut, 0, rx_sched[0])],
        "eth1_rx": [phy_rx(dut, 1, rx_sched[1])],
        "eth0_tx": [phy_tx(dut, 0, tx_out[0], N_SYS * 3)],
        "eth1_tx": [phy_tx(dut, 1, tx_out[1], N_SYS * 3)],
    }
    clocks = {"sys": 22, "eth0_rx": 8, "eth0_tx": 8, "eth1_rx": 8, "eth1_tx": 8}
    run_simulation(dut, gens, clocks=clocks)

    # 1, 2) 受信: 4つとも FCS を外して出る(同時に来た B と C も)
    got = [f for f, err in rx_out if not err]
    print("1) 受信: %d 本 / エラー %d 本" % (len(got), sum(1 for _, e in rx_out if e)))
    for name, f in (("A", to_me_a), ("B", arp_b), ("C", arp_c), ("D", udp_d)):
        assert any(g[:len(f)] == f for g in got), "%s のフレームが sys 側に出ていない" % name

    # 4〜7) 送信
    def tags(port):
        return [payload_of(w)[14:18] for w in tx_out[port]]
    t0, t1 = tags(0), tags(1)
    print("4) LAN0 へ: %s" % [t.decode() for t in t0])
    print("   LAN1 へ: %s" % [t.decode() for t in t1])
    assert t1 == [b"toX0", b"toA1"], "LAN1 に出たもの: %s" % t1
    assert t0 == [b"toB1", b"toD1", b"toE1", b"toC1", b"toA2", b"toB2"], \
        "LAN0 に出たもの: %s" % t0
    # 7) 捨てても共有の送信路は止まらない(最後の送信まで受け付けた)
    assert len(accepted) == len(tx_sched), "送信が途中で止まった"
    for w in tx_out[0] + tx_out[1]:
        assert len(payload_of(w)) >= 60, "最短長まで詰めていない"
    print("\n[OK] 2ポート MAC: 両ポートから受ける(同時でも)/自分宛だけで覚える/"
          "覚えたポートへ送る/既定のポート/止まったポートを避ける・捨てても詰まらない")


if __name__ == "__main__":
    main()
