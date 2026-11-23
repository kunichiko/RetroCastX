#!/usr/bin/env python3
"""デジタルRGB(RGBI + HS/VS)を**絵として取り込む**(sys ドメイン)。

HSYNC からドットを作り直してラインバッファに書き、送出部(RetroCastXStreamer)へ
渡す。極性・fH・fV などの測定もここで行う(CONFIG 0x70〜0x75)。

★**実機では未確認。** デジタルRGB を出す機種も、ループバックの配線
  (`docs/TODO.md` §0b)もまだ無い状態で、シミュレーション
  (`sim_drgb_capture.py`)だけで書いた。

## 送り方(`docs/protocol-v0.md`)

- CONFIG 0x003A `video_src` = 1 で、TvpCapture の代わりにこちらを送出部へ繋ぐ
  (`CaptureSel`)。ANNOUNCE の caps bit5(CAP_DRGB)で取り込めることを知らせる
- 1ライン = `pll_divide`(0x0012)ドット、ドット内の位置は `phase`(0x001F)
- LINE は pixfmt 6(RGBI4)。1バイトに2画素、下位ニブルが先、bit0=R 1=G 2=B 3=I。
  ライン全体を送る
- 色はボードで決めない(8色 / 16色は Viewer が機種で決める)
"""
from migen import *
from litex.soc.interconnect import stream


