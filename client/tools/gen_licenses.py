#!/usr/bin/env python3
"""Viewer に同梱する依存ライブラリのライセンス一覧(assets/third-party-licenses.txt)を作る。

About 画面の「オープンソースライセンス」(src/about.rs)がこのファイルを埋め込んで表示する。
依存を足したり版を上げたりしたら作り直してコミットする(CI の checks が古さを検出する)。

    python3 tools/gen_licenses.py           # 書き出す
    python3 tools/gen_licenses.py --check   # 今のファイルと食い違えば 1 で終わる

★**配布する全プラットフォームの和を取る。** 画面は1つなので、iOS で使わない
  Windows 専用のクレートが並んでも害は無いが、足りないのは困る。
★**本文はクレートに入っているファイルをそのまま使う。** MIT は著作権者の行が
  クレートごとに違うので、テンプレートで済ませない。同じ本文は1つにまとめる
  (`MIT OR Apache-2.0` の Apache の本文などは大半が同一)。
★**ファイルを持たないクレートは SPDX の名前だけを出し、標準の本文を付ける。**
  どれがそうかは書き出しのときに表示する。

形式(src/about.rs が読む):
    @pkg <名前> <版>
    license <SPDX式>
    repo <URL>             (あれば)
    text <番号> <ファイル名>  (0個以上)
    ...
    @text <番号>
    <本文>                  (次の @text か終わりまで)
"""
from __future__ import annotations
import json
import pathlib
import re
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
CLIENT = HERE.parent
OUT = CLIENT / "assets" / "third-party-licenses.txt"
ROOTS = {"retrocastx-viewer", "retrocastx-android"}
TARGETS = [
    "aarch64-apple-ios",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-linux-android",
    "x86_64-unknown-linux-gnu",
]
# ライセンスの本文として拾うファイル名(大文字小文字は問わない)
NAME_RE = re.compile(r"^(licen[sc]e|copying|copyright|notice|unlicense)([-_.].*)?$", re.I)
# フォントなど、同梱物のライセンスが置いてある下のフォルダ
SUBDIRS = ["fonts", "license", "licenses", "LICENSES"]

MIT_TEMPLATE = """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def standard_text(spdx: str, pkg) -> str | None:
    """ファイルを持たないクレートに付ける標準の本文"""
    if spdx == "MIT":
        authors = ", ".join(pkg.get("authors") or []) or f"the {pkg['name']} authors"
        return f"MIT License\n\nCopyright (c) {authors}\n\n{MIT_TEMPLATE}"
    if spdx == "Apache-2.0":
        return (CLIENT.parent / "LICENSE").read_text()
    return None


def metadata(target: str):
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target],
        cwd=CLIENT, check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def collect():
    pkgs = {}
    for t in TARGETS:
        m = metadata(t)
        by_id = {p["id"]: p for p in m["packages"]}
        nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
        stack = [p["id"] for p in m["packages"] if p["name"] in ROOTS]
        seen = set()
        while stack:
            i = stack.pop()
            if i in seen:
                continue
            seen.add(i)
            for d in nodes[i]["deps"]:
                # 通常とビルド時の依存だけ(dev は配布物に入らない)
                if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
                    stack.append(d["pkg"])
        for i in seen:
            p = by_id[i]
            # 自分たちのクレート(Viewer・Android・ダミーデバイス)は除く
            if p["name"].startswith("retrocastx-"):
                continue
            pkgs[(p["name"], p["version"])] = p
    return [pkgs[k] for k in sorted(pkgs, key=lambda k: (k[0].lower(), k[1]))]


def license_files(p) -> list[pathlib.Path]:
    root = pathlib.Path(p["manifest_path"]).parent
    files = []
    if p.get("license_file"):
        f = root / p["license_file"]
        if f.is_file():
            files.append(f)
    for d in [root] + [root / s for s in SUBDIRS]:
        if not d.is_dir():
            continue
        for f in sorted(d.iterdir()):
            if not f.is_file() or f in files:
                continue
            if NAME_RE.match(f.name) or (d != root and f.suffix.lower() == ".txt"):
                files.append(f)
    return files


def render() -> tuple[str, list[str]]:
    texts: dict[str, int] = {}
    lines = []
    fallback = []

    def text_id(body: str) -> int:
        body = body.replace("\r\n", "\n").strip("\n") + "\n"
        # 本文の行頭が @ だと区切りと紛れるので、1文字ずらす
        body = re.sub(r"(?m)^@", " @", body)
        if body not in texts:
            texts[body] = len(texts)
        return texts[body]

    for p in collect():
        lic = (p.get("license") or "").strip()
        lines.append(f"@pkg {p['name']} {p['version']}")
        lines.append(f"license {lic or '(記載なし)'}")
        if p.get("repository"):
            lines.append(f"repo {p['repository']}")
        files = license_files(p)
        for f in files:
            body = f.read_text(errors="replace")
            rel = f.relative_to(pathlib.Path(p["manifest_path"]).parent)
            lines.append(f"text {text_id(body)} {rel}")
        if not files:
            ids = [s for s in re.split(r"\s+(?:OR|AND|WITH)\s+|/|[()]", lic) if s]
            added = False
            for spdx in ids:
                body = standard_text(spdx, p)
                if body:
                    lines.append(f"text {text_id(body)} ({spdx} の標準の本文)")
                    added = True
                    break
            fallback.append(f"{p['name']} {p['version']} ({lic or '記載なし'})"
                            + ("" if added else " ★本文なし"))
    out = ["# 生成物。手で直さない(client/tools/gen_licenses.py)"] + lines
    for body, i in sorted(texts.items(), key=lambda kv: kv[1]):
        out.append(f"@text {i}")
        out.append(body.rstrip("\n"))
    return "\n".join(out) + "\n", fallback


def main():
    text, fallback = render()
    if "--check" in sys.argv:
        if not OUT.is_file() or OUT.read_text() != text:
            print(f"{OUT.relative_to(CLIENT)} が依存と合っていません。"
                  "python3 client/tools/gen_licenses.py で作り直してください", file=sys.stderr)
            sys.exit(1)
        return
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text)
    n = text.count("\n@pkg ")
    print(f"{OUT.relative_to(CLIENT)}: {n} クレート, {len(text.encode()) // 1024} KiB")
    if fallback:
        print("ライセンスのファイルを持たないクレート(標準の本文を付けた):")
        for f in fallback:
            print("  " + f)


if __name__ == "__main__":
    main()
