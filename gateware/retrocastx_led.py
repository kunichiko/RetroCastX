"""RetroCastX のシリアルLED(WS2812 互換、XL-2020RGBC)を駆動する。

基板上のLEDは2本の独立したチェーンになっている(hardware/adc-frontend/main.ato):

- メイン基板の状態LED ×1 … SO-DIMM 54(M1)。デバッグ端子 dbg01 と共用
- 小亀基板のLED ×N   … SO-DIMM 154(A2)。FFC 経由。個数は CONFIG で変えられる

状態LEDは**ゲートウェアが自分で色を決める**(Viewer が居なくても意味を持つ
必要があるので)。小亀のLEDは**5つの効果(消灯・固定色・点滅・呼吸・
レインボー)をゲートウェアが作る**。Viewer は効果の種類・色・周期・明るさを
CONFIG で書くだけ(KokameLeds の説明)。

★以前はキーフレーム表を Viewer が書き、FPGA が補間しながら再生していた
  (どんな効果でも作れる汎用の仕組み)。約 900 LUT と EBR 2個を使っていたので、
  ロジック使用率を下げるため 2026-11-23 に置き換えた。

**Viewer が居ないあいだ**(映像を送っていないあいだ)の動作は、表とは別に
ゲートウェアが決める(居ないのだから Viewer は指示できない)。値は EEPROM の
個体設定ページに置き、電源投入直後から効く(retrocastx_i2c.py、CONFIG 0x97):

- 0 = そのまま光らせる(既定)
- 1 = 呼吸。明るさを約6秒周期でゆっくり上下させる(山は通常の明るさの1/4)
- 2 = 消灯

呼吸は**明るさだけを変え、表の再生は止めない**。固定色ならその色のまま、
レインボーなら色を回しながら明滅する。
"""
from migen import *

from litex.gen import LiteXModule


# CONFIG キー(target=0)。protocol-v0.md と揃えること
KEY_KLED_COUNT  = 0x0090   # 小亀のLEDの個数(1〜8)
KEY_KLED_BRIGHT = 0x0093   # 小亀のLEDの明るさ(0〜255)
KEY_STAT_BRIGHT = 0x0094   # 状態LEDの明るさ(0〜255)
KEY_STAT_STATE  = 0x0095   # 状態LEDの状態(読み取り専用)0=赤 1=橙 2=青
KEY_KLED_EFFECT = 0x0098   # 効果 0=消灯 1=固定色 2=点滅 3=呼吸 4=レインボー
KEY_KLED_COLOR  = 0x0099   # 色 0x00RRGGBB(固定色・点滅・呼吸)
KEY_KLED_RATE   = 0x009A   # 1ms ごとに16bitの位相へ足す量 = 65536 / 周期[ms]
KEY_KLED_SPREAD = 0x009B   # レインボーで LED ごとにずらす色相(16bit で1周)
MAX_LEDS        = 8
EFF_OFF, EFF_SOLID, EFF_BLINK, EFF_BREATHE, EFF_RAINBOW = range(5)

# 状態LEDの色(明るさを掛ける前)
# 「Viewer が居ないあいだ」の動作(CONFIG 0x97 / EEPROM の個体設定ページ 0x09)
IDLE_KEEP, IDLE_BREATHE, IDLE_OFF = 0, 1, 2
# 呼吸の1段[ms]。0→255→0 の 510段で1周 = 約6秒
BREATHE_STEP_MS = 12

STATE_COLORS = {
    0: 0xFF0000,   # 赤: Viewer から何も来ていない
    1: 0xFF5000,   # 橙: Viewer が見つけているが受信していない
    2: 0x0000FF,   # 青: 映像・音声を送信中
}


