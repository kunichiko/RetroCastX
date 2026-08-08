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


# メタデータ(pix→sys AsyncFIFO のペイロード)。ts はそのラインの先頭画素における
# DATACLK自走カウンタ値(プロトコルのタイムスタンプ=ドットクロック単位)。
def _meta_layout(nface_bits, row_bits):
    return [("face", nface_bits), ("row", row_bits),
            ("frame", 16), ("field", 1), ("ts", 32)]


class TvpCapture(Module):
    """pads: r(8) g(8) b(8) hs vs [fid] を持つ Record。dataclk は呼び出し側で
    cd_pix に接続済みであること。width は偶数(2px/entry)。"""
    def __init__(self, pads, width=1024, height=512, nface=8, fifo_depth=4,
                 hs_active_low=True, vs_active_low=True,
                 hs_offset=0, vs_offset=0, vs_min_rows=0, vtotal=0,
                 vs_row_at_sync=0, hs_total=0, sys_clk_freq=45e6, measure=True):
        # measure: 実測タイミング(周波数カウンタ)を作るか。34.8MHzで回る32bitカウンタが
        #   増えるため、ノイズ切り分け用に無効化できるようにしてある。無効時はMODEの
        #   諸元が0になる。
        # sys_clk_freq: 実測タイミングの1秒窓を作るのに使う(sysは25MHz水晶由来で正確)。
        # hs_total: 1ライン当たりのDATACLK数(=H-PLL分周比)。hs_offset+width がこれを
        #   超えると、行末側の列には一度も書き込まれずリングバッファの古い内容が
        #   残って見える。その範囲を毎ライン先頭(バックポーチ期間)に黒で埋める。
        # vs_row_at_sync (vtotal時のみ): VSYNC検出時に row へ入れる値。既定0=
        #   「キャプチャ窓の先頭=VSYNC」。N を指定すると窓はVSYNCの N 行手前から
        #   始まる(=映像がN行下がって見える)。フレーム境界(frameの歩進)は常に
        #   row のラップ点=窓の先頭なので、1枚の絵が2つのframe番号に割れない。
        # vs_min_rows: VSYNCを「フレーム開始」として受理する最小行数。0=ガード無効。
        #   row(=vsync以降に数えたHSYNC数)が vs_min_rows 未満のVSYNCエッジは無視する。
        #   VSOUTの等化/セレーションやノイズでフレーム途中に出る偽VSYNCで frame が
        #   多重に進む(受信側で内容が縦にロール)のを防ぐ。DATACLK周波数に非依存。
        # vtotal: >0 なら「HSYNC を vtotal 行数えたら1フレーム」の自走カウンタで
        #   フレーム境界を決める(VSOUTにノイズが乗っても崩れない)。TVP実測の
        #   Lines/Frame(例 568)を渡す。さらに vs_min_rows を併用すると、フレーム
        #   末尾付近のVSYNCエッジだけを受理して row=0 に再整列するため、垂直位置が
        #   本物のVSYNCへ自動で合う(自走のみだと開始位相が電源投入時のまま任意)。
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
        self.line_ts    = Signal(32)              # 先頭ラインの先頭画素のDATACLKカウンタ
        self.line_ack   = Signal()                # 1パルスで pop(送出開始時)
        self.rd_face    = Signal(nface_bits)      # 読み出す面(送信側がラッチして固定)
        self.rd_word    = Signal(max=max(entries, 2))   # 面内ワード位置(=x/2)
        self.rd_data    = Signal(32)              # {pix1, pix0}
        # 診断用(sysで観測)
        self.cap_frame  = Signal(16)
        self.cap_drops  = Signal(16)
        # --- モードパラメータ(実行時に変更可。モード自動判定が書き換える)---
        # 既定値はコンストラクタ引数。cfg_* を駆動しなければその値のまま動く。
        # pixドメインで使うので、sysから変えるときは値が落ち着いてから使うこと
        # (モード切替時の1〜2フレームが乱れるだけなので、CDCは掛けていない)。
        self.cfg_vtotal        = Signal(13, reset=vtotal or 0)
        self.cfg_vs_min_rows   = Signal(13, reset=vs_min_rows)
        self.cfg_vs_row_at_sync = Signal(13, reset=vs_row_at_sync)
        self.cfg_hs_offset     = Signal(13, reset=hs_offset)
        self.cfg_vs_offset     = Signal(13, reset=vs_offset)
        # 末尾クリアの開始entry。= (hs_total - hs_offset)/2。0ならクリアしない。
        _fu = ((hs_total - hs_offset) // 2
               if (hs_total and hs_offset + width > hs_total) else 0)
        self.cfg_clear_from    = Signal(max=entries + 1, reset=_fu)

        # --- 実測タイミング(sysドメイン)。MODEパケットで報告する ---
        # ビルド時の仮定値ではなく実信号から測る。1秒窓のカウントなので単位はHz。
        self.meas_dotclk = Signal(32)             # DATACLK周波数 [Hz]
        self.meas_hfreq  = Signal(32)             # 水平周波数 [Hz]
        self.meas_vfreq  = Signal(32)             # 垂直周波数 [Hz]
        self.meas_htotal = Signal(16)             # 1ライン当たりDATACLK数
        self.meas_vtotal = Signal(16)             # 1フレーム当たりライン数

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
        # RGB入力は必ず pix ドメインで1段レジスタに受ける。
        # 以前はピンから組合せ論理(ビット切り出し)を経てBRAMの書き込みデータ端子へ
        # 直結しており、ピン→BRAMの経路遅延が配置配線任せだった。DATACLKの分配遅延と
        # データ経路遅延の差が実効サンプリング点を決めるので、ビルドごとに配置が変わると
        # サンプリング点が動き、取りこぼし(下位ビットの化け=点状ノイズ)を起こし得る。
        # レジスタで受ければピン→FFが短く固定され、以降はnextpnrが時間検証できる
        # 内部経路になる。データはDATACLK同期なので1段でよい(非同期信号ではない)。
        r = Signal(8); g = Signal(8); b = Signal(8)
        self.sync.pix += [r.eq(pads.r), g.eq(pads.g), b.eq(pads.b)]
        # 入力2段FF(HSOUT/VSOUTはDATACLK同期だが念のため)
        hs0 = Signal(); vs0 = Signal(); hs = Signal(); vs = Signal()
        self.sync.pix += [hs0.eq(pads.hs), vs0.eq(pads.vs),
                          hs.eq(hs0), vs.eq(vs0)]
        hs_p = Signal(); vs_p = Signal()
        self.sync.pix += [hs_p.eq(hs), vs_p.eq(vs)]
        hs_edge = (hs_p & ~hs) if hs_active_low else (~hs_p & hs)  # アクティブ開始
        vs_edge_raw = (vs_p & ~vs) if vs_active_low else (~vs_p & vs)
        # vs_edge は row 定義後に行数ガードを掛ける(下記)
        vs_edge = Signal()

        x     = Signal(16)                        # 行内DATACLKカウンタ(hs後,飽和)
        face  = Signal(nface_bits)                # 書き込み中の面
        row   = Signal(13)                        # vsync以降の行番号(blanking含む)
        frame = Signal(16)
        field = Signal()
        wrote = Signal()                          # 現ラインに1画素以上書いたか
        pair_lo = Signal(16)                      # 偶数xピクセル保持

        # VSYNCエッジ行数ガード: 前回受理VSYNCからの経過行数(vrow)が vs_min_rows 以上の
        # エッジのみ受理する。row ではなく vrow を使うのは、vs_row_at_sync で表示位相を
        # ずらしても(=VSYNC時のrowが0でなくても)ガードが正しく効くようにするため。
        vrow = Signal(13)
        # cfg_vs_min_rows==0 ならガード無効(全VSYNCを受理)
        self.comb += vs_edge.eq(vs_edge_raw &
                                ((self.cfg_vs_min_rows == 0) |
                                 (vrow >= self.cfg_vs_min_rows)))
        self.sync.pix += [
            If(hs_edge, If(vrow != 0x1FFF, vrow.eq(vrow + 1))),
            If(vs_edge, vrow.eq(0)),
        ]

        pix555 = Signal(16)
        self.comb += pix555.eq(rgb888_to_555(r, g, b))

        # 有効行 row_eff = row - vs_offset(アクティブ行の0起点index)
        row_eff = Signal(13)
        row_ok  = Signal()
        self.comb += [
            row_eff.eq(row - self.cfg_vs_offset),
            row_ok.eq((row >= self.cfg_vs_offset) & (row_eff < height)),
        ]
        xpix   = Signal(16)                       # 有効ピクセルx(= x - hs_offset)
        active = Signal()
        pix_we = Signal()                         # 画素ペア書込(末尾クリアと区別)
        # entry = xpix/2(entry_bits幅に切出)。xpix[1:]は15bit幅なので明示スライス
        # しないと face がアドレス上位へ押し出される
        pix_adr = Signal(entry_bits)
        self.comb += [
            xpix.eq(x - self.cfg_hs_offset),
            active.eq((x >= self.cfg_hs_offset) & (xpix < width) & row_ok),
            pix_we.eq(active & xpix[0]),          # 奇数pxが揃った時に1ペア書込
            pix_adr.eq(xpix[1:1 + entry_bits]),
        ]

        # 窓が行末を超える分(= 毎ライン書かれないままリングの古い内容が見える列)を
        # ライン先頭のバックポーチ期間に黒で埋める。1サイクル1エントリ。
        # cfg_clear_from(=(hs_total-hs_offset)/2)から entries-1 までを埋める。
        # モードが変わると必要範囲も変わるので実行時信号にしてある。0ならクリアしない。
        # バックポーチ中は active=0 なので画素書込と衝突しない。間に合わなくても
        # そのラインの末尾が古い値のまま残るだけで、破綻はしない。
        clr_adr = Signal(max=entries + 1)
        clr_busy = Signal()
        self.comb += clr_busy.eq((self.cfg_clear_from != 0) &
                                 (x < self.cfg_hs_offset) &
                                 (clr_adr < entries))
        self.sync.pix += If(hs_edge, clr_adr.eq(self.cfg_clear_from)) \
                         .Elif(clr_busy, clr_adr.eq(clr_adr + 1))
        self.comb += If(clr_busy,
            wr.adr.eq(Cat(clr_adr[:entry_bits], face)), wr.dat_w.eq(0), wr.we.eq(1),
        ).Else(
            wr.adr.eq(Cat(pix_adr, face)),
            wr.dat_w.eq(Cat(pair_lo, pix555)),   # {奇数px, 偶数px}
            wr.we.eq(pix_we),
        )

        # メタ push(hs_edge時、直前ラインが有効(row_ok)かつ1画素以上書いた場合)。
        # FIFO満杯なら drop。
        # DATACLK自走カウンタ = プロトコルのタイムスタンプ(ドットクロック単位)。
        # 定数(htotal×frame番号)から算出すると仮定したモードでしか合わないが、
        # 実カウンタなら常に正確で、音声との同期もモードに依らず成立する。
        ts_cnt = Signal(32)
        cur_ts = Signal(32)      # 現ラインの先頭画素(x=hs_offset)でのカウンタ値
        self.sync.pix += [
            ts_cnt.eq(ts_cnt + 1),
            If(hs_edge, cur_ts.eq(ts_cnt + hs_offset)),
        ]

        push = Signal()
        self.comb += [
            push.eq(hs_edge & wrote & row_ok),
            meta.sink.face.eq(face),
            meta.sink.row.eq(row_eff[:row_bits]),
            meta.sink.frame.eq(frame),
            meta.sink.field.eq(field),
            # hs_edge時点の cur_ts は「今終わったライン」の先頭値(更新はクロック端で
            # 起きるため)。push するのもそのラインなので一致する。
            meta.sink.ts.eq(cur_ts),
            meta.sink.valid.eq(push),
        ]
        drops = Signal(16)
        _hs_body = [
            x.eq(0),
            wrote.eq(0),
            If(push & meta.sink.ready, face.eq(face + 1)),   # 受理時のみ面前進
            If(push & ~meta.sink.ready, drops.eq(drops + 1)),
        ]
        self.sync.pix += [
            If(active & ~xpix[0], pair_lo.eq(pix555)),  # 偶数x: 低位に保持
            If(pix_we, wrote.eq(1)),   # 末尾クリアでは立てない(黒だけの行を送らない)
            If(x != 0xFFFF, x.eq(x + 1)),               # 行内カウンタ(飽和)
        ]
        # 自走(row==cfg_vtotal-1でラップ)+ 位相ゲート付きVSYNC再整列。
        # フレーム境界(frame歩進)は常に row のラップ点=キャプチャ窓の先頭に固定。
        # VSYNCは row を cfg_vs_row_at_sync に入れ直すだけで frame は進めない。これに
        # より、窓をVSYNCより手前から開いても1枚の絵が2つのframe番号に割れない。
        # VSYNCは即座に row を書き換えず「次のHSYNCで row:=cfg_vs_row_at_sync」と
        # 保留する(vs_pend)。即時に書くと、その行は次のhs_edgeで row+1 されて
        # しまい1行まるごと欠落する。また行の途中で row が変わると行番号がずれる。
        # cfg_vtotal==0 なら自走せずVSYNCだけでフレーム境界を決める(旧来の挙動)。
        vs_pend = Signal()
        # フレーム開始 = 「row が 0 に戻る瞬間」。自走ラップか、cfg_vs_row_at_sync==0
        # のときはVSYNC再整列もそれに当たる。この規則にしないと、VSYNCがちょうど
        # ラップ点に来る場合に vs_pend がラップを打ち消して frame が永久に進まなくなる。
        wrap_now = Signal()
        frame_start = Signal()
        free_run = Signal()
        self.comb += [
            free_run.eq(self.cfg_vtotal != 0),
            wrap_now.eq(hs_edge & ~vs_pend & free_run &
                        (row == self.cfg_vtotal - 1)),
            frame_start.eq(wrap_now |
                           (hs_edge & vs_pend & (self.cfg_vs_row_at_sync == 0)) |
                           (vs_edge & ~free_run)),
        ]
        self.sync.pix += [
            # vs_pend は自走時のみ使う(非自走ではVSYNCで即座にrow=0にするので、
            # 保留すると次のhs_edgeでもrowが0に戻り、行番号が1つ重複してしまう)
            If(vs_edge & free_run, vs_pend.eq(1)),
            If(hs_edge,
                *_hs_body,
                If(vs_pend,
                    row.eq(self.cfg_vs_row_at_sync), vs_pend.eq(0),
                ).Elif(wrap_now,
                    row.eq(0),
                ).Else(
                    row.eq(row + 1),
                ),
            ),
            # 自走しない設定ではVSYNCで即座にrowを0へ(旧来の挙動)
            If(vs_edge & ~free_run, row.eq(0), wrote.eq(0)),
            If(frame_start, frame.eq(frame + 1), field.eq(~field)),
        ]

        # ================= sys ドメイン =================
        self.comb += [
            self.line_valid.eq(meta.source.valid),
            self.line_face.eq(meta.source.face),
            self.line_row.eq(meta.source.row),
            self.line_frame.eq(meta.source.frame),
            self.line_field.eq(meta.source.field),
            self.line_ts.eq(meta.source.ts),
            meta.source.ready.eq(self.line_ack),
            rd.adr.eq(Cat(self.rd_word, self.rd_face)),  # 送信側ラッチ面を読む
            self.rd_data.eq(rd.dat_r),
        ]
        # 診断CDC
        self.specials += MultiReg(frame, self.cap_frame, "sys")
        self.specials += MultiReg(drops, self.cap_drops, "sys")
