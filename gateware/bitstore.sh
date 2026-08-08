#!/bin/sh
# ビットストリームを名前付きで保管し、再ビルドせずに焼き替えられるようにする。
# A/B比較(ノイズ量の測定など)で古い版に戻したいときに使う。
#
#   ./bitstore.sh save <名前> [メモ...]   直近のビルド結果を bitstreams/ に保存
#   ./bitstore.sh list                    保管済みを一覧
#   ./bitstore.sh flash <名前>            保管済みをSRAMへ書き込み
#   ./bitstore.sh flash-perm <名前>       保管済みをSPIフラッシュへ書き込み(永続)
#
# bitstreams/ は .gitignore 済み(バイナリはコミットしない)。
set -e
cd "$(dirname "$0")"
DIR=bitstreams
BIT=build/colorlight_i5/gateware/colorlight_i5.bit
export PATH="$HOME/opt/oss-cad-suite/bin:$PATH"

case "$1" in
save)
    [ -n "$2" ] || { echo "使い方: $0 save <名前> [メモ...]" >&2; exit 2; }
    [ -f "$BIT" ] || { echo "ビルド結果が見つかりません: $BIT" >&2; exit 1; }
    mkdir -p "$DIR"
    cp "$BIT" "$DIR/$2.bit"
    name="$2"; shift 2
    printf '%s\t%s\t%s\n' "$name" "$(git rev-parse --short HEAD)" "$*" \
        >> "$DIR/index.tsv"
    echo "保存: $DIR/$name.bit  (HEAD $(git rev-parse --short HEAD)) $*"
    ;;
list)
    [ -d "$DIR" ] || { echo "(保管なし)"; exit 0; }
    printf '%-24s %-10s %s\n' 名前 コミット メモ
    [ -f "$DIR/index.tsv" ] && awk -F'\t' \
        '{printf "%-24s %-10s %s\n", $1, $2, $3}' "$DIR/index.tsv"
    ;;
flash)
    [ -f "$DIR/$2.bit" ] || { echo "無い: $DIR/$2.bit" >&2; exit 1; }
    openFPGALoader -b colorlight-i5 "$DIR/$2.bit"
    ;;
flash-perm)
    [ -f "$DIR/$2.bit" ] || { echo "無い: $DIR/$2.bit" >&2; exit 1; }
    openFPGALoader -b colorlight-i5 -f --unprotect-flash "$DIR/$2.bit"
    ;;
*)
    sed -n '2,12p' "$0"
    exit 2
    ;;
esac