class Ws2812Chain(LiteXModule):
    """WS2812 の1本線へ `colors[0:n]` を送り続ける。

    `start` で1回送る(busy の間は無視)。色は 0x00RRGGBB で受け、線上では
    GRB の順で MSB から出す。1ビット 1.25µs、H 幅 0=0.35µs / 1=0.75µs。
    フレームの後に 300µs の Low(リセット)を入れてから busy を落とす。

    with_mem=True なら色は `colors` ではなく小さな LUT RAM に置き、
    `wr_adr`/`wr_dat`/`wr_we` で書く。★LED 8個ぶんをレジスタで持つと 192 個の
    FF と、送るLEDを選ぶ 24bit の8→1選択が要り、小亀の LED だけで LUT 約1,000
    を食っていた(配線の混雑で配置が収束しなくなった一因)。
    """
    def __init__(self, pad, sys_clk_freq, n_max, with_mem=False):
        self.n      = Signal(max=n_max + 1, reset=n_max)
        self.start  = Signal()
        self.busy   = Signal()

        # # #

        t_bit = int(round(sys_clk_freq * 1.25e-6))
        t_0h  = int(round(sys_clk_freq * 0.35e-6))
        t_1h  = int(round(sys_clk_freq * 0.75e-6))
        t_rst = int(sys_clk_freq * 300e-6)

        idx   = Signal(max=max(n_max, 2))
        bit   = Signal(5)               # 23 → 0
        cnt   = Signal(max=t_rst + 1)
        shreg = Signal(24)
        out   = Signal()
        grb = lambda c: Cat(c[0:8], c[16:24], c[8:16])   # → [23:16]=G [15:8]=R [7:0]=B
        cur = Signal(24)
        if with_mem:
            self.wr_adr = Signal(max=max(n_max, 2))
            self.wr_dat = Signal(24)
            self.wr_we  = Signal()
            self.specials.mem = mem = Memory(24, n_max)
            wp = mem.get_port(write_capable=True)
            rp = mem.get_port(async_read=True)
            self.specials += wp, rp
            self.comb += [
                wp.adr.eq(self.wr_adr), wp.dat_w.eq(self.wr_dat), wp.we.eq(self.wr_we),
                rp.adr.eq(idx), cur.eq(grb(rp.dat_r)),
            ]
        else:
            self.colors = [Signal(24, name=f"color{i}") for i in range(n_max)]
            self.comb += Case(idx, {i: cur.eq(grb(c)) for i, c in enumerate(self.colors)})
        self.sync += pad.eq(out)        # 出力はレジスタから出す(グリッチを外へ出さない)

        self.fsm = fsm = FSM(reset_state="IDLE")
        fsm.act("IDLE",
            If(self.start & (self.n != 0),
                NextValue(idx, 0), NextState("LOAD")))
        fsm.act("LOAD", self.busy.eq(1),
            NextValue(shreg, cur), NextValue(bit, 23), NextValue(cnt, 0),
            NextState("BIT"))
        fsm.act("BIT", self.busy.eq(1),
            out.eq(cnt < Mux(shreg[23], t_1h, t_0h)),
            NextValue(cnt, cnt + 1),
            If(cnt == t_bit - 1,
                NextValue(cnt, 0),
                NextValue(shreg, shreg << 1),
                If(bit == 0,
                    If(idx == self.n - 1,
                        NextState("RESET"),
                    ).Else(
                        NextValue(idx, idx + 1), NextState("LOAD"),
                    ),
                ).Else(NextValue(bit, bit - 1)),
            ))
        fsm.act("RESET", self.busy.eq(1),
            NextValue(cnt, cnt + 1),
            If(cnt == t_rst, NextState("IDLE")))


class SerialMul(LiteXModule):
    """8bit × 9bit の掛け算を1ビットずつ(9サイクル)で行う。

    ★**ハードの乗算器(MULT18X18D)を使わないため。** 色の計算は10msに1回で
    速さは要らない。一方で乗算器は配置が固定のブロックなので、sys の論理を
    そこまで引き回すと配置が変わり、隣の eth_rx(LiteEth の CDC FIFO。配線律速で
    もともと余裕が無い)が閉じなくなった(LED 追加後 13シード全滅、2026-11-07)。
    """
    def __init__(self):
        self.x     = Signal(8)
        self.y     = Signal(9)
        self.start = Signal()
        self.busy  = Signal()
        self.p     = Signal(17)

        # # #

        xs  = Signal(17)
        ys  = Signal(9)
        cnt = Signal(4)
        self.comb += self.busy.eq(cnt != 0)
        self.sync += [
            If(self.start,
                self.p.eq(0), xs.eq(self.x), ys.eq(self.y), cnt.eq(9),
            ).Elif(cnt != 0,
                If(ys[0], self.p.eq(self.p + xs)),
                xs.eq(xs << 1), ys.eq(ys >> 1), cnt.eq(cnt - 1),
            ),
        ]


