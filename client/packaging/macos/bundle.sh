#!/bin/sh
# 実行ファイル1つから macOS の .app を組み立てる。
#
#   bundle.sh <実行ファイル> <version> <build番号> <出力先ディレクトリ>
#
# 例(手元で試すとき):
#   cd client && cargo build --release
#   packaging/macos/bundle.sh target/release/retrocastx-viewer 0.1.0 0 /tmp/out
#   open "/tmp/out/RetroCast X.app"
#
# CI(.github/workflows/viewer-release.yml)からも同じスクリプトを呼ぶ。
# **YAMLの中にロジックを書かない**こと: 署名で詰まったときに手元で同じものを
# 組み立てて試せる形にしておくため。署名/公証はこのスクリプトの外(呼び出し側)。
#
# Viewer は SVG も含めて実行ファイルに埋め込んである(bezel.rs の include_str!)ので、
# 同梱するリソースは無い。アイコンだけは任意で AppIcon.icns があれば入れる。
set -eu

BIN="${1:?実行ファイルのパス}"
VERSION="${2:?バージョン (例 0.1.0)}"
BUILD="${3:?ビルド番号 (例 GITHUB_RUN_NUMBER)}"
OUTDIR="${4:?出力先ディレクトリ}"

HERE=$(cd "$(dirname "$0")" && pwd)
APP_NAME="RetroCast X"
BUNDLE_ID="jp.ohnaka.RetroCastX"
EXECUTABLE="retrocastx-viewer"

APP="${OUTDIR}/${APP_NAME}.app"
rm -rf "${APP}"
mkdir -p "${APP}/Contents/MacOS" "${APP}/Contents/Resources"

sed -e "s|@NAME@|${APP_NAME}|g" \
    -e "s|@BUNDLE_ID@|${BUNDLE_ID}|g" \
    -e "s|@EXECUTABLE@|${EXECUTABLE}|g" \
    -e "s|@VERSION@|${VERSION}|g" \
    -e "s|@BUILD@|${BUILD}|g" \
    "${HERE}/Info.plist.in" > "${APP}/Contents/Info.plist"

# 古い Finder / LaunchServices 向け。無くても動くが慣例として入れる
printf 'APPL????' > "${APP}/Contents/PkgInfo"

cp "${BIN}" "${APP}/Contents/MacOS/${EXECUTABLE}"
chmod +x "${APP}/Contents/MacOS/${EXECUTABLE}"

# アイコンは任意。無ければ汎用アイコンになるだけで動作に影響しない
if [ -f "${HERE}/AppIcon.icns" ]; then
    cp "${HERE}/AppIcon.icns" "${APP}/Contents/Resources/AppIcon.icns"
else
    echo "note: ${HERE}/AppIcon.icns が無いので汎用アイコンになります"
fi

plutil -lint "${APP}/Contents/Info.plist" >/dev/null
echo "できました: ${APP}"
echo "  version ${VERSION} (build ${BUILD})  $(lipo -archs "${APP}/Contents/MacOS/${EXECUTABLE}" 2>/dev/null || echo '?')"
