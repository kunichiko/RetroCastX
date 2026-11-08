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
#                       App Store へ出すときは "Apple Distribution: ..."
#   IOS_PROFILE         .mobileprovision のパス(バンドルIDと一致するもの)
#   IOS_ENTITLEMENTS    entitlements の plist(任意。省略時はプロファイルから作る)
# IOS_SIGN_IDENTITY が無ければ ad-hoc 署名にする(実機には入らない。CI の PR で
# 組み立てが壊れていないかを見るためのもの)。
#
# App Store へ出せる形にしてある(.ipa にするのは ipa.sh):
#   - アイコンは asset catalog(Assets.car)にする。App Store は 1024px のアイコンを
#     asset catalog に入れることを求める(ITMS-90704)。actool は Xcode に入っている
#   - Xcode が書く DT* のキー(SDK と Xcode の版)を入れる。無いと「対応していない
#     SDK / Xcode で作った」として弾かれる(ITMS-90534)
#   - プライバシーマニフェスト(PrivacyInfo.xcprivacy)を入れる
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

cp "${BIN}" "${APP}/${EXECUTABLE}"
chmod +x "${APP}/${EXECUTABLE}"

# アイコン。iOS は角丸のマスクを OS がかけるので、全面ベタの master(1024px、
# 透明なし)をそのまま使う(macOS 用の余白付きの絵だと、ホーム画面で一回り小さく
# 見える)。1枚の 1024px から actool が各サイズと Assets.car を作り、Info.plist に
# 入れるべきキー(CFBundleIcons)を partial.plist に書き出すので、それを足す
case "${KIND}" in
    simulator) SDK=iphonesimulator ;;
    device) SDK=iphoneos ;;
esac
WORK="${OUTDIR}/.bundle-work"
rm -rf "${WORK}"
mkdir -p "${WORK}/Assets.xcassets/AppIcon.appiconset"
echo '{"info":{"author":"xcode","version":1}}' > "${WORK}/Assets.xcassets/Contents.json"
cp "${HERE}/../AppIcon.png" "${WORK}/Assets.xcassets/AppIcon.appiconset/AppIcon.png"
cat > "${WORK}/Assets.xcassets/AppIcon.appiconset/Contents.json" <<'JSON'
{"images":[{"filename":"AppIcon.png","idiom":"universal","platform":"ios","size":"1024x1024"}],
 "info":{"author":"xcode","version":1}}
JSON
xcrun actool "${WORK}/Assets.xcassets" --compile "${APP}" \
    --platform "${SDK}" --minimum-deployment-target "${MIN_OS}" \
    --target-device iphone --target-device ipad --app-icon AppIcon \
    --output-partial-info-plist "${WORK}/partial.plist" \
    --output-format human-readable-text --errors >/dev/null
/usr/libexec/PlistBuddy -c "Merge ${WORK}/partial.plist" "${APP}/Info.plist" >/dev/null

# Xcode が書く SDK / Xcode の版。App Store はこれで「どの SDK で作ったか」を見る
SDK_VER=$(xcrun --sdk "${SDK}" --show-sdk-version)
SDK_BUILD=$(xcrun --sdk "${SDK}" --show-sdk-build-version)
XCODE_VER=$(xcodebuild -version | awk 'NR==1 {print $2}')
XCODE_BUILD=$(xcodebuild -version | awk 'NR==2 {print $3}')
# 26.1 → 2610、16.4.1 → 1641(Xcode の DTXcode と同じ書き方)
DT_XCODE=$(echo "${XCODE_VER}" | awk -F. '{printf "%02d%d%d", $1, $2, $3}')
for kv in "DTCompiler:com.apple.compilers.llvm.clang.1_0" \
          "DTPlatformName:${SDK}" "DTPlatformVersion:${SDK_VER}" "DTPlatformBuild:${SDK_BUILD}" \
          "DTSDKName:${SDK}${SDK_VER}" "DTSDKBuild:${SDK_BUILD}" \
          "DTXcode:${DT_XCODE}" "DTXcodeBuild:${XCODE_BUILD}" \
          "BuildMachineOSBuild:$(sw_vers -buildVersion)"; do
    /usr/libexec/PlistBuddy -c "Add :${kv%%:*} string ${kv#*:}" "${APP}/Info.plist"
done
# iOS の .app はバイナリ plist を期待する(XML でも読めるが、Xcode の出力に合わせる)
plutil -convert binary1 "${APP}/Info.plist"

# プライバシーマニフェスト。理由の申告が要る API を使っているため(中身の注記を参照)
cp "${HERE}/PrivacyInfo.xcprivacy" "${APP}/"
rm -rf "${WORK}"

if [ "${KIND}" = "simulator" ]; then
    # シミュレータは ad-hoc 署名でよい(Apple Silicon では無署名だと起動しない)
    codesign --force --sign - --timestamp=none "${APP}"
elif [ -z "${IOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --sign - --timestamp=none "${APP}"
    echo "注意: IOS_SIGN_IDENTITY が無いので ad-hoc 署名にしました(実機には入りません)"
else
    : "${IOS_PROFILE:?プロビジョニングプロファイル (IOS_PROFILE)}"
    cp "${IOS_PROFILE}" "${APP}/embedded.mobileprovision"
    ENT="${IOS_ENTITLEMENTS:-}"
    if [ -z "${ENT}" ]; then
        # プロファイルに書かれた entitlements をそのまま使う。マルチキャストの
        # 許可(com.apple.developer.networking.multicast)が下りていれば、ここに入る
        ENT="${OUTDIR}/entitlements.plist"
        security cms -D -i "${IOS_PROFILE}" > "${OUTDIR}/profile.plist"
        /usr/libexec/PlistBuddy -x -c "Print :Entitlements" "${OUTDIR}/profile.plist" > "${ENT}"
        # チームのワイルドカードのプロファイル(App ID が "TEAM.*")なら、署名に
        # 入れる ID は実際のバンドルIDにする。"*" のまま入れると実機が受け付けない
        APPID=$(/usr/libexec/PlistBuddy -c "Print :application-identifier" "${ENT}")
        case "${APPID}" in
            *'*') /usr/libexec/PlistBuddy -c "Set :application-identifier ${APPID%%.*}.${BUNDLE_ID}" "${ENT}" ;;
        esac
        # キーチェーンは使わない。プロファイルには "TEAM.*" のワイルドカードで
        # 入っていて、そのまま署名に入れると App Store が受け付けない
        /usr/libexec/PlistBuddy -c "Delete :keychain-access-groups" "${ENT}" 2>/dev/null || true
    fi
    codesign --force --sign "${IOS_SIGN_IDENTITY}" --entitlements "${ENT}" --timestamp=none "${APP}"
fi

echo "できました: ${APP}"
echo "  version ${VERSION} (build ${BUILD})  ${KIND}  $(lipo -archs "${APP}/${EXECUTABLE}" 2>/dev/null || echo '?')"
