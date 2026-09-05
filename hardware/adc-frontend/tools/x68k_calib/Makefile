# X68000 校正パターン FD イメージ
#
# ツールチェーンは X68000_MSX_Simulator と同じ流儀にしてある:
#   - ソースは UTF-8 で管理し、HAS060 が要求する CP932 に変換してから渡す
#   - アセンブラ/リンカは run68 で X68000 実行ファイルを動かす
#   - 環境依存のパスは ~/.human68k.mk か local.mk で上書きできる
#
# 使い方:
#   make            calib.xdf を作る
#   make run        XEiJ で起動して確認する
#   make clean

-include $(HOME)/.human68k.mk
-include local.mk

RUN68   ?= $(HOME)/work/X68000/run68mac/build/run68
HAS060  ?= $(HOME)/work/XEiJ/HFS/prog/HAS/HAS060.X
HLK     ?= $(HOME)/work/XEiJ/HFS/prog/HLK/hlk.x
XEIJ    ?= $(HOME)/work/xeij/current/XEiJ.jar

BUILD    = build
# HAS060 は SJIS しか読めない。UTF-8 のまま渡すとコメントの2バイト目が
# 命令に見えて意味不明なエラーになるので、変換したシャドウを作る
UTF8TOCP932 = python3 -c 'import sys;sys.stdout.buffer.write(open(sys.argv[1],encoding="utf-8").read().encode("cp932"))'

.PHONY: all run clean check

all: calib.xdf

$(BUILD):
	@mkdir -p $(BUILD)

$(BUILD)/calib.has: calib.has | $(BUILD)
	$(UTF8TOCP932) $< > $@

$(BUILD)/calib.o: $(BUILD)/calib.has
	$(RUN68) $(HAS060) -w0 $< -o $@

# HLK は .x を吐く。再配置表が空であることは mkxdf.py が検査する
$(BUILD)/calib.x: $(BUILD)/calib.o
	$(RUN68) $(HLK) $< -o $@

calib.xdf: $(BUILD)/calib.x mkxdf.py
	python3 mkxdf.py $< $@

# XEiJ で FD0 から起動する。引数は -keyword=value 形式
# (出典: https://stdkmd.net/xeij/parameter.htm)
run: calib.xdf
	java -jar $(XEIJ) -fd0=$(CURDIR)/calib.xdf -boot=fd0

check:
	@test -x $(RUN68)  || echo "run68 が無い: $(RUN68)"
	@test -f $(HAS060) || echo "HAS060.X が無い: $(HAS060)"
	@test -f $(HLK)    || echo "hlk.x が無い: $(HLK)"
	@test -f $(XEIJ)   || echo "XEiJ.jar が無い: $(XEIJ)"

clean:
	rm -rf $(BUILD) calib.xdf
