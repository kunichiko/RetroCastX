#!/bin/sh
# Android 版をビルドして APK にする(arm64-v8a のみ)。
#
#   build-apk.sh <出力先ディレクトリ> [versionCode]
#
# 例:
#   cd client && packaging/android/build-apk.sh /tmp/out
#   adb install -r /tmp/out/RetroCastX_android-dev-0.12.0.apk
#   adb logcat -s RetroCastX            # eprintln! の出力(src/android_log.rs)
#
# Gradle は使わない。Java/Kotlin のコードが無い(OS 標準の NativeActivity が
# 共有ライブラリを直接読む)ので、APK は「マニフェスト + アイコン + .so」を
# SDK の aapt2 / zipalign / apksigner で束ねれば足りる。
#
# 要るもの(環境変数で上書きできる):
#   ANDROID_HOME   SDK(既定 ~/Library/Android/sdk)。ndk/ build-tools/ platforms/ の最新を使う
#   rustup target add aarch64-linux-android
#
# 署名は既定でデバッグ鍵(~/.android/debug.keystore)。配布用は
#   ANDROID_KEYSTORE / ANDROID_KEYSTORE_PASS / ANDROID_KEY_ALIAS
#   ANDROID_KEY_PASS(鍵のパスワード。省略時はキーストアと同じ)
# を渡す。
#
# アプリの ID と表示名は MimicX と同じ使い分け。**既定は開発版**で、本番版とは
# 別アプリとして並べて入れられる。CI のリリースビルドだけが APP_IDENTITY=prod を渡す。
#   開発版(既定)        jp.ohnaka.RetroCastX.dev  "RetroCast X Dev"
#   本番版(APP_IDENTITY=prod)  jp.ohnaka.RetroCastX      "RetroCast X"
set -eu

OUTDIR="${1:?出力先ディレクトリ}"
HERE=$(cd "$(dirname "$0")" && pwd)
CLIENT=$(cd "${HERE}/../.." && pwd)

case "${APP_IDENTITY:-dev}" in
    prod) PACKAGE="jp.ohnaka.RetroCastX"; APP_NAME="RetroCast X" ;;
    dev) PACKAGE="jp.ohnaka.RetroCastX.dev"; APP_NAME="RetroCast X Dev" ;;
    *) echo "APP_IDENTITY は prod か dev" >&2; exit 2 ;;
esac
# AAudio(cpal の Android 経路)が使える下限
MIN_SDK=26
TARGET_SDK=35

ANDROID_HOME="${ANDROID_HOME:-${HOME}/Library/Android/sdk}"
latest() { ls -1 "$1" | sort -V | tail -1; }
NDK="${ANDROID_NDK_HOME:-${ANDROID_HOME}/ndk/$(latest "${ANDROID_HOME}/ndk")}"
BT="${ANDROID_HOME}/build-tools/$(latest "${ANDROID_HOME}/build-tools")"
PLATFORM_JAR="${ANDROID_HOME}/platforms/android-${TARGET_SDK}/android.jar"
[ -f "${PLATFORM_JAR}" ] || { echo "platforms/android-${TARGET_SDK} が要る (sdkmanager \"platforms;android-${TARGET_SDK}\")" >&2; exit 1; }

case "$(uname -s)" in
    Darwin) HOST_TAG=darwin-x86_64 ;;   # NDK は Apple Silicon でもこの名前
    Linux) HOST_TAG=linux-x86_64 ;;
    *) echo "未対応のホスト: $(uname -s)" >&2; exit 1 ;;
esac
TC="${NDK}/toolchains/llvm/prebuilt/${HOST_TAG}/bin"

# バージョンの正は client/Cargo.toml(デスクトップ版と同じ)。
# versionCode は省略時に x.y.z → x*10000 + y*100 + z
VERSION=$(grep -m1 -E '^version[[:space:]]*=' "${CLIENT}/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')
VERSION_CODE="${2:-$(echo "${VERSION}" | awk -F. '{print $1*10000 + $2*100 + $3}')}"

# --- Rust ---
export CC_aarch64_linux_android="${TC}/aarch64-linux-android${MIN_SDK}-clang"
export AR_aarch64_linux_android="${TC}/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="${TC}/aarch64-linux-android${MIN_SDK}-clang"
(cd "${CLIENT}" && cargo build --release -p retrocastx-android --target aarch64-linux-android)
SO="${CLIENT}/target/aarch64-linux-android/release/libretrocastx.so"

# --- APK ---
mkdir -p "${OUTDIR}"
WORK="${OUTDIR}/apk-work"
rm -rf "${WORK}"
mkdir -p "${WORK}/res" "${WORK}/lib/arm64-v8a"

sed -e "s|@PACKAGE@|${PACKAGE}|g" \
    -e "s|@LABEL@|${APP_NAME}|g" \
    -e "s|@VERSION@|${VERSION}|g" \
    -e "s|@VERSION_CODE@|${VERSION_CODE}|g" \
    -e "s|@MIN_SDK@|${MIN_SDK}|g" \
    -e "s|@TARGET_SDK@|${TARGET_SDK}|g" \
    "${HERE}/AndroidManifest.xml.in" > "${WORK}/AndroidManifest.xml"

# ランチャーアイコン。Android も iOS と同じく全面ベタの master を縮める
# (丸や角丸の切り抜きはランチャー側がやる)。sips は macOS 標準、Linux なら ImageMagick
MASTER="${CLIENT}/packaging/AppIcon.png"
for spec in mdpi:48 hdpi:72 xhdpi:96 xxhdpi:144 xxxhdpi:192; do
    d="${WORK}/res/mipmap-${spec%%:*}"
    px="${spec##*:}"
    mkdir -p "${d}"
    if command -v sips >/dev/null; then
        sips -z "${px}" "${px}" "${MASTER}" --out "${d}/ic_launcher.png" >/dev/null
    else
        convert "${MASTER}" -resize "${px}x${px}" "${d}/ic_launcher.png"
    fi
done

"${BT}/aapt2" compile --dir "${WORK}/res" -o "${WORK}/res.zip"
"${BT}/aapt2" link -o "${WORK}/unaligned.apk" -I "${PLATFORM_JAR}" \
    --manifest "${WORK}/AndroidManifest.xml" "${WORK}/res.zip"

cp "${SO}" "${WORK}/lib/arm64-v8a/"
# .so を無圧縮で入れる。APK から直接 mmap できるので展開の手間とストレージが要らない
(cd "${WORK}" && zip -q -0 "unaligned.apk" "lib/arm64-v8a/libretrocastx.so")

# -P 16: .so を 16KB 境界に揃える(Android 15 以降の 16KB ページ端末で必須)
"${BT}/zipalign" -f -P 16 4 "${WORK}/unaligned.apk" "${WORK}/aligned.apk"

APK="${OUTDIR}/RetroCastX_android-${APP_IDENTITY:-dev}-${VERSION}.apk"
KS="${ANDROID_KEYSTORE:-${HOME}/.android/debug.keystore}"
"${BT}/apksigner" sign \
    --ks "${KS}" \
    --ks-pass "pass:${ANDROID_KEYSTORE_PASS:-android}" \
    --ks-key-alias "${ANDROID_KEY_ALIAS:-androiddebugkey}" \
    --key-pass "pass:${ANDROID_KEY_PASS:-${ANDROID_KEYSTORE_PASS:-android}}" \
    --out "${APK}" "${WORK}/aligned.apk"
rm -rf "${WORK}"

echo "できました: ${APK}"
echo "  ${PACKAGE}  version ${VERSION} (code ${VERSION_CODE})  arm64-v8a  署名: ${KS}"
