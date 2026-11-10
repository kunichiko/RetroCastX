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
#   1. LiteX で Verilog とビルド用スクリプトを作り、yosys を1回だけ回す
#      (yosys は数分かかるので、ここを繰り返さないのが肝)
#   2. nextpnr をシードを変えて回し、タイミングが閉じた最初のシードで止める
#   3. そのシードで ecppack する
#
# nextpnr/ecppack のコマンドは LiteX が生成した build_*.sh から取り出すので、
# LiteX 側のオプションが変わっても追従する(ここに複製しない)。
#
# 使い方:
#   ./tools/build_closed.sh --no-mclk-out        # 以降の引数はそのまま渡る
#   SEEDS="3 1 4 6" ./tools/build_closed.sh      # 試す順番を変える
#   SEED_TIMEOUT=600 ./tools/build_closed.sh     # 1シードの上限(秒)
#
set -euo pipefail

cd "$(dirname "$0")/.."
GW=build/colorlight_i5/gateware

# ★**ログはこの checkout の build フォルダに置く。** 以前は /tmp/build_closed.log と
#   決まった名前だったので、別の worktree で同時にビルドすると互いのログを
#   上書きし、タイミングの判定が相手のログを読んだ(2026-11-10: 相手の「配置
#   しきれない」エラーを自分のものと取り違え、収束していない配線と誤読した)
mkdir -p "$GW"
LOG="$PWD/$GW/build_closed.log"
seed_log() { echo "$PWD/$GW/build_closed_seed$1.log"; }

# 手元は専用 venv、CI は素の python。決め打ちにすると CI から使えないので
# 環境変数で差し替えられるようにしてある。
PYTHON="${PYTHON:-./.venv/bin/python}"

# ★**前回のリリースで閉じたシードを最初に試す。** tools/closed_seed は
#   gateware-release.yml がリリースのたびに書いて main にコミットする。論理の
#   変更が小さければ、同じシードがまた閉じる見込みが高い。
#   以前は固定の順で振っていて、gw-v1.5.0 のタグビルドは先頭の seed 4 の配線が
#   収束せず1時間止まった。
#   ★**手元で閉じたシードは CI では役に立たない。** 配置は「ネットリスト・nextpnr
#   の版・シード」に加えて**OS/CPU でも変わる**。oss-cad-suite を CI と同じ
#   2026-09-03 に揃えた macOS arm64 で seed 6 が eth_rx 132.89MHz で閉じたのに、
#   CI(Linux x64)の seed 6 は 121.60MHz で落ちた(2026-11-10)。だからこの
#   スクリプトはファイルを読むだけで、書くのは CI に任せている。
SEED_FILE=tools/closed_seed
HINT=""
if [ -f "$SEED_FILE" ]; then
    HINT=$(grep -v '^#' "$SEED_FILE" | awk 'NF {print $1; exit}')
fi

# 既定の順。過去に余裕が大きかったものを前に置いてある(2026-09-03 実測:
# seed 4=143.7 / 6=140.1 / 7=137.9 / 1=137.8 MHz)。論理が変われば当たり外れは
# 変わるので、あくまで「通るまで振る」ための候補。
SEEDS="${SEEDS:-$(printf '%s ' $HINT; for s in 4 6 7 1 9 12 11 10 5 3 8 13 2; do
    [ "$s" = "$HINT" ] || printf '%s ' "$s"; done)}"

# ★**1シードあたりの上限(秒)。** nextpnr の配線は、外れのシードだと収束せずに
#   延々と回り続けることがある(gw-v1.5.0 の seed 4 は CI で40分を超えた)。
#   普段は手元で1〜2分、CI でも yosys 込みで4〜8分なので、それを十分超えたら
#   次へ進む。macOS には timeout(1) が無いので自前で見張る。
SEED_TIMEOUT="${SEED_TIMEOUT:-900}"