class StatusLed(LiteXModule):
    """メイン基板の状態LED。色は接続状態から決める。"""
    def __init__(self, pad, sys_clk_freq, period_cycles=None):
        self.host_seen = Signal()   # 10秒以内に Viewer から SUBSCRIBE/CONFIG が来た
        self.streaming = Signal()   # 本購読中(映像・音声を送っている)
        self.bright    = Signal(8, reset=32)
        self.state     = Signal(2)

        # # #

        self.chain = chain = Ws2812Chain(pad, sys_clk_freq, 1)
        self.mul = mul = SerialMul()
        self.comb += self.state.eq(Mux(self.streaming, 2, Mux(self.host_seen, 1, 0)))
        base = Signal(24)
        self.comb += Case(self.state, {s: base.eq(c) for s, c in STATE_COLORS.items()})
        # 20ms ごとに 色×明るさ をチャンネルごとに計算して送り直す。WS2812 は最後に
        # 受けた色を保持するので、送り直すのは色が変わったときに追従するためと、
        # ノイズで化けたときに戻すため
        period = period_cycles or int(sys_clk_freq * 20e-3)
        tick = Signal(max=period + 1)
        j = Signal(2)
        self.comb += [
            mul.x.eq(Mux(j == 0, base[0:8], Mux(j == 1, base[8:16], base[16:24]))),
            mul.y.eq(self.bright + 1),
        ]
        self.fsm = fsm = FSM(reset_state="WAIT")
        fsm.act("WAIT",
            # j を 0 に戻してから次の状態で始める(同じサイクルで始めると
            # 掛け算が前回の j のまま被乗数を取り込む)
            If(tick == period,
                NextValue(tick, 0), NextValue(j, 0), NextState("NEXT"),
            ).Else(NextValue(tick, tick + 1)))
        fsm.act("MUL",
            If(~mul.busy & ~mul.start,
                *[If(j == k, NextValue(chain.colors[0][8*k:8*k + 8], mul.p[8:16]))
                  for k in range(3)],
                If(j == 2,
                    NextState("SEND"),
                ).Else(NextValue(j, j + 1), NextState("NEXT"))))
        fsm.act("NEXT", mul.start.eq(1), NextState("MUL"))
        fsm.act("SEND", chain.start.eq(1), NextState("WAIT"))


