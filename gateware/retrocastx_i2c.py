#!/usr/bin/env python3
"""共有I2Cバイトマスタ + ステータス表示シーケンサ(CPU不介在)。

I2CByteMaster: START/WRITE/READ/STOP を op 単位で実行するビットバングマスタ。
  open-drain(scl_low/sda_low=1でLow駆動, 0で解放=プルアップHigh)。sda_in=線の実値。
  op: 0=START, 1=WRITE(wdata送出→ackr), 2=READ(rd_nack指定→rdata), 3=STOP。
  go=1パルスで開始、done=1パルスで完了。

StatusDisplay: 1本のI2Cバスを時分割し、
  - TVP7002(0x5C): RESETB解除 → 初期化数レジスタ書込 → レジスタ読出(ACK/値取得)
  - SSD1306 OLED(0x3C): 初期化 → テキストRAMを常時再描画(ライブ値をhex整形)
"""
from migen import *

try:
    from .oled_font import FONT8
except ImportError:
    import sys, os
    sys.path.insert(0, os.path.dirname(__file__))
    from oled_font import FONT8

OP_START, OP_WRITE, OP_READ, OP_STOP = 0, 1, 2, 3


class I2CByteMaster(Module):
    def __init__(self, sys_clk_freq=45e6, i2c_freq=400e3):
        self.op       = Signal(2)
        self.wdata    = Signal(8)
        self.rd_nack  = Signal()      # READ後に master が返すACKビット(1=NACK)
        self.go       = Signal()
        self.done     = Signal()
        self.rdata    = Signal(8)
        self.ackr     = Signal()      # WRITE後の slave ACK (0=ACK)
        self.busy     = Signal()
        # open-drain 線
        self.scl_low  = Signal()
        self.sda_low  = Signal()
        self.sda_in   = Signal(reset=1)

        Q = max(2, int(round(sys_clk_freq / (4 * i2c_freq))))
        div   = Signal(max=Q)
        run   = Signal()
        en    = Signal()
        phase = Signal(2)
        self.sync += [
            If(run,
                If(div == Q - 1, div.eq(0)).Else(div.eq(div + 1)),
            ).Else(div.eq(0)),
        ]
        self.comb += en.eq(run & (div == Q - 1))
        self.sync += If(en, phase.eq(phase + 1))
        cell = Signal(); self.comb += cell.eq(en & (phase == 3))

        sh    = Signal(8)
        bit   = Signal(4)   # 0..8
        opr   = Signal(2)
        bus_free = Signal(reset=1)   # STOP後=1(解放), トランザクション中=0(IDLEでSCL Low保持)

        self.submodules.fsm = fsm = FSM(reset_state="IDLE")
        fsm.act("IDLE",
            # トランザクション途中(bus_free=0)はSCLをLow保持し疑似START/STOPを防ぐ
            self.scl_low.eq(~bus_free), self.sda_low.eq(0),
            If(self.go,
                NextValue(opr, self.op),
                NextValue(sh, self.wdata),
                NextValue(bit, 0),
                NextValue(run, 1),
                NextValue(phase, 0),
                Case(self.op, {
                    OP_START: NextState("START"),
                    OP_WRITE: NextState("BYTE"),
                    OP_READ:  NextState("BYTE"),
                    OP_STOP:  NextState("STOP"),
                }),
            ).Else(NextValue(run, 0)),
        )
        # START: SCL high(0,1)->low(2,3), SDA high(0)->low(1..)
        fsm.act("START",
            self.scl_low.eq(phase >= 2),
            self.sda_low.eq(phase >= 1),
            If(cell, NextValue(run, 0), NextValue(bus_free, 0),
               self.done.eq(1), NextState("IDLE")),
        )
        # STOP: SCL low(0)->high(1..), SDA low(0,1)->high(2,3)
        fsm.act("STOP",
            self.scl_low.eq(phase == 0),
            self.sda_low.eq(phase <= 1),
            If(cell, NextValue(run, 0), NextValue(bus_free, 1),
               self.done.eq(1), NextState("IDLE")),
        )
        # BYTE: 8 data + 1 ack。SCL: high in phase 1,2。
        is_wr = (opr == OP_WRITE)
        fsm.act("BYTE",
            self.scl_low.eq((phase == 0) | (phase == 3)),
            # SDA駆動
            If(bit < 8,
                If(is_wr,
                    self.sda_low.eq(~sh[7]),      # WRITE: MSB送出
                ).Else(
                    self.sda_low.eq(0),           # READ: 解放して受信
                ),
            ).Else(
                # ACKビット
                If(is_wr,
                    self.sda_low.eq(0),           # WRITE: 解放(slaveがACK)
                ).Else(
                    self.sda_low.eq(~self.rd_nack),  # READ: masterがACK(0)/NACK(1)
                ),
            ),
            # サンプル(SCL立上り = phase==1)
            If(en & (phase == 1),
                If(bit < 8,
                    If(~is_wr, NextValue(self.rdata, Cat(self.sda_in, self.rdata[0:7]))),
                ).Else(
                    If(is_wr, NextValue(self.ackr, self.sda_in)),
                ),
            ),
            # ビット送り
            If(cell,
                NextValue(sh, Cat(Signal(1), sh[0:7])),   # <<1
                If(bit == 8,
                    NextValue(run, 0), self.done.eq(1), NextState("IDLE"),
                ).Else(
                    NextValue(bit, bit + 1),
                ),
            ),
        )
        self.comb += self.busy.eq(~fsm.ongoing("IDLE"))


# ---- SSD1306 128x64 初期化列 + 表示範囲 ----
OLED_INIT = [
    0xAE, 0xD5,0x80, 0xA8,0x3F, 0xD3,0x00, 0x40, 0x8D,0x14, 0x20,0x00,
    0xA1, 0xC8, 0xDA,0x12, 0x81,0xCF, 0xD9,0xF1, 0xDB,0x40, 0xA4, 0xA6,
    0x2E, 0xAF, 0x21,0x00,0x7F, 0x22,0x00,0x07,
]
COLS, ROWS = 16, 8


