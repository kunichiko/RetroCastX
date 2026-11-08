#!/bin/sh
# bundle.sh が作った .app を、App Store Connect へ上げる .ipa にする。
#
#   ipa.sh <.app のパス> <出力する .ipa のパス>
#
# .ipa は「Payload/<名前>.app」を zip にしただけのもの。Xcode の
# -exportArchive がやることのうち、署名は bundle.sh で済ませてあるので、
# ここでは包むだけ。拡張属性やリソースフォークは入れない(AppleDouble の
# ._ ファイルや __MACOSX が混ざると、App Store の検証で弾かれる)。
set -eu

APP="${1:?.app のパス}"
IPA="${2:?出力する .ipa のパス}"
mkdir -p "$(dirname "${IPA}")"
IPA="$(cd "$(dirname "${IPA}")" && pwd)/$(basename "${IPA}")"

codesign --verify --strict "${APP}"

WORK=$(mktemp -d)
trap 'rm -rf "${WORK}"' EXIT
mkdir -p "${WORK}/Payload"
cp -R "${APP}" "${WORK}/Payload/"
rm -f "${IPA}"
(cd "${WORK}" && ditto -c -k --norsrc --noextattr --noqtn --keepParent Payload "${IPA}")
echo "できました: ${IPA}"
