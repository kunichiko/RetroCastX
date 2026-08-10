#!/bin/sh
# アプリアイコンを master の PNG から作り直す。
#
#   packaging/make-icons.sh [master.png]     既定: packaging/AppIcon.png
#
# 出力(どれもコミットする。CIには画像変換ツールを入れない方針):
#   packaging/macos/AppIcon.icns    .app に入れる(bundle.sh が拾う)
#   packaging/windows/AppIcon.ico   exe に埋め込む(build.rs が拾う)
#   packaging/AppIcon-256.png       実行時に窓/タスクバーへ設定する(src/appicon.rs が埋め込む)
#
# macOS 標準の sips / iconutil だけで作る(Homebrew等に依存しない)。
# .ico は各サイズのPNGを収めた形式にする(Windows Vista以降が扱える)。
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
MASTER="${1:-${HERE}/AppIcon.png}"
[ -f "${MASTER}" ] || { echo "master が無い: ${MASTER}" >&2; exit 1; }

TMP=$(mktemp -d)
trap 'rm -rf "${TMP}"' EXIT

# --- macOS: .icns ---
#
# **macOS はアイコンに角丸マスクをかけない。** 丸みも余白も素材側で作る決まりで、
# 全面ベタ(角が不透明)の絵をそのまま入れると Dock で四角いアイコンになる。
# 実際それで一度作り直した(Finderは小さく表示するので黒角が目立たず気づけない)。
#
# Apple の配置規則(Big Sur以降): 1024の画布に本体 824×824 を中央配置、角丸半径
# 185.4。ここでは Pillow で本体を丸め、周囲は透明にする。
# Pillow が無ければ止まる(生成物はコミットしてあるので、作り直すときだけ必要):
#     python3 -m pip install pillow
MAC_SRC="${TMP}/mac-1024.png"
python3 - "${MASTER}" "${MAC_SRC}" <<'PY'
import sys
try:
    from PIL import Image, ImageDraw
except ImportError:
    sys.exit("Pillow が必要です: python3 -m pip install pillow")

CANVAS, BODY, RADIUS, SS = 1024, 824, 185.4, 4   # SS: マスクを4倍で描いて縮小(なめらかに)
src, out = sys.argv[1], sys.argv[2]

art = Image.open(src).convert("RGBA").resize((BODY, BODY), Image.LANCZOS)
mask = Image.new("L", (BODY * SS, BODY * SS), 0)
ImageDraw.Draw(mask).rounded_rectangle(
    (0, 0, BODY * SS - 1, BODY * SS - 1), radius=RADIUS * SS, fill=255)
art.putalpha(mask.resize((BODY, BODY), Image.LANCZOS))

canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
canvas.paste(art, ((CANVAS - BODY) // 2, (CANVAS - BODY) // 2), art)
canvas.save(out)
print(f"macOS用: 本体{BODY}px/角丸{RADIUS} を {CANVAS}px の画布へ(周囲は透明)")
PY

SET="${TMP}/AppIcon.iconset"
mkdir -p "${SET}"
for spec in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" \
            "128 128x128" "256 128x128@2x" "256 256x256" "512 256x256@2x" \
            "512 512x512" "1024 512x512@2x"; do
    px=$(echo "${spec}" | cut -d' ' -f1)
    name=$(echo "${spec}" | cut -d' ' -f2)
    sips -s format png -z "${px}" "${px}" "${MAC_SRC}" --out "${SET}/icon_${name}.png" >/dev/null
done
iconutil -c icns "${SET}" -o "${HERE}/macos/AppIcon.icns"

# --- Windows: .ico ---
# こちらは master をそのまま使う(全面ベタ)。Windowsのアイコンは四角が普通で、
# macOSのような角丸+余白にすると小さく見えてしまう。
#
# ICOのディレクトリ構造を自前で書く。各エントリはPNGのまま入れる。
# 256px は幅/高さフィールドに 0 を入れる決まり(1バイトで表せないため)。
for px in 16 24 32 48 64 128 256; do
    sips -s format png -z "${px}" "${px}" "${MASTER}" --out "${TMP}/${px}.png" >/dev/null
done
python3 - "${TMP}" "${HERE}/windows/AppIcon.ico" <<'PY'
import struct, sys, pathlib
tmp, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
sizes = [16, 24, 32, 48, 64, 128, 256]
images = [(px, (tmp / f"{px}.png").read_bytes()) for px in sizes]
header = struct.pack("<HHH", 0, 1, len(images))          # reserved, type=icon, count
offset = len(header) + 16 * len(images)
entries, blobs = b"", b""
for px, data in images:
    entries += struct.pack("<BBBBHHII", px % 256, px % 256, 0, 0, 1, 32,
                           len(data), offset)
    blobs += data
    offset += len(data)
out.write_bytes(header + entries + blobs)
print(f"{out.name}: {len(sizes)} sizes, {len(header + entries + blobs)} bytes")
PY

# --- 実行時に窓/タスクバーへ設定する用 ---
# winit はウィンドウクラスを hIcon=0 で登録し、window_icon が None なら明示的に
# アイコンを外す。**exeに埋め込んだだけではタスクバーに出ない**ので、実行時に
# 設定するための小さめのPNGも用意する(1024pxを毎起動デコードするのは無駄)。
sips -s format png -z 256 256 "${MASTER}" --out "${HERE}/AppIcon-256.png" >/dev/null

echo "できました:"
echo "  ${HERE}/macos/AppIcon.icns"
echo "  ${HERE}/windows/AppIcon.ico"
echo "  ${HERE}/AppIcon-256.png"
