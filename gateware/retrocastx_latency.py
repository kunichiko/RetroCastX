#!/usr/bin/env python3
"""表示遅延の計測(フォトトランジスタ)。

ボードが映像を送り出してから、Viewer の画面(PC のモニタ)に実際に出るまでを
測る。Viewer 内の時刻(client/src/latency.rs)では測れない、OS のコンポジタと
液晶自身の遅延まで含めた**端から端まで**の値になる。

    1. 映像を黒に差し替えて送り、センサーが暗くなるのを待つ
    2. 少し(100〜165ms、毎回ばらつかせる)待ってから、次のフレームを白にする
    3. 白いフレームの最初のラインを送り始めた時刻から、センサーが光るまでを数える
    4. 1 へ戻る(止められるまで繰り返す)。min / max / 合計 / 回数を持つ

★**時刻の起点は「白いフレームの最初のラインを送り始めた瞬間」。**
  キャプチャ窓の先頭(vbp=0 なので VSYNC の直後)のラインで、ボードは
  取り込んだラインをすぐ送るので「直結のモニタがそのラインを受け取った時刻」
  とほぼ同じ。画面全体を白にするので、センサーの位置(画面の上下)によって
  液晶の走査ぶんが乗る。上端に貼れば最小になる。
★**待ちをばらつかせる。** 毎回同じ間隔で白にすると、ソースのフレームと
  PC のモニタのリフレッシュの位相がいつも同じところに来て、min と max が
  実際の幅を表さない。60Hz の4周期ぶん(65ms)を乱数で振る。
★**白/黒の切り替えはフレーム単位。** 差し替えはストリーマが行う
  (retrocastx_stream.py の `lat_*`)。ここは「次のフレームを白にしたい」と
  言うだけで、白いフレームを実際に送り始めたらストリーマから `white_go` が来る。
  ライン単位で切り替えると、どのフレームが白になったのかが曖昧になる。

センサーの回路(デバッグ端子 J7):

    pin 9 (dbg7 / R3) ──┬── フォトトランジスタのコレクタ
                       └── (FPGA 内蔵のプルアップ)
    pin 1/2 (GND) ─────── フォトトランジスタのエミッタ

  光るとコレクタが引き下げられて Low になる(既定の極性)。比較器付きの
  センサー基板のように「光ると High」のものは CONFIG 0x00A0 bit1 で反転する。

オシロ用のマーカー(デバッグ端子 J7 の pin10 / dbg8 / N4):

    計測中(白いフレームを送り始めてから、光ったと判定するまで)だけ High。
    立ち上がり = 計測の起点、立ち下がり = 判定した時刻(光り始め + 10µs)。
    ch1 をこれ(立ち上がりでトリガ)、ch2 をセンサー(pin9)にすると、液晶が
    光り始めるまでと、しきい値を切るまで(センサーの立ち下がりの速さ)が分けて読める。
    幅は計測値 + 10µs。時間切れのときは 2 秒で落ちる

CONFIG(すべて target=0):

    0xA0  制御。bit0=計測する(0→1 で集計をクリア)/ bit1=光るとHigh
    0xA1  状態(読み取り専用)。[1:0] 0=停止 1=暗転待ち 2=白待ち 3=計測中 /
          [4] センサーが光っているか(極性を反映、ばらつき除去前)/
          [5] 購読が切れたので止めた(次に bit0 を 0→1 するまで止まったまま)
    0xA2  [15:0] 計れた回数 / [31:16] 時間切れの回数(2秒光らなかった)
    0xA3  直近の値 [µs]
    0xA4  最小 [µs]
    0xA5  最大 [µs]
    0xA6  合計 [µs](平均 = 合計 / 回数)
"""

from migen import *
from migen.genlib.cdc import MultiReg


# 状態(0xA1 の [1:0])
S_OFF, S_DARK, S_ARMED, S_MEASURE = 0, 1, 2, 3


