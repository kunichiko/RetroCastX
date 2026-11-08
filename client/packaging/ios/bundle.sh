#!/bin/sh
# 実行ファイル1つから iOS / iPadOS の .app を組み立てて署名する。
#
#   bundle.sh <実行ファイル> <version> <build番号> <出力先ディレクトリ> <simulator|device>
#
# 例(シミュレータで試すとき):
#   cd client && cargo build --release --target aarch64-apple-ios-sim
#   packaging/ios/bundle.sh target/aarch64-apple-ios-sim/release/retrocastx-viewer \
#       0.12.0 0 /tmp/out simulator
#   xcrun simctl install booted "/tmp/out/RetroCast X Dev.app"
#   xcrun simctl launch --console-pty booted jp.ohnaka.RetroCastX.dev
#
# アプリの ID と表示名は MimicX と同じ使い分け。**既定は開発版**で、本番版とは
# 別アプリとして並べて入れられる。CI のリリースビルドだけが APP_IDENTITY=prod を渡す。
#   開発版(既定)        jp.ohnaka.RetroCastX.dev  "RetroCast X Dev"
#   本番版(APP_IDENTITY=prod)  jp.ohnaka.RetroCastX      "RetroCast X"
#
# 実機(device)は署名に次の環境変数が要る:
#   IOS_SIGN_IDENTITY   "Apple Development: ..." など(security find-identity -p codesigning)
#   IOS_PROFILE         .mobileprovision のパス(バンドルIDと一致するもの)
#   IOS_ENTITLEMENTS    entitlements の plist(任意。省略時はプロファイルから作る)
#
# Xcode のプロジェクトは使わない。winit は iOS でも普通の実行ファイル(main)から
# UIApplicationMain を呼ぶので、Rust がそのまま作るバイナリを .app に入れれば動く。
# macOS 版の bundle.sh と同じく、**ロジックは CI の YAML ではなくここに置く。**
set -eu

BIN="${1:?実行ファイルのパス}"
VERSION="${2:?バージョン (例 0.1.0)}"
BUILD="${3:?ビルド番号}"
OUTDIR="${4:?出力先ディレクトリ}"
KIND="${5:?simulator か device}"

HERE=$(cd "$(dirname "$0")" && pwd)
case "${APP_IDENTITY:-dev}" in
    prod) BUNDLE_ID="jp.ohnaka.RetroCastX"; APP_NAME="RetroCast X" ;;
    dev) BUNDLE_ID="jp.ohnaka.RetroCastX.dev"; APP_NAME="RetroCast X Dev" ;;
    *) echo "APP_IDENTITY は prod か dev" >&2; exit 2 ;;
esac
EXECUTABLE="retrocastx-viewer"
# rustc の aarch64-apple-ios の既定の最低版より上げてある。wgpu の Metal 経路が
# 前提にする版に揃える
MIN_OS="16.0"

case "${KIND}" in
    simulator) PLATFORM_NAME="iPhoneSimulator" ;;
    device) PLATFORM_NAME="iPhoneOS" ;;
    *) echo "5番目の引数は simulator か device" >&2; exit 2 ;;
esac

APP="${OUTDIR}/${APP_NAME}.app"
rm -rf "${APP}"
mkdir -p "${APP}"

sed -e "s|@NAME@|${APP_NAME}|g" \
    -e "s|@BUNDLE_ID@|${BUNDLE_ID}|g" \
    -e "s|@EXECUTABLE@|${EXECUTABLE}|g" \
    -e "s|@VERSION@|${VERSION}|g" \
    -e "s|@BUILD@|${BUILD}|g" \
    -e "s|@PLATFORM_NAME@|${PLATFORM_NAME}|g" \
    -e "s|@MIN_OS@|${MIN_OS}|g" \
    "${HERE}/Info.plist.in" > "${APP}/Info.plist"
plutil -lint "${APP}/Info.plist" >/dev/null
# iOS の .app はバイナリ plist を期待する(XML でも読めるが、Xcode の出力に合わせる)
plutil -convert binary1 "${APP}/Info.plist"

cp "${BIN}" "${APP}/${EXECUTABLE}"
chmod +x "${APP}/${EXECUTABLE}"

# アイコン。iOS は角丸のマスクを OS がかけるので、全面ベタの master をそのまま縮める
# (macOS 用の余白付きの絵を使うと、ホーム画面で一回り小さく見える)。
# sips は macOS に標準で入っているので、CI に画像ツールを足さずに済む
MASTER="${HERE}/../AppIcon.png"
for spec in "AppIcon60x60@2x:120" "AppIcon60x60@3x:180" "AppIcon76x76@2x:152"; do
    name="${spec%%:*}"
    px="${spec##*:}"
    sips -z "${px}" "${px}" "${MASTER}" --out "${APP}/${name}.png" >/dev/null
done

if [ "${KIND}" = "simulator" ]; then
    # シミュレータは ad-hoc 署名でよい(Apple Silicon では無署名だと起動しない)
    codesign --force --sign - --timestamp=none "${APP}"
else
    : "${IOS_SIGN_IDENTITY:?実機用の署名ID (IOS_SIGN_IDENTITY)}"
    : "${IOS_PROFILE:?プロビジョニングプロファイル (IOS_PROFILE)}"
    cp "${IOS_PROFILE}" "${APP}/embedded.mobileprovision"
    ENT="${IOS_ENTITLEMENTS:-}"
    if [ -z "${ENT}" ]; then
        # プロファイルに書かれた entitlements をそのまま使う。マルチキャストの
        # 許可(com.apple.developer.networking.multicast)が下りていれば、ここに入る
        ENT="${OUTDIR}/entitlements.plist"
        security cms -D -i "${IOS_PROFILE}" > "${OUTDIR}/profile.plist"
        /usr/libexec/PlistBuddy -x -c "Print :Entitlements" "${OUTDIR}/profile.plist" > "${ENT}"
    fi
    codesign --force --sign "${IOS_SIGN_IDENTITY}" --entitlements "${ENT}" --timestamp=none "${APP}"
fi

echo "できました: ${APP}"
echo "  version ${VERSION} (build ${BUILD})  ${KIND}  $(lipo -archs "${APP}/${EXECUTABLE}" 2>/dev/null || echo '?')"
