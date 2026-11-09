"""RetroCastX のシリアルLED(WS2812 互換、XL-2020RGBC)を駆動する。

基板上のLEDは2本の独立したチェーンになっている(hardware/adc-frontend/main.ato):

- メイン基板の状態LED ×1 … SO-DIMM 54(M1)。デバッグ端子 dbg01 と共用
- 小亀基板のLED ×N   … SO-DIMM 154(A2)。FFC 経由。個数は CONFIG で変えられる

状態LEDは**ゲートウェアが自分で色を決める**(Viewer が居なくても意味を持つ
必要があるので)。小亀のLEDは**キーフレーム表を再生する**。表は Viewer が
CONFIG で書き込み、FPGA は補間しながらループ再生する。固定色・点滅・呼吸・
レインボーはすべて Viewer 側で表として作る(新しい効果のためにゲートウェアを
更新しなくて済むように)。

表の形式(1語=32bit、バンクごとに256語):

    フレーム k の先頭 = k × (1 + n_led)
      +0          ヘッダ: [15:0]=保持[ms] / [31:16]=フェード速度
      +1 .. +n    LED i の色 0x00RRGGBB

- 保持のあと次フレームへフェードする。フェード速度は「1ms ごとに 16bit の
  位相へ足す量」で、65536 / フェード時間[ms]。**割り算は Viewer がやる**
  (FPGA で割らないため)。0 ならフェードせずに切り替える
- 最後のフレームの次は先頭へ戻る(ループ)

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
KEY_KLED_FRAMES = 0x0091   # 再生するフレーム数(0=消灯)
KEY_KLED_BANK   = 0x0092   # 再生するバンク(0/1)。書き換えると先頭から再生し直す
KEY_KLED_BRIGHT = 0x0093   # 小亀のLEDの明るさ(0〜255)
KEY_STAT_BRIGHT = 0x0094   # 状態LEDの明るさ(0〜255)
KEY_STAT_STATE  = 0x0095   # 状態LEDの状態(読み取り専用)0=赤 1=橙 2=青
KEY_TABLE_BASE  = 0x0400   # 表の語。0x0400〜0x05FF(バンク0=0x0400〜、バンク1=0x0500〜)
TABLE_WORDS     = 512
MAX_LEDS        = 8

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


def rainbow_table(n_led=4, fade_ms=600):
    """電源投入時の既定パターン(バンク0)。6色を LED ごとにずらして回す。

    原色と二次色の間を RGB で線形補間すると、彩度最大の HSV の色相を一周するのと
    一致する。だから6フレームで滑らかなレインボーになる。
    """
    palette = [0xFF0000, 0xFFFF00, 0x00FF00, 0x00FFFF, 0x0000FF, 0xFF00FF]
    rate = 65536 // fade_ms
    words = []
    for k in range(len(palette)):
        words.append(rate << 16)   # 保持 0ms、すぐにフェード
        words += [palette[(k + i) % len(palette)] for i in range(n_led)]
    return words, len(palette)


class Ws2812Chain(LiteXModule):
    """WS2812 の1本線へ `colors[0:n]` を送り続ける。

    `start` で1回送る(busy の間は無視)。色は 0x00RRGGBB で受け、線上では
    GRB の順で MSB から出す。1ビット 1.25µs、H 幅 0=0.35µs / 1=0.75µs。
    フレームの後に 300µs の Low(リセット)を入れてから busy を落とす。
    """
    def __init__(self, pad, sys_clk_freq, n_max):
        self.colors = [Signal(24, name=f"color{i}") for i in range(n_max)]
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


class KeyframeLeds(LiteXModule):
    """小亀基板のLED。キーフレーム表を補間しながらループ再生する。"""
    def __init__(self, pad, sys_clk_freq, n_max=MAX_LEDS, ms_cycles=None,
                 breathe_step_ms=BREATHE_STEP_MS):
        # ms_cycles: 1ms 相当のサイクル数。シミュレーションで時間を縮めるためだけの口
        # (breathe_step_ms も同じ。呼吸の1周を短くする)
        init, n_frames = rainbow_table(4)
        # CONFIG から
        self.n_led   = Signal(4, reset=4)
        self.frames  = Signal(8, reset=n_frames)
        self.bank    = Signal()
        self.bright  = Signal(8, reset=64)
        # Viewer が居ないあいだの動作。streaming が 0 の間だけ効く
        self.streaming = Signal()
        self.idle_mode = Signal(2)
        # 表の書き込み/読み返し(CONFIG のキー 0x0400〜)
        self.tbl_adr   = Signal(9)
        self.tbl_we    = Signal()
        self.tbl_dat_w = Signal(32)
        self.tbl_dat_r = Signal(32)

        # # #

        self.specials.mem = mem = Memory(32, TABLE_WORDS, init=init)
        wp = mem.get_port(write_capable=True)
        rp = mem.get_port()
        self.specials += wp, rp
        self.comb += [
            wp.adr.eq(self.tbl_adr), wp.we.eq(self.tbl_we),
            wp.dat_w.eq(self.tbl_dat_w), self.tbl_dat_r.eq(wp.dat_r),
        ]

        self.chain = chain = Ws2812Chain(pad, sys_clk_freq, n_max)
        n_led = Signal(4)
        self.comb += [
            n_led.eq(Mux(self.n_led == 0, 1, Mux(self.n_led > n_max, n_max, self.n_led))),
            chain.n.eq(n_led),
        ]

        # --- 時間の進行(1ms 刻み)---
        ms_div = ms_cycles or int(sys_clk_freq / 1000)
        ms_cnt = Signal(max=ms_div)
        ms     = Signal()
        self.sync += [
            ms.eq(0), ms_cnt.eq(ms_cnt + 1),
            If(ms_cnt == ms_div - 1, ms_cnt.eq(0), ms.eq(1)),
        ]
        refresh = Signal(4)              # 10ms ごとに LED へ送る

        # --- Viewer が居ないあいだの明るさ ---
        # 表の色に掛ける明るさ `level` を、ここで毎回作り直しておく。呼吸は
        # 三角波 tri(0〜255)を2乗して(ガンマ2)、明るさの1/4を山にする。
        # 掛け算は色の計算とは別の SerialMul で行う(どちらも急がない)
        idle  = Signal()
        off   = Signal()
        level = Signal(8)
        tri   = Signal(8, reset=255)
        down  = Signal(reset=1)
        b_cnt = Signal(max=max(breathe_step_ms, 2))
        sq    = Signal(8)
        b_lv  = Signal(8)
        self.comb += idle.eq(~self.streaming &
                             ((self.idle_mode == IDLE_BREATHE) | (self.idle_mode == IDLE_OFF)))
        self.sync += [
            # 映像を送っている間は山で待ち、居なくなったら山から下り始める
            If(~idle,
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
            off.eq(idle & (self.idle_mode == IDLE_OFF)),
            # ★Mux の選択に ~idle を使わない。Migen のシミュレータは 1bit の ~ を
            #   -2(真)と評価し、居ないあいだも通常の明るさのままに見えた
            level.eq(Mux(idle, Mux(self.idle_mode == IDLE_BREATHE, b_lv, 0),
                         self.bright)),
        ]
        self.bmul = bmul = SerialMul()
        self.bfsm = bfsm = FSM(reset_state="SQ")
        # tri × tri / 256 → sq、sq × (明るさ+1) / 1024 → b_lv(山は明るさの1/4)
        bfsm.act("SQ",
            bmul.x.eq(tri), bmul.y.eq(tri), bmul.start.eq(1),
            NextState("SQ_W"))
        bfsm.act("SQ_W",
            If(~bmul.busy, NextValue(sq, bmul.p[8:16]), NextState("LV")))
        bfsm.act("LV",
            bmul.x.eq(sq), bmul.y.eq(self.bright + 1), bmul.start.eq(1),
            NextState("LV_W"))
        bfsm.act("LV_W",
            If(~bmul.busy, NextValue(b_lv, bmul.p[10:17]), NextState("SQ")))

        stride  = Signal(5)
        self.comb += stride.eq(n_led + 1)
        f       = Signal(8)              # いまのフレーム番号
        ptr     = Signal(8)              # その先頭語(バンク内)
        nptr    = Signal(8)              # 次のフレームの先頭語
        last    = Signal()
        self.comb += [
            last.eq(f + 1 >= self.frames),
            nptr.eq(Mux(last, 0, ptr + stride)),
        ]
        hold    = Signal(16)
        rate    = Signal(16)
        elapsed = Signal(16)
        fading  = Signal()
        frac    = Signal(16)
        i       = Signal(4)
        a       = Signal(24)
        b       = Signal(24)
        ch      = Signal(2)              # いま計算しているチャンネル(0=B 1=G 2=R)
        ph      = Signal(2)
        p1      = Signal(17)
        mixc    = Signal(8)
        out     = Signal(24)
        self.mul = mul = SerialMul()
        # 表の構成が変わったら先頭から読み直す
        cfg_now  = Signal(14)
        cfg_prev = Signal(14)
        self.comb += cfg_now.eq(Cat(self.frames, self.bank, n_led))

        def rd(addr):
            return rp.adr.eq(Cat(addr[0:8], self.bank))

        t = Signal(8)
        self.comb += t.eq(Mux(fading, frac[8:16], 0))
        sel = lambda w: Mux(ch == 0, w[0:8], Mux(ch == 1, w[8:16], w[16:24]))
        self.comb += [
            mul.x.eq(Mux(ph == 0, sel(a), Mux(ph == 1, sel(b), mixc))),
            mul.y.eq(Mux(ph == 0, 256 - t, Mux(ph == 1, t, level + 1))),
        ]

        self.fsm = fsm = FSM(reset_state="RESTART")
        fsm.act("RESTART",
            NextValue(cfg_prev, cfg_now),
            NextValue(f, 0), NextValue(ptr, 0),
            NextState("HDR_RD"))
        fsm.act("HDR_RD", rd(ptr), NextState("HDR"))
        fsm.act("HDR", rd(ptr),
            NextValue(hold, rp.dat_r[0:16]), NextValue(rate, rp.dat_r[16:32]),
            NextValue(elapsed, 0), NextValue(fading, 0), NextValue(frac, 0),
            NextState("IDLE"))
        fsm.act("IDLE",
            If(cfg_now != cfg_prev,
                NextState("RESTART"),
            ).Elif(ms,
                NextValue(refresh, Mux(refresh == 9, 0, refresh + 1)),
                NextState("STEP")))
        fsm.act("STEP",
            If(self.frames == 0,
                NextState("PAINT"),
            ).Elif(~fading,
                If(elapsed >= hold,
                    If(rate == 0,
                        NextState("ADVANCE"),
                    ).Else(
                        NextValue(fading, 1), NextValue(frac, 0), NextState("PAINT"),
                    ),
                ).Else(
                    NextValue(elapsed, elapsed + 1), NextState("PAINT"),
                ),
            ).Else(
                If(frac + rate >= 0x10000,
                    NextState("ADVANCE"),
                ).Else(
                    NextValue(frac, frac + rate), NextState("PAINT"),
                ),
            ))
        fsm.act("ADVANCE",
            NextValue(f, Mux(last, 0, f + 1)), NextValue(ptr, nptr),
            NextState("ADV_RD"))
        fsm.act("ADV_RD", rd(ptr), NextState("ADV_HDR"))
        fsm.act("ADV_HDR", rd(ptr),
            NextValue(hold, rp.dat_r[0:16]), NextValue(rate, rp.dat_r[16:32]),
            NextValue(elapsed, 0), NextValue(fading, 0), NextValue(frac, 0),
            NextState("PAINT"))
        # 10ms に1回、送信器が空いていれば全LEDの色を作って送る
        fsm.act("PAINT",
            If((refresh == 0) & ~chain.busy,
                NextValue(i, 0), NextState("A_RD"),
            ).Else(NextState("IDLE")))
        fsm.act("A_RD", rd(ptr + 1 + i), NextState("A"))
        fsm.act("A", rd(nptr + 1 + i),
            NextValue(a, rp.dat_r), NextState("B"))
        fsm.act("B", NextValue(b, rp.dat_r),
            NextValue(ch, 0), NextValue(ph, 0), NextState("MSTART"))
        # 1チャンネルにつき掛け算3回(SerialMul を順番に使う):
        #   ph0: a×(256−t)  ph1: b×t → mix = 和/256  ph2: mix×(level+1)/256
        # 重みの和が256なので mix は a と b の間に収まり、符号付きの演算が要らない
        fsm.act("MSTART", mul.start.eq(1), NextState("MWAIT"))
        fsm.act("MWAIT",
            If(~mul.busy,
                If(ph == 0,
                    NextValue(p1, mul.p), NextValue(ph, 1), NextState("MSTART"),
                ).Elif(ph == 1,
                    NextValue(mixc, (p1 + mul.p)[8:16]), NextValue(ph, 2),
                    NextState("MSTART"),
                ).Else(
                    *[If(ch == k, NextValue(out[8*k:8*k + 8], mul.p[8:16]))
                      for k in range(3)],
                    If(ch == 2,
                        NextState("STORE"),
                    ).Else(
                        NextValue(ch, ch + 1), NextValue(ph, 0), NextState("MSTART"),
                    ),
                ),
            ))
        fsm.act("STORE",
            *[If(i == k, NextValue(chain.colors[k], Mux((self.frames == 0) | off, 0, out)))
              for k in range(n_max)],
            If(i == n_led - 1,
                NextState("SEND"),
            ).Else(
                NextValue(i, i + 1), NextState("A_RD"),
            ))
        fsm.act("SEND", chain.start.eq(1), NextState("IDLE"))
