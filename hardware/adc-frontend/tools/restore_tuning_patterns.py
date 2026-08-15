#!/usr/bin/env python3
"""空の等長配線オブジェクト(tuning pattern)を .kicad_pcb から取り除く。

★**`ato build` を走らせたら必ずこれを実行すること。**

## なぜ削除するのか(2026-08-15)

KiCad の等長配線(蛇行)は `(generated ... (type tuning_pattern) ...)` というオブジェクトで
表され、原点・終点・振幅などのパラメータと、実際の配線を指す `members` を持つ。

**atopile のPCBシリアライザはこの中身をモデル化しておらず、書き出すときに
uuid / type / name / layer / members だけを残して全部捨てる。**

    pcbnewが保存した版   トークン数 30
    ato build 後         トークン数  6

パラメータが消えたファイルを pcbnew が開くと `Unknown length tuning token` 等の
assert が出て、KiCad はパターンを作り直し、**member の配線を別の場所へ移動させる**。

当初はパラメータを「復元」していたが、**復元しても再発した**。調べると origin/end は
最初から中身が無かった:

    #1〜#6  origin=(103.32, 44.75) → end=(113.32, 44.75)   基板の外(基板は y=49 から)
    #7〜#11 origin=(0, 0)          → end=(10, 0)            原点

どれも「長さ10mmの水平線」という既定値で、実際の蛇行位置(x 163.7〜174.2)と無関係。
pcbnew自身が保存した ec4b624 の時点で既にこうなっていた(最初の ato build が剥ぎ取り、
KiCadが既定値で埋め戻したものと思われる)。

つまり **generated は情報を持っておらず、KiCadが再生成するたびに member の配線を
プレースホルダの原点へ引きずるだけ**だった。実際に2回発火し、GbEの等長配線67本が
基板外(x=277.5)まで飛んだ。

→ **generated ブロックは削除する。** 蛇行は普通の segment/arc として残るので形は
   変わらない。代償は「長さ調整ツールで対話的に再チューニングできなくなる」ことだけで、
   必要になれば pcbnew で引き直せばよい。

★**このツールは配線の座標を書き換えない。** 以前は「基準ファイルと座標が違う配線を
  戻す」機能を持たせていたが、**利用者が意図して動かした配線まで巻き戻した**
  (2026-08-15に実際に vsync_a / vin_vsync2 を巻き戻した)。配線の復旧が要るときは
  ネットを限定した使い捨てスクリプトで行うこと。

## 使い方

    # ato build の直後に
    python3 tools/restore_tuning_patterns.py
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PCB = ROOT / "layouts/default/default.kicad_pcb"


def main():
    s = PCB.read_text()
    s2, n = re.subn(r"\t\(generated\b.*?\n\t\)\n", "", s, flags=re.S)
    if not n:
        print("generated ブロックなし(何もしない)")
        return 0
    PCB.write_text(s2)
    print(f"generated ブロック {n}個を削除した")
    print("  蛇行そのものは普通の配線として残っている(形は変わらない)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
