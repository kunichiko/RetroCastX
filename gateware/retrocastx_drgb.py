#!/usr/bin/env python3
"""デジタルRGB(TTL RGBI + HS/VS)の**試験信号生成**と**信号測定**。

## なぜ生成器から作るか

デジタルRGBを出せる実機が手元に無い。基板には入力経路(J13 → ESD → プルダウン →
SN74LVC2G17 → FPGA直結、`hardware/adc-frontend/README.md`)が既にあるが、
**何も繋がずにキャプチャを書くと、動かないときに「経路が悪いのか論理が悪いのか」
が切り分けられない**。

デバッグ端子(J4、空きGPIO 20本)から擬似的な RGBI + HS/VS を出し、6本のジャンパで
J13 へ戻す。**レベル変換器とコネクタを含む本番の経路**を通るので、実機が来たときに
疑う場所が論理だけに絞れる。

★**ループバックで確かめられないこと。** 生成側と測定側が同じクロックなので、
  **送り側と受け側のクロック差(ドリフト)は試験できない**。実機は水晶が別なので
  1ライン当たりのサンプル数が少しずつ動く。そこは実機でしか出ない。

## 狙うタイミング(PC-8001 / PC-8001mkII 系)

    ドットクロック 14.31818MHz(NTSC副搬送波の4倍)
    htotal 910  →  fH = 15,734.3Hz   ← NTSC そのもの
    vtotal 262  →  fV = 60.05Hz

**PC-8001 はコンポジット出力も持つので、NTSC 準拠でないと成立しない。**
だから 910 なのは偶然ではない。

★**確度は「おそらく正しい」止まり。** 出どころは μPD3301 の実装解析(Web)で、
  **回路図やサービスマニュアルのような一次資料では確認できていない**。
  実機か回路図が手に入ったら検算すること。なお PC-8801 系は別で、実測 15.957kHz
  (NTSC非準拠)。8001 の値として 8801 のものを使わないよう注意。

## 生成側のクロックは実機と同じにしない

sys(45MHz)の**整数分周**で作る。

    15.0MHz = 45MHz / 3      htotal 953 → fH = 15,739.8Hz  (実機比 +0.03%)

★**新しいクロックドメインを作らない**のが肝。eth_rx(125MHz)のタイミングが
  配置シード次第で落ちる基板なので、クロックを増やすのは最後の手段
  (retrocastx_stream.py の --seed 参照)。整数分周ならジッタも無い。
★**ドットクロックの絶対値が 4.8% 違っても構わない。** 受け側は同期から作り直すし、
  htotal は機種ごとの設定値として外から与えるものだから、試験で 910 でなく 953 を
  使っても失うものが無い。**合わせるべきは fH の方**(測定値として画面に出る)。

## 極性は測る、決め打ちしない

デジタルRGBの HS/VS が正極性か負極性かは機種による。生成側は切り替えられるように
し、測定側は極性を**測る**(パルスは1ラインのごく一部なので、Low の割合で分かる)。
「たぶん負極性」で書いて合わなかったときに、原因が極性なのか他なのか分からなく
なるのを避ける。
"""
from migen import *

# 45MHz を 3 分周して 15.0MHz のドットクロックにする
DOT_DIV = 3
# 15.0MHz / 953 = 15,739.8Hz。実機(14.31818MHz / 910 = 15,734.3Hz)に +0.03%
HTOTAL = 953
HACTIVE = 640
VTOTAL = 262
VACTIVE = 200
HS_WIDTH = 64      # 約4.3us。実機の値は未確認
VS_WIDTH = 3
HSTART = 120
VSTART = 40
# 実機の値(検算用。生成には使わない)
REAL_DOT_HZ = 14_318_180
REAL_HTOTAL = 910


