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
SET="${TMP}/AppIcon.iconset"
mkdir -p "${SET}"
for spec in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" \
            "128 128x128" "256 128x128@2x" "256 256x256" "512 256x256@2x" \
            "512 512x512" "1024 512x512@2x"; do
    px=$(echo "${spec}" | cut -d' ' -f1)
    name=$(echo "${spec}" | cut -d' ' -f2)
    sips -s format png -z "${px}" "${px}" "${MASTER}" --out "${SET}/icon_${name}.png" >/dev/null
done
iconutil -c icns "${SET}" -o "${HERE}/macos/AppIcon.icns"

# --- Windows: .ico ---
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
