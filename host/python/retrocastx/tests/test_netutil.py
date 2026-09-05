#!/usr/bin/env python3
"""netutil のブロードキャスト宛先の選び方を検証する(実機・NIC構成に依存しない)。

確かめたいのは1点だけ。**実在するNICがあるなら限定ブロードキャスト
(255.255.255.255)を返してはいけない。**

限定ブロードキャストはサブネット経路を持たないので既定経路に載る。既定経路が
point-to-point の VPN トンネルだと sendto そのものが EADDRNOTAVAIL で落ちる。
discover はその例外で終了してしまうので、症状が「ボードが見つからない」になり、
ボードの故障やケーブル断と見分けがつかない。2026-09-02 と 2026-09-04 に
それぞれ時間を溶かしている。

Run:  python3 -m retrocastx.tests.test_netutil
"""
import sys

from .. import netutil

# 実測した構成(2026-09-04、VPN接続中の作業機)。
#   lo0    ループバック。broadcast なし
#   en0/1  有線とWi-Fi。同じサブネットなので宛先は1つに集約される
#   utun4  VPNトンネル。point-to-point なので broadcast なし
REAL = [
    ("127.0.0.1", None, False),
    ("192.168.11.24", "192.168.11.255", False),
    ("192.168.11.33", "192.168.11.255", False),
    ("172.23.24.193", None, True),
]

FAILS = []


def check(name, got, want):
    if got == want:
        print("  [OK] %s" % name)
    else:
        print("  [NG] %s: %r (期待 %r)" % (name, got, want))
        FAILS.append(name)


def main():
    print("実測のNIC構成から宛先を選ぶ")
    check("VPN下でもサブネット宛を選ぶ",
          netutil.pick_targets(REAL), ["192.168.11.255"])
    check("限定ブロードキャストを混ぜない",
          netutil.LIMITED in netutil.pick_targets(REAL), False)

    print("複数サブネットなら全部へ出す(どれがボードと同じL2かは分からない)")
    multi = [
        ("192.168.11.24", "192.168.11.255", False),
        ("10.0.0.5", "10.0.0.255", False),
        ("127.0.0.1", None, False),
    ]
    check("2つ並ぶ", netutil.pick_targets(multi),
          ["192.168.11.255", "10.0.0.255"])

    print("--bind を指定したらそのNICだけに絞る")
    check("10.0.0.5 を指定",
          netutil.pick_targets(multi, "10.0.0.5"), ["10.0.0.255"])
    check("どのNICにも無いアドレスなら候補ゼロ→最後の手段",
          netutil.pick_targets(multi, "192.168.99.1"), [netutil.LIMITED])

    print("使えるNICが1つも無いときだけ限定ブロードキャストへ落ちる")
    none = [("127.0.0.1", None, False), ("172.23.24.193", None, True)]
    check("フォールバック", netutil.pick_targets(none), [netutil.LIMITED])
    check("説明文がpsutilに触れる",
          "psutil" in netutil.explain_failure([netutil.LIMITED]), True)

    print("\nALL OK" if not FAILS else "\n★失敗: %s" % FAILS)
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