def _banner():
    lines = [
        "RetroCastX  i5",
        "----------------",
        "TVP : ---",        # ACK/NAK @ col6..8
        "SYNC: 0x--",       # 0x14 hex @ col8,9
        "LPF : 0x----",     # 0x37:0x38 hex @ col8..11
        "CPL : 0x----",     # 0x39:0x3A hex @ col8..11
        "UP  : 0x--------",  # hex @ col8..15
        "in3 RGB  0x5C/3C",
    ]
    buf = []
    for r in range(ROWS):
        s = (lines[r] if r < len(lines) else "")
        s = (s + " " * COLS)[:COLS]
        buf += [ord(c) & 0x7F for c in s]
    return buf


class StatusDisplay(Module):
    """共有I2C(SDA/SCL) + RESETB を使い、TVP7002の応答/レジスタを読み OLEDに表示。"""
    def __init__(self, pads=None, sys_clk_freq=45e6, i2c_freq=400e3,
                 tvp_addr=0x5C, oled_addr=0x3C,
                 green_input=3, red_input=3, blue_input=3, pll_divide=0):
        # pll_divide: H-PLL帰還分周比(=1ライン当たりのDATACLK数)。0なら書かない
        #   (TVP既定1650)。入力の実水平トータル[ドット]に合わせると1サンプル=1ドットに
        #   なる。既定1650のままだと実htotal(X68000 31kHz≒1104)より細かくサンプルする
        #   ので、有効領域がキャプチャ幅1024を超えて右が切れる。
        assert pll_divide == 0 or 1 <= pll_divide <= 0xFFF
        # {green,red,blue}_input: 各チャネルに使う入力ピン番号。既定3(基板配線)。
        #   0x19 = [7:6]SOG [5:4]Red [3:2]Green [1:0]Blue、各 00=_1 01=_2 10=_3 11=_4。
        #   緑のクランプ/レベル異常の切り分け用。R/Bも切り替えられるようにして
        #   「muxの切り替えでクランプ電圧が別ピンへ移動するか」の対照実験に使う。
        assert green_input in (1, 2, 3, 4)
        assert red_input in (1, 2, 3) and blue_input in (1, 2, 3)  # _4はR/Bに無い
        self.submodules.m = m = I2CByteMaster(sys_clk_freq, i2c_freq)
        # open-drain 線(sim用に公開)
        self.scl_low = m.scl_low
        self.sda_low = m.sda_low
        self.resetb  = Signal(reset=0)      # TVP RESETB (0=reset, 1=解除)
        # 観測用
        self.tvp_ack = Signal()
        self.syncdet = Signal(8)  # reg0x14 Sync Detect Status
        # ★名前が逆。_hi が LSB 側、_lo が MSB 側。
        #   Linux drivers/media/i2c/tvp7002_reg.h より:
        #     0x37 L_FRAME_STAT_LSBS / 0x38 L_FRAME_STAT_MSBS
        #     0x39 CLK_L_STAT_LSBS   / 0x3A CLK_L_STAT_MSBS
        #   MSBs側は下位4bitが値の[11:8]で、上位はステータス(bit5=P/I detect)。
        #   OLEDの表示(lpf_lo[0:4],lpf_hi[4:8],lpf_hi[0:4])はこの並びで正しい。
        #   名前に釣られて MSBs/LSBs を取り違えやすいので注意(実際に踏んだ)。
        self.lpf_hi = Signal(8); self.lpf_lo = Signal(8)  # 0x37(LSBs)/0x38(MSBs)
        self.cpl_hi = Signal(8); self.cpl_lo = Signal(8)  # 0x39(LSBs)/0x3A(MSBs)
        # H-PLL帰還分周比(=1ライン当たりDATACLK数)。実行時に変更可。
        # このFSMは初期化書き込みを毎周(約30回/秒)繰り返すので、値を変えれば
        # 次の周で自動的にTVPへ書き込まれる(別途トリガは不要)。
        # pll_divide=0 でビルドした場合は 0x01/0x02 を書かないので効かない。
        self.cfg_pll_divide = Signal(12, reset=pll_divide)
        # アナログ映像帯域(レジスタ0x3F)。0=最大 〜 15=最小(約95MHz)。
        # 既定を最小にしている。実測でエッジ後の残留エコーが +2.55 → +0.01 と
        # 完全に消え、立ち上がり(2サンプル)は変わらず最細部が6%落ちるだけだった。
        self.cfg_video_bw = Signal(4, reset=0xF)
        # サンプリング位相(レジスタ0x04 bit[7:3])。1ドット周期を32分割した何番目で
        # ADCがサンプルするか。既定16(=180度)は「ドットの中央あたり」を狙った値だが、
        # 実際の最適点はケーブル遅延やTVP内部の遅延で変わるので実測で決める。
        # ずれていると全画素が一様にぼける(白が灰色に見える)ため、鮮鋭度に直結する。
        self.cfg_phase = Signal(5, reset=16)
        # 同期制御(レジスタ0x0E)。既定0x52。
        #
        # TVPのVSOUTがインターレース入力の半ライン位相を潰してしまう問題の切り分け用。
        # 実測(24kHz 1024x848): 生信号はフィールドごとにVSYNCが来ている(オシロで
        # VSYNCトリガごとにHSYNCが半ラインずれる)のに、VSOUTは931ラインに1パルスしか
        # 出ず、VSYNCの水平位相は300回読んで完全固定だった。VSOUTを再生成ではなく
        # 素通しにできる設定があれば位相が残るはずなので、実行時に振って探す。
        self.cfg_sync_ctl = Signal(8, reset=0x52)
        # ファインクランプ制御(レジスタ0x2A)。既定0x07。
        #   bit7 CM Offset(チャンネル間の分離改善) / bit[4:3] Fine swsel(時定数)
        #   bit1 Fine GB / bit0 Fine R
        # 既定は時定数が最長。G/Bだけ明部の先頭20サンプルが2〜3%低く出る現象の
        # 切り分け用に実行時から変えられるようにする。
        # 既定を 0x87 にしている(CM Offset 有効)。実測で白ベタ先頭の落ち込みが
        # B +2.0% → +0.3%、G +2.6% → +2.2% と改善した。時定数(bit[4:3])は
        # 効果が無く、原因はチャンネル間のクロストークだった。
        self.cfg_fine_clamp = Signal(8, reset=0x87)

        # --- CSYNC(映像レベル)を SOG 経由で受けるための同期分離パラメータ ---
        # MSX のように C-SYNC しか出さない機種を SOGIN に 1nF で結合して受ける運用で
        # 効くレジスタ群。cfg_sync_ctl(0Eh) を 0x5B にすると H も V も SOG から取る。
        # いずれも実機を見ながら振る前提で、CONFIG から実行時に変えられるようにした。

        # SOGスライスレベル(レジスタ 10h の bit[7:3])。クランプしたsync tipから
        # 何mV上で切るか。C-SYNCの振幅は機種で違うので、SOGOUT(FPGAに来ている)を
        # 見ながら合わせる。
        # レジスタ 10h は複合で、bit0=Red CS / bit2=Blue CS のクランプ選択が同居する
        # (0x58 = R/G/B全てbottom-levelクランプ。崩すと背景が紫がかる)。そのため
        # **閾値だけを5bitで持ち**、書き込み時に下位3bitを cfg_clamp_sel から足す。
        self.cfg_sog_thresh = Signal(5, reset=0x58 >> 3)

        # ファインクランプの基準レベル選択(レジスタ 10h の bit[2:0])。
        # データシート 10h: bit2=Blue CS / bit1=Green CS / bit0=Red CS。
        # 0=ボトムレベル(ブランキングをコード0へ) / 1=ミッドレベル(512へ)。
        # TVP既定は 5Dh = R/Bがミッドレベル(Pb/Pr向け)だが、**RGB入力では
        # 全チャネルをボトムレベルにする**(0x58)。崩すと背景が紫がかる実測がある。
        #
        # コンポジットを緑に入れるときだけ 0b010(Greenのみミッドレベル)にする。
        # ボトムレベルだとブランキングが約64コード(粗オフセット1Fhの既定 +64)にしか
        # 座らないので、それを中心に振れるカラーバースト(±20 IRE)の下半分が
        # クリップして位相ロックできない。
        #
        # ★ミッドレベルにすると**白側のヘッドルームが半分になる**。ブランキングが
        #   512に座るので +100 IRE(100%白)に使えるのは残り512コードだけ。
        #   コンポジット1Vpp=140IRE のままでは飽和するので、**粗ゲイン(1Bh)を
        #   同時に下げること**。データシート1Bh は「Vpp × Gain < 1Vpp」を要求する。
        #   → cfg_coarse_gain_gb を参照
        self.cfg_clamp_sel = Signal(3, reset=0x58 & 0x07)

        # 粗アナログゲイン(レジスタ 1Bh。bit[7:4]=Green / bit[3:0]=Blue)。
        # Gain = 0.5 + N/10(N=0..15)。TVP既定は 77h(N=7 → 1.2倍、700mVpp入力向け)。
        # ADCフルスケールは 1Vpp なので、入力に許される振幅は 1Vpp / Gain。
        #
        # コンポジット(1Vpp = 140 IRE)をミッドレベルクランプで受けるときの必要条件は
        # 「ブランキング512から上に +131 IRE(100%カラーバーの黄のピーク)が乗る」こと:
        #
        #   Gain  ADC FS入力  1 IRE  100%バー黄131IREの行き先  バースト40IRE p-p
        #   0.5   2.00 Vpp    3.66c  512+479 =  991  ○         146c(10bit) ≈ 36c(8bit)
        #   0.6   1.67 Vpp    4.39c  512+575 = 1087  ×飽和     176c(10bit) = 44c(8bit)
        #   0.7   1.43 Vpp    5.11c  512+669 = 1181  ×飽和     204c(10bit) = 51c(8bit)
        #   1.2   0.83 Vpp    8.77c  そもそも同期チップが飽和   ×
        #
        # なので**コンポジットは 1Bh を 0x07(Green=0.5) にする**のが出発点。
        # RGB入力では既定の 0x77 に戻す(モード固有の設定)。
        self.cfg_coarse_gain_gb = Signal(8, reset=0x77)

        # 粗アナログオフセット Green(レジスタ 1Fh の bit[5:0]。6bit符号+絶対値)。
        # データシート: 10h = **+64コード**(既定) / 1Fh = +124コード / 20h = -0 /
        # 3Fh = -124コード。「10h より小さいとADC入力でボトム側がクリップし得る」。
        # ALCの手順が「粗オフセットでブランキングを32コードに合わせる」と書いている
        # ことからも、これはADC出力のブランキング位置を動かすノブである。
        #
        # ★つまりボトムレベルクランプでもブランキングはコード0ではなく**約64**にいる。
        #   ここを 1Fh(+124)まで上げると、ミッドレベルにしなくてもバーストの下半分を
        #   救える。ミッドレベルより白側ヘッドルームが広いので**粗ゲインを上げられ、
        #   結果としてバーストの分解能が良くなる**:
        #
        #     ボトム + off=1Fh + gain 0.8 : 1 IRE=5.85c, バースト40IRE=234c → 58c(8bit)
        #     ミッド        + gain 0.5 : 1 IRE=3.66c, バースト40IRE=146c → 36c(8bit)
        #
        #   (ボトム側の制約は「バースト下端 20IRE×k ≤ 124」と「白131IRE×k ≤ 1023-124」)
        # どちらが実際に素直かは実測で決める。両方振れるようにしてある。
        self.cfg_coarse_off_g = Signal(6, reset=0x10)

        # --- 赤チャネルの粗ゲイン/粗オフセット(レジスタ 1Ch / 20h)---
        #
        # **1Bh は Blue と Green しか持っていない。赤は別レジスタ。**
        # S端子では C(色信号)が RIN_3 に入るので、Y とは独立にゲインを決められないと
        # S端子にする意味(チャネルごとに振幅へ最適化できること)が出ない。
        #
        # ★1Ch のビット割りは 1Bh と違う。[7:4]は Reserved で、**[3:0]だけが赤のN**。
        #   同じ「1.2倍」でも 1Bh では 0x77、1Ch では 0x07 になる。取り違えやすい。
        #
        # S端子の C は搬送波抑圧なのでブランキング(=無彩色)を中心に振れる。
        # ミッドレベルクランプで512に座らせたとき、飽和させない条件は
        # 「100%飽和色のクロマ片振幅 0.35V ≤ (1Vpp/Gain)/2」→ **Gain ≤ 1.43**。
        # つまり TVP既定の 1.2倍(N=7)がそのまま使える:
        #
        #   Gain  ADC FS入力  バースト0.286Vpp p-p     クロマ片振幅0.35Vの行き先
        #   1.2   0.833 Vpp   351c(10bit) ≈ 88c(8bit)  512+430 = 942  ○
        #   1.4   0.714 Vpp   410c(10bit) ≈ 102c(8bit) 512+502 = 1014 ○(ぎりぎり)
        #   1.5   0.667 Vpp   439c(10bit) ≈ 110c(8bit) 512+538 = 1050 ×飽和
        #
        # バースト88コードはコンポジット(36コード)の約2.4倍。実測して余裕が
        # あれば 0x08/0x09 まで上げられる。
        self.cfg_coarse_gain_r = Signal(8, reset=0x07)
        self.cfg_coarse_off_r = Signal(6, reset=0x10)

        # 入力MUX選択1(レジスタ 19h)。[7:6]=SOG [5:4]=Red [3:2]=Green [1:0]=Blue。
        # 各 00=_1 / 01=_2 / 10=_3(Greenのみ 11=_4)。
        #
        # ★以前はビルド時定数 MUX1 だったが、**実行時に振れないと入力方式を
        #   切り替えられない**ので CONFIG キーにした(key 0x69)。
        #   手組みボードの配線で必要になった具体例:
        #
        #     コンポジット  Gin3 + SOGin3      → 0xAA (SOG=_3, R/G/B=_3)
        #     MSX RGB       Rin3/Gin3/Bin3 + SOGin2 → **0x6A** (SOG=_2, R/G/B=_3)
        #     X68000 RGB    Rin3/Gin3/Bin3 + HSYNC_A/VSYNC_A → 0xAA (SOGは不使用)
        #
        #   MSX だけ SOG の入力ピンが違うので、19h が固定だと**ビットストリームを
        #   焼き直さないと追従できなかった**。
        #
        # 同期入力(HSYNC_A/_B, VSYNC_A/_B)の選択は 19h ではなく 1Ah bit0/bit2
        # (= cfg_in_mux2、key 0x5D)。混同しないこと。
        #
        # 初期値はビルド引数から作る(SOGは_3、R/G/Bは引数)。以降はCONFIGで上書き。
        MUX1 = ((2 << 6) | ((red_input - 1) << 4) |
                ((green_input - 1) << 2) | (blue_input - 1))
        self.cfg_in_mux1 = Signal(8, reset=MUX1)

        # 同期セパレータ閾値(レジスタ 11h)。**TVPの既定 20h は使わない。**
        # 「内部クロック基準(約6.5MHz)を何周期数えたらH/Vを切り替えるか」= 長いパルスを
        # Vとみなす境界。データシートの条件は
        #   Threshold × 最小クロック周期(133ns) > 負同期パルス幅
        # で、480i60Hz(=MSXと同じ15.7kHz族)の範囲は MIN 1Fh / MID 75h / MAX ABh。
        # 「40h = 大半のフォーマットの推奨値」「中央値でマージン最大」とある。
        #
        # TVP既定の 20h は MIN(1Fh)の真上でマージンが無く、実測(MSXのC-SYNCをSOGで受用)
        # では水平パルスまでVと判定してしまい **Lines per Frame が 1**(毎ラインVSYNC)に
        # なった。0x50 以上で 262(MSXの正解値)に張り付く。推奨の中央値を既定にする。
        self.cfg_sep_thresh = Signal(8, reset=0x75)

        # H-PLL Pre-Coast / Post-Coast(レジスタ 12h / 13h)。**両方とも既定 00h**。
        # C-SYNCでは垂直区間のパルス列がH-PLLを引っ張るので、その間だけPLLを
        # 保持(coast)させる必要がある。データシートに
        #   「Pre-Coast: A minimum setting of 1 is required to guarantee
        #    generation of an internal coast signal.」
        # と明記されており、0 のままでは coast が生成されない。単位はHSYNC周期。
        # 推奨値の表: 480i/p・576i/p は 3/3、1080i/p・720p・PC SOG Graphics は 1/0。
        # 15kHz機(MSX等)は480i/p族なので 3/3 を既定にする。
        self.cfg_precoast = Signal(8, reset=3)
        self.cfg_postcoast = Signal(8, reset=3)

        # 同期処理制御(レジスタ 22h)。既定 08h。bit0=VS Bypass, bit1=VS Select。
        # 既定では「同期セパレータの活動が無いとき VSOUT はハーフライン積算器が
        # 生成する」。積算器はインターレース(NTSC 525ライン/フレーム)前提なので、
        # 262ライン progressive の MSX では2フレームに1回しかVを出さない。実測では
        # どちらが勝つかがロック毎に決まり 1×/2× が50/50で双安定になった。
        # bit0=1 で VSOUT を同期セパレータ直結にすると決定的になる。
        self.cfg_sync_ctl2 = Signal(8, reset=0x08)
        # 同期バイパス(レジスタ 36h)。既定 00h。bit0=HS BP, bit1=VS BP。
        # HS BP=1 で HSOUT が生の未処理HSYNCになる。HSOUTの2倍化がスライサ由来か
        # 後段由来かの切り分け用(通常運用には非推奨とデータシートにある)。
        self.cfg_sync_bypass = Signal(8, reset=0x00)
        # 入力Mux選択2(レジスタ 1Ah)。既定 C2h。
        #   bit[7:6] SOG LPF SEL  00=2.5MHz 01=10MHz 10=33MHz 11=バイパス(既定)
        #   bit[5:4] CLP LPF SEL  00=4.8MHz(既定,HDTV/グラフィックス向け)
        #                         01=0.5MHz「Suitable for SDTV formats」 10=1.7MHz
        #   bit3 CLK SEL / bit2 VS SEL / bit1 PCLK SEL / bit0 HS SEL は既定のまま(0b0010)
        # データシートの Glitch Immunity 節:
        #   「During white-to-black transitions, the input video waveform may undershoot
        #    below the sync slicer threshold. To help attenuate the amplitude of such
        #    glitches, a single-pole low-pass filter ... is provided at the input of the
        #    SOG voltage comparator circuit. This filter is bypassed in the default mode.」
        # 15kHz機(MSX等)はSDTVなので SOG=2.5MHz / CLP=0.5MHz の 0x12 を既定にする。
        # 既定の C2h はどちらもHDTV/グラフィックス向けで15kHz機に合っていない。
        # 注: 「Excessive filtering can lead to sync detection issues and increased
        #      sample clock jitter」とあるので、効果はCONFIGで振って確認する。
        self.cfg_in_mux2 = Signal(8, reset=0x12)
        # PLL設定(0x03)とクランプ位置(0x05/0x06)はTVPの既定値のまま書く。
        # データシートの規定と合っていない箇所があるので実測したが、良好な電源の
        # 下ではどちらも測定可能な効果が無かった(下記)。実行時に変更できるように
        # だけしてある(CONFIG key 0x19 / 0x1A / 0x1B)。
        # H-PLL制御(レジスタ03h)。VCOレンジ[7:6] + チャージポンプ電流[5:3]。
        #
        # データシート:
        #   VCO 00 = Ultra low (KVCO  75)  PCLK < 36 MHz
        #       01 = Low       (KVCO  85)  36 ≤ PCLK < 70
        #       10 = Medium    (KVCO 150)  70 ≤ PCLK < 135   ← 既定(A8h)
        #       11 = High      (KVCO 200)  135 ≤ PCLK ≤ 165
        #   ICP = 40 × KVCO / (pixels per line)
        #
        # X68000の全モードは PCLK < 36MHz なので Ultra low が正しい:
        #   31kHz 768x512   34.78MHz / 1104 → ICP 3 → 18h
        #   24kHz 1024x848  34.77MHz / 1408 → ICP 2 → 10h
        #   15kHz 512x512   19.43MHz / 1216 → ICP 2 → 10h
        #   31kHz 512x512   23.18MHz /  736 → ICP 4 → 20h
        #
        # 以前は既定の A8h(Medium)をそのまま書いていた。レンジ外で動かすと
        # データシートが言う "improve the noise performance" が効かず、位相ノイズが
        # 増えてラインごとにサンプリング位相が揺れる(実機で行単位の横ずれとして出た)。
        # ここでは最も使う 31kHz 768x512 の値を既定にし、Viewerがモードから
        # 計算して送り直す(CONFIG key 0x19)。
        self.cfg_pll_ctl = Signal(8, reset=0x18)
        self.cfg_clamp_start = Signal(8, reset=0x32)
        self.cfg_clamp_width = Signal(8, reset=0x20)
        # 細ゲイン(08h=Blue / 09h=Green / 0Ah=Red)。Gain = 1 + N/256。
        # 既定0だと白がフルスケールに届かない。X68000で実測すると白が210/255
        # (82%)しかなく、階調とSNRを2割損していた。粗ゲインは既定で1.2倍あり、
        # データシートも "For a normal PC graphics input, the fine gain is used
        # mostly" としているので、ここで補う。
        # ★ここは**電源投入時の値**。CONFIG(key 0x1C/0x1D/0x1E)で上書きされる。
        #   Viewer のプロファイル(client/src/profiles.rs)と client/src/main.rs の
        #   初期表示にも同じ値がある。**3箇所を揃えること。**
        #   揃っていないと「Viewerを再起動したらゲインが下がった」ように見える
        #   (2026-09-03 に実際に起きた。ボードを焼き直すとここの値に戻り、
        #    Viewer はボードから読み戻すので正直にそれを表示する)。
        #
        # 2026-09-03: ohnakaさんの目視で 64/61/57 の方が鮮やかとのことで採用。
        #
        # 飽和を疑ったが、**実測で否定された**(x68k_calib のグレースケールを
        # calib_pattern.py で、pll_divide=736 の1サンプル=1ドットで測定):
        #
        #     ゲイン      白             取り込めた段  欠けている5bit値
        #     39/33/35   239 (5bit 29)   28個         1, 2, 30, 31
        #     64/61/57   255 (5bit 31)   30個         1, 2
        #
        # ★**白が255なのは潰れているのではなく、最上位の階調に到達したということ。**
        #   飽和しているなら 30 と 31 が同じコードに merge して段が減るはずだが、
        #   逆に2段増えている。上端を使い切れていなかったのを直した形。
        #
        # ※以前の記録は (54,51,58) で飽和5626画素としていたが、これは測定条件が
        #   違う(オーバーサンプリング状態で測っていた可能性が高い)。上の測定は
        #   ツール側が サンプル/ドット=1.000 を自己検査した上での値。
        #
        # 残っている問題は**下端**(5bit の 1 と 2 がどのゲインでも落ちる)。
        # これはゲインではなくオフセット/クランプ側の話で、別途。
        self.cfg_gain_b = Signal(8, reset=57)
        self.cfg_gain_g = Signal(8, reset=61)
        self.cfg_gain_r = Signal(8, reset=64)

        TVP_W = (tvp_addr << 1) & 0xFE       # 0xB8
        TVP_R = TVP_W | 1                    # 0xB9
        OLED_W = (oled_addr << 1) & 0xFE     # 0x78

        # ROM/RAM
        self.specials.font = Memory(8, 1024, init=FONT8)
        frd = self.font.get_port(); self.specials += frd
        self.specials.text = Memory(8, COLS*ROWS, init=_banner())
        trd = self.text.get_port(); twr = self.text.get_port(write_capable=True)
        self.specials += trd, twr

        init_rom = Array(Constant(v, 8) for v in OLED_INIT)
        NINIT = len(OLED_INIT)

        # uptime 秒カウンタ(hex表示用)
        subsec = Signal(max=int(sys_clk_freq)); sec = Signal(32)
        self.sync += [
            If(subsec == int(sys_clk_freq)-1, subsec.eq(0), sec.eq(sec+1)
              ).Else(subsec.eq(subsec+1)),
        ]

        # --- I2C op 発行ヘルパ(pumpパターン) ---
        sent = Signal()
        def issue(op, data=0, nack=0):
            return [m.op.eq(op), m.wdata.eq(data), m.rd_nack.eq(nack),
                    If(~sent & ~m.busy, m.go.eq(1))]
        # sentは done で解除、go後にセット
        self.sync += [
            If(m.go, sent.eq(1)),
            If(m.done, sent.eq(0)),
        ]

        # フレーム用インデックス
        idx = Signal(11)
        # OLED data 座標
        k = Signal(11); page = Signal(3); xcol = Signal(7)
        self.comb += [k.eq(idx-2), page.eq(k[7:10]), xcol.eq(k[0:7])]
        self.comb += [trd.adr.eq((page<<4) + xcol[3:7]),
                      frd.adr.eq((trd.dat_r<<3) + xcol[0:3])]
        prefetch = Signal(2)
        databyte = Signal(8)

        # 起動後リセット解除タイマ
        rst_cnt = Signal(max=int(sys_clk_freq//10)+1)

        # TVPステップ: 先頭NWRITE個=初期化書込(reg<-val)、残り=ステータス読出
        #   write: 0x19<-0xAA(SOG/R/G/B 全て _3 入力を選択, OSSC互換)
        #          0x0E<-0x52(AHSS=0/AVSS=0: 外部HSYNC/VSYNC を有効HSYNC/VSYNCに)
        #               0x53=4線(HSYNCピンのTTL CSYNC、Vは内部分離)
        #               0x5B=SOG(HもVもSOGから。映像レベルCSYNC/sync-on-green)
        #          0x11<-同期セパレータ閾値, 0x12/0x13<-Pre/Post-Coast
        #               (既定0でcoast未生成。CSYNC運用では要設定)
        #          0x17<-0x00(Output En=0 かつ SOG En=0)
        #            bit0 Output En: 0で RGB/DATACLK/HSOUT/VSOUT/FIDOUT を出力。
        #              既定0x03はbit0=1で全出力Hi-Z=DATACLKが出ない。
        #            bit1 SOG En: 0で SOGOUT(同期スライサの出力そのもの)を出力。
        #              既定1はHi-Z。**インターレースのフィールド極性のために有効にする。**
        #              C-SYNCのみの機種(MSX等)をSOG経由で受けると、TVPのVSOUTもFIDOUTも
        #              フィールド極性を出さない(実機測定: FIDOUT=1固定、VSOUT位相=1066固定)。
        #              半ライン位相は入力のC-SYNCには存在するので、スライサ直後の
        #              SOGOUTをFPGAで直接見れば、hs_raw/vs_raw と同じやり方で測れる。
        #              新基板では SO-DIMM pin131(C4) へ引き出してある(main.ato)。
        #            bit[6:4] Test output control: 000 = Field ID output(既定)
        #          0x18<-0x01(CLK POL=1: データをDATACLK立下りでlaunch。FPGAは立上りで
        #                     安定サンプルできる。他ビットは既定0)
        #          0x31<-0x18(ALC Placement: データシートが「PCグラフィックス/バイレベル
        #                     同期のSDTV」に指定する値。既定0x5AはHDTV三値同期用)
        #          0x10<-0x58(既定0x5DのRed CS(bit0)/Blue CS(bit2)を0にし、R/G/B全て
        #                     bottom-levelクランプへ。既定はYPbPr向けで Pr(赤)/Pb(青)が
        #                     mid-levelクランプ = blankレベルが512(中央値)にマップされ、
        #                     黒が R,B≈128 になって背景が紫がかっていた。データシート:
        #                     "Bottom-level clamping is required for Y and RGB inputs,
        #                      while mid-level clamping is required for Pb and Pr inputs"。
        #                     SOG Threshold[7:3]は既定のまま)
        #   read : 0x14(SyncDet) 0x37/0x38(Lines/Frame) 0x39/0x3A(Clocks/Line)
        # 0x19: cfg_in_mux1(key 0x69)で実行時に振る。WR_VAL の MUX1 は初期値だけ
        #   (MSXのように SOG だけ別ピンに来る配線があるので、固定にはできない)。
        #          0x3F<-video_bw(アナログ映像帯域。0=最大(既定) 〜 15=最小 約95MHz)
        #                     TVPのアナログ帯域は350〜500MHzある一方、こちらの
        #                     サンプリングは8〜44MHzしかないので、ナイキストより上の
        #                     雑音がそのまま折り返して絵に乗る。最小設定でも50MHz以上
        #                     なので折り返しは残るが、それより上の雑音は減らせる。
        #                     実行時に振って効果を測れるようCONFIGから変えられる。
        #          0x1B<-coarse_gain_gb(粗アナログゲイン Green/Blue。既定0x77=1.2倍)
        #          0x1C<-coarse_gain_r (粗アナログゲイン Red。既定0x07=1.2倍)
        #          0x1F<-coarse_off_g  (粗アナログオフセット Green。既定0x10)
        #          0x20<-coarse_off_r  (粗アナログオフセット Red。既定0x10)
        #                     コンポジット(緑にCVBS)とS端子(緑にY/赤にC)で、チャネル
        #                     ごとに振幅が違うため独立に振れる必要がある。詳細は
        #                     cfg_coarse_gain_gb / cfg_coarse_gain_r の説明。
        WR_REG = [0x19, 0x0E, 0x17, 0x18, 0x31, 0x10, 0x11, 0x12, 0x13,
                  0x22, 0x36, 0x1A,
                  0x3F, 0x2A, 0x03, 0x05, 0x06,
                  0x08, 0x09, 0x0A, 0x04, 0x1B, 0x1F, 0x1C, 0x20]
        WR_VAL = [MUX1, 0x52, 0x00, 0x01, 0x18, 0x58, 0x75, 3, 3,
                  0x08, 0x00, 0x12,
                  0x0F, 0x87, 0x18, 0x32, 0x20,
                  35, 33, 39, 0x80, 0x77, 0x10, 0x07, 0x10]
        if pll_divide:
            # 0x01=PLL divide[11:4], 0x02=[7:4]にPLL divide[3:0]。データシート指定どおり
            # MSBs(0x01)を先に書く。
            WR_REG += [0x01, 0x02]
            WR_VAL += [(pll_divide >> 4) & 0xFF, (pll_divide & 0x0F) << 4]
        RD_REG = [0x14, 0x37, 0x38, 0x39, 0x3A]
        NWRITE = len(WR_REG); NREAD = len(RD_REG); NSTEP = NWRITE + NREAD
        step = Signal(max=NSTEP + 1)
        wreg_rom = Array(Constant(v, 8) for v in WR_REG)
        wval_rom = Array(Constant(v, 8) for v in WR_VAL)
        rreg_rom = Array(Constant(v, 8) for v in RD_REG)
        rstep = Signal(max=NREAD + 1); self.comb += rstep.eq(step - NWRITE)
        is_wstep = Signal(); self.comb += is_wstep.eq(step < NWRITE)
        reg_b = Signal(8); self.comb += reg_b.eq(Mux(is_wstep, wreg_rom[step], rreg_rom[rstep]))
        wval_b = Signal(8); self.comb += wval_b.eq(wval_rom[step])
        # 分周比(0x01/0x02)の書込値は cfg_pll_divide から取る(後の代入が優先される)
        if 0x01 in WR_REG:
            self.comb += If(step == WR_REG.index(0x01),
                            wval_b.eq(self.cfg_pll_divide[4:12]))
        if 0x02 in WR_REG:
            self.comb += If(step == WR_REG.index(0x02),
                            wval_b.eq(Cat(C(0, 4), self.cfg_pll_divide[0:4])))
        if 0x3F in WR_REG:
            self.comb += If(step == WR_REG.index(0x3F),
                            wval_b.eq(self.cfg_video_bw))
        if 0x2A in WR_REG:
            self.comb += If(step == WR_REG.index(0x2A),
                            wval_b.eq(self.cfg_fine_clamp))
        if 0x03 in WR_REG:
            self.comb += If(step == WR_REG.index(0x03),
                            wval_b.eq(self.cfg_pll_ctl))
        if 0x05 in WR_REG:
            self.comb += If(step == WR_REG.index(0x05),
                            wval_b.eq(self.cfg_clamp_start))
        if 0x06 in WR_REG:
            self.comb += If(step == WR_REG.index(0x06),
                            wval_b.eq(self.cfg_clamp_width))
        if 0x0E in WR_REG:
            self.comb += If(step == WR_REG.index(0x0E),
                            wval_b.eq(self.cfg_sync_ctl))
        if 0x10 in WR_REG:
            # 10h は SOG Threshold[7:3] とクランプ基準選択[2:0]の相乗り。生の8bitを
            # 1本のキーで書くと、片方を触るともう片方を巻き込んで壊す(背景が紫がかる
            # 問題の原因がこれだった)。**別々のキーで持ち、書くときに連結する。**
            self.comb += If(step == WR_REG.index(0x10),
                            wval_b.eq(Cat(self.cfg_clamp_sel, self.cfg_sog_thresh)))
        if 0x19 in WR_REG:
            self.comb += If(step == WR_REG.index(0x19),
                            wval_b.eq(self.cfg_in_mux1))
        if 0x1B in WR_REG:
            self.comb += If(step == WR_REG.index(0x1B),
                            wval_b.eq(self.cfg_coarse_gain_gb))
        if 0x1C in WR_REG:
            # 1Ch は [7:4]=Reserved / [3:0]=Red Gain。1Bh とビット割りが違う
            self.comb += If(step == WR_REG.index(0x1C),
                            wval_b.eq(self.cfg_coarse_gain_r))
        if 0x1F in WR_REG:
            self.comb += If(step == WR_REG.index(0x1F),
                            wval_b.eq(Cat(self.cfg_coarse_off_g, C(0, 2))))
        if 0x20 in WR_REG:
            self.comb += If(step == WR_REG.index(0x20),
                            wval_b.eq(Cat(self.cfg_coarse_off_r, C(0, 2))))
        if 0x11 in WR_REG:
            self.comb += If(step == WR_REG.index(0x11),
                            wval_b.eq(self.cfg_sep_thresh))
        if 0x12 in WR_REG:
            self.comb += If(step == WR_REG.index(0x12),
                            wval_b.eq(self.cfg_precoast))
        if 0x13 in WR_REG:
            self.comb += If(step == WR_REG.index(0x13),
                            wval_b.eq(self.cfg_postcoast))
        if 0x22 in WR_REG:
            self.comb += If(step == WR_REG.index(0x22),
                            wval_b.eq(self.cfg_sync_ctl2))
        if 0x36 in WR_REG:
            self.comb += If(step == WR_REG.index(0x36),
                            wval_b.eq(self.cfg_sync_bypass))
        if 0x1A in WR_REG:
            self.comb += If(step == WR_REG.index(0x1A),
                            wval_b.eq(self.cfg_in_mux2))
        if 0x04 in WR_REG:
            self.comb += If(step == WR_REG.index(0x04),
                            wval_b.eq(Cat(C(0, 3), self.cfg_phase)))
        for _reg, _sig in ((0x08, "cfg_gain_b"), (0x09, "cfg_gain_g"), (0x0A, "cfg_gain_r")):
            if _reg in WR_REG:
                self.comb += If(step == WR_REG.index(_reg),
                                wval_b.eq(getattr(self, _sig)))

        # FORMAT
        fi = Signal(5)   # 0..20 (NFMT=21)
        def hexch(nib):
            return Mux(nib < 10, ord('0') + nib, ord('A') - 10 + nib)

        self.submodules.fsm = fsm = FSM(reset_state="POR0")

        # 1) RESET: resetb=0 保持 -> 1 解除 -> 少し待つ
        fsm.act("POR0",
            self.resetb.eq(0),
            NextValue(rst_cnt, rst_cnt+1),
            If(rst_cnt == int(sys_clk_freq//100), NextValue(rst_cnt,0), NextState("POR1")),
        )
        fsm.act("POR1",
            self.resetb.eq(1),
            NextValue(rst_cnt, rst_cnt+1),
            If(rst_cnt == int(sys_clk_freq//100), NextState("OI_START")),
        )

        # 2) OLED 初期化フレーム: START,0x78,0x00,<INIT>,STOP
        fsm.act("OI_START", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextValue(idx,0), NextState("OI_BODY")))
        fsm.act("OI_BODY", self.resetb.eq(1),
            *issue(OP_WRITE, Mux(idx==0, OLED_W, Mux(idx==1, 0x00, init_rom[idx-2]))),
            If(m.done,
                If(idx == 2+NINIT-1, NextState("OI_STOP")).Else(NextValue(idx, idx+1))))
        fsm.act("OI_STOP", self.resetb.eq(1), *issue(OP_STOP),
            If(m.done, NextState("TP_START")))

        # 3) TVP read (reg -> r01/r02): S,0xB8,reg,Sr,0xB9,READ(nack),P
        fsm.act("TP_START", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextState("TP_ADDRW")))
        fsm.act("TP_ADDRW", self.resetb.eq(1), *issue(OP_WRITE, TVP_W),
            If(m.done, NextValue(self.tvp_ack, ~m.ackr), NextState("TP_REG")))
        fsm.act("TP_REG", self.resetb.eq(1), *issue(OP_WRITE, reg_b),
            If(m.done, If(is_wstep, NextState("TP_WVAL")).Else(NextState("TP_RSTART"))))
        # 書込ステップ: 値を書いてSTOP
        fsm.act("TP_WVAL", self.resetb.eq(1), *issue(OP_WRITE, wval_b),
            If(m.done, NextState("TP_STOP")))
        # 読出ステップ: repeated START, 読出アドレス, READ
        fsm.act("TP_RSTART", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextState("TP_ADDRR")))
        fsm.act("TP_ADDRR", self.resetb.eq(1), *issue(OP_WRITE, TVP_R),
            If(m.done, NextState("TP_READ")))
        fsm.act("TP_READ", self.resetb.eq(1), *issue(OP_READ, nack=1),
            If(m.done,
                Case(rstep, {
                    0: NextValue(self.syncdet, m.rdata),
                    1: NextValue(self.lpf_hi, m.rdata),
                    2: NextValue(self.lpf_lo, m.rdata),
                    3: NextValue(self.cpl_hi, m.rdata),
                    4: NextValue(self.cpl_lo, m.rdata),
                }),
                NextState("TP_STOP")))
        fsm.act("TP_STOP", self.resetb.eq(1), *issue(OP_STOP),
            If(m.done,
                If(step == NSTEP - 1,
                    NextValue(step, 0), NextValue(fi, 0), NextState("FMT"),
                ).Else(
                    NextValue(step, step + 1), NextState("TP_START"),
                )))

        # 4) FORMAT: 動的21文字(ACK/SYNC/LPF/CPL/UP)をテキストRAMへ書込
        NFMT = 21
        faddr = Signal(7); fchar = Signal(8)
        up = sec
        self.comb += [
            Case(fi, {
                0:  [faddr.eq(38), fchar.eq(Mux(self.tvp_ack, ord('A'), ord('N')))],
                1:  [faddr.eq(39), fchar.eq(Mux(self.tvp_ack, ord('C'), ord('A')))],
                2:  [faddr.eq(40), fchar.eq(ord('K'))],
                3:  [faddr.eq(56), fchar.eq(hexch(self.syncdet[4:8]))],
                4:  [faddr.eq(57), fchar.eq(hexch(self.syncdet[0:4]))],
                # Lines/Frame[11:0]=(0x38[3:0]<<8)|0x37 → 4桁表示 0x0NNN(上位nibは常に0)
                5:  [faddr.eq(72), fchar.eq(ord('0'))],
                6:  [faddr.eq(73), fchar.eq(hexch(self.lpf_lo[0:4]))],
                7:  [faddr.eq(74), fchar.eq(hexch(self.lpf_hi[4:8]))],
                8:  [faddr.eq(75), fchar.eq(hexch(self.lpf_hi[0:4]))],
                # Clocks/Line[11:0]=(0x3A[3:0]<<8)|0x39 → 4桁表示 0x0NNN
                9:  [faddr.eq(88), fchar.eq(ord('0'))],
                10: [faddr.eq(89), fchar.eq(hexch(self.cpl_lo[0:4]))],
                11: [faddr.eq(90), fchar.eq(hexch(self.cpl_hi[4:8]))],
                12: [faddr.eq(91), fchar.eq(hexch(self.cpl_hi[0:4]))],
                13: [faddr.eq(104), fchar.eq(hexch(up[28:32]))],
                14: [faddr.eq(105), fchar.eq(hexch(up[24:28]))],
                15: [faddr.eq(106), fchar.eq(hexch(up[20:24]))],
                16: [faddr.eq(107), fchar.eq(hexch(up[16:20]))],
                17: [faddr.eq(108), fchar.eq(hexch(up[12:16]))],
                18: [faddr.eq(109), fchar.eq(hexch(up[8:12]))],
                19: [faddr.eq(110), fchar.eq(hexch(up[4:8]))],
                20: [faddr.eq(111), fchar.eq(hexch(up[0:4]))],
            }),
        ]
        fsm.act("FMT", self.resetb.eq(1),
            twr.adr.eq(faddr), twr.dat_w.eq(fchar), twr.we.eq(1),
            If(fi == NFMT - 1, NextState("OD_START")).Else(NextValue(fi, fi+1)))

        # 5) OLED data フレーム: START,0x78,0x40,<1024>,STOP
        fsm.act("OD_START", self.resetb.eq(1), *issue(OP_START),
            If(m.done, NextValue(idx,0), NextValue(prefetch,0), NextState("OD_BODY")))
        fsm.act("OD_BODY", self.resetb.eq(1),
            # idx0,1 は 0x78,0x40。idx>=2 はフォントバイト(2サイクルprefetch)
            If(idx < 2,
                *issue(OP_WRITE, Mux(idx==0, OLED_W, 0x40)),
                If(m.done, NextValue(idx, idx+1)),
            ).Else(
                # prefetch: 0->1->2 でfrd.dat_r確定
                If(prefetch != 2,
                    NextValue(prefetch, prefetch+1),
                ).Else(
                    *issue(OP_WRITE, frd.dat_r),
                    If(m.done,
                        NextValue(prefetch, 0),
                        If(idx == 2+1024-1, NextState("OD_STOP")).Else(NextValue(idx, idx+1))),
                ),
            ),
        )
        fsm.act("OD_STOP", self.resetb.eq(1), *issue(OP_STOP),
            If(m.done, NextValue(rst_cnt,0), NextState("DWELL")))
        # 6) 次周まで少し待つ(~30fps)
        dwell = Signal(24)
        fsm.act("DWELL", self.resetb.eq(1),
            NextValue(dwell, dwell+1),
            If(dwell == int(sys_clk_freq/30), NextValue(dwell,0), NextState("TP_START")))

        # --- 実機pad(open-drain SDA/SCL + push-pull RESETB) ---
        if pads is not None:
            for sig, low in ((pads.scl, m.scl_low), (pads.sda, m.sda_low)):
                t = TSTriple(); self.specials += t.get_tristate(sig)
                self.comb += [t.o.eq(0), t.oe.eq(low)]
                if sig is pads.sda:
                    self.comb += m.sda_in.eq(t.i)
            if hasattr(pads, "resetb"):
                self.comb += pads.resetb.eq(self.resetb)