class KokameLeds(LiteXModule):
    """小亀基板のLED。5つの効果(消灯・固定色・点滅・呼吸・レインボー)を自分で作る。

    ★**以前はキーフレーム表(512語、EBR 2個)を補間しながら再生していた**
      (2026-11-23 まで)。どんな効果でも作れる汎用の仕組みだったが、Viewer が
      使うのはこの5つだけで、表の読み出し・保持・フェード・2フレームの補間で
      約 900 LUT を使っていた。ロジック使用率を下げるため、効果を直接作る形に
      置き換えた。

    時間は **16bit の位相**で数える。1ms ごとに `rate` を足し、1周 = 65536。
    rate は Viewer が `65536 / 周期[ms]` で計算して書く(FPGA で割らない)。

    - 点滅: 位相の前半だけ光る
    - 呼吸: 位相から三角波(0〜255〜0)を作り、2乗(ガンマ2)して明るさに掛ける
    - レインボー: 色相 = 位相 + LED番号 × spread。色相を6区間に分け、原色と
      二次色の間を直線でつなぐ(彩度最大の HSV の一周と同じ)
    - 固定色・消灯: 色をそのまま / 0

    Viewer が居ないあいだの呼吸(idle_mode)は、効果より優先して明るさだけを変える
    (山は通常の明るさの1/4)。色は効果のまま。
    """
    def __init__(self, pad, sys_clk_freq, n_max=MAX_LEDS, ms_cycles=None,
                 breathe_step_ms=BREATHE_STEP_MS):
        # ms_cycles: 1ms 相当のサイクル数。シミュレーションで時間を縮めるためだけの口
        # (breathe_step_ms も同じ。待機中の呼吸の1周を短くする)
        # CONFIG から(既定は電源投入時のレインボー。Viewer の LedSettings の既定と同じ)
        self.n_led  = Signal(4, reset=4)
        self.effect = Signal(3, reset=EFF_RAINBOW)
        self.color  = Signal(24, reset=0x0080FF)
        self.rate   = Signal(16, reset=65536 // 4000)          # 周期 4秒
        self.spread = Signal(16, reset=65536 // 2 // 4)        # 4個で半周
        self.bright = Signal(8, reset=64)
        # Viewer が居ないあいだの動作。streaming が 0 の間だけ効く
        self.streaming = Signal()
        self.idle_mode = Signal(2)

        # # #

        self.chain = chain = Ws2812Chain(pad, sys_clk_freq, n_max, with_mem=True)
        n_led = Signal(4)
        self.comb += [
            n_led.eq(Mux(self.n_led == 0, 1, Mux(self.n_led > n_max, n_max, self.n_led))),
            chain.n.eq(n_led),
        ]

        # --- 時間の進行(1ms 刻み)---
        ms_div = ms_cycles or int(sys_clk_freq / 1000)
        ms_cnt = Signal(max=ms_div)
        ms     = Signal()
        phase  = Signal(16)
        refresh = Signal(4)              # 10ms ごとに LED へ送る
        self.sync += [
            ms.eq(0), ms_cnt.eq(ms_cnt + 1),
            If(ms_cnt == ms_div - 1, ms_cnt.eq(0), ms.eq(1)),
            If(ms,
                phase.eq(phase + self.rate),
                refresh.eq(Mux(refresh == 9, 0, refresh + 1)),
            ),
        ]

        # --- Viewer が居ないあいだの呼吸(三角波を breathe_step_ms ごとに1段)---
        idle  = Signal()
        idle_br = Signal()
        idle_off = Signal()
        tri   = Signal(8, reset=255)
        down  = Signal(reset=1)
        b_cnt = Signal(max=max(breathe_step_ms, 2))
        self.comb += [
            idle.eq(~self.streaming),
            idle_br.eq(idle & (self.idle_mode == IDLE_BREATHE)),
            idle_off.eq(idle & (self.idle_mode == IDLE_OFF)),
        ]
        self.sync += [
            # 映像を送っている間は山で待ち、居なくなったら山から下り始める
            If(~idle_br,
                tri.eq(255), down.eq(1), b_cnt.eq(0),
            ).Elif(ms,
                b_cnt.eq(Mux(b_cnt == breathe_step_ms - 1, 0, b_cnt + 1)),
                If(b_cnt == breathe_step_ms - 1,
                    If(down,
                        If(tri == 0, down.eq(0)).Else(tri.eq(tri - 1)),
                    ).Else(
                        If(tri == 255, down.eq(1)).Else(tri.eq(tri + 1)),
                    ),
                ),
            ),
        ]
        # 効果の呼吸の三角波は位相から作る(前半で上り、後半で下る)
        etri = Signal(8)
        self.comb += etri.eq(Mux(phase[15], ~phase[7:15], phase[7:15]))

        # --- 色相 → いま掛け算するチャンネルの値(レインボー)---
        #
        # 6区間それぞれで、各チャンネルは 0 / 255 / 上り(fr)/ 下り(~fr)のどれか:
        #
        #   区間      0 赤→黄  1 黄→緑  2 緑→シアン  3 シアン→青  4 青→マゼンタ  5 マゼンタ→赤
        #   R          255      下り       0           0           上り          255
        #   G          上り     255        255         下り        0             0
        #   B          0        0          上り        255         255           下り
        #
        # ★**24bit の色を作らず、掛け算するチャンネルの 8bit だけを選ぶ。** 24bit の
        #   6択とそれを置くレジスタで約 170 LUT を使っていた
        hue = Signal(16)
        h6  = Signal(19)
        seg = Signal(3)
        fr  = Signal(8)
        hsel = Signal(2)                 # 0=0 1=255 2=上り 3=下り
        hch = Signal(8)
        ch  = Signal(2)                  # 0=B 1=G 2=R
        Z, F, U, D = 0, 1, 2, 3
        tbl = {0: (Z, U, F), 1: (Z, F, D), 2: (U, F, Z),     # (B, G, R)
               3: (F, D, Z), 4: (F, Z, U), 5: (D, Z, F)}
        self.comb += [
            h6.eq(Cat(C(0, 1), hue) + Cat(C(0, 2), hue)),      # hue × 6
            seg.eq(h6[16:19]), fr.eq(h6[8:16]),
            Case(Cat(ch, seg), {(sg << 2) | c: hsel.eq(v[c])
                                for sg, v in tbl.items() for c in range(3)}),
            hch.eq(Mux(hsel[1], Mux(hsel[0], ~fr, fr), Mux(hsel[0], 255, 0))),
        ]

        # --- 明るさと色の計算(SerialMul を順番に使う)---
        #   SQ : tri × tri / 256 → sq
        #   LV : sq × (明るさ+1) → level(待機中の呼吸は /1024 で山を1/4に)
        #   各LED・各チャンネル: 色 × (level+1) / 256
        self.mul = mul = SerialMul()
        sq    = Signal(8)
        level = Signal(8)
        i     = Signal(4)
        out   = Signal(24)
        src   = Signal(8)
        self.comb += src.eq(Mux(idle_br, tri, etri))
        sel = lambda w: Mux(ch == 0, w[0:8], Mux(ch == 1, w[8:16], w[16:24]))
        self.fsm = fsm = FSM(reset_state="IDLE")
        fsm.act("IDLE",
            If(ms & (refresh == 0) & ~chain.busy, NextState("SQ")))
        fsm.act("SQ", mul.start.eq(1), NextState("SQ_W"))
        fsm.act("SQ_W",
            If(~mul.busy & ~mul.start, NextValue(sq, mul.p[8:16]), NextState("LV")))
        fsm.act("LV", mul.start.eq(1), NextState("LV_W"))
        fsm.act("LV_W",
            If(~mul.busy & ~mul.start,
                If(idle_br,
                    NextValue(level, mul.p[10:17]),
                ).Elif(idle_off | (self.effect == EFF_OFF),
                    NextValue(level, 0),
                ).Elif(self.effect == EFF_BREATHE,
                    NextValue(level, mul.p[8:16]),
                ).Elif((self.effect == EFF_BLINK) & phase[15],
                    NextValue(level, 0),
                ).Else(
                    NextValue(level, self.bright),
                ),
                NextValue(i, 0), NextValue(hue, phase), NextValue(ch, 0),
                NextState("MSTART")))
        # 掛け算の入力は状態で選ぶ(代入は1か所にまとめる)
        in_sq = Signal(); in_lv = Signal()
        self.comb += [
            in_sq.eq(fsm.ongoing("SQ")), in_lv.eq(fsm.ongoing("LV")),
            mul.x.eq(Mux(in_sq, src, Mux(in_lv, sq,
                         Mux(self.effect == EFF_RAINBOW, hch, sel(self.color))))),
            mul.y.eq(Mux(in_sq, src, Mux(in_lv, self.bright + 1, level + 1))),
        ]
        fsm.act("MSTART", mul.start.eq(1), NextState("MWAIT"))
        fsm.act("MWAIT",
            If(~mul.busy & ~mul.start,
                *[If(ch == k, NextValue(out[8*k:8*k + 8], mul.p[8:16]))
                  for k in range(3)],
                If(ch == 2,
                    NextState("STORE"),
                ).Else(
                    NextValue(ch, ch + 1), NextState("MSTART"),
                )))
        self.comb += [
            chain.wr_adr.eq(i),
            chain.wr_dat.eq(out),
        ]
        fsm.act("STORE",
            chain.wr_we.eq(1),
            If(i == n_led - 1,
                NextState("SEND"),
            ).Else(
                NextValue(i, i + 1), NextValue(hue, hue + self.spread),
                NextValue(ch, 0), NextState("MSTART"),
            ))
        fsm.act("SEND", chain.start.eq(1), NextState("IDLE"))
