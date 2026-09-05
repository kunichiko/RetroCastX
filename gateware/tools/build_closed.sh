#!/bin/bash
#
# タイミングが閉じるまで配置シードを自動で振ってビルドする。
#
# なぜ要るか
# ----------
# eth_rx(125MHz)のクリティカルパスは LiteEth の CDC 非同期FIFO の中にあり、
#
#     グレイポインタFF → 残量比較LUT列 → 読ポインタ → RAM読出 → 出力FF
#
# が1サイクルに繋がっている。実測 8.13ns のうち **配線が 6.18ns(76%)** で、
# 論理をわずかに足しただけでも配置が変わって当たり外れが入れ替わる。
# 同じ論理でシードだけ変えると **122.6〜143.7 MHz と ±9% 振れる**(12シード中9通過)。
#
# 2026-09-03 に「根本的に1回で通す」手を3つ実測して、全部効かないことを確認済み
# (詳細は retrocastx_stream.py の RetroCastXUDPIPCore 呼び出し部のコメント):
#   目標周波数を上げる → 完全に無効 / BRAM化 → 差し引きゼロ / router2 → 大幅悪化
#
# ばらつき自体は消せないので、**手作業の方を消す**のがこのスクリプト。
#
# やること
# --------
#   1. 通常どおり1回ビルドする(yosys → nextpnr → ecppack)
#   2. タイミングが閉じていればそれで終わり
#   3. 閉じていなければ **yosys の結果を使い回して nextpnr だけ**シードを変えて回す
#      (yosys は数分かかるので、ここを繰り返さないのが肝)
#   4. 閉じたシードで ecppack し直し、採用シードと達成周波数を表示する
#
# nextpnr/ecppack のコマンドは LiteX が生成した build_*.sh から取り出すので、
# LiteX 側のオプションが変わっても追従する(ここに複製しない)。
#
# 使い方:
#   ./tools/build_closed.sh --eth-phy 1          # 以降の引数はそのまま渡る
#   SEEDS="3 1 4 6" ./tools/build_closed.sh      # 試す順番を変える
#
set -euo pipefail

cd "$(dirname "$0")/.."
GW=build/colorlight_i5/gateware

# 手元は専用 venv、CI は素の python。決め打ちにすると CI から使えないので
# 環境変数で差し替えられるようにしてある。
PYTHON="${PYTHON:-./.venv/bin/python}"

# 最初に試す順。過去に余裕が大きかったものを前に置いてある(2026-09-03 実測:
# seed 4=143.7 / 6=140.1 / 7=137.9 / 1=137.8 MHz)。あくまで初期値で、
# 論理が変われば当たり外れは変わるので「通るまで振る」ことに意味がある。
SEEDS="${SEEDS:-4 6 7 1 9 12 11 10 5 3 8 13 2}"

# タイミング判定: nextpnr が出す各クロックの**最後の**報告を見て FAIL が無いこと。
# (最初の報告は配線前の見積もりなので、必ず最後を採る)
timing_ok() {
    local log="$1"
    # クロック名ごとに最後の行だけ残して FAIL を数える
    local fails
    fails=$(grep "Max frequency for clock" "$log" \
            | sed -E "s/.*for clock +'([^']+)'.*/\1\t&/" \
            | awk -F'\t' '{last[$1]=$2} END {for (c in last) print last[c]}' \
            | grep -c "FAIL" || true)
    [ "$fails" -eq 0 ]
}

report() {
    local log="$1"
    grep "Max frequency for clock" "$log" \
        | sed -E "s/.*for clock +'([^']+)'.*/\1\t&/" \
        | awk -F'\t' '{last[$1]=$2} END {for (c in last) print last[c]}' \
        | sed -E "s/^[A-Za-z]+: +//; s/\\\$glbnet\\\$//; s/\\\$TRELLIS_IO_(IN|OUT)//" \
        | sed 's/^/  /' | sort
}

set -- "$@"
FIRST_SEED="${SEEDS%% *}"
REST="${SEEDS#* }"

echo "=== 1回目のビルド (seed $FIRST_SEED) ==="
"$PYTHON" retrocastx_stream.py --build --seed "$FIRST_SEED" "$@" \
    > /tmp/build_closed.log 2>&1 || {
        echo "ビルドが失敗しました。/tmp/build_closed.log を見てください" >&2
        tail -20 /tmp/build_closed.log >&2
        exit 1
    }

if timing_ok /tmp/build_closed.log; then
    echo "タイミング成立 (seed $FIRST_SEED)"
    report /tmp/build_closed.log
    exit 0
fi

echo "seed $FIRST_SEED では閉じませんでした。nextpnr だけ振り直します"
report /tmp/build_closed.log

# LiteX が生成したコマンドを取り出す(オプションを複製しないため)
NEXTPNR_CMD=$(grep '^nextpnr-ecp5' "$GW/build_colorlight_i5.sh")
ECPPACK_CMD=$(grep '^ecppack'      "$GW/build_colorlight_i5.sh")
[ -n "$NEXTPNR_CMD" ] && [ -n "$ECPPACK_CMD" ] || {
    echo "build_colorlight_i5.sh から nextpnr/ecppack の行を取れませんでした" >&2
    exit 1
}

for s in $REST; do
    echo "--- seed $s ---"
    # 既存の --seed を差し替える
    cmd=$(printf '%s' "$NEXTPNR_CMD" | sed -E "s/--seed [0-9]+/--seed $s/")
    ( cd "$GW" && eval "$cmd" ) > /tmp/build_closed_seed$s.log 2>&1 || true
    report /tmp/build_closed_seed$s.log
    if timing_ok /tmp/build_closed_seed$s.log; then
        echo "=== タイミング成立: seed $s。ecppack します ==="
        ( cd "$GW" && eval "$ECPPACK_CMD" )
        echo
        echo "採用シード: $s"
        echo "次回から --seed $s を既定にしておくと1回で通る見込みが高いです"
        echo "(ただし論理を足すと当たり外れは変わります)"
        exit 0
    fi
done

echo "どのシードでもタイミングが閉じませんでした: $SEEDS" >&2
echo "論理を減らすか、SEEDS 環境変数で別の値を試してください" >&2
exit 1
