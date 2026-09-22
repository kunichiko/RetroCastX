#!/usr/bin/env python3
"""フットプリントの中身を直したとき、**配置を失わずに** PCB へ反映する。

## なぜ要るか

`ato build` は「部品の品番が変わった」ときは PCB のフットプリントをその場で
差し替えるが、**同じ品番のまま .kicad_mod の中身だけ直した場合は反映しない**
(フットプリント名で照合しているため)。反映させるには PCB 側のフットプリント
ブロックを一度消して作り直させるしかないが、そうすると**その部品の配置が失われる**。

    2026-09-21  PCBファイルごと消して作り直し、小亀基板の配置を全部飛ばした
    2026-09-22  ブロックを1個だけ消して、メイン基板の J13 が (86.8,107.2) から
                (86.3,107.5) へずれた

このスクリプトは「位置と回転を控える → ブロックを消す → build → 書き戻す」を
まとめてやる。pcbnew を開けるなら **Tools → Update Footprints from Library** の方が
素直なので、そちらが使えるならそれでよい(これは CLI 作業用)。

## build を2回走らせる理由

置き換わったパッドは**1回目の build ではネットが付かない**(差し替えとネット割当が
同じパスで走るため)。ラッツネストが出ない/DRCの未配線が減る/パッドが他ネットの
配線に乗って shorting_items が出る、という形で現れる。2回目で正しくなる。

## 使い方

    python3 tools/refresh_footprint.py din8            # atopile_address を指定
    python3 tools/refresh_footprint.py din8 j_drgb     # 複数可

後片付け(lock_designators.py / restore_pcb_settings.py)はこのスクリプトが
**最後の build の後に**呼ぶ。★ロックの後に build してはいけない(PCB 側の
デジグネータだけ欠番に戻ることがある)。
"""
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PCB = ROOT / "layouts/default/default.kicad_pcb"
ATO = shutil.which("ato") or str(pathlib.Path.home() / ".local/bin/ato")


def footprint_blocks(text):
    """(footprint ...) の範囲を返す。descr 等の文字列内の括弧を数えないこと。"""
    out = []
    for m in re.finditer(r'\(footprint "', text):
        i = m.start()
        depth = 0
        j = i
        instr = False
        esc = False
        while True:
            ch = text[j]
            if instr:
                if esc:
                    esc = False
                elif ch == "\\":
                    esc = True
                elif ch == '"':
                    instr = False
            else:
                if ch == '"':
                    instr = True
                elif ch == "(":
                    depth += 1
                elif ch == ")":
                    depth -= 1
                    if depth == 0:
                        break
            j += 1
        out.append((i, j + 1))
    return out


def info(text, addr):
    for a, b in footprint_blocks(text):
        blk = text[a:b]
        if f'(property "atopile_address" "{addr}"' in blk:
            at = re.search(r'\n\t\t\(at ([-\d.]+) ([-\d.]+)(?: ([-\d.]+))?\)', blk)
            ref = re.search(r'\(property "Reference" "([^"]+)"', blk)
            return (a, b, at.group(0) if at else None,
                    ref.group(1) if ref else "?")
    return None


def build():
    subprocess.run([ATO, "build"], cwd=ROOT, check=True,
                   stdout=subprocess.DEVNULL)


def main(addrs):
    if not addrs:
        print(__doc__)
        return 1
    s = PCB.read_text()
    saved = {}
    for addr in addrs:
        got = info(s, addr)
        if not got:
            print(f"ERR {addr}: PCB に見つからない")
            return 1
        a, b, at, ref = got
        saved[addr] = (at, ref)
        print(f"控えた  {addr:12} {ref:6} {at.strip() if at else '(at なし)'}")

    # 新しい順に消す(インデックスがずれないように)
    for addr in sorted(addrs, key=lambda x: info(s, x)[0], reverse=True):
        a, b, _, _ = info(s, addr)
        s = s[:a] + s[b:]
    PCB.write_text(s)
    print(f"削除    {len(addrs)} ブロック")

    build(); print("build 1回目 ✓")
    build(); print("build 2回目 ✓(パッドのネット割当はここで付く)")

    s = PCB.read_text()
    for addr, (at, ref) in saved.items():
        got = info(s, addr)
        if not got:
            print(f"ERR {addr}: build 後に見つからない")
            return 1
        a, b, now, _ = got
        if at and now and at != now:
            s = s[:a] + s[a:b].replace(now, at, 1) + s[b:]
            print(f"復元    {addr:12} {now.strip()} -> {at.strip()}")
        else:
            print(f"位置OK  {addr:12} {now.strip() if now else ''}")
    PCB.write_text(s)

    # デジグネータのロックが無いプロジェクトでは、控えた参照番号を書き戻す
    # (ロックがある場合は lock_designators.py が正典なので触らない)
    if not (ROOT / "tools/lock_designators.py").exists():
        s = PCB.read_text()
        for addr, (at, ref) in saved.items():
            a, b, _, now_ref = info(s, addr)
            if now_ref != ref:
                blk = s[a:b].replace(f'(property "Reference" "{now_ref}"',
                                     f'(property "Reference" "{ref}"', 1)
                s = s[:a] + blk + s[b:]
                print(f"参照復元 {addr:12} {now_ref} -> {ref}")
        PCB.write_text(s)
        refs = re.findall(r'\(property "Reference" "([^"]+)"', s)
        dup = sorted({r for r in refs if refs.count(r) > 1})
        if dup:
            print(f"★参照番号が重複: {dup} — pcbnew で直すこと")

    for tool in ("tools/lock_designators.py", "tools/restore_pcb_settings.py"):
        if (ROOT / tool).exists():
            print(f"--- {tool}")
            subprocess.run([sys.executable, tool], cwd=ROOT, check=True)

    # 検証
    s = PCB.read_text()
    bad = 0
    for a, b in footprint_blocks(s):
        blk = s[a:b]
        for pm in re.finditer(r'\(pad "[^"]+"[\s\S]{0,700}?(?=\(pad "|\Z)', blk):
            if not re.search(r'\(net \d+ "', pm.group(0)):
                bad += 1
    print(f"\n検証: ネット未割当パッド {bad} 個(コネクタのN.C.ピン等は元から未割当)")
    for addr, (at, ref) in saved.items():
        a, b, now, ref2 = info(s, addr)
        print(f"      {addr:12} {ref2:6} {now.strip() if now else ''}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
