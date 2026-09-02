#!/usr/bin/env python3
"""HLK が吐いた .x から生のブートセクタを取り出し、XDF イメージに仕上げる。

X68000 の実行ファイル(HU形式)は $40 バイトのヘッダ + text + data +
再配置表 + シンボル表という構造。IPL は「論理セクタ0の 1024 バイトを
$002000 へ読んでそこから実行する」だけなので、ヘッダを剥がした text+data
をそのまま先頭に置く。

★**再配置表が空でなければエラーにする。** ラベル参照が絶対アドレスで
  残っていると、$002000 にロードした時点で壊れる(HLK の既定ベースは
  $002000 ではない)。PC相対で書けているかの機械的な検査になる。

XDF は 2HD の生セクタ列:
    77 トラック × 2 ヘッド × 8 セクタ × 1024 バイト = 1,261,568 バイト
"""
import struct
import sys

SECTOR = 1024
XDF_SIZE = 77 * 2 * 8 * SECTOR          # 1,261,568

def main():
    if len(sys.argv) != 3:
        sys.exit("usage: mkxdf.py <input.x> <output.xdf>")
    src, dst = sys.argv[1], sys.argv[2]
    x = open(src, "rb").read()

    if x[:2] != b"HU":
        sys.exit("%s: HU 形式ではない (先頭2バイト = %r)" % (src, x[:2]))
    # HU ヘッダ($40バイト)。ビッグエンディアン
    base, entry, tsize, dsize, bsize, rsize = struct.unpack_from(">6L", x, 0x04)
    print("  base=$%06X entry=$%06X text=%d data=%d bss=%d reloc=%d"
          % (base, entry, tsize, dsize, bsize, rsize))

    if rsize != 0:
        sys.exit("★再配置表が %d バイトある。絶対アドレスのラベル参照が残っている。\n"
                 "  ブートセクタは $002000 に生ロードされるので、全て PC 相対で\n"
                 "  書く必要がある(ハードウェアアドレスの即値は対象外)。" % rsize)
    if entry != base:
        sys.exit("★実行開始が先頭ではない (entry=$%06X base=$%06X)。\n"
                 "  IPL は読み込んだ先頭から実行するので一致していること。"
                 % (entry, base))

    code = x[0x40:0x40 + tsize + dsize]
    if len(code) > SECTOR:
        sys.exit("★コードが %d バイトで 1 セクタ(%d)を超える。\n"
                 "  IPL が読むのは 1 セクタだけなので、追加セクタを自分で\n"
                 "  読み込む処理が必要になる。" % (len(code), SECTOR))
    if code[0] != 0x60:
        sys.exit("★先頭バイトが $%02X で BRA ($60) ではない。IPL が実行しない。"
                 % code[0])

    img = bytearray(XDF_SIZE)
    img[:len(code)] = code
    open(dst, "wb").write(img)
    print("  %s: %d バイト (コード %d バイト / 1セクタ %d)"
          % (dst, len(img), len(code), SECTOR))
    print("  bss %d バイトは 0 クリアされない。使うなら自分で消すこと" % bsize)


if __name__ == "__main__":
    main()