class DigitalRgbCapture(Module):
    """TvpCapture と**同じ sys 側の口**(line_valid / line_row / … / rd_word /
    rd_data と MODE 用の meas_*)を持つ。

    ## ドットの作り直し

    外からドットクロックは来ない。sys(45MHz)でオーバーサンプリングし、
    HS から HS までを `cfg_htotal`(= Viewer が書く pll_divide)等分する。
    除算器は置かず、分数の足し込みで数える:

        acc += htotal × 256 ;  acc >= hsum なら { acc -= hsum ; ドットを1つ拾う }

    `hsum` は**256ライン分の HS 間隔の合計**。1ラインの長さだけで割ると、
    sys の量子化(±1 サンプル = 実機で ±0.32 ドット)がライン端まで積もるので、
    平均で消す(`docs/TODO.md` §0b の「1. ライン長を256ライン平均」)。

    ドット内の拾う位置は `cfg_phase`(0〜31)。各ラインの acc の初期値を
    `hsum − hsum × (2·phase + 1) / 64` にすると、ドットの頭から
    (2·phase + 1)/64 の位置で拾う。16 でほぼ中央(既定)。

    ★**HS から数えたドット 0 がラインの先頭。** TVP の取り込み(hs_offset=0)と
      同じで、有効映像の位置は Viewer の管面(H位置)で合わせる。

    ★**ドットクロックが sys より速い機種は取れない**(htotal が1ラインの sys
      クロック数を超えると1サイクルに2ドットが要る)。45MHz に対して
      14.3MHz(3.1 サンプル/ドット)は取れる。28.6MHz 級は §0b の IDDR の案が要る。

    ## ラインバッファ

    1画素4bit、1語(32bit)に8画素(下位ニブルが先 = RGBI4 の並びそのまま)。
    送出部は語をそのまま LINE のペイロードに載せる。
    width 2048 × 4面 = 1024語 × 32bit(EBR 2個)。
    ★**8画素に満たないラインの末尾は書かない**(htotal が8の倍数でないとき、
      最後の数ドット = 前ポーチは前のラインの内容のまま)。

    ## 行の位置

    TvpCapture と同じく**半ライン単位のスロット**で、プログレッシブなので
    偶数スロットだけを使う(行 n → スロット 2n)。VS の後の最初の HS で始まる
    ラインが行 0。★**インターレースは未対応**(フィールドを見分けていない)。

    ## 極性

    HS/VS が正極性か負極性かは機種による。決め打ちせず**測る**(パルスは1ラインの
    ごく一部なので、約20ms の窓で Low の時間が半分未満なら正極性)。
    `pol` を外から渡したときはそちらを使う(シミュレーション用)。

    ## 診断(CONFIG 0x70〜0x75 で読む)

    stat_fh / stat_fv / stat_lines / stat_hlen / stat_level / stat_pol。
    ★以前は別の測定器(DigitalRgbProbe)が同じキーで出していたが、ループバック
      試験用の生成器と一緒に外した(2026-11-23、ロジック使用率を下げるため)。
      キーの意味は同じ。測定器の覗き窓(0x76/0x77、0x79〜0x7D)は無くなった。
    """

    def __init__(self, pads, pol=None, width=2048, height=2048, nface=4, fifo_depth=2,
                 sys_clk_freq=45e6, meas_clks=None, frame_timeout=None,
                 avg_log2=8, pol_clks=None):
        # avg_log2: ライン長を平均する本数(2のべき乗の指数)。実機は 8(256本)。
        #   シミュレーションでは小さくして、平均が確定するまでの時間を縮める
        assert width % 8 == 0 and (width & (width - 1)) == 0, "width は2のべき乗"
        AVG_N = 1 << avg_log2
        assert nface >= fifo_depth + 2 and (nface & (nface - 1)) == 0
        meas_clks = int(meas_clks or sys_clk_freq)
        frame_timeout = int(frame_timeout or sys_clk_freq * 3 // 2)   # 約1.5秒
        pol_clks = int(pol_clks or sys_clk_freq // 50)               # 約20ms(1フレーム超)
        entries = width // 8
        entry_bits = log2_int(entries)
        nface_bits = log2_int(nface)
        row_bits = log2_int(height)
        self.width = width
        self.height = height

        # --- 設定(sys) ---
        self.cfg_htotal = Signal(12)          # 1ラインのドット数(pll_divide)
        self.cfg_phase = Signal(5, reset=16)  # ドット内の拾う位置(16 = 中央)

        # --- 送出部への口(TvpCapture と同じ名前と意味) ---
        self.line_valid = Signal()
        self.line_row = Signal(row_bits)
        self.line_frame = Signal(16)
        self.line_field = Signal()
        self.line_face = Signal(nface_bits)
        self.line_ts = Signal(32)
        self.line_first = Signal(entry_bits)  # RGBI4 はライン全体を送るので使わない
        self.line_last = Signal(entry_bits)
        self.line_ack = Signal()
        self.rd_face = Signal(nface_bits)
        self.rd_word = Signal(entry_bits)
        self.rd_data = Signal(64)             # 下位32bit = 8画素
        self.il_slot1 = Signal()              # インターレースは未対応(常に0)
        self.out_vactive = Signal(max=2 * height + 1)

        self.meas_dotclk = Signal(32)         # 拾ったドットの数/秒 [Hz]
        self.meas_hfreq = Signal(32)          # [Hz]
        self.meas_vfreq = Signal(32)          # [mHz](1秒窓なので 1Hz 刻み)
        self.meas_htotal = Signal(16)
        self.meas_vtotal = Signal(16)
        self.meas_vtotal_stable = Signal(16)
        self.meas_frame_clocks = Signal(32)   # 直近2フレームの sys クロック数
        self.meas_frame_lines = Signal(16)    # 同じ2フレームのライン数
        self.meas_frame_new = Signal()

        # 診断
        self.stat_hsum = Signal(24)           # 256ライン分の HS 間隔の合計(0 = 未確定)
        self.stat_drops = Signal(16)          # 送出が間に合わず捨てたライン
        self.stat_fh = self.meas_hfreq        # 0x70 [Hz]
        self.stat_fv = self.meas_vfreq        # 0x71 [mHz]
        self.stat_lines = self.meas_vtotal    # 0x72 VS 間のライン数
        self.stat_hlen = Signal(16)           # 0x73 1ラインの sys クロック数(平均)
        self.stat_level = Signal(8)           # 0x74 いまの生レベル {vs,hs,i,b,g,r}
        self.stat_pol = Signal(8)             # 0x75 bit0=HS 負極性 bit1=VS 負極性

        # # #

        # 非同期入力なので2段で受ける
        raw = Signal(6); s1 = Signal(6); s2 = Signal(6)
        self.comb += raw.eq(Cat(pads.r, pads.g, pads.b, pads.i, pads.hs, pads.vs))
        self.sync += [s1.eq(raw), s2.eq(s1)]
        self.comb += self.stat_level.eq(s2)

        # --- 極性を測る ---
        if pol is None:
            pol_cnt = Signal(max=pol_clks + 1)
            hs_low = Signal(max=pol_clks + 1)
            vs_low = Signal(max=pol_clks + 1)
            self.sync += If(pol_cnt == pol_clks - 1,
                pol_cnt.eq(0),
                self.stat_pol.eq(Cat(hs_low < (pol_clks >> 1),
                                     vs_low < (pol_clks >> 1), C(0, 6))),
                hs_low.eq(0), vs_low.eq(0),
            ).Else(
                pol_cnt.eq(pol_cnt + 1),
                If(~s2[4], hs_low.eq(hs_low + 1)),
                If(~s2[5], vs_low.eq(vs_low + 1)),
            )
            pol = self.stat_pol[:2]
        else:
            self.comb += self.stat_pol.eq(pol)
        color = Signal(4)
        hs_act = Signal(); vs_act = Signal()
        self.comb += [
            color.eq(s2[0:4]),
            hs_act.eq(s2[4] ^ pol[0]),
            vs_act.eq(s2[5] ^ pol[1]),
        ]
        hs_d = Signal(); vs_d = Signal()
        hs_edge = Signal(); vs_edge = Signal()
        self.sync += [hs_d.eq(hs_act), vs_d.eq(vs_act)]
        self.comb += [hs_edge.eq(hs_act & ~hs_d), vs_edge.eq(vs_act & ~vs_d)]

        # --- 1ラインの長さ(256ライン平均) ---
        #
        # ★**外れたラインは足さない。** VSYNC 期間に等化パルスや切り込みを持つ機種
        #   では HS 間隔が半分になる。混ぜると平均がずれて全ラインのドット位置が
        #   崩れる。平均から ±1/16 を外れたラインは捨て、255本続けて外れたら
        #   モードが変わったとみて平均を作り直す。
        hcnt = Signal(16)
        hsum = Signal(24)          # 確定した 256 ライン分の合計(0 = まだ無い)
        hsum_acc = Signal(24)
        hsum_n = Signal(8)
        hrej = Signal(8)
        href = Signal(16)
        hok = Signal()
        seen_hs = Signal()
        self.comb += [
            href.eq(hsum[avg_log2:]),
            hok.eq((hsum == 0)
                   | ((hcnt > href - (href >> 4)) & (hcnt < href + (href >> 4)))),
            self.stat_hsum.eq(hsum),
            self.stat_hlen.eq(href),
        ]
        self.sync += If(hs_edge,
            hcnt.eq(1),
            seen_hs.eq(1),
            If(seen_hs,
                If(hok,
                    hrej.eq(0),
                    If(hsum_n == AVG_N - 1,
                        hsum.eq(hsum_acc + hcnt),
                        hsum_acc.eq(0),
                        hsum_n.eq(0),
                    ).Else(
                        hsum_acc.eq(hsum_acc + hcnt),
                        hsum_n.eq(hsum_n + 1),
                    ),
                ).Else(
                    If(hrej == 255,
                        hsum.eq(0), hsum_acc.eq(0), hsum_n.eq(0), hrej.eq(0),
                    ).Else(hrej.eq(hrej + 1)),
                ),
            ),
        ).Elif(hcnt != 0xFFFF, hcnt.eq(hcnt + 1))

        # --- ドットの位置(分数の足し込み) ---
        step = Signal(12 + avg_log2)
        phase_mul = Signal(30)
        acc0 = Signal(25)
        self.sync += [
            step.eq(Cat(C(0, avg_log2), self.cfg_htotal)),
            # hsum は256ラインに1回しか変わらないので、2段のレジスタに分けてよい
            phase_mul.eq(hsum * Cat(C(1, 1), self.cfg_phase)),   # hsum × (2p+1)
            # ★**検出の遅れを 2 サイクル分取り戻す**(+ step × 2)。
            #   HS の本当の縁は検出したサイクルより前で、acc を入れ直すのにも
            #   1サイクルかかる。補わないと拾う位置が後ろへずれ、位相16 が
            #   ドットの中央ではなく次のドットとの境目になった(シミュレーションで
            #   縞の半分が隣のドットの色になった)。1.5 サイクルでは 3.14 サンプル/
            #   ドットで正しく取れる位相が 6〜16(中央 11)と、まだ遅れていた
            acc0.eq(hsum - phase_mul[6:] + Cat(C(0, 1), step)),
        ]
        acc = Signal(25)
        dotn = Signal(13)
        dot = Signal()
        ready = Signal()
        self.comb += ready.eq((hsum != 0) & (self.cfg_htotal != 0))
        self.sync += [
            dot.eq(0),
            If(~ready,
                acc.eq(0), dotn.eq(0x1FFF),
            ).Elif(hs_edge,
                acc.eq(acc0),
                dotn.eq(0x1FFF),       # 次に拾うドットが 0
            ).Elif(acc >= hsum,
                acc.eq(acc - hsum + step),
                If(dotn != 0x1FFE, dotn.eq(dotn + 1)),
                dot.eq(1),
            ).Else(
                acc.eq(acc + step),
            ),
        ]
        # dot が立ったサイクルの dotn が拾ったドットの番号。色は判定したサイクルの
        # ものを1段遅らせて dot と同じ段に揃える
        color_d = Signal(4)
        self.sync += color_d.eq(color)

        # --- ラインバッファ ---
        mem = Memory(32, nface * entries)
        wr = mem.get_port(write_capable=True)
        rd = mem.get_port(async_read=False)
        self.specials += mem, wr, rd
        wface = Signal(nface_bits)
        pack = Signal(32)
        word = Signal(32)
        self.comb += word.eq(Cat(pack[4:], color_d))
        self.sync += If(dot, pack.eq(word))
        self.comb += [
            wr.adr.eq(Cat(dotn[3:3 + entry_bits], wface)),
            wr.dat_w.eq(word),
            wr.we.eq(dot & (dotn[:3] == 7) & (dotn < width)),
            rd.adr.eq(Cat(self.rd_word, self.rd_face)),
            self.rd_data.eq(Cat(rd.dat_r, C(0, 32))),
        ]

        # --- 行・フレーム・ts。HS ごとに1ラインを閉じる ---
        line = Signal(16)
        frame = Signal(16)
        ts_line = Signal(32)
        vs_pend = Signal()
        vs_now = Signal()
        self.comb += vs_now.eq(vs_pend | vs_edge)
        meta = stream.SyncFIFO([("face", nface_bits), ("row", row_bits),
                                ("frame", 16), ("ts", 32)], fifo_depth)
        self.submodules += meta
        push_ok = Signal()
        self.comb += [
            push_ok.eq(hs_edge & ready & seen_hs & (line < (height >> 1))),
            meta.sink.valid.eq(push_ok),
            meta.sink.face.eq(wface),
            meta.sink.row.eq(Cat(C(0, 1), line)),
            meta.sink.frame.eq(frame),
            meta.sink.ts.eq(ts_line),
        ]
        self.sync += [
            If(push_ok,
                If(meta.sink.ready,
                    wface.eq(wface + 1),
                ).Else(self.stat_drops.eq(self.stat_drops + 1)),
            ),
            If(vs_edge & ~hs_edge, vs_pend.eq(1)),
            If(hs_edge,
                ts_line.eq(ts_line + self.cfg_htotal),
                If(vs_now,
                    vs_pend.eq(0),
                    line.eq(0),
                    frame.eq(frame + 1),
                ).Elif(line != 0xFFFF, line.eq(line + 1)),
            ),
        ]
        self.comb += [
            self.line_valid.eq(meta.source.valid),
            self.line_face.eq(meta.source.face),
            self.line_row.eq(meta.source.row),
            self.line_frame.eq(meta.source.frame),
            self.line_ts.eq(meta.source.ts),
            self.line_first.eq(0),
            self.line_last.eq(entries - 1),
            meta.source.ready.eq(self.line_ack),
        ]

        # --- 測定(MODE 用) ---
        # 1秒窓: fH / fV / ドットの数
        # ★カウンタは必要な幅だけにする(ロジック使用率が厳しい)。1秒窓で
        #   HS は 20万回(200kHz)、VS は 1000回、ドットは sys の数を超えない
        tick = Signal(max=meas_clks)
        hs_n = Signal(18); vs_n = Signal(10); dot_n = Signal(max=meas_clks + 1)
        self.sync += If(tick == meas_clks - 1,
            tick.eq(0),
            self.meas_hfreq.eq(hs_n),
            self.meas_vfreq.eq(vs_n * 1000),
            self.meas_dotclk.eq(dot_n),
            hs_n.eq(0), vs_n.eq(0), dot_n.eq(0),
        ).Else(
            tick.eq(tick + 1),
            If(hs_edge, hs_n.eq(hs_n + 1)),
            If(vs_edge, vs_n.eq(vs_n + 1)),
            If(dot, dot_n.eq(dot_n + 1)),
        )
        self.comb += self.meas_htotal.eq(self.cfg_htotal)

        # フレームの組: 行 0 が始まる HS から次の行 0 の HS まで。
        # 直近2フレームの合計を出し、同じ値が続いたときだけ更新する
        # (TvpCapture の meas_frame_* と同じ意味。MEAS_FRAME_VSYNCS = 2)
        fclk = Signal(max=frame_timeout + 1)
        prev_clk = Signal(max=frame_timeout + 1); prev_lines = Signal(16)
        last_pair = Signal(16)
        cur_lines = Signal(16)
        pair_clk = Signal(32); pair_lines = Signal(16)
        self.comb += [
            cur_lines.eq(line + 1),
            pair_clk.eq(prev_clk + fclk),
            pair_lines.eq(prev_lines + cur_lines),
        ]
        self.sync += [
            self.meas_frame_new.eq(0),
            If(hs_edge & vs_now,
                fclk.eq(1),
                prev_clk.eq(fclk),
                prev_lines.eq(cur_lines),
                self.meas_vtotal.eq(cur_lines),
                last_pair.eq(pair_lines),
                If((pair_lines == last_pair) & (prev_lines == cur_lines)
                   & (prev_lines != 0),
                    self.meas_vtotal_stable.eq(cur_lines),
                    self.meas_frame_clocks.eq(pair_clk),
                    self.meas_frame_lines.eq(pair_lines),
                    If(self.meas_frame_lines != pair_lines,
                       self.meas_frame_new.eq(1)),
                ),
            ).Elif(fclk >= frame_timeout,
                # VS が来なくなったら組を消す(受信側は1秒窓の値に戻る)
                self.meas_frame_clocks.eq(0),
                self.meas_frame_lines.eq(0),
                prev_lines.eq(0),
            ).Else(fclk.eq(fclk + 1)),
        ]
        vact = Signal(16)
        self.comb += [
            vact.eq(Mux(self.meas_vtotal_stable > (height >> 1),
                        height >> 1, self.meas_vtotal_stable)),
            self.out_vactive.eq(Cat(C(0, 1), vact)),
        ]


class CaptureSel(Module):
    """TvpCapture と DigitalRgbCapture を `sel` で切り替えて送出部へ渡す。

    送出部が読むラインと MODE の口だけを切り替え、それ以外(TVP の診断・設定
    `cfg_*` / `stat_*`)は TvpCapture のものをそのまま見せる。
    sel = 1 でデジタルRGB(CONFIG 0x003A video_src)。

    ★**選ばれていない側のラインは pop しない**ので、その側のメタ FIFO は
      満杯になってラインを捨て続ける(どちらも捨てたラインを数えるだけで害は無い)。
    """

    OUTS = ("line_valid", "line_row", "line_frame", "line_field", "line_face",
            "line_ts", "line_first", "line_last", "rd_data", "il_slot1",
            "out_vactive", "meas_dotclk", "meas_hfreq", "meas_vfreq",
            "meas_htotal", "meas_vtotal", "meas_vtotal_stable",
            "meas_frame_clocks", "meas_frame_lines", "meas_frame_new")

    def __init__(self, tvp, drgb, sel):
        self._tvp = tvp
        self.width = tvp.width
        self.height = tvp.height
        for n in self.OUTS:
            t = getattr(tvp, n)
            o = Signal(len(t), name="sel_" + n)
            setattr(self, n, o)
            self.comb += o.eq(Mux(sel, getattr(drgb, n), t))
        self.line_ack = Signal()
        self.rd_face = Signal(len(tvp.rd_face))
        self.rd_word = Signal(len(tvp.rd_word))
        self.comb += [
            tvp.line_ack.eq(self.line_ack & ~sel),
            drgb.line_ack.eq(self.line_ack & sel),
            tvp.rd_face.eq(self.rd_face), drgb.rd_face.eq(self.rd_face),
            tvp.rd_word.eq(self.rd_word), drgb.rd_word.eq(self.rd_word),
        ]

    def __getattr__(self, name):
        try:
            return Module.__getattr__(self, name)
        except AttributeError:
            if name.startswith("_"):
                raise
            return getattr(self.__dict__["_tvp"], name)