class PhotoLatency(Module):
    def __init__(self, pad, sys_clk_freq, dark_hold_us=100_000,
                 timeout_us=2_000_000, debounce_us=10, jitter_bits=16):
        # --- CONFIG ---
        self.cfg_ctl = Signal(2)
        # --- ストリーマとのやり取り ---
        self.force = Signal()       # 映像を白/黒で差し替える
        self.white_req = Signal()   # 次のフレームから白にする
        self.white_go = Signal()    # 入力: 白いフレームの最初のラインを送り始めた
        self.alive = Signal(reset=1)  # 入力: Viewer へ映像を送っている(本購読中)
        self.frame_white = Signal()   # 入力: いま送っているフレームが白か
        # --- 読み出し ---
        self.marker = Signal()        # 出力: 計測中だけ High(オシロ用、上の説明)
        self.stat_state = Signal(32)
        self.stat_count = Signal(32)
        self.stat_last = Signal(32)
        self.stat_min = Signal(32)
        self.stat_max = Signal(32)
        self.stat_sum = Signal(32)

        # µs の刻み。sys は 45MHz なので割り切れる
        div = int(sys_clk_freq // 1e6)
        assert div * 1e6 == sys_clk_freq, "sys_clk_freq は 1MHz の整数倍"
        pre = Signal(max=max(div, 2))
        tick = Signal()
        self.sync += If(pre == div - 1, pre.eq(0)).Else(pre.eq(pre + 1))
        self.comb += tick.eq(pre == div - 1)

        # --- センサー ---
        raw = Signal()
        self.specials += MultiReg(pad, raw, "sys")
        # ★**Viewer が居なくなったら止める。** 計測中に Viewer を閉じると、
        #   次に繋いだ Viewer に白黒の点滅だけが届く。購読が切れたら止め、
        #   0xA0 bit0 を一度 0 に戻すまで(次の開始まで)再開しない
        abandoned = Signal()
        self.sync += If(~self.cfg_ctl[0],
            abandoned.eq(0),
        ).Elif(~self.alive,
            abandoned.eq(1),
        )
        run = Signal()
        self.comb += run.eq(self.cfg_ctl[0] & ~abandoned)
        lit = Signal()
        self.comb += lit.eq(raw ^ ~self.cfg_ctl[1])
        # 光り続けている / 暗いままの長さ [µs]。暗い側は暗転の確認に使う
        lit_us = Signal(max=debounce_us + 1)
        jit = 2 ** jitter_bits
        dark_us = Signal(max=dark_hold_us + jit + 1)
        self.sync += If(tick,
            If(lit,
                dark_us.eq(0),
                If(lit_us != debounce_us, lit_us.eq(lit_us + 1)),
            ).Else(
                lit_us.eq(0),
                If(dark_us != dark_hold_us + jit, dark_us.eq(dark_us + 1)),
            ),
        )

        # 待ちのばらつき。16bit の LFSR を毎サイクル回し、暗転待ちに入るときに取る
        lfsr = Signal(16, reset=0xACE1)
        self.sync += lfsr.eq(Cat(lfsr[1:], lfsr[0] ^ lfsr[2] ^ lfsr[3] ^ lfsr[5]))
        wait_us = Signal(max=dark_hold_us + jit)

        state = Signal(2)
        t_us = Signal(32)        # 白を送り始めてからの時間
        t_on = Signal(32)        # 光り始めた時刻(ばらつき除去の分を戻すため)
        count = Signal(16)
        timeouts = Signal(16)
        run_d = Signal()
        res_min = Signal(32)
        res_max = Signal(32)
        res_sum = Signal(32)
        res_last = Signal(32)

        to_dark = [
            state.eq(S_DARK),
            wait_us.eq(dark_hold_us + lfsr[:jitter_bits]),
        ]
        self.sync += [
            run_d.eq(run),
            If(~run,
                state.eq(S_OFF),
            ).Elif(~run_d,
                # 開始: 集計をクリアして暗転から
                count.eq(0), timeouts.eq(0),
                res_min.eq(2 ** 32 - 1), res_max.eq(0),
                res_sum.eq(0), res_last.eq(0),
                *to_dark,
            ).Elif(state == S_DARK,
                # ★**黒いフレームを送り始めたことも条件にする。** 時間切れ
                #   (センサーが一度も光らなかった)の後はセンサーが暗いままなので、
                #   暗さだけを見ると白いフレームの途中で白待ちに戻ってしまう。
                #   白の要求がフレームの境目で一度も下りないので、黒→白の切り替えが
                #   起きず、白い画面のまま起点を待ち続けた(sim_latency で検出)
                If((dark_us >= wait_us) & ~self.frame_white,
                    state.eq(S_ARMED)),
            ).Elif(state == S_ARMED,
                # 白を出す前に光ったら(外光など)やり直す。そのまま進むと
                # 計測に入った瞬間にばらつき除去が済んでいて、古い t_on が結果になる
                If(lit_us != 0,
                    *to_dark,
                ).Elif(self.white_go, state.eq(S_MEASURE), t_us.eq(0)),
            ).Elif(state == S_MEASURE,
                If(tick, t_us.eq(t_us + 1)),
                # 光り始めた時刻を覚えておき、光り続けたらその時刻を結果にする
                If(lit_us == 0, t_on.eq(t_us)),
                If(lit_us == debounce_us,
                    count.eq(count + 1),
                    res_last.eq(t_on),
                    res_sum.eq(res_sum + t_on),
                    If(t_on < res_min, res_min.eq(t_on)),
                    If(t_on > res_max, res_max.eq(t_on)),
                    *to_dark,
                ).Elif(t_us >= timeout_us,
                    timeouts.eq(timeouts + 1),
                    *to_dark,
                ),
            ),
        ]
        self.sync += self.marker.eq(run & (state == S_MEASURE))
        self.comb += [
            self.force.eq(state != S_OFF),
            self.white_req.eq((state == S_ARMED) | (state == S_MEASURE)),
            self.stat_state.eq(Cat(state, C(0, 2), lit, abandoned)),
            self.stat_count.eq(Cat(count, timeouts)),
            self.stat_last.eq(res_last),
            # 1回も計れていないときは 0 を返す(初期値の全1を見せない)
            self.stat_min.eq(Mux(count == 0, 0, res_min)),
            self.stat_max.eq(res_max),
            self.stat_sum.eq(res_sum),
        ]
