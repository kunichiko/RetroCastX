#!/usr/bin/env python3
"""`ato build` が壊す等長配線(tuning pattern)を復元する。

★**`ato build` を走らせたら必ずこれを実行すること。**

## 何が起きるか

KiCad の等長配線(蛇行)は `(generated ... (type "tuning_pattern") ...)` という
オブジェクトで、原点・終点・振幅・間隔・目標長などのパラメータを持つ。
**atopile のPCBシリアライザはこの中身をモデル化しておらず、書き出すときに
uuid / type / name / layer / members だけを残して全部捨てる。**

    pcbnewが保存した版   トークン数 30  ← 完全
    ato build 後         トークン数  6  ← パラメータが消えている

パラメータが消えた状態で pcbnew がファイルを開くと、KiCad はパターンを解釈できず
(`Unknown length tuning token` 等のassertが出る)、**蛇行部の配線を作り直して
まったく別の場所へ移動させる**。2026-08-15 に GbE の等長配線4ペア(67本)が
基板外(x=207.5)へ飛んだ。

## 使い方

    # ato build の直後に
    /Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3 \
        tools/restore_tuning_patterns.py [基準ファイル]

基準ファイルを省略すると `git show HEAD:<pcb>` を使う。**基準は「pcbnewが保存した
正常な版」でなければならない。** ato build 後のものを基準にすると壊れたまま固定される。

配線の座標も比較し、基準と違っていれば戻す(pcbnewが既に動かしてしまった場合の復旧)。
ネット番号は atopile が振り直すので、**現在のファイルの値を維持**して座標だけ入れ替える。
"""
import re
import subprocess
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
PCB = ROOT / "layouts/default/default.kicad_pcb"
REL = "hardware/adc-frontend/layouts/default/default.kicad_pcb"


def blocks(s, kinds=("segment", "arc", "via", "generated")):
    """トップレベルの各ブロックを uuid をキーに取り出す。"""
    out = {}
    for kind in kinds:
        for m in re.finditer(r"^\t\(" + kind + r"\b", s, re.M):
            a = m.start()
            d, j = 0, a
            while True:
                if s[j] == "(":
                    d += 1
                elif s[j] == ")":
                    d -= 1
                    if d == 0:
                        break
                j += 1
            blk = s[a:j + 1]
            u = re.search(r'\(uuid "([^"]+)"\)', blk)
            if u:
                out[u.group(1)] = (kind, blk, a, j + 1)
    return out


def geom(b):
    return tuple(re.findall(r"\((?:start|end|mid|at) ([-\d.]+) ([-\d.]+)\)", b))


def ntok(b):
    return len(re.findall(r"\(\w+", b))


def main():
    if len(sys.argv) > 1:
        ref = pathlib.Path(sys.argv[1]).read_text()
        src = sys.argv[1]
    else:
        ref = subprocess.run(
            ["git", "show", f"HEAD:{REL}"], cwd=ROOT.parent.parent,
            capture_output=True, text=True, check=True).stdout
        src = "git HEAD"
    cur = PCB.read_text()
    old, new = blocks(ref), blocks(cur)

    targets = []
    for u in set(old) & set(new):
        if old[u][0] == "generated":
            # パラメータが剥ぎ取られているものを戻す
            if ntok(new[u][1]) < ntok(old[u][1]):
                targets.append(u)
        elif geom(old[u][1]) != geom(new[u][1]):
            targets.append(u)

    if not targets:
        print(f"復元不要(基準: {src})")
        return 0

    edits = []
    for u in targets:
        ob, nb = old[u][1], new[u][1]
        # ネット番号は現在の値を維持する(atopileが振り直すため)
        cn = re.search(r"\(net (\d+)\)", nb)
        rep = re.sub(r"\(net \d+\)", cn.group(0), ob) if cn else ob
        edits.append((new[u][2], new[u][3], rep))
    edits.sort()
    out, prev = [], 0
    for a, b, rep in edits:
        out.append(cur[prev:a])
        out.append(rep)
        prev = b
    out.append(cur[prev:])
    PCB.write_text("".join(out))

    ngen = sum(1 for u in targets if old[u][0] == "generated")
    print(f"復元(基準: {src}): tuning pattern {ngen}個 / 配線 {len(targets) - ngen}本")
    return 0


if __name__ == "__main__":
    sys.exit(main())
