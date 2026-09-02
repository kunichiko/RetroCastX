#!/usr/bin/env python3
"""映像ソースのプロファイルから pll_divide を決める。

pll_divide は「1ラインを何サンプルで取るか」= H-PLLの帰還分周比で、
1サンプル=1ドットにするには入力の水平トータル(htotal)に一致させる必要がある。

絵の内容から探す方法(スペクトル占有率、鮮鋭度の山登り)は、絵が真っ黒だったり
細かい模様が無かったりすると当てにならず、実機でも変な値に着地することが多かった。

こちらは信号だけで決める。レトロPCのドットクロックは水晶を分周した有限個の値
しか取らないので、

    htotal = f_dot / fH

の f_dot に候補を入れて「整数になるもの」を選べばよい。fH は pll_divide に
依存しない絶対値として測れている(生HSYNCのエッジを1秒数える。CONFIG key 0x2A)。

精度: fH の測定誤差は ±1Hz。htotal への影響は htotal/fH で、31.5kHz・htotal 1104
なら ±0.035カウント。整数を選び分けるには十分すぎる。実際、X68000の3帯域すべてで
正解の残差は 0.008〜0.035、外れは 0.2〜0.5 と桁で分かれた。

実測で確かめた対応(このプロジェクトの記録と一致):
    31.499kHz → 69.55199MHz/2 = 34.776MHz → htotal 1104(残差 0.035)  768x512テキスト
    31.499kHz → 69.55199MHz/3 = 23.184MHz → htotal  736(2026-09-02実測)  512x512グラフィック
    24.699kHz → 69.55199MHz/2 = 34.776MHz → htotal 1408(残差 0.008)
    15.980kHz → 38.86363MHz/2 = 19.432MHz → htotal 1216(残差 0.008)
"""
from dataclasses import dataclass, field
from typing import List, Optional, Tuple

# TVP7002のピクセルクロック下限[Hz]。データシートの保証範囲は12〜165MHz で、
# 下回るとクランプが効かなくなり画面の下ほど色がずれる(実測: 12.02MHz は正常、
# 11.25MHz から崩れ、9.72MHz では白の青/赤比が上下で0.43違った)。
TVP_DOTCLK_MIN = 12.0e6
# gateware 側の制限と同じ。超えるとDATACLKがpixドメインのタイミング制約を
# 超え、実機ではボードごとハングした(4095で14秒)。
PLL_MIN, PLL_MAX = 200, 2304
# ラインバッファの幅[サンプル](gateware の TvpCapture width)。1ラインが
# 入り切らないと外接矩形が有効映像の幅を表さなくなり、以降の判断が全部狂う。
# pll_divide の実用上限はこちら。
LINE_BUFFER_W = 2048


@dataclass(frozen=True)
class Profile:
    """ある映像ソースが出せるドットクロックの集合。

    oscillators × dividers で候補を作る。実機の水晶と分周器をそのまま書く方が、
    モードごとのドットクロックを列挙するより漏れにくい。
    """
    name: str
    desc: str
    oscillators: Tuple[Tuple[str, float], ...]
    dividers: Tuple[int, ...] = (1, 2, 4, 8)
    # htotal の粒度。X68000のCRTCは水平トータルを8ドット単位で持つので、
    # 正解は必ず8の倍数になる。この制約が候補をさらに絞る
    htotal_multiple: int = 1
    # 実機で裏を取れているか。取れていないものは候補の並びを見る参考に留める
    verified: bool = False

    def dot_clocks(self):
        for label, f in self.oscillators:
            for d in self.dividers:
                yield (f"{label}/{d}" if d != 1 else label), f / d


PROFILES = {
    # 実測で裏を取った唯一のプロファイル。3帯域すべてが2つの水晶の分周で説明できた
    "x68000": Profile(
        name="x68000",
        desc="X68000(水晶 69.55199MHz / 38.86363MHz、CRTCは8ドット単位)",
        oscillators=(("69.55199MHz", 69.55199e6), ("38.86363MHz", 38.86363e6)),
        dividers=(1, 2, 3, 4, 8),   # ★3 は512ドット系グラフィック(/3=23.184MHz→htotal 736)
        htotal_multiple=8,
        verified=True,
    ),
    # 以下は未検証。実機で確かめるまでは「候補の並びを見る」用途に留めること。
    # 数値の出どころを書いておく: VGAはIBM VGAの標準ドットクロック。
    "vga": Profile(
        name="vga",
        desc="IBM VGA互換(25.175MHz / 28.322MHz)【未検証】",
        oscillators=(("25.175MHz", 25.175e6), ("28.322MHz", 28.322e6)),
        dividers=(1, 2),
    ),
    "pc98": Profile(
        name="pc98",
        desc="PC-9801(21.0525MHz / 25.175MHz)【未検証】",
        oscillators=(("21.0525MHz", 21.0525e6), ("25.175MHz", 25.175e6)),
        dividers=(1, 2),
    ),
}


