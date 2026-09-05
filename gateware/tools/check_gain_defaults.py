#!/usr/bin/env python3
"""細ゲインの既定値が3箇所で揃っているか確かめる。

## なぜ要るか

同じ既定値が**4箇所**にある:

  1. gateware/retrocastx_stream.py cfg_gain_r/g/b  ★**電源投入時の実効値**
  2. gateware/retrocastx_i2c.py    cfg_gain_r/g/b  1 に comb で上書きされる(死んでいる)
  3. client/src/profiles.rs        REGS_X68000     「入力設定を書く」で送る値
  4. client/src/main.rs            tune_gain_*     読み戻す前の初期表示

2026-09-03 に2回はまった:

  ・3 だけを変えたため「Viewer を再起動したらゲインが下がった」ように見えた。
    実際には失われておらず、焼き直して 1 の値に戻ったのを Viewer が正直に
    読み戻していただけ。
  ・そこで 2 を直したが **2 は死んでいる**ので効かず、焼き直しても古い値が出た。
    このツールも 2 しか見ていなかったため「3箇所とも一致」と**偽の安心**を
    返した。検査が嘘をつくのは検査が無いより悪い。

値そのものより食い違いに気づけないことが問題なので機械で見る。

    python3 tools/check_gain_defaults.py     # 揃っていなければ非0で終了
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent


def grab(path, patterns):
    text = (ROOT / path).read_text(encoding="utf-8")
    out = {}
    for ch, pat in patterns.items():
        m = re.search(pat, text)
        if not m:
            sys.exit("%s から %s の既定値を読めません(パターン: %s)" % (path, ch, pat))
        out[ch] = int(m.group(1))
    return out


sources = {
    "gateware/retrocastx_stream.py ★実効値": grab(
        "gateware/retrocastx_stream.py",
        {c: r"cfg_gain_%s     = Signal\(8, reset=(\d+)\)" % c for c in "rgb"}),
    "gateware/retrocastx_i2c.py (上書きされる)": grab(
        "gateware/retrocastx_i2c.py",
        {c: r"cfg_gain_%s = Signal\(8, reset=(\d+)\)" % c for c in "rgb"}),
    "client/src/profiles.rs (入力設定を書く)": grab(
        "client/src/profiles.rs",
        {c: r"CFG_KEY_GAIN_%s, (\d+)," % c.upper() for c in "rgb"}),
    "client/src/main.rs (初期表示)": grab(
        "client/src/main.rs",
        {c: r"tune_gain_%s: (\d+)," % c for c in "rgb"}),
}

ref_name, ref = next(iter(sources.items()))
bad = {n: v for n, v in sources.items() if v != ref}
for name, vals in sources.items():
    print("  %-40s R=%-3d G=%-3d B=%d" % (name, vals["r"], vals["g"], vals["b"]))
if bad:
    print("\n細ゲインの既定値が揃っていません。%d箇所とも同じ値にしてください。"
          % len(sources), file=sys.stderr)
    print("(ボードを焼き直すと電源投入時の値に戻り、Viewer はそれを読み戻します)",
          file=sys.stderr)
    sys.exit(1)
print("\n細ゲインの既定値: %d箇所とも一致" % len(sources))
