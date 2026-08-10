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
def _meta_layout(nface_bits, row_bits, entry_bits):
    # x_lo/x_hi はライン内の「黒でない範囲」をentry単位(=2画素)で示す。
    # (first/last はLiteXのストリームendpointで予約済みの名前なので使えない)
    # hs_offset を 0 にしてラインの頭から取り込む代わりに、送るのはこの範囲だけに
    # する。こうすると hs_offset は調整項目でなくなり、帯域も有効映像の幅ぶんで済む。
    return [("face", nface_bits), ("row", row_bits),
            ("frame", 16), ("field", 1), ("ts", 32),
            ("x_lo", entry_bits), ("x_hi", entry_bits)]


class TvpCapture(Module):
    """pads: r(8) g(8) b(8) hs vs [fid] を持つ Record。dataclk は呼び出し側で
    cd_pix に接続済みであること。width は偶数(2px/entry)。"""
    def __init__(self, pads, width=1024, height=512, nface=8, fifo_depth=4,
                 hs_active_low=True, vs_active_low=True,
                 hs_offset=0, vs_offset=0, vs_min_rows=0, vtotal=0,
                 vs_row_at_sync=0, hs_total=0, sys_clk_freq=45e6, measure=True,
                 auto_vtotal=False, vbp=43, interlace=False):
        # interlace: 1回のVSYNCの中にフィールドが2枚入る信号を、元の行数に織り直す。
        #   X68000の 24kHz 1024x848 は VSYNC がフレームに1回(vtotal 931行)しか来ず、
        #   その中に縦半分の解像度で画面全体を描いたフィールドが2枚並ぶ。そのまま
        #   送ると同じ絵が2回出る(実測: 466行周期で繰り返した)。
        # auto_vtotal: 実測vtotalに追従してフレーム境界と垂直位置を自動設定する。
        # vbp: 窓の先頭をVSYNCの何行後にするか(垂直バックポーチ相当)。
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
        self.line_first = Signal(entry_bits)      # 非黒範囲の先頭entry(=x/2)
        self.line_last  = Signal(entry_bits)      # 非黒範囲の末尾entry(内包)
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
        # 垂直バックポーチ[行]。窓の先頭をVSYNCの何行後にするか。モードごとに違うので
        # 実行時に変えられるようにする(CONFIGパケットから調整する)。
        self.cfg_vbp           = Signal(13, reset=vbp)
        # 送出する行数。vtotalが小さいモードで512行送ると空きが出るので、
        # min(height, vtotal - vbp) を自動で入れる。
        self.cfg_vactive       = Signal(max=height + 1, reset=height)
        # 窓の長さ(折り返す前の row で数えた行数)。
        #
        # VSYNCで row には vtotal - vbp が入り、そこから増えて vtotal で0に戻るので、
        # VSYNC直後の vbp 本は row が大きい。折り返しを入れるとこの row が f2_row を
        # 超えて「第2フィールド」と判定され frow が小さくなり、窓の外なのに範囲内と
        # 見なされる(実機で帰線期間の行が画面下部に現れた)。折り返しはスロット番号を
        # 決めるためだけに使い、窓の判定は折り返す前の row で行う。
        # 既定は制限なし。auto_vtotal が vtotal - vbp を入れる。
        self.cfg_win_rows      = Signal(13, reset=2 ** 13 - 1)
        # インターレース(ウィーブ)。cfg_f2_row は第2フィールドが始まる row。
        # 0 なら cfg_vtotal/2 を使う。半ライン分ずれるモードのために手で微調整できる。
        # 0=なし / 1=1つのVSYNCに2フィールド(24kHz 1024x848) /
        # 2=フィールドごとにVSYNC(15kHz 512x512。標準的なインターレース)
        self.cfg_interlace     = Signal(2, reset=int(interlace))
        self.cfg_f2_row        = Signal(13, reset=0)
        # フィールドの偶奇を入れ替える。どちらのフィールドが偶数ラインを描いて
        # いるかは信号からは分からないので、実機で見て決められるようにする。
        # 取り違えると1行ずつ食い違い、斜め線や円がギザギザに見える。
        self.cfg_field_swap    = Signal()
        # 方式2でフィールド極性をどこから取るか。0=VSYNCの水平位相 / 1=TVPのFIDOUT。
        # どちらが正しく出るかは実機で確かめられるよう選べるようにしてある。
        self.cfg_field_src     = Signal()
        # TVPが検出したインターレース(レジスタ38h bit5 P/I detect の反転)。
        # データシート: "Progressive/interlaced video detection status. Not dependent
        # on the H-PLL being locked. 0 = Interlaced video detected"。
        # 1 なら折り返し+スロット偶奇の振り分けを自動で行う。
        self.cfg_il_detect     = Signal()
        # 1 で生同期の位相を使わない(切り分け用)。既定0=生同期があれば使う。
        self.cfg_no_raw_phase  = Signal()
        # 1ライン当たりのDATACLK数(=H-PLL分周比)。位相判定のしきい値に使う
        self.cfg_hs_total      = Signal(13, reset=hs_total or 0)
        # 診断用(sysで観測): VSYNCエッジ時の行内カウンタと、そのときのFIDOUT
        self.stat_vs_x         = Signal(16)
        self.stat_fid          = Signal()
        # 受信側へ報告する行数。インターレースでは織り込み後なので2倍になる。
        self.out_vactive       = Signal(max=2 * height + 1)

        # --- 実測タイミング(sysドメイン)。MODEパケットで報告する ---
        # ビルド時の仮定値ではなく実信号から測る。1秒窓のカウントなので単位はHz。
        self.meas_dotclk = Signal(32)             # DATACLK周波数 [Hz]
        self.meas_hfreq  = Signal(32)             # 水平周波数 [Hz]
        self.meas_vfreq  = Signal(32)             # 垂直周波数 [mHz] (8秒積算)
        self.meas_htotal = Signal(16)             # 1ライン当たりDATACLK数
        self.meas_vtotal = Signal(16)             # 1フレーム当たりライン数

        # --- ラインバッファ(true dual-port: write=pix / read=sys)---
        self.specials.mem = mem = Memory(32, nface * entries)
        wr = mem.get_port(write_capable=True, clock_domain="pix")
        rd = mem.get_port(clock_domain="sys")
        self.specials += wr, rd

        # --- メタデータ CDC(pix → sys)。sink=pix / source=sys ---
        layout = _meta_layout(nface_bits, row_bits, entry_bits)
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
        # 測定用のVSYNCは cfg_vs_min_rows に依存させない。設定が現在のモードに
        # 合っていないと(例: vtotal 262 のモードでガードが497)VSYNCが全て捨てられ、
        # vtotalが測れなくなってモード追従が始められない(鶏と卵)。
        # 等化パルス除けの固定ガードだけ掛ける(実在モードの最小vtotalより十分小さい値)。
        VS_MEAS_GUARD = 64
        vrow_m = Signal(13)
        vs_meas = Signal()
        self.comb += vs_meas.eq(vs_edge_raw & (vrow_m >= VS_MEAS_GUARD))
        self.sync.pix += [
            If(hs_edge, If(vrow_m != 0x1FFF, vrow_m.eq(vrow_m + 1))),
            If(vs_meas, vrow_m.eq(0)),
        ]

        pix555 = Signal(16)
        self.comb += pix555.eq(rgb888_to_555(r, g, b))
        # 「黒でない」の判定しきい値(RGB555の各成分, 0..31)。ノイズで真っ暗な所が
        # 0にならないので、少し上げないと範囲が毎ライン全幅に広がってしまう。
        self.cfg_black_th = Signal(5, reset=2)

        # 有効行 row_eff = row - vs_offset(アクティブ行の0起点index)
        #
        # インターレース時は row をフィールド内行に直し、2倍して極性を足す
        # (= 元の行番号に織り戻す)。極性はVSYNCの位相ではなく「rowが折り返し点
        # f2_row を超えたか」で決める。この信号はVSYNCがフィールドごとに来ない
        # (フレームに1回だけ)ので、位相からは極性を判定できない。
        # cfg_interlace=0 のときは in_f2=0 / frow=row となり、従来と同じ式になる。
        # --- インターレースのフィールド極性 ---
        # 方式1は row が折り返し点を超えたかで決まる(VSYNCが1回しか来ないので
        # 位相は使えない)。方式2はVSYNCがフィールドごとに来るので、標準どおり
        # 「VSYNCがラインの境界に来るか中央に来るか」で判別できる。TVP7002の
        # FIDOUTも同じ判定をしているはずなので、どちらを使うか選べるようにする。
        fid_in = Signal()
        if hasattr(pads, "fid"):
            fid0 = Signal()
            self.sync.pix += [fid0.eq(pads.fid), fid_in.eq(fid0)]
        # --- 生同期(TVPを通らない経路)の半ライン位相を測る ---
        #
        # TVPのVSOUTはインターレース入力の半ライン位相を保たない。生のHSYNC/VSYNCを
        # 直接見れば、第2フィールドのVSYNCが半ラインずれて来るのが分かる。位相が
        # 分かれば行位置の偶奇が決まり、il/f2_row/swap/field_src が不要になる。
        #
        # ここではまず測定だけを入れる(位置決めにはまだ使わない)。実機で位相が
        # 交互に出ることを確認してから繋ぐ。極性やRC遅延の影響で期待どおりに
        # ならない可能性があるので、先に切り分けられるようにしておく。
        self.stat_vs_x_raw = Signal(16)     # 生VSYNCエッジ時の生ライン内カウンタ
        self.stat_hs_len_raw = Signal(16)   # 生HSYNCの周期[pixクロック]
        ph_raw = Signal()                   # 生同期から見た半ライン位相
        raw_ok = Signal()                   # 生同期が使える状態か
        if hasattr(pads, "hs_raw") and hasattr(pads, "vs_raw"):
            hr0 = Signal(); hr = Signal(); hr_p = Signal()
            vr0 = Signal(); vr = Signal(); vr_p = Signal()
            self.sync.pix += [hr0.eq(pads.hs_raw), hr.eq(hr0), hr_p.eq(hr),
                              vr0.eq(pads.vs_raw), vr.eq(vr0), vr_p.eq(vr)]
            # 極性は不明なので両エッジで測り、周期が妥当な方を採る…のは複雑なので
            # まずアクティブローと仮定する(TVP経由と同じ想定)。ずれていたら
            # 位相が周期の端に張り付くので実機で判別できる。
            hr_edge = Signal(); vr_edge = Signal()
            self.comb += [hr_edge.eq(hr_p & ~hr), vr_edge.eq(vr_p & ~vr)]
            xr = Signal(16)                 # 生HSYNCからの行内カウンタ
            hs_len_r = Signal(16)
            vs_xr = Signal(16)
            self.sync.pix += [
                If(xr != 0xFFFF, xr.eq(xr + 1)),
                If(hr_edge, xr.eq(0), hs_len_r.eq(xr)),
                If(vr_edge, vs_xr.eq(xr)),
            ]
            self.specials += MultiReg(vs_xr, self.stat_vs_x_raw, "sys")
            self.specials += MultiReg(hs_len_r, self.stat_hs_len_raw, "sys")

            # 半ライン位相のビット。VSYNCがラインの中央付近に来たフィールドは
            # 半ライン分あとから始まるので、そのラインは1スロット下へ落ちる。
            # どちらが先かは物理で決まるので swap の設定は要らない。
            #
            # 実測(24kHz 1024x848): 位相は 9 と 561 を 45:55 で交互に取り、差は
            # 552 = 生HSYNC周期1103 のちょうど半分だった。判定はライン周期の
            # 1/4〜3/4 を「中央付近」とする(TVP経由の判定と同じ考え方)。
            q_r = Signal(16)
            self.comb += q_r.eq(hs_len_r[2:])       # 周期/4
            self.sync.pix += If(vr_edge,
                ph_raw.eq((xr > q_r) & (xr < (hs_len_r - q_r))),
                # 生同期が生きているか。周期が妥当な範囲にあるかで見る。
                # 配線が無い/浮いている場合は周期が0か飽和値になる
                raw_ok.eq((hs_len_r > 64) & (hs_len_r < 0xF000)),
            )

        vs_x   = Signal(16)          # VSYNCエッジ時の行内カウンタ(=位相)
        fid_vs = Signal()            # そのときのFIDOUT
        ph     = Signal()            # 位相から見たフィールド極性
        fld    = Signal()            # 方式2のフィールド極性(VSYNCごとに更新)
        q = Signal(13)
        self.comb += [
            q.eq(self.cfg_hs_total[2:]),          # hs_total/4
            ph.eq((x > q) & (x < (self.cfg_hs_total - q))),   # ラインの中央付近か
        ]
        self.sync.pix += If(vs_edge_raw,
            vs_x.eq(x), fid_vs.eq(fid_in),
            fld.eq(Mux(self.cfg_field_src, fid_in, ph)),
        )

        # 位相から決まるフィールド極性。VSYNCがラインの中央付近に来たフィールドは
        # 半ライン分あとから始まるので、そのラインは1スロット下へ落ちる。
        # どちらが先かは物理で決まるので swap の設定は要らない。
        f2_row = Signal(13)
        in_f2  = Signal()
        fld_pos = Signal()
        frow   = Signal(13)          # フィールド内行
        fe     = Signal(13)          # フィールド内の有効行index
        # インターレースの扱い。
        #
        # TVPはインターレース入力でも Lines per Frame をフレーム全体(両フィールド
        # 合計)で報告し、VSOUTもフレームに1回しか出さない(データシート Table 16:
        # 480i60Hz も 480p60Hz も lines per frame = 525)。したがって row は両
        # フィールドを通して数える。折り返し点 vtotal/2 で前半/後半に分け、後半を
        # 1スロット下へ置けば織り込みになる。vtotal が奇数(X68000 24kHz 1024x848
        # では931)なので、この半端が半ラインに相当する。
        #
        # 判定はTVPの検出ビット(38h bit5)を使う。cfg_interlace は手動での上書き。
        il_det = Signal()
        self.comb += [
            il_det.eq(self.cfg_il_detect | (self.cfg_interlace != 0)),
            f2_row.eq(Mux(self.cfg_f2_row != 0, self.cfg_f2_row,
                          self.cfg_vtotal[1:])),        # 既定は vtotal/2
            in_f2.eq(il_det & (row >= f2_row)),
            frow.eq(row - Mux(in_f2, f2_row, 0)),
            fe.eq(frow - self.cfg_vs_offset),
            # スロットの偶奇。
            #
            # どちらのフィールドが半ライン下かは物理で決まるので設定は要らない。
            #
            # 垂直位置はVSYNCからの経過時間で決まり、そのフィールドの最初のラインは
            # 「VSYNCの次のHSYNC」から始まる。位相を p(直前のHSYNCから何サンプル後に
            # VSYNCが来たか)とすると、次のHSYNCまでは (htotal - p) サンプル。
            # つまり p が大きいフィールドほど早く始まり、上に来る。
            #
            # 実測(24kHz 1024x848, htotal 1104):
            #   p = 9   → 次のHSYNCは1095サンプル後 → 遅い → 下(スロット +1)
            #   p = 561 → 次のHSYNCは 543サンプル後 → 早い → 上(スロット +0)
            #
            # ph_raw は「pがライン中央付近」なので、下になるのは ~ph_raw の側。
            # 当初 ph_raw をそのまま使って実機で織り込みが逆になった。
            #
            # 生同期が無い場合だけ「TVPのフレームの後半が第2フィールド」と推測する。
            # これはVSOUTが2本のVSYNCのどちらで出るかに依存するので当たらないことが
            # あり、実機では逆になった。その場合の逃げ道として swap を残す。
            # プログレッシブでは1フィールドしかないので偶奇は常に0。
            # 位相による反転はインターレースの第2フィールドを見分けるためのもので、
            # 無条件に適用すると全ての行が奇数スロットへ入り、半ライン分ずれた位置に
            # 置かれる(実機で「全行が同じようにずれる」形で出た)。
            If(~il_det,
                fld_pos.eq(0),
            ).Elif(raw_ok & ~self.cfg_no_raw_phase,
                fld_pos.eq(~ph_raw),
            ).Else(
                fld_pos.eq(in_f2 ^ self.cfg_field_swap),
            ),
        ]
        # row_ok / row_eff はレジスタに落とす。これらは画素書込のイネーブル
        # (wr.we)まで届くので、組合せのままだと減算2段+比較がBRAMの書込端子
        # 直前まで伸びる。実際 sys が 53.6→42.5MHz まで落ちて要求45MHzを割った。
        # row は hs_edge でしか変わらず、画素が始まるのは hs_offset(数百クロック)
        # 後なので、1クロック遅れても取りこぼさない。hs_edge 時点の値は「いま
        # 終わったライン」のもので、push の判定に使う値として正しい。
        row_eff = Signal(14)
        row_ok  = Signal()
        field_r = Signal()
        ph_r    = Signal()           # 半ライン位相(このVSYNC周期のフィールド)
        il_on   = Signal()
        self.comb += il_on.eq(self.cfg_interlace != 0)
        # 行位置は「VSYNCから何半ライン後か」で決める。
        #
        # インターレースの原理そのもので、第2フィールドのVSYNCは半ライン分ずれた
        # 位置に来るので、そのフィールドのラインは第1フィールドのラインの物理的に
        # 間へ落ちる。だから位置を時間(半ライン単位)で決めれば、織り込みは設定
        # 無しで勝手に成立する。以前は il/f2_row/swap/field_src の4つで教えて
        # いたが、どれも「行番号で並べる」という実装の都合から生まれたもので、
        # 信号自体には無かった情報である。
        #
        # プログレッシブでは ph が毎フレーム同じ値になるので、スロットは1つ飛びに
        # 並ぶ。空くスロットは受信側が「次のラインまでの間隔」ぶん太らせて埋める
        # (ビームには太さがあるので物理的にも正しい)。
        #
        # cfg_half_line=0 にすると位相を無視する(位相判定が誤る信号に当たった
        # ときの逃げ道。既定は1)。
        self.sync.pix += [
            # 窓の内か。フィールドあたりの行数(cfg_vactive)と、折り返す前の row で
            # 数えた窓の長さ(cfg_win_rows)の両方で判定する。後者が無いと、VSYNC直後の
            # 帰線期間の行が折り返しで「第2フィールド」と誤判定されて画面下部に出る。
            row_ok.eq((frow >= self.cfg_vs_offset)
                      & (fe < self.cfg_vactive)
                      & (row < self.cfg_win_rows)),
            ph_r.eq(fld_pos),
            field_r.eq(fld_pos),
            # = fe*2 + 位相。cfg_half_line は位相の「取得元」を選ぶ信号なので、
            # ここで偶奇の適用をゲートしてはいけない
            row_eff.eq(Cat(fld_pos, fe)),
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

        # --- ライン内の「黒でない範囲」を entry 単位で記録する ---
        # 送るのはこの範囲だけにする。ライン全体(ブランキング込み)を送ると帯域が
        # 1.4倍になって入らないため。範囲はラインが終わってから push するので、
        # 1ライン分バッファしている今の構造のまま実現できる。
        def _nz(p):
            return ((p[0:5] > self.cfg_black_th)
                    | (p[5:10] > self.cfg_black_th)
                    | (p[10:15] > self.cfg_black_th))
        pair_nz = Signal()
        self.comb += pair_nz.eq(pix_we & (_nz(pix555) | _nz(pair_lo)))
        ln_first = Signal(entry_bits, reset=2 ** entry_bits - 1)
        ln_last = Signal(entry_bits)
        ln_any = Signal()
        self.sync.pix += [
            If(pair_nz,
                If(~ln_any | (pix_adr < ln_first), ln_first.eq(pix_adr)),
                If(~ln_any | (pix_adr > ln_last), ln_last.eq(pix_adr)),
                ln_any.eq(1),
            ),
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
        # クリアはライン先頭のバックポーチ中しか進められないので、1ライン当たり
        # hs_offset エントリしか消せない。毎ライン先頭に戻していたため、窓が
        # ラインより大幅に広いモード(15kHz 512x512: 1ライン544クロックに対し窓
        # 1024画素)ではクリアが先頭付近しか届かず、右側にリングバッファの古い
        # 内容が残った。ラインをまたいで進め、末端まで行ったら先頭へ戻す。
        # 消した所には二度と画素が書かれないので、一巡すれば黒のまま安定する。
        self.sync.pix += [
            If(clr_busy,
                If(clr_adr >= entries - 1,
                    clr_adr.eq(self.cfg_clear_from),
                ).Else(
                    clr_adr.eq(clr_adr + 1),
                ),
            ),
            # cfg_clear_from が動いた(モードや設定の変更)ときは範囲内へ引き戻す
            If(clr_adr < self.cfg_clear_from, clr_adr.eq(self.cfg_clear_from)),
        ]
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
            # インターレース時は実際のフィールド極性を載せる(非対応時は従来の交互)
            meta.sink.field.eq(Mux(il_on, field_r, field)),
            # hs_edge時点の cur_ts は「今終わったライン」の先頭値(更新はクロック端で
            # 起きるため)。push するのもそのラインなので一致する。
            meta.sink.ts.eq(cur_ts),
            meta.sink.x_lo.eq(ln_first),
            meta.sink.x_hi.eq(ln_last),
            meta.sink.valid.eq(push),
        ]
        drops = Signal(16)
        _hs_body = [
            x.eq(0),
            wrote.eq(0),
            ln_any.eq(0), ln_first.eq(2 ** entry_bits - 1), ln_last.eq(0),
            If(push & meta.sink.ready, face.eq(face + 1)),   # 受理時のみ面前進
            If(push & ~meta.sink.ready, drops.eq(drops + 1)),
        ]
        self.sync.pix += [
            If(active & ~xpix[0], pair_lo.eq(pix555)),  # 偶数x: 低位に保持
            # 「1画素以上書いたか」。末尾クリアでは立てない。
            # 黒でない画素があったかどうかとは別に持つ。全黒の行を送らない設計に
            # すると、Viewer側が「黒い行」と「届かなかった行」を区別できなくなり、
            # 前フレームの内容が残る(インターレースのSIMでも、内容が0の行が
            # まるごと落ちて織り込みが崩れた)。行は必ず送り、中身の範囲だけを
            # x_lo/x_hi で示す(全黒なら空範囲)。
            If(pix_we, wrote.eq(1)),
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

        # --- 実測タイミング: 1秒窓(sysクロック基準=25MHz水晶由来で正確)で
        #     DATACLK/HSYNC/VSYNC を数える。窓が1秒ちょうどなのでカウント値がHz。
        #     htotal/vtotal は行内カウンタ/行カウンタをそのままラッチして得る。
        if measure:
            m_clk = Signal(32); m_hs = Signal(32); m_vs = Signal(32)
            s_clk = Signal(32); s_hs = Signal(32); s_vs = Signal(32)
            s_htot = Signal(16); s_vtot = Signal(16)
            htot_now = Signal(16); vtot_now = Signal(16)
            self.sync.pix += [
                If(hs_edge, htot_now.eq(x + 1)),   # xはライン先頭で0→次のedgeでperiod-1
                If(vs_meas, vtot_now.eq(vrow_m)),  # 前回VSYNCからのHSYNC数
            ]
            # sys → pix へ「今のカウンタを確定して0に戻せ」のパルスを送る
            self.submodules._samp = _samp = PulseSynchronizer("sys", "pix")
            self.sync.pix += [
                If(_samp.o,
                    s_clk.eq(m_clk), s_hs.eq(m_hs), s_vs.eq(m_vs),
                    s_htot.eq(htot_now), s_vtot.eq(vtot_now),
                    m_clk.eq(0), m_hs.eq(hs_edge), m_vs.eq(vs_meas),
                ).Else(
                    m_clk.eq(m_clk + 1),
                    If(hs_edge, m_hs.eq(m_hs + 1)),
                    If(vs_meas, m_vs.eq(m_vs + 1)),
                ),
            ]
            # sys側: 1秒ごとにサンプルパルスを出し、少し待って確定値を取り込む。
            # s_* はパルス後しか変化しないので、待ってから MultiReg で受ければ安全。
            WIN = max(int(sys_clk_freq), 4)
            win = Signal(max=WIN)
            wait = Signal(6)
            vs_stable = None
            for src, dst in ((s_clk, self.meas_dotclk), (s_hs, self.meas_hfreq),
                             (s_vs, None), (s_htot, self.meas_htotal),
                             (s_vtot, self.meas_vtotal)):
                stable = Signal(len(src))
                self.specials += MultiReg(src, stable, "sys")
                if dst is None:
                    vs_stable = stable      # VSYNCは下の8秒積算で使う
                else:
                    self.sync += If(wait == 1, dst.eq(stable))
            self.comb += _samp.i.eq(win == WIN - 1)
            self.sync += [
                If(win == WIN - 1, win.eq(0)).Else(win.eq(win + 1)),
                If(win == WIN - 1, wait.eq(32)).Elif(wait != 0, wait.eq(wait - 1)),
            ]
            # vfreqは1秒窓のエッジ数だと分解能±1Hz(55.456Hz→55と表示された)。
            # 8秒積算して×125すれば mHz 単位で 0.125Hz 分解能になる。htotal/vtotal と
            # dotclk は1秒窓のままにして応答性(モード変化の検出)を保つ。
            vacc = Signal(24)
            vwin = Signal(3)
            self.sync += If(wait == 1,
                If(vwin == 7,
                    self.meas_vfreq.eq((vacc + vs_stable) * 125),   # mHz
                    vacc.eq(0), vwin.eq(0),
                ).Else(
                    vacc.eq(vacc + vs_stable), vwin.eq(vwin + 1),
                ))

        # --- モード自動追従(垂直) ---
        # 実測 vtotal が安定したら cfg_* に反映する。これをやらないと、モードが
        # 変わったとき自走カウンタのラップ点が合わず、絵が縦に繰り返して見える
        # (実測: 24kHz 1024x424 は vtotal 930。568固定のままだと2枚に割れる)。
        # 位置(vs_row_at_sync)は「VSYNCの何行後から窓を開くか」で持つ。
        # 垂直バックポーチはモードで多少違うが、まず vbp 固定で追従させる。
        # 揺れで書き換え続けないよう、同じ値が連続してから反映する(ヒステリシス)。
        if auto_vtotal:
            v_last = Signal(16)
            v_cnt = Signal(3)
            self.sync += [
                If(self.meas_vtotal != v_last,
                    v_last.eq(self.meas_vtotal), v_cnt.eq(0),
                ).Elif(v_cnt != 4,
                    v_cnt.eq(v_cnt + 1),
                ),
                # 現実的な範囲の値が3回続いたら採用する
                If((v_cnt >= 3) & (self.meas_vtotal >= 100)
                                & (self.meas_vtotal < 1500),
                    self.cfg_vtotal.eq(self.meas_vtotal),
                ),
            ]
            # vbp から導く値は毎サイクル更新する。以前は上の「安定判定が成立した1回」
            # の中で代入していたため、vtotalが安定している間はvbpを変えても何も
            # 起きなかった(実機で発覚)。
            # 組合せにするとsysのレジスタからpixドメインへ減算器を挟んで渡ることに
            # なり、配置次第でタイミングが悪化した(eth_rxが130→116MHzに落ちた)。
            # レジスタのまま毎サイクル更新すれば、1クロック遅れるだけで即座に反映され、
            # pix側は短いレジスタ出力を見るだけで済む。
            vstart = Signal(13)      # 窓の先頭から frame 末尾までの行数
            fstart = Signal(13)      # インターレース時のフィールドあたり有効行数
            vs_row = Signal(13)      # VSYNC検出時に row へ入れる値
            self.comb += [
                vstart.eq(self.cfg_vtotal - self.cfg_vbp),
                fstart.eq(self.cfg_vtotal[1:] - self.cfg_vbp),
                # row に入れる値は vtotal 未満でなければならない。vbp=0 だと
                # vtotal - vbp = vtotal になり、ラップ判定(row == vtotal-1)を
                # 通り越して二度と回らず、フレームが進まなくなって映像が止まる
                # (実機で vbp=0 にして 0fps になった)。0 は「VSYNCが窓の先頭」
                # そのものなので 0 に折り返す。
                If((self.cfg_vbp == 0) | (self.cfg_vbp >= self.cfg_vtotal),
                    vs_row.eq(0),
                ).Else(
                    vs_row.eq(vstart),
                ),
            ]
            self.sync += [
                # VSYNC位相ゲートは vtotal の 3/8。
                #
                # 以前は 3/4 にしていたため、インターレース信号の「フィールド毎に
                # 来るVSYNC」のうち2本目(前のVSYNCから約 vtotal/2 後)が捨てられて
                # いた。すると vtotal が2フィールド分(931など)として測られ、その
                # 大きな vtotal からゲートが更に上がって自己強化する。結果
                # 「1VSYNCに2フィールド入っている」ように見えていた(24kHz
                # 1024x848 で vtotal 931 と測れたのはこれ)。等化パルスは
                # VSYNCの直近に出るので 3/8 でも十分に落とせる。
                self.cfg_vs_min_rows.eq((self.cfg_vtotal >> 2) +
                                        (self.cfg_vtotal >> 3)),
                # 窓の先頭 = VSYNCの vbp 行後
                self.cfg_vs_row_at_sync.eq(vs_row),
                # 窓の長さ(折り返す前の row で数えた行数)= vtotal - vbp
                self.cfg_win_rows.eq(vstart),
                # 送出行数(フィールド内の有効行数) = min(height/2, vtotal - vbp)。
                #
                # 行位置は常に半ライン単位のスロットなので、スロットは最大で
                # vactive*2 になる。バッファの行数を超えないよう上限は height/2。
                # 以前は cfg_interlace で分岐していたが、位置決めが位相ベースに
                # なったので分岐は要らない。
                If(vstart < (height >> 1),
                    self.cfg_vactive.eq(vstart),
                ).Else(
                    self.cfg_vactive.eq(height >> 1),
                ),
            ]

        # 報告用の行数。行位置は常に半ライン単位のスロットなので、フィールド内
        # 行数の2倍になる(プログレッシブでも1つ飛びに並ぶだけで範囲は2倍)。
        self.comb += self.out_vactive.eq(Cat(0, self.cfg_vactive))   # = vactive*2

        # ================= sys ドメイン =================
        self.comb += [
            self.line_valid.eq(meta.source.valid),
            self.line_face.eq(meta.source.face),
            self.line_row.eq(meta.source.row),
            self.line_frame.eq(meta.source.frame),
            self.line_field.eq(meta.source.field),
            self.line_ts.eq(meta.source.ts),
            self.line_first.eq(meta.source.x_lo),
            self.line_last.eq(meta.source.x_hi),
            meta.source.ready.eq(self.line_ack),
            rd.adr.eq(Cat(self.rd_word, self.rd_face)),  # 送信側ラッチ面を読む
            self.rd_data.eq(rd.dat_r),
        ]
        # 診断CDC
        self.specials += MultiReg(frame, self.cap_frame, "sys")
        self.specials += MultiReg(drops, self.cap_drops, "sys")
        self.specials += MultiReg(vs_x, self.stat_vs_x, "sys")
        self.specials += MultiReg(fid_vs, self.stat_fid, "sys")
