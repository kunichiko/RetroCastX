#!/usr/bin/env python3
"""TVP7002 ピクセルキャプチャ front-end(DATACLKドメイン → sysドメイン CDC)。

- cd_pix(=DATACLK) で RGB[9:2]=RGB888 をサンプルし RGB555 に変換
- HSOUT/VSOUT のアクティブエッジから 行内x / フレーム内row / frame / field を復元
- N面リングのラインバッファ(32bit=2px/entry, width/2 深さ×N面)に1ラインを書く
- ライン完成ごとにメタデータ {face, row, frame, field} を AsyncFIFO で sys へ渡す
  (pix→sys CDC)。sysのストリーマは pop して該当面を読み LINE パケット化する。
  FIFOが満杯(sysが間に合わない)なら、そのラインは書かずに捨てる=ライン単位ドロップ
  (画面の途中破壊=テアリングは起きない。面は TX 完了まで上書きされない)。

TVP設定(retrocastx_i2c 側): 0x18[0] CLK POL=1 にすると データは DATACLK 立下りで
launch されるので、pixドメイン(=DATACLK 立上り)で安定サンプルできる。

RGB555 レイアウト: 0RRRRRGGGGGBBBBB(host/python と一致)。
2px/entry: 低位16bit=偶数x(pix0), 高位16bit=奇数x(pix1)。
"""
from migen import *
from migen.genlib.cdc import PulseSynchronizer, MultiReg
from litex.soc.interconnect import stream


def rgb888_to_555(r, g, b):
    """8bit×3 → 16bit 0RRRRRGGGGGBBBBB(上位ビットを採用)。"""
    return Cat(b[3:8], g[3:8], r[3:8], C(0, 1))


# メタデータ(pix→sys AsyncFIFO のペイロード)
def _meta_layout(nface_bits, row_bits):
    return [("face", nface_bits), ("row", row_bits),
            ("frame", 16), ("field", 1)]


