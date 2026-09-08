#!/usr/bin/env python3
"""シェルスクリプトが macOS の bash でも動くか検査する。

## なぜ要るか

macOS の `/bin/bash` は **3.2**(2007年)のまま。CI の Ubuntu は bash 5 なので、
**CI は通るのに手元の Mac だけ落ちる**という形の壊れ方をする。実際に踏んだ:

    gateware/tools/build_closed.sh:108
      echo "=== タイミング成立: seed $s。ecppack します ==="

bash 3.2 は `$s` の直後にある全角「。」の**1バイト目まで変数名として読む**ので、
`s\xe3` という変数を探しに行き、`set -u` のため即終了する。落ちる位置が最悪で、

    タイミングの合うシードを見つけた直後、ecppack を実行する手前

だった。`.bit` を書くのは ecppack なので**新しいビットストリームは作られない**のに、
ログの最後は全ドメイン PASS の表なので**成功に見える**。しかもこの echo は
「シードを振り直して当たった」経路にしかなく、1回目で閉じたときは通る ─
つまり**このスクリプトが存在する理由そのものの経路だけ**が壊れていた。

## 何を見るか

1. `$VAR` の直後がマルチバイト文字(上記そのもの)
2. bash 4 以降にしか無い構文(macOS の bash 3.2 では構文エラーか誤動作)

いずれも `${VAR}` にする・別の書き方にする、で直る。
"""
import io
import re
import subprocess
import sys

# ★**`${VAR}` は対象外。** 波括弧で範囲が明示されていれば bash 3.2 でも正しい。
#   ここで拾いたいのは「区切りが曖昧なまま全角文字が続いている」場合だけ。
MULTIBYTE_AFTER_VAR = re.compile(r'\$[A-Za-z_][A-Za-z0-9_]*(?=[^\x00-\x7f])')

# bash 4 以降の構文。macOS の bash 3.2 には無い。
BASH4_ONLY = [
    (re.compile(r'\$\{[A-Za-z_][A-Za-z0-9_]*(\[[^\]]*\])?(\^\^|,,|\^|,)'),
     "大文字/小文字変換 ${var^^} ${var,,} は bash 4 以降"),
    (re.compile(r'\bdeclare\s+-[A-Za-z]*A'), "連想配列 declare -A は bash 4 以降"),
    (re.compile(r'\b(readarray|mapfile)\b'), "readarray/mapfile は bash 4 以降"),
    (re.compile(r'\|&'), "|& は bash 4 以降(2>&1 | と書く)"),
    (re.compile(r'&>>'), "&>> は bash 4 以降(>>file 2>&1 と書く)"),
]


def scan_text(text):
    """1ファイル分のテキストを見て [(行番号, 抜粋, 理由)] を返す。"""
    out = []
    for i, line in enumerate(text.splitlines(), 1):
        # コメント行は実行されないので見ない(説明として例を書きたいことがある)
        if line.lstrip().startswith("#"):
            continue
        for m in MULTIBYTE_AFTER_VAR.finditer(line):
            out.append((i, m.group(0), "%s の直後がマルチバイト文字。"
                        "bash 3.2 は1バイト目まで変数名として読む → ${%s} にする"
                        % (m.group(0), m.group(0)[1:])))
        for pat, why in BASH4_ONLY:
            if pat.search(line):
                out.append((i, line.strip()[:60], why))
    return out


def tracked_shell_files():
    files = subprocess.check_output(["git", "ls-files"], text=True).split()
    return [f for f in files if f.endswith((".sh", ".bash"))]


def selftest():
    """★**検査が本当に捕まえられることを確かめる。**

    実際に踏んだ行をそのまま食わせる。検査が嘘をつくのは検査が無いより悪い
    (このリポジトリでは check_gain_defaults.py が一度そうなった)。
    """
    bad = 'echo "=== タイミング成立: seed $s。ecppack します ==="'
    hits = scan_text(bad)
    assert hits, "実際に落ちた行を検出できていない: %r" % bad
    ok = 'echo "=== タイミング成立: seed ${s}。ecppack します ==="'
    assert not scan_text(ok), "波括弧で直した行を誤検出している"
    # ASCII が続く場合は区切りが曖昧でないので拾わない
    assert not scan_text('echo "seed $s done"'), "ASCII 区切りを誤検出している"
    # コメント行は実行されない
    assert not scan_text('# 例: seed $s。'), "コメントを誤検出している"
    print("selftest: OK")


def main():
    if "--selftest" in sys.argv:
        selftest()
        return 0
    selftest()          # 本番の前に必ず自分を試す
    bad = 0
    for f in tracked_shell_files():
        try:
            text = io.open(f, encoding="utf-8").read()
        except UnicodeDecodeError:
            continue
        for lineno, excerpt, why in scan_text(text):
            print("%s:%d: %s" % (f, lineno, why))
            print("    %s" % excerpt)
            bad += 1
    if bad:
        print("\n%d 件。macOS の bash 3.2 で落ちるか誤動作します。" % bad)
        return 1
    print("シェルスクリプト: macOS の bash 3.2 でも動く形になっています")
    return 0


if __name__ == "__main__":
    sys.exit(main())