# タイミング判定: nextpnr が出す各クロックの**最後の**報告を見て FAIL が無いこと。
# (最初の報告は配線前の見積もりなので、必ず最後を採る)
timing_ok() {
    local log="$1"
    # 配線を終えていなければ、最後の報告も配置後の見積もりなので採らない
    grep -q "Info: Program finished normally" "$log" || return 1
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

# 上限つきでコマンドを走らせる。上限を超えたら止めて 124 を返す(timeout(1) と同じ)
run_limited() {
    local limit="$1" out="$2" cmd="$3"
    # exec で置き換えるので、$! が nextpnr 自身になり kill が確実に届く
    ( cd "$GW" && eval "exec $cmd" ) > "$out" 2>&1 &
    local pid=$! t=0
    while kill -0 "$pid" 2>/dev/null; do
        if [ "$t" -ge "$limit" ]; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            return 124
        fi
        sleep 5
        t=$((t + 5))
    done
    wait "$pid"
}

echo "=== 生成と論理合成 ==="
# --build を付けないと LiteX は Verilog と build_*.sh を書くだけで止まる
"$PYTHON" retrocastx_stream.py "$@" > "$LOG" 2>&1 || {
    echo "生成が失敗しました。$LOG を見てください" >&2
    tail -20 "$LOG" >&2
    exit 1
}

# LiteX が生成したコマンドを取り出す(オプションを複製しないため)
YOSYS_CMD=$(grep '^yosys'          "$GW/build_colorlight_i5.sh" || true)
NEXTPNR_CMD=$(grep '^nextpnr-ecp5' "$GW/build_colorlight_i5.sh" || true)
ECPPACK_CMD=$(grep '^ecppack'      "$GW/build_colorlight_i5.sh" || true)
[ -n "$YOSYS_CMD" ] && [ -n "$NEXTPNR_CMD" ] && [ -n "$ECPPACK_CMD" ] || {
    echo "build_colorlight_i5.sh から yosys/nextpnr/ecppack の行を取れませんでした" >&2
    exit 1
}

( cd "$GW" && eval "$YOSYS_CMD" ) >> "$LOG" 2>&1 || {
    echo "論理合成が失敗しました。$LOG を見てください" >&2
    tail -20 "$LOG" >&2
    exit 1
}

echo "試す順: $SEEDS(1シードの上限 ${SEED_TIMEOUT} 秒)"
for s in $SEEDS; do
    echo "--- seed ${s} ---"
    # 既存の --seed を差し替える
    cmd=$(printf '%s' "$NEXTPNR_CMD" | sed -E "s/--seed [0-9]+/--seed $s/")
    slog=$(seed_log "$s")
    rc=0
    run_limited "$SEED_TIMEOUT" "$slog" "$cmd" || rc=$?
    if [ "$rc" -eq 124 ]; then
        echo "  ${SEED_TIMEOUT} 秒で配線が終わらないので次へ"
        continue
    fi
    report "$slog"
    if timing_ok "$slog"; then
        # ★**波括弧が要る。** macOS の bash は 3.2 で、`$s` の直後に全角文字が
        #   来ると1バイト目まで変数名として読み、`s\xe3: unbound variable` で
        #   落ちる(set -u のため)。CI の bash 5 では通るので気付きにくい:
        #   実際、シードを見つけたのに ecppack を実行できずに終わった。
        echo "=== タイミング成立: seed ${s}。ecppack します ==="
        ( cd "$GW" && eval "$ECPPACK_CMD" )
        # 生成された build_*.sh にも採用シードを反映しておく(手で再実行したとき用)
        sed -i.bak -E "s/--seed [0-9]+/--seed $s/" "$GW/build_colorlight_i5.sh"
        rm -f "$GW/build_colorlight_i5.sh.bak"
        cp "$slog" "$LOG.nextpnr"
        echo "採用シード: ${s}"
        exit 0
    fi
done

echo "どのシードでもタイミングが閉じませんでした: $SEEDS" >&2
echo "論理を減らすか、SEEDS 環境変数で別の値を試してください" >&2
exit 1