class DigitalRgbGen(Module):
    """デバッグ端子へ擬似デジタルRGBを出す。

    パターンは**2つの領域**に分ける:

      上半分  16色の縦カラーバー  → R/G/B/I の4bitが全部通るか
      下半分  1ドット幅の白黒縦縞  → ドットクロックの再生ができるか

    ★**1ドット縞を必ず入れる。** カラーバーだけだと、ドットクロックが2倍
      ずれていても「色は合っている」ように見えてしまう。細かい縞が潰れるか
      どうかが、位相とクロックが合っている唯一の証拠になる。

    寸法を引数にしてあるのは**シミュレーションを現実的な時間で回すため**。
    実寸(953×262×3 = 74万サイクル/フレーム)を Migen で回すと1フレームに
    数分かかり、試験として使えない。
    """

    def __init__(self, htotal=HTOTAL, vtotal=VTOTAL, hactive=HACTIVE,
                 vactive=VACTIVE, hstart=HSTART, vstart=VSTART,
                 hs_width=HS_WIDTH, vs_width=VS_WIDTH, dot_div=DOT_DIV):
        self.htotal, self.vtotal = htotal, vtotal
        self.hactive, self.vactive = hactive, vactive
        self.hstart, self.vstart = hstart, vstart
        self.dot_div = dot_div

        self.enable = Signal(reset=1)
        self.neg_sync = Signal(reset=1)     # 1 = アイドルHigh、パルスでLow

        self.r = Signal(); self.g = Signal(); self.b = Signal(); self.i = Signal()
        self.hs = Signal(); self.vs = Signal()
        self.x = Signal(max=htotal)
        self.y = Signal(max=vtotal)

        # # #

        dot = Signal()
        if dot_div > 1:
            div = Signal(max=dot_div)
            self.sync += If(div == dot_div - 1, div.eq(0)).Else(div.eq(div + 1))
            self.comb += dot.eq(div == dot_div - 1)
        else:
            self.comb += dot.eq(1)

        self.sync += If(dot,
            If(self.x == htotal - 1,
                self.x.eq(0),
                If(self.y == vtotal - 1, self.y.eq(0)).Else(self.y.eq(self.y + 1)),
            ).Else(self.x.eq(self.x + 1)),
        )

        hs_pulse = Signal(); vs_pulse = Signal()
        self.comb += [
            hs_pulse.eq(self.x < hs_width),
            vs_pulse.eq(self.y < vs_width),
            self.hs.eq(hs_pulse ^ self.neg_sync),
            self.vs.eq(vs_pulse ^ self.neg_sync),
        ]

        active = Signal()
        px = Signal(max=htotal)
        py = Signal(max=vtotal)
        self.comb += [
            active.eq((self.x >= hstart) & (self.x < hstart + hactive)
                      & (self.y >= vstart) & (self.y < vstart + vactive)),
            px.eq(self.x - hstart),
            py.eq(self.y - vstart),
        ]

        # ★**バー幅は2のべき乗にする。** 640/16 = 40 で割ると除算器が要る。
        #   幅をビットスライスで取れる値にして、余りは黒のままにする ─ 見たいのは
        #   色が通るかどうかで、バーの幅そのものではない。
        bar_shift = max(0, (hactive // 16).bit_length() - 1)
        bar_w = 1 << bar_shift
        self.bar_w = bar_w
        bar = Signal(4)
        in_bars = Signal()
        self.comb += [
            bar.eq(px[bar_shift:bar_shift + 4]),
            in_bars.eq(px < bar_w * 16),
        ]

        rgbi = Signal(4)
        self.comb += If(~active | ~self.enable,
            rgbi.eq(0),
        ).Elif(py < vactive // 2,
            If(in_bars, rgbi.eq(bar)).Else(rgbi.eq(0)),
        ).Else(
            rgbi.eq(Mux(px[0], 0b1111, 0b0000)),     # 1ドット幅の白黒縞
        )
        self.comb += [
            self.r.eq(rgbi[0]), self.g.eq(rgbi[1]),
            self.b.eq(rgbi[2]), self.i.eq(rgbi[3]),
        ]


class DigitalRgbProbe(Module):
    """デジタルRGB入力を**測る**(まだ絵にはしない)。

    ★**キャプチャの前に測定器を作る。** 経路(コネクタ・レベル変換・ピン割当)が
      生きているかを、ライン組立やパケット送出と切り離して確かめられる。
      ここが通ってから絵にする。「映らない」の原因候補を先に減らす。

    出す値はすべて CONFIG の読み取り専用キーで読める。

    `meas_clks` は fH/fV を publish する周期[sysクロック]。実機では1秒。
    シミュレーションでは短くする(でないと1回も publish されない)。
    """

    def __init__(self, pads, sys_clk_freq, meas_clks=None, pol_clks=None):
        meas_clks = int(meas_clks or sys_clk_freq)
        pol_clks = int(pol_clks or sys_clk_freq // 50)     # 約20ms(1フレーム超)
        self.meas_clks = meas_clks

        self.cfg_htotal = Signal(12, reset=HTOTAL)
        self.cfg_row    = Signal(9, reset=VSTART + 4)
        self.cfg_dot    = Signal(10, reset=0)
        self.cfg_hstart = Signal(10, reset=HSTART)
        self.cfg_hactive = Signal(11, reset=HACTIVE)

        self.stat_fh     = Signal(32)   # 水平同期の回数/publish周期(実機では Hz)
        self.stat_fv     = Signal(32)   # 垂直同期の回数×1000(実機では mHz)
        self.stat_lines  = Signal(16)   # VS間のHSパルス数(=vtotal)
        self.stat_hlen   = Signal(16)   # 1ラインのsysクロック数
        self.stat_level  = Signal(8)    # いまの生レベル {vs,hs,i,b,g,r}
        self.stat_pol    = Signal(8)    # bit0=HSは負極性 bit1=VSは負極性
        self.stat_pixel  = Signal(8)    # 覗いた位置の色(下位4bit)+ 有効(bit4)
        self.stat_edges  = Signal(16)   # 覗いたラインの色の変化回数

        # # #

        # 非同期入力なので2段で受ける(準安定を持ち込まない)
        raw = Signal(6); s1 = Signal(6); s2 = Signal(6)
        self.comb += raw.eq(Cat(pads.r, pads.g, pads.b, pads.i, pads.hs, pads.vs))
        self.sync += [s1.eq(raw), s2.eq(s1)]
        r, g, b, i, hs, vs = (s2[k] for k in range(6))
        self.comb += self.stat_level.eq(s2)

        # --- 極性を測る ---
        pol_cnt = Signal(max=pol_clks + 1)
        hs_low = Signal(max=pol_clks + 1)
        vs_low = Signal(max=pol_clks + 1)
        self.sync += If(pol_cnt == pol_clks - 1,
            pol_cnt.eq(0),
            self.stat_pol.eq(Cat(hs_low < (pol_clks >> 1), vs_low < (pol_clks >> 1),
                                 C(0, 6))),
            hs_low.eq(0), vs_low.eq(0),
        ).Else(
            pol_cnt.eq(pol_cnt + 1),
            If(~hs, hs_low.eq(hs_low + 1)),
            If(~vs, vs_low.eq(vs_low + 1)),
        )

        # --- 同期エッジ。**極性の測定結果に合わせてパルスの始まりへ揃える** ---
        #
        # ★立ち下がり固定で書くと、正極性の機種でブランキング側を1ラインとして
        #   数える。周期は同じなので fH は正しく見えるのに有効映像の位置がずれる ─
        #   症状から原因が遠い。
        hs_act = Signal(); vs_act = Signal()
        self.comb += [
            hs_act.eq(hs ^ self.stat_pol[0]),
            vs_act.eq(vs ^ self.stat_pol[1]),
        ]
        hs_d = Signal(); vs_d = Signal()
        hs_edge = Signal(); vs_edge = Signal()
        self.sync += [hs_d.eq(hs_act), vs_d.eq(vs_act)]
        self.comb += [hs_edge.eq(hs_act & ~hs_d), vs_edge.eq(vs_act & ~vs_d)]

        # --- 1ラインの長さ[sysクロック] ---
        hcnt = Signal(16)
        self.sync += If(hs_edge,
            hcnt.eq(1),
            If(hcnt != 0, self.stat_hlen.eq(hcnt)),
        ).Else(hcnt.eq(hcnt + 1))

        # --- fH / fV は「一定時間に何回来たか」で出す ---
        #
        # ★**除算器を置かない。** 周期から割り算で出すと sys のクリティカルパスに
        #   乗る。この基板は配置シード次第でタイミングが落ちるので数える方式にする。
        tick = Signal(max=meas_clks)
        hs_n = Signal(32); vs_n = Signal(32)
        self.sync += If(tick == meas_clks - 1,
            tick.eq(0),
            self.stat_fh.eq(hs_n),
            self.stat_fv.eq(vs_n * 1000),
            hs_n.eq(0), vs_n.eq(0),
        ).Else(
            tick.eq(tick + 1),
            If(hs_edge, hs_n.eq(hs_n + 1)),
            If(vs_edge, vs_n.eq(vs_n + 1)),
        )

        # --- VS間のライン数(=vtotal) ---
        #
        # ★**VS と HS が同じサイクルに来る場合を落とさない。** 生成器は
        #   ライン先頭で VS を立てるので両方が同時に来て、素直に書くとその1本を
        #   数え落として vtotal が1本足りなくなる(26 → 25)。実機でも VS が
        #   ライン境界に揃う機種では同じことが起きる。
        lines = Signal(16)
        self.sync += If(vs_edge,
            # 同時に来た HS は、閉じるフレームの最初の1本として数える。
            # 新しいフレームでは 0 から数え直す(その HS が line 0 になる)
            If(lines != 0, self.stat_lines.eq(lines + hs_edge)),
            lines.eq(0),
        ).Elif(hs_edge, lines.eq(lines + 1))

        # --- 覗き窓: 指定した (row, dot) の色を1つ取る ---
        #
        # ドット位置は「1ラインのsysクロック数 ÷ htotal」で決まるが、**割り算を
        # 置かない**。分数の足し込みで数える:
        #
        #     acc += htotal ; acc >= hlen なら { acc -= hlen ; dot += 1 }
        #
        # ★45MHz で 15MHz のドットを見るので**1ドット3.14サンプル**しかない。
        #   端で拾うと遷移中を掴むので、半ドットぶん進めて**中央**で拾う。
        row = Signal(16)
        acc = Signal(20)
        dotn = Signal(12)
        dot_step = Signal()
        self.sync += If(vs_edge, row.eq(0)).Elif(hs_edge, row.eq(row + 1))
        self.sync += [
            dot_step.eq(0),
            If(hs_edge,
                acc.eq(self.stat_hlen >> 1),      # 半ドットぶん進めて中央へ
                dotn.eq(0),
            ).Elif(acc >= self.stat_hlen,
                acc.eq(acc - self.stat_hlen + self.cfg_htotal),
                dotn.eq(dotn + 1),
                dot_step.eq(1),
            ).Else(
                acc.eq(acc + self.cfg_htotal),
            ),
        ]

        color = Signal(4)
        on_row = Signal()
        want_dot = Signal(12)
        in_active = Signal()
        self.comb += [
            color.eq(Cat(r, g, b, i)),
            on_row.eq(row == self.cfg_row),
            want_dot.eq(self.cfg_hstart + self.cfg_dot),
            in_active.eq((dotn >= self.cfg_hstart)
                         & (dotn < self.cfg_hstart + self.cfg_hactive)),
        ]

        prev = Signal(4)
        edges = Signal(16)
        self.sync += If(hs_edge,
            If(on_row, self.stat_edges.eq(edges)),
            edges.eq(0),
            prev.eq(0),
        ).Elif(dot_step & on_row,
            If(dotn == want_dot,
                self.stat_pixel.eq(Cat(color, C(1, 1), C(0, 3))),
            ),
            # 有効映像内で色が変わった回数。**1ドット縞が潰れていない証拠**
            If(in_active,
                If(color != prev, edges.eq(edges + 1)),
                prev.eq(color),
            ),
        )