class TvpCapture(Module):
    """pads: r(8) g(8) b(8) hs vs [fid] を持つ Record。dataclk は呼び出し側で
    cd_pix に接続済みであること。width は偶数(2px/entry)。"""
    def __init__(self, pads, width=1024, height=512, nface=8, fifo_depth=4,
                 hs_active_low=True, vs_active_low=True,
                 hs_offset=0, vs_offset=0):
        assert width % 2 == 0 and (width & (width - 1)) == 0, "width は2のべき乗"
        assert nface >= 2 and (nface & (nface - 1)) == 0
        # 面数 > 未処理メタ数 にして、送信中(head)の面が書込ポインタに追いつかれ
        # 上書きされる事を防ぐ(CDCのパイプライン段も見込んで +2 マージン)
        assert nface >= fifo_depth + 2, "nface は fifo_depth+2 以上"
        self.width = width
        self.height = height
        nface_bits = log2_int(nface)
        row_bits = max(1, log2_int(height))
        entries = width // 2                      # 面あたり32bitワード数
        entry_bits = log2_int(entries)            # entry(=xpix/2)のアドレスbit幅

        # --- sysドメイン: ストリーマ向けI/F ---
        self.line_valid = Signal()                # 送出可能ラインが有る
        self.line_row   = Signal(row_bits)        # 先頭ラインのフレーム内行
        self.line_frame = Signal(16)
        self.line_field = Signal()
        self.line_face  = Signal(nface_bits)      # FIFO先頭ラインの面(pop前にラッチ)
        self.line_ack   = Signal()                # 1パルスで pop(送出開始時)
        self.rd_face    = Signal(nface_bits)      # 読み出す面(送信側がラッチして固定)
        self.rd_word    = Signal(max=max(entries, 2))   # 面内ワード位置(=x/2)
        self.rd_data    = Signal(32)              # {pix1, pix0}
        # 診断用(sysで観測)
        self.cap_frame  = Signal(16)
        self.cap_drops  = Signal(16)

        # --- ラインバッファ(true dual-port: write=pix / read=sys)---
        self.specials.mem = mem = Memory(32, nface * entries)
        wr = mem.get_port(write_capable=True, clock_domain="pix")
        rd = mem.get_port(clock_domain="sys")
        self.specials += wr, rd

        # --- メタデータ CDC(pix → sys)。sink=pix / source=sys ---
        layout = _meta_layout(nface_bits, row_bits)
        self.submodules.meta = meta = stream.ClockDomainCrossing(
            layout, cd_from="pix", cd_to="sys", depth=max(fifo_depth, 4))

        # ================= pix ドメイン =================
        r, g, b = pads.r, pads.g, pads.b
        # 入力2段FF(HSOUT/VSOUTはDATACLK同期だが念のため)
        hs0 = Signal(); vs0 = Signal(); hs = Signal(); vs = Signal()
        self.sync.pix += [hs0.eq(pads.hs), vs0.eq(pads.vs),
                          hs.eq(hs0), vs.eq(vs0)]
        hs_p = Signal(); vs_p = Signal()
        self.sync.pix += [hs_p.eq(hs), vs_p.eq(vs)]
        hs_edge = (hs_p & ~hs) if hs_active_low else (~hs_p & hs)  # アクティブ開始
        vs_edge = (vs_p & ~vs) if vs_active_low else (~vs_p & vs)

        x     = Signal(16)                        # 行内DATACLKカウンタ(hs後,飽和)
        face  = Signal(nface_bits)                # 書き込み中の面
        row   = Signal(12)                        # vsync以降の行番号(blanking含む,12bit)
        frame = Signal(16)
        field = Signal()
        wrote = Signal()                          # 現ラインに1画素以上書いたか
        pair_lo = Signal(16)                      # 偶数xピクセル保持

        pix555 = Signal(16)
        self.comb += pix555.eq(rgb888_to_555(r, g, b))

        # 有効行 row_eff = row - vs_offset(アクティブ行の0起点index)
        row_eff = Signal(12)
        row_ok  = Signal()
        self.comb += [
            row_eff.eq(row - vs_offset),
            row_ok.eq((row >= vs_offset) & (row_eff < height)),
        ]
        xpix   = Signal(16)                       # 有効ピクセルx(= x - hs_offset)
        active = Signal()
        self.comb += [
            xpix.eq(x - hs_offset),
            active.eq((x >= hs_offset) & (xpix < width) & row_ok),
            # entry = xpix/2(entry_bits幅に切出), 面=face。xpix[1:]は15bit幅なので
            # 明示スライスしないと face がアドレス上位へ押し出される
            wr.adr.eq(Cat(xpix[1:1 + entry_bits], face)),
            wr.dat_w.eq(Cat(pair_lo, pix555)),    # {奇数px, 偶数px}
            wr.we.eq(active & xpix[0]),           # 奇数pxが揃った時に1ペア書込
        ]

        # メタ push(hs_edge時、直前ラインが有効(row_ok)かつ1画素以上書いた場合)。
        # FIFO満杯なら drop。
        push = Signal()
        self.comb += [
            push.eq(hs_edge & wrote & row_ok),
            meta.sink.face.eq(face),
            meta.sink.row.eq(row_eff[:row_bits]),
            meta.sink.frame.eq(frame),
            meta.sink.field.eq(field),
            meta.sink.valid.eq(push),
        ]
        drops = Signal(16)
        self.sync.pix += [
            If(active & ~xpix[0], pair_lo.eq(pix555)),  # 偶数x: 低位に保持
            If(wr.we, wrote.eq(1)),
            If(x != 0xFFFF, x.eq(x + 1)),               # 行内カウンタ(飽和)
            If(hs_edge,
                x.eq(0),
                wrote.eq(0),
                If(push & meta.sink.ready, face.eq(face + 1)),  # 受理時のみ面前進
                If(push & ~meta.sink.ready, drops.eq(drops + 1)),
                If(row != 0xFFF, row.eq(row + 1)),
            ),
            If(vs_edge,
                row.eq(0),
                wrote.eq(0),
                frame.eq(frame + 1),
                field.eq(~field),
            ),
        ]

        # ================= sys ドメイン =================
        self.comb += [
            self.line_valid.eq(meta.source.valid),
            self.line_face.eq(meta.source.face),
            self.line_row.eq(meta.source.row),
            self.line_frame.eq(meta.source.frame),
            self.line_field.eq(meta.source.field),
            meta.source.ready.eq(self.line_ack),
            rd.adr.eq(Cat(self.rd_word, self.rd_face)),  # 送信側ラッチ面を読む
            self.rd_data.eq(rd.dat_r),
        ]
        # 診断CDC
        self.specials += MultiReg(frame, self.cap_frame, "sys")
        self.specials += MultiReg(drops, self.cap_drops, "sys")