@dataclass
class Candidate:
    htotal: int             # 整数に丸めた水平トータル[ドット]
    f_dot: float            # そのときのドットクロック[Hz]
    label: str              # どの水晶/分周から来たか
    residual: float         # 整数からのずれ[カウント]
    multiple_ok: bool       # プロファイルの粒度(8の倍数など)を満たすか
    pll_divide: int         # 実際に設定する値(TVPの下限を満たすよう整数倍した後)
    oversample: int         # 何倍にしたか(1なら1サンプル=1ドット)

    @property
    def rel_err(self) -> float:
        """相対周波数誤差。どの水晶から来たかを見分けるのはこちら。

        残差[カウント]は htotal に比例するので、同じ水晶の分周違い(276/552/
        1104/2208)を比べると小さい方が必ず良く見えてしまい、選定に使えない。
        残差/htotal にすると分周に依らない量になり、水晶が合っていれば同じ値
        (実測 3.2e-5)、違う水晶なら桁で外れる(1.6e-4)。
        """
        return self.residual / max(self.htotal, 1)

    def __str__(self):
        s = (f"htotal {self.htotal:5d}  f_dot {self.f_dot/1e6:8.4f}MHz  "
             f"残差 {self.residual:.3f}  ({self.label})")
        if self.oversample > 1:
            s += f"  → pll_divide {self.pll_divide} (×{self.oversample})"
        else:
            s += f"  → pll_divide {self.pll_divide}"
        if not self.multiple_ok:
            s += "  ※粒度を満たさない"
        return s


def candidates(prof: Profile, fh_hz: float,
               max_residual: float = 0.15,
               max_pll: int = LINE_BUFFER_W) -> List[Candidate]:
    """fH[Hz] から htotal 候補を出す。確からしい順(残差の小さい順)。

    max_residual は「整数からどれだけ離れていても候補に残すか」[カウント]。
    fH の測定誤差(±1Hz)が htotal に効く量は htotal/fH なので、31.5kHz なら
    0.035程度。0.15 は5倍近い余裕で、測定が1秒未満で粗いときも拾える。
    """
    out = []
    for label, f_dot in prof.dot_clocks():
        if fh_hz <= 0:
            continue
        ht = f_dot / fh_hz
        n = int(round(ht))
        if n < 8:
            continue
        resid = abs(ht - n)
        if resid > max_residual:
            continue
        # TVPの下限を満たす最小の整数倍にする。オーバーサンプリングなので情報は
        # 失われず、8の倍数のままでもある(15kHz 512x512 の 1:1 は 608 = 9.7MHz
        # で下限割れなので、2倍の1216を使う)
        pll, over = n, 1
        while f_dot * over < TVP_DOTCLK_MIN and pll * 2 <= max_pll:
            pll *= 2
            over *= 2
        if not (PLL_MIN <= pll <= max_pll):
            continue
        out.append(Candidate(
            htotal=n, f_dot=f_dot, label=label, residual=resid,
            multiple_ok=(n % prof.htotal_multiple == 0),
            pll_divide=pll, oversample=over))
    out.sort(key=lambda c: (not c.multiple_ok, c.rel_err, -c.pll_divide))
    return out


def best(prof: Profile, fh_hz: float, **kw) -> Optional[Candidate]:
    """いちばん確からしい候補を1つ返す。

    選び方は2段構え:

    1. 相対誤差でどの水晶かを決める。正しい水晶の分周違いは相対誤差が同じ値に
       揃うので、これで一族が分かる
    2. その一族の中で「ラインバッファに収まる最大の pll_divide」を採る

    2. の向きが重要。真の htotal より小さい値を選ぶとドットを取りこぼす(1104が
    正解のときに552を選ぶと1ドットおきにしか読まない = 破壊的)のに対し、大きい
    値は単なる整数倍オーバーサンプルで情報は失われない。だから迷ったら大きい側へ
    倒す。上限はラインバッファ幅で、これを超えると1ラインが入り切らなくなる。
    """
    cs = candidates(prof, fh_hz, **kw)
    if not cs:
        return None
    # 粒度を満たすものがあればその中だけで選ぶ
    pool = [c for c in cs if c.multiple_ok] or cs
    lo = min(c.rel_err for c in pool)
    # 同じ水晶から来たものだけに絞る。相対誤差は桁で分かれるので3倍で十分切れる
    fam = [c for c in pool if c.rel_err <= max(lo * 3.0, 1e-9)]
    return max(fam, key=lambda c: c.pll_divide)


def best_over_all(fh_hz: float, **kw):
    """全プロファイルを試して、いちばん確からしい候補を返す。

    正解の残差(0.008〜0.035)と外れ(0.2〜0.5)は桁で違うので、プロファイルを
    人が選ばなくても判別できることが多い。返り値は (プロファイル, 候補)。
    """
    ranked = []
    for prof in PROFILES.values():
        c = best(prof, fh_hz, **kw)
        if c is not None:
            ranked.append((prof, c))
    if not ranked:
        return None
    ranked.sort(key=lambda pc: (not pc[1].multiple_ok, pc[1].residual))
    return ranked[0]
