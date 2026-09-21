#!/usr/bin/env python3
"""ROHM EMZT6.8ET2R のランドを、データシート推奨の**5パッド**に作り直す。

## なぜ

LCSC/EasyEDA から取り込んだフットプリントは汎用の**6パッド EMT6** ランドだったが、
実物は EMD5 (JEITA SC-75A) で**リードは5本**しかない
(datasheets/emzt6.8e.pdf、2026-08-18 に 900dpi で拡大して確認)。

    ランド図: 下段3パッド + 上段2パッド(左右のみ)。上段中央にリードは無い
    リード番号 1=下段左 2=下段中(共通アノード) 3=下段右 4=上段右 5=上段左

6パッドのままだと困ることが3つある:

    1. 上段中央がリードの載らない裸パッドになる(手半田で半田を盛るとブリッジ)
    2. ★PCBA のメタルマスクがその裸パッドにペーストを刷る。行き場の無い半田が
       ボール化・ブリッジの原因になる
    3. パッド幅が 0.28mm しかなく、リード幅 0.22±0.05mm(最大0.27)に対して
       **片側フィレットが最小 0.005mm** = 実質ゼロ。ROHM 推奨は外側0.4/中央0.3

## 何をするか (パッド番号で対応付ける。座標では判定しない)

    旧pad  新pad   新x      新幅    備考
      1  →   1   -0.55    0.4
      2  →   2    0.0     0.3     共通アノード
      3  →   3   +0.55    0.4
      4  →   4   +0.55    0.4
      5  →  削除                   リードが載らない裸パッド
      6  →   5   -0.55    0.4     データシートのリード5(上段左)

y座標は**一切変更しない**。高さも現状の 0.6mm を維持する(ROHM図は0.45だが、
上下行の間隙が 0.9mm あってブリッジ要因にならず、トゥ/ヒールが広い方が
手半田と外観検査で有利なため)。

★旧pad5(裸パッド)には GND 配線が繋がっているが、**パッドを消しても切れない**。
  配線同士は端点の共有で繋がるのでパッドの有無に依存しない。実際この配線は
  pad2(本物の共通アノード)へ通過していくもので、リードの無いパッドへの接続は
  もともと電気的に無意味だった。

## ★過去の失敗を繰り返さないための注意 (2026-08-18)

**y座標の符号で上下段を判定してはいけない。** KiCad は footprint を裏面(B.Cu)へ
反転配置するとき**ローカル座標の y を反転して保存する**:

    D1 (F.Cu, 反転なし)  pad1(-0.50, +0.75)  pad4(+0.50, -0.75)
    D5 (B.Cu, 反転あり)  pad1(-0.50, **-0.75**)  pad4(+0.50, **+0.75**)

最初に書いた版は y の符号で行を判定していたため、裏面の6個で番号が総入れ替わりに
なり、**共通アノードの pad2 を削除**していた。しかも「5更新+1削除」という
件数チェックは通ってしまい検出できなかった。

→ 対応付けは**パッド番号**で行い、検証は**ネット割り当ての前後比較**で行う。
  件数だけの検証は無意味。

## 使い方

    python3 tools/fix_esd_land.py --check   # 現状を表示するだけ
    python3 tools/fix_esd_land.py           # 適用(前後のネット割り当てを検証)
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MOD = ROOT / "parts/ROHM_EMZT6_8ET2R/EMT6_L1.6-W1.2-P0.50-LS1.6-BL.kicad_mod"
PCB = ROOT / "layouts/default/default.kicad_pcb"
FP_NAME = "ROHM_EMZT6_8ET2R"
KIPY = "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3"
H = 0.6

# 旧pad番号 -> (新pad番号, 新x, 新幅)。None は削除。
MAP = {"1": ("1", -0.55, 0.40),
       "2": ("2", 0.0, 0.30),
       "3": ("3", 0.55, 0.40),
       "4": ("4", 0.55, 0.40),
       "5": None,
       "6": ("5", -0.55, 0.40)}
# 旧xの符号がこれと違ったら、前提が崩れているので止める
EXPECT_SIGN = {"1": -1, "2": 0, "3": 1, "4": 1, "5": 0, "6": -1}


def blocks(s, pat):
    """pat にマッチする位置から括弧の釣り合う範囲を切り出す。"""
    for m in re.finditer(pat, s, re.M):
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
        yield a, j + 1


def patch_footprint(blk, st):
    out, prev = [], 0
    for a, b in blocks(blk, r'^\t*\(pad "'):
        pad = blk[a:b]
        num = re.search(r'\(pad "([^"]+)"', pad).group(1)
        m = re.search(r'\(at ([-\d.]+) ([-\d.]+)((?: [-\d.]+)?)\)', pad)
        x, y, rot = float(m.group(1)), float(m.group(2)), m.group(3)

        sign = 0 if abs(x) < 0.1 else (1 if x > 0 else -1)
        assert sign == EXPECT_SIGN[num], f"pad{num} の x 符号が想定外: {x}"
        assert abs(abs(y) - 0.75) < 1e-6, f"pad{num} の |y| が想定外: {y}"

        out.append(blk[prev:a])
        prev = b
        if MAP[num] is None:
            st["deleted"] += 1
            while out and out[-1].endswith(("\n\t\t", "\n\t\t\t")):
                out[-1] = out[-1].rstrip("\t")
            continue
        new, nx, w = MAP[num]
        pad = re.sub(r'\(pad "[^"]+"', f'(pad "{new}"', pad, count=1)
        # y は触らない(裏面反転で符号が入れ替わっているため)
        pad = re.sub(r'\(at [-\d.]+ [-\d.]+(?: [-\d.]+)?\)',
                     f'(at {nx} {y}{rot})', pad, count=1)
        pad = re.sub(r'\(size [\d.]+ [\d.]+\)', f'(size {w} {H})', pad, count=1)
        out.append(pad)
        st["moved"] += 1
    out.append(blk[prev:])
    return "".join(out)


def netmap():
    """pcbnew で {ref: {pad番号: ネット名}} を取る。"""
    code = ("import pcbnew,json\n"
            f"b=pcbnew.LoadBoard({str(PCB)!r})\n"
            "out={}\n"
            "for f in b.GetFootprints():\n"
            # ★GetLibItemName() には**ライブラリ名が入らない**ので使わないこと。
            #   ここを間違えて対象0個になり、検証が素通りした(2026-08-18)。
            f"    if {FP_NAME!r} not in str(f.GetFPID().GetUniStringLibId()): continue\n"
            "    out[f.GetReference()]={p.GetNumber():p.GetNetname() for p in f.Pads()}\n"
            "print(json.dumps(out))")
    r = subprocess.run([KIPY, "-c", code], capture_output=True, text=True)
    import json
    for line in reversed(r.stdout.strip().splitlines()):
        if line.startswith("{"):
            d = json.loads(line)
            assert d, "★対象の footprint が0個。検証が素通りするので中断する"
            return d
    raise RuntimeError(f"pcbnew の出力を解釈できない: {r.stdout[-400:]} {r.stderr[-400:]}")


def show():
    for path, pat in ((MOD, r'^\(footprint '), (PCB, r'^\t\(footprint "' + FP_NAME)):
        s = path.read_text()
        for a, b in blocks(s, pat):
            blk = s[a:b]
            r = re.search(r'\(property "Reference" "([^"]+)"', blk)
            pads = re.findall(
                r'\(pad "([^"]+)"[^\n]*\n\s*\(at ([-\d.]+) ([-\d.]+)(?: [-\d.]+)?\)'
                r'\s*\n\s*\(size ([\d.]+) ([\d.]+)\)', blk)
            print(f"  {(r.group(1) if r else path.name):<6} {len(pads)}パッド: " +
                  " ".join(f"{p}({float(x):+.2f},{float(y):+.2f})/{w}"
                           for p, x, y, w, _ in pads))


def main():
    if "--check" in sys.argv:
        show()
        return 0

    before = netmap()

    s = MOD.read_text()
    st = {"moved": 0, "deleted": 0}
    a, b = next(blocks(s, r'^\(footprint '))
    MOD.write_text(s[:a] + patch_footprint(s[a:b], st) + s[b:])
    print(f"{MOD.name}: {st['moved']}更新 / {st['deleted']}削除")

    s = PCB.read_text()
    out, prev, n = [], 0, 0
    st = {"moved": 0, "deleted": 0}
    for a, b in blocks(s, r'^\t\(footprint "' + FP_NAME):
        out.append(s[prev:a])
        out.append(patch_footprint(s[a:b], st))
        prev = b
        n += 1
    out.append(s[prev:])
    PCB.write_text("".join(out))
    print(f"{PCB.name}: footprint {n}個 / {st['moved']}更新 / {st['deleted']}削除")

    # ★本命の検証: ネット割り当てが意図どおり移ったか
    after = netmap()
    bad = []
    for ref, pads in before.items():
        want = {"1": pads["1"], "2": pads["2"], "3": pads["3"],
                "4": pads["4"], "5": pads["6"]}   # 旧pad6 の網が新pad5 へ
        got = after.get(ref, {})
        if got != want:
            bad.append(f"  {ref}: 期待 {want}\n      実際 {got}")
        if pads["2"] != "gnd":
            bad.append(f"  {ref}: 変更前の pad2 が gnd でない({pads['2']})")
    if bad:
        print("★ネット割り当ての検証に失敗:\n" + "\n".join(bad))
        return 1
    print(f"検証OK: {len(before)}個すべてでネット割り当てが保存された "
          "(pad2=gnd, 旧pad6→新pad5, 裸パッド削除)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
