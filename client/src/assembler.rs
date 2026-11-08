//! Frame reassembly (mirror of `host/python/retrocastx/receiver.py` FrameAssembler).
//! Output is RGBA8 so the frame can go straight to a GPU texture.

use crate::decode_worker::DecodeWorker;
use crate::ntsc;
use crate::protocol::{self as proto, Packet};

pub struct CompletedFrame {
    pub frame_idx: u16,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub fill_ratio: f32,
    /// 遅延計測用の時刻。受信側が Shared へ渡す直前に埋める
    pub stamps: Option<crate::latency::FrameStamps>,
}

#[derive(Default)]
pub struct Stats {
    pub lost_packets: u64,
    pub orphan_lines: u64,
    pub packets: u64,
    pub bytes: u64,
    pub frames: u64,
    /// 復調が追いつかず、復調せずに飛ばしたフィールドの数
    pub decode_skipped: u64,
}

pub struct FrameAssembler {
    pub mode: Option<proto::Mode>,
    pub stats: Stats,
    fb: Vec<u8>, // RGBA
    /// YC8のときだけ使う生サンプル(2B/px: 下位=緑ch=CVBS/Y、上位=赤ch=C)。
    ///
    /// **1本ずつ復調できないので溜める。** Y/C分離のコムは上下のラインを
    /// 使うので、フィールドが揃うまで待つ必要がある。フレーム完成時に
    /// まとめて復調して fb へ書く。
    raw: Vec<u8>,
    /// 3次元(フレームコム)用の履歴。**同じライン番号は2ボードフレームおきに
    /// しか来ない**(フィールドごとにパリティが反転する)ので、
    ///   `raw_p2` = 2ボードフレーム前 = 1 NTSCフレーム前 → 副搬送波の位相が180°
    ///   `raw_p4` = 4ボードフレーム前 = 2 NTSCフレーム前 → 位相が0°
    /// になる(実測 175.8° / 4.2°)。前者でフレームコム、後者で動き検出。
    raw_p2: Vec<u8>,
    raw_p4: Vec<u8>,
    /// 行ごとに「何回書かれたか」。3(=p2とp4の両方が実データ)で3次元が使える
    hist_n: Vec<u8>,
    /// 復調の結果(パネル表示と診断用)
    pub ntsc_info: Option<ntsc::Info>,
    width: usize,
    height: usize,
    cur_frame: Option<u16>,
    px_filled: usize,
    last_seq: Option<u16>,
    line_seen: Vec<bool>, // このフレームで各lineを受信したか
    /// 太らせても埋まらなかった行数(診断用)
    pub unfilled_rows: u32,
    /// 直前のフレームで埋まっていた行のパリティ(None=偏りが無く判定不能)
    last_parity: Option<usize>,
    /// パリティが交互になった確度。閾値を超えたらインタレースとみなす
    parity_alt: i32,
    /// 測定で「インタレースの1フィールド」と判定しているか(診断用)
    pub interlace_measured: bool,
    /// **ドット復元(1タップ逆フィルタ)の係数 a を ×1000 した値。0 = 無効。**
    ///
    /// X68000 の 768x512(ドットクロック 34.776MHz)では、**本体の出力段で
    /// 既に1ドットのパルスが立ち切っていない**。2026-09-02 に v0.9.0 実機で実測:
    ///
    ///   1ドット幅 → 白の 64% / 2ドット幅 → 82% / 4ドット以上 → 100%
    ///
    /// オシロでコネクタ直後と基板フィルタ通過後を比べて差が無かったので、
    /// **基板の RC(220Ω×33pF, τ=7.26ns)ではなく信号源側**が原因と確定した。
    ///
    /// 一次ローパスを通った信号をサンプルすると、理想のドット値 d[n] とは
    ///   v[n] = a·v[n-1] + (1-a)·d[n]
    /// の関係になる。孤立1ドットパルスの到達率がそのまま (1-a) なので、
    /// 実測 64% から **a = 0.36** が直接決まる(τ を推定する必要が無い)。
    /// 逆算は1タップで済む:
    ///   d[n] = (v[n] - a·v[n-1]) / (1-a)
    ///
    /// 汎用のピーキングと違い**劣化モデルの厳密な逆演算**なので、a が実測と
    /// 合っていればオーバーシュート(輪郭のリンギング)は出ない。
    /// 高域が 1/(1-a) 倍に持ち上がるぶん雑音も増えるが、実測 noise 0.4% なら問題ない。
    pub dot_a_milli: u32,
    /// RGB888 の3チャンネルが **Pr/Y/Pb**(コンポーネント)か。
    ///
    /// TVP7002 は YPbPr→RGB の色空間変換を持たない(RGB→YCbCr の向きだけ)ので、
    /// ボードは赤ch=Pr / 緑ch=Y / 青ch=Pb をそのまま RGB888 の枠で送ってくる。
    /// ここで BT.601 のスタジオレンジ(Y 16〜235 / 色差 128±112)として RGB に直す。
    /// レベルの根拠は profiles.rs の REGS_COMPONENT(粗ゲイン1.2倍・粗オフセット+16)。
    pub ycbcr: bool,
    /// コンポーネントの黒の基準(行ごと)。[Y, Cb, Cr] の生コード。
    ///
    /// ★**ライン先頭のバックポーチを毎ライン測って、そこを Y=16 / 色差=128 とみなす**
    ///   (デジタルのクランプ)。TVP の粗オフセットで黒は 16 付近に来る設計だが、
    ///   実機では 17〜20、ラインごとにも 19〜23 と揺れ、色差の中心も 126〜127 だった
    ///   (2026-11-08)。黒が (2,7,2) の緑がかった灰色になるのを、固定値ではなく
    ///   実測で消す。ラインごとに測るので、ライン間の揺れも一緒に取れる。
    ///   (行が始まる断片(offset 0)が来たときに更新し、来なければ前回の値を使う)
    ycc_ref: Vec<[i32; 3]>,
    /// ycc_ref の全体の目安(×16 の固定小数)。行の値が信用できないときに使う
    ycc_ref_avg: [i32; 3],
    /// 行ごとの「直前に書いた生の値」と「その次のx」。断片化したラインで
    /// v[n-1] を跨いで繋ぐために持つ。**復元後ではなく生の値**を保つこと。
    last_raw: Vec<[u8; 3]>,
    last_x: Vec<usize>,
    decay: f32,           // 欠損ラインの減衰率(1.0=前フレーム保持のまま, 0.8=毎フレーム80%へ暗転)
    /// インターレース時の減衰率。既定 1.0(=減衰しない)。
    ///
    /// インターレースでは「毎フレーム半分の行が来ない」のが正常状態なので、
    /// パケットロス用の減衰(decay)をそのまま適用すると全行が 100%⇄80% を
    /// フィールドレートで往復し、面全体がちらつく(実機で確認)。プロトコルの
    /// 想定も「前フィールドの行が残るため自然に weave 合成になる」なので、
    /// 既定は減衰なしにする。
    /// 一方、CRTの残光を模すなら少し減衰させるのが実物に近いという見方もあり、
    /// プログレッシブ変換的にチラつき無しで見たい人もいる。好みの幅があるので
    /// 設定で変えられるようにしてある。
    interlace_decay: f32,
    /// 復調を止めて生のYをそのまま見る(切り分け用)
    raw_view: bool,
    /// 3次元コム(前後のフレームを使う Y/C 分離)を使うか。切ると2次元だけになる。
    ///
    /// ★**モバイルの CPU では追いつかない。** Pixel 10 で 8fsc のコンポジットを
    ///   復調すると組立が 720ms まで遅れ、受信キューから溢れた。静止部分の色の
    ///   にじみが少し増える代わりに負担が減る
    pub comb3d: bool,
    /// 見た目の調整(彩度・明るさ・コントラスト・色相)
    adjust: ntsc::Adjust,
    /// 表示するフィールド。0=織り込み(通常) / 1=偶数スロットのみ / 2=奇数スロットのみ。
    /// 選んだ側の行を隣のスロットへ複製して全高で出す(=ラインダブラ)。
    field_view: u8,
    /// 公開用バッファの使い回し先。UIが使い終わったものを recycle() で戻す。
    /// 毎フレームの確保をなくすためだけのもので、中身に意味は無い。
    spare: Option<Vec<u8>>,
    /// 復調を任せる常駐スレッド。None なら emit() の中で復調する
    worker: Option<DecodeWorker>,
    /// まだ復調へ渡せていないフィールドの届いた行
    pending_seen: Vec<bool>,
    /// まだ復調へ渡せていないフィールドの仕上げの条件
    pending: Option<Pending>,
    /// 直前の feed() でフィールドを閉じて復調へ回した
    field_end: bool,
}

struct Pending {
    interlaced: bool,
    decay: f32,
    frame_idx: u16,
    fill_ratio: f32,
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self {
            mode: None,
            stats: Stats::default(),
            fb: Vec::new(),
            raw: Vec::new(),
            raw_p2: Vec::new(),
            raw_p4: Vec::new(),
            hist_n: Vec::new(),
            ntsc_info: None,
            spare: None,
            worker: None,
            pending_seen: Vec::new(),
            pending: None,
            field_end: false,
            width: 0,
            height: 0,
            cur_frame: None,
            px_filled: 0,
            last_seq: None,
            line_seen: Vec::new(),
            unfilled_rows: 0,
            last_parity: None,
            parity_alt: 0,
            interlace_measured: false,
            dot_a_milli: 0,
            ycbcr: false,
            ycc_ref: Vec::new(),
            ycc_ref_avg: [16 * 16, 128 * 16, 128 * 16],
            last_raw: Vec::new(),
            last_x: Vec::new(),
            decay: 0.8,
            interlace_decay: 1.0,
            raw_view: false,
            comb3d: true,
            field_view: 0,
            adjust: ntsc::Adjust::default(),
        }
    }

    /// このフレームで埋まった行のパリティ。偏りが無ければ None。
    ///
    /// ボードは行位置を「フィールド内の行×2 + 極性」で送るので、
    /// **プログレッシブでもインタレースでも1つ飛びに埋まる**。違うのは:
    ///
    ///     プログレッシブ  極性が固定 → パリティが**常に同じ**
    ///     インタレース    フィールドごとに極性が反転 → パリティが**交互**
    ///
    /// なのでパリティ単体では判定できず、フレーム間の変化を見る必要がある。
    fn filled_parity(&self) -> Option<usize> {
        let (mut odd, mut even) = (0usize, 0usize);
        for (i, s) in self.line_seen.iter().enumerate() {
            if *s {
                if i & 1 == 1 { odd += 1 } else { even += 1 }
            }
        }
        let total = odd + even;
        // 片方が全体の1割未満なら「そのパリティに偏っている」とみなす。
        // 完全一致にすると、1本ドロップや迷子ラインで判定が飛ぶ
        if total < 16 || odd.min(even) * 10 >= total {
            return None;
        }
        Some(if odd > even { 1 } else { 0 })
    }

    /// **インタレースかどうかを測定で決める。** mflags では決まらない。
    ///
    /// `mflags bit0` は「スロットが vtotal 個(フレーム単位の織り込み)」の意味で、
    /// **VSYNCがフィールドごとに来る本物のインタレースでは 0** になる。それを
    /// 「インタレースではない」と読むと2つ壊れる(どちらも実機で出た):
    ///
    ///   1. 太らせが動いて、空いているスロット(=別フィールドの行)に**隣の行を
    ///      複製する**。次のフィールドでは逆向きに上書きされるので、各行が
    ///      「本物の内容」と「隣の行のコピー」をフィールドレートで往復する。
    ///      拡大すると同じラインに縞が交互に出てチラチラ見える
    ///   2. 欠損減衰(0.8)が半分の行に毎フィールド掛かる(1で全行が埋まって
    ///      しまうので実際には出番が無かったが、太らせを止めると効いてくる)
    ///
    /// 判定はパリティの交互性。非対称に増減させて、途切れたら早く抜ける。
    fn update_interlace_measure(&mut self) {
        let p = self.filled_parity();
        if let (Some(cur), Some(last)) = (p, self.last_parity) {
            self.parity_alt += if cur != last { 1 } else { -2 };
            self.parity_alt = self.parity_alt.clamp(0, 8);
        }
        if p.is_some() {
            self.last_parity = p;
        }
        self.interlace_measured = self.parity_alt >= 3;
    }

    /// 欠損ライン減衰率を設定(1.0で従来の前フレーム保持)。
    pub fn set_decay(&mut self, d: f32) {
        self.decay = d;
    }

    /// インターレース時の減衰率を設定(1.0=減衰しない)。
    /// 復調を止めて生のYを見るか。**artefactの出所を分けるための道具。**
    /// ライン受信時に fb へ書いているグレースケールがそのまま残る。
    pub fn set_raw_view(&mut self, v: bool) {
        self.raw_view = v;
    }

    /// 片方のフィールドだけを見る。0=織り込み / 1=偶数スロット / 2=奇数スロット。
    ///
    /// **織り込みの影響を外して見るための道具。** 縦線が二重に見えるとき、
    /// 2枚のフィールドがずれているのか、1枚の中で既に二重なのかを分けられる。
    /// 選んだ側を隣のスロットへ複製するので、縦解像度は半分だが全高で出る
    /// (黒で間引くとスキャンラインが乗って、かえって見分けにくい)。
    /// 見た目の調整を差し替える。**復調の校正は触らない**(数値で追えなくなるため)
    pub fn set_adjust(&mut self, a: ntsc::Adjust) {
        self.adjust = a;
    }

    pub fn set_field_view(&mut self, v: u8) {
        self.field_view = v;
    }

    pub fn set_interlace_decay(&mut self, d: f32) {
        self.interlace_decay = d;
    }

    fn track_seq(&mut self, seq: u16) {
        if let Some(last) = self.last_seq {
            let gap = seq.wrapping_sub(last);
            if gap == 0 || gap > 0x8000 {
                return; // duplicate or reordered
            }
            self.stats.lost_packets += (gap - 1) as u64;
        }
        self.last_seq = Some(seq);
    }

    /// Feed one datagram; returns a frame completed by this packet, if any.
    /// コンポーネントの行の黒の基準を返す(行の頭の断片ならバックポーチで測り直す)。
    ///
    /// `px` は RGB888 の枠(byte0=Pr / byte1=Y / byte2=Pb)で、ラインの `off` 画素目
    /// から始まる。バックポーチの位置は BT.601 の 858 サンプル/ラインで、ボードが
    /// 同期の立ち下がりからラインを取る(hs_offset=0)前提。480i(13.5MHz)も
    /// 480p(27MHz)も同期 0〜62・バックポーチ 62〜120 で同じ位置に来る(実測:
    /// 同期は 0〜61 がコード0)。
    ///
    /// ★**送出範囲を固定(0x38/0x39)していると、バックポーチは終わりの数サンプル
    ///   しか届かない**(1ラインを1パケットに収めるため 112 から送る)。届いた分と
    ///   測定窓 72..120 の重なりで測り、サンプルが少ないときは行ごとにフレームを
    ///   またいで均して雑音を抑える。
    fn update_ycc_ref(&mut self, row: usize, off: usize, px: &[u8]) -> [i32; 3] {
        const BP_START: usize = 72;
        const BP_END: usize = 120;
        /// これより少ないと測らない(1サンプルの雑音で黒が揺れる)
        const MIN_SAMPLES: usize = 4;
        let avg = self.ycc_ref_avg.map(|v| (v + 8) >> 4);
        let Some(slot) = self.ycc_ref.get_mut(row) else {
            return avg;
        };
        let lo = BP_START.max(off);
        let hi = BP_END.min(off + px.len() / 3);
        if hi >= lo + MIN_SAMPLES {
            let mut sum = [0i32; 3];
            for p in px[(lo - off) * 3..(hi - off) * 3].chunks_exact(3) {
                sum[0] += p[1] as i32;
                sum[1] += p[2] as i32;
                sum[2] += p[0] as i32;
            }
            let n = (hi - lo) as i32;
            let mut m = sum.map(|v| (v + n / 2) / n);
            // 少ないサンプルで測った行は、前フレームの同じ行と均す
            if hi - lo < 16 && *slot != [16, 128, 128] {
                for c in 0..3 {
                    m[c] = (m[c] + slot[c] + 1) / 2;
                }
            }
            // 垂直帰線の行(バックポーチに等化パルスが来る)や絵の頭が入り込んだ行は
            // 外れ値になる。目安から大きく離れたら使わない
            if (m[0] - avg[0]).abs() <= 12
                && (m[1] - avg[1]).abs() <= 12
                && (m[2] - avg[2]).abs() <= 12
            {
                *slot = m;
            } else {
                *slot = avg;
            }
            // 目安は外れ値も含めて少しずつ寄せる(1行で動くのは最大 1/4 コード)。
            // 外れ値で弾くだけにすると、本当の黒が目安から 12 以上離れていたときに
            // 二度と追いつけない
            for c in 0..3 {
                let d = (m[c] * 16 - self.ycc_ref_avg[c]) / 64;
                self.ycc_ref_avg[c] += d.clamp(-4, 4);
            }
        }
        *slot
    }

    pub fn feed(&mut self, datagram: &[u8]) -> Option<CompletedFrame> {
        let pkt = proto::parse(datagram).ok()?;
        self.stats.packets += 1;
        self.stats.bytes += datagram.len() as u64;
        match pkt {
            // 音声は receiver 側で再生器へ渡す。ここでは共通seq空間の追跡だけ。
            Packet::Audio(a) => {
                self.track_seq(a.seq);
                None
            }
            Packet::Announce(a) => {
                self.track_seq(a.seq); // ANNOUNCEも共通seq空間を消費する
                None
            }
            Packet::Mode(m) => {
                self.track_seq(m.seq);
                // フレームバッファの作り直しは解像度が変わった時だけ(毎秒のMODEで
                // 描画中フレームを捨てないため)。
                // 幅は hactive ではなく htotal に絞る。
                //
                // hactive はボードのキャプチャ幅(2048固定。どのモードの htotal も
                // 入る大きさ)で、実際に画素が書かれるのは 0..htotal だけ。31kHz なら
                // 1104 で、残り944列は常に黒のまま毎フレーム確保・複製・GPU転送されて
                // いた。2048×1136×4 = 9.3MB/フレーム、54fpsで約500MB/秒がフレームの
                // clone とテクスチャ転送の両方にかかり、受信スレッドが詰まって
                // UDPを取りこぼした(Windowsの低スペック機で lost 9.9%)。
                // htotal に絞ると 1104×1136×4 = 5.0MB で46%削減できる。
                //
                // シェーダは絶対サンプル位置をテクスチャ幅で割って参照するので、
                // 幅が変わっても対応は崩れない(位置は 0..htotal に収まる)。
                let want_w = if m.htotal > 0 {
                    (m.htotal as usize).min(m.hactive as usize)
                } else {
                    m.hactive as usize
                };
                let resized = self.width != want_w
                    || self.height != m.vactive as usize;
                if self.mode.as_ref().map(|c| c.mode_id) != Some(m.mode_id) || resized {
                    self.width = want_w;
                    self.height = m.vactive as usize;
                    self.fb = vec![0u8; self.width * self.height * 4];
                    // alpha=255 で初期化
                    for px in self.fb.chunks_exact_mut(4) {
                        px[3] = 255;
                    }
                    // YC8のときだけ生サンプルを溜める(2B/px)。他の形式では
                    // 使わないので確保しない(1820×526×2 = 1.9MB)
                    let need = if proto::is_raw8(m.pixfmt) {
                        self.width * self.height * 2
                    } else {
                        0
                    };
                    self.raw = vec![0u8; need];
                    // 3次元コム用の履歴2面。1820×526×2 = 1.9MB ずつ
                    self.raw_p2 = vec![0u8; need];
                    self.raw_p4 = vec![0u8; need];
                    self.hist_n = vec![0u8; if need > 0 { self.height } else { 0 }];
                    self.ntsc_info = None;
                    self.line_seen = vec![false; self.height];
                    self.last_raw = vec![[0u8; 3]; self.height];
                    self.ycc_ref = vec![[16, 128, 128]; self.height];
                    self.last_x = vec![usize::MAX; self.height];
                    self.cur_frame = None;
                    self.px_filled = 0;
                }
                // 諸元(dotclk/hfreq/vfreq/htotal/vtotal)はボードが毎秒実測して送るので
                // mode_idが同じでも必ず取り込む。以前はmode_id変化時のみ更新していたため
                // 起動後に測定値が更新されても表示が古いままだった。
                self.mode = Some(m);
                None
            }
            Packet::Line(mut l) => {
                self.track_seq(l.seq);
                // 帯域を減らす形式は、既存の形式に展開してから同じ処理に流す
                // (YCC422 → RGB888 の Pr/Y/Pb、CVBS8 → YC8)。復調・色変換・黒の
                // 基準取りを形式ごとに複製しないため
                let expanded: Vec<u8>;
                match l.pixfmt {
                    proto::PIXFMT_YCC422 => {
                        expanded = expand_ycc422(l.pixels);
                        l.pixfmt = proto::PIXFMT_RGB888;
                        l.pixels = &expanded;
                    }
                    proto::PIXFMT_CVBS8 => {
                        expanded = l.pixels.iter().flat_map(|&v| [v, 0]).collect();
                        l.pixfmt = proto::PIXFMT_YC8;
                        l.pixels = &expanded;
                    }
                    _ => {}
                }
                let mode = match &self.mode {
                    Some(m) if m.mode_id == l.mode_id => m,
                    _ => {
                        self.stats.orphan_lines += 1;
                        return None;
                    }
                };
                let vactive = mode.vactive;
                let mut completed = None;
                if let Some(cur) = self.cur_frame {
                    if l.frame != cur {
                        completed = self.emit();
                    }
                }
                if self.cur_frame.is_none() {
                    self.cur_frame = Some(l.frame);
                }
                if l.line >= vactive {
                    return completed; // 行がモードの範囲外
                }
                // 範囲がバッファ幅を超えていても、ラインごと捨てずに入る分だけ書く。
                //
                // 捨てると line_seen が立たず、太らせの対象からも外れるので、その行は
                // 前フレームの残りを保持し続けて薄い影として見える。しかもボード側で
                // HSYNCを取り逃がした行は範囲が右へ伸びるため、絵の内容が増えるほど
                // 該当行が増える(実機で「U>を増やすほど未充填の行が増える」形で出た)。
                // 幅は htotal に絞ってあるので、範囲がそれを超える行は入る分だけ書く
                let off = (l.offset_px as usize).min(self.width);
                let fit = (self.width - off).min(l.count_px as usize);
                // このフレームでその行を初めて受けたら、行全体を黒で消してから書く。
                //
                // ボードはライン毎に「黒でない範囲」だけを送る(offset_px/count_px が
                // 内容に応じて変わる)。範囲の外側を消さないと前フレームの内容が
                // 残る。黒判定のしきい値付近の画素はノイズで揺れるので範囲の端も
                // ラインごと・フレームごとに2画素単位で動き、結果として絵の端が
                // ラインごとにギザギザに見えた(実機で発生)。
                // 固定幅を送っていた頃には無かった副作用。
                if !self.line_seen.get(l.line as usize).copied().unwrap_or(true) {
                    let row = &mut self.fb[l.line as usize * self.width * 4
                        ..(l.line as usize + 1) * self.width * 4];
                    for px in row.chunks_exact_mut(4) {
                        px[0] = 0;
                        px[1] = 0;
                        px[2] = 0;
                        // alpha(px[3])は不変
                    }
                    // 行を消したので、ドット復元の「左隣」の連結も切る。
                    // 残すと前フレームの右端が今フレームの左端の v[n-1] になる。
                    if let Some(x) = self.last_x.get_mut(l.line as usize) {
                        *x = usize::MAX;
                    }
                }
                let base = (l.line as usize * self.width + off) * 4;
                match l.pixfmt {
                    proto::PIXFMT_RGB888 => {
                        // ★ドット復元は **RGB555 と同じ**ように掛ける。
                        //   劣化は信号源(X68000 の出力段)で起きているので、
                        //   伝送形式とは無関係。8bit なので 5→8 の展開が要らず、
                        //   量子化が粗くないぶん RGB555 より素直に効く。
                        //   ※2026-09-03: RGB888 化の直後、この分岐にだけ
                        //     フィルタを入れ忘れていて「スライダーを動かしても
                        //     絵が変わらない」という形で出た。
                        let row = l.line as usize;
                        let a = self.dot_a_milli.min(900) as i32;
                        let mut prev: [i32; 3] =
                            if a > 0 && self.last_x.get(row) == Some(&off) {
                                let p = self.last_raw[row];
                                [p[0] as i32, p[1] as i32, p[2] as i32]
                            } else {
                                [0, 0, 0]
                            };
                        let yref = if self.ycbcr {
                            self.update_ycc_ref(row, off, &l.pixels[..fit * 3])
                        } else {
                            [16, 128, 128]
                        };
                        for (i, px) in l.pixels.chunks_exact(3).enumerate().take(fit) {
                            let o = base + i * 4;
                            let px = if self.ycbcr {
                                ypbpr_to_rgb(
                                    px[1] as i32 - yref[0] + 16,
                                    px[2] as i32 - yref[1] + 128,
                                    px[0] as i32 - yref[2] + 128,
                                )
                            } else {
                                [px[0], px[1], px[2]]
                            };
                            if a == 0 {
                                self.fb[o..o + 3].copy_from_slice(&px);
                            } else {
                                let raw = [px[0] as i32, px[1] as i32, px[2] as i32];
                                // d[n] = (v[n] - a·v[n-1]) / (1-a)
                                for c in 0..3 {
                                    let d = (raw[c] * 1000 - a * prev[c]) / (1000 - a);
                                    self.fb[o + c] = d.clamp(0, 255) as u8;
                                }
                                prev = raw;
                            }
                        }
                        if a > 0 && fit > 0 && row < self.last_x.len() {
                            self.last_raw[row] =
                                [prev[0] as u8, prev[1] as u8, prev[2] as u8];
                            self.last_x[row] = off + fit;
                        }
                    }
                    proto::PIXFMT_RGB555 => {
                        let row = l.line as usize;
                        // a は 0.9 で頭打ち。1 に近づけると 1/(1-a) が発散する
                        let a = self.dot_a_milli.min(900) as i32;
                        // 断片の左隣の**生の**値。前の断片が直前まで書いていれば
                        // それを継ぐ。そうでなければ 0(ボードは非黒範囲だけを
                        // 送るので、範囲の外は黒とみなして良い)。
                        let mut prev: [i32; 3] =
                            if a > 0 && self.last_x.get(row) == Some(&off) {
                                let p = self.last_raw[row];
                                [p[0] as i32, p[1] as i32, p[2] as i32]
                            } else {
                                [0, 0, 0]
                            };
                        for (i, px) in l.pixels.chunks_exact(2).enumerate().take(fit) {
                            let v = u16::from_le_bytes([px[0], px[1]]);
                            let (r5, g5, b5) = ((v >> 10) & 0x1F, (v >> 5) & 0x1F, v & 0x1F);
                            // 5bit→8bit はビット複製(受信リファレンスと同一)
                            let raw = [
                                ((r5 << 3) | (r5 >> 2)) as i32,
                                ((g5 << 3) | (g5 >> 2)) as i32,
                                ((b5 << 3) | (b5 >> 2)) as i32,
                            ];
                            let o = base + i * 4;
                            if a == 0 {
                                self.fb[o] = raw[0] as u8;
                                self.fb[o + 1] = raw[1] as u8;
                                self.fb[o + 2] = raw[2] as u8;
                            } else {
                                // d[n] = (v[n] - a·v[n-1]) / (1-a)
                                for c in 0..3 {
                                    let d = (raw[c] * 1000 - a * prev[c]) / (1000 - a);
                                    self.fb[o + c] = d.clamp(0, 255) as u8;
                                }
                                prev = raw;
                            }
                        }
                        if a > 0 && fit > 0 && row < self.last_x.len() {
                            self.last_raw[row] =
                                [prev[0] as u8, prev[1] as u8, prev[2] as u8];
                            self.last_x[row] = off + fit;
                        }
                    }
                    proto::PIXFMT_YC8 => {
                        // 生ADC値。byte0 = 緑ch(コンポジットならCVBSそのもの、
                        // S-VideoならY)、byte1 = 赤ch(S-VideoならC)。
                        //
                        // **ここでは復調しない。** Y/C分離のコムが上下のラインを
                        // 使うので、フィールドが揃うまで待つ必要がある。生のまま
                        // 溜めて、フレーム完成時(emit)にまとめて復調する。
                        //
                        // 同時にYをグレースケールで fb にも置く。復調がロックする
                        // 前や、バーストが無い信号(モノクロ/同期だけ)でも波形が
                        // そのまま絵になり、クランプとゲインを目で確認できる。
                        // この行のこのフレームでの**最初の断片**なら、履歴を1段ずらす。
                        // いま raw[row] に入っているのは2ボードフレーム前の内容
                        // (同じ行は2フレームおきにしか書かれない)。
                        // line_seen はフレーム頭で false に戻るので、これで
                        // 「フレーム内の初回」を判定できる。
                        let row = l.line as usize;
                        if !self.raw.is_empty() && row < self.height
                            && !self.line_seen.get(row).copied().unwrap_or(true)
                        {
                            let (a, b) = (row * self.width * 2, (row + 1) * self.width * 2);
                            self.raw_p4[a..b].copy_from_slice(&self.raw_p2[a..b]);
                            self.raw_p2[a..b].copy_from_slice(&self.raw[a..b]);
                            if let Some(n) = self.hist_n.get_mut(row) {
                                *n = (*n + 1).min(3);
                            }
                        }
                        let rowbase = (l.line as usize * self.width + off) * 2;
                        for (i, px) in l.pixels.chunks_exact(2).enumerate().take(fit) {
                            if rowbase + i * 2 + 1 < self.raw.len() {
                                self.raw[rowbase + i * 2] = px[0];
                                self.raw[rowbase + i * 2 + 1] = px[1];
                            }
                            let o = base + i * 4;
                            self.fb[o] = px[0];
                            self.fb[o + 1] = px[0];
                            self.fb[o + 2] = px[0];
                        }
                    }
                    _ => return completed,
                }
                self.px_filled += fit;
                if (l.line as usize) < self.line_seen.len() {
                    self.line_seen[l.line as usize] = true;
                }
                completed
            }
            Packet::Other { ptype, flags, seq } => {
                // AUDIO/CONFIG応答もボードの共通seq空間を消費する。
                // SUBSCRIBE(アプリ発、自分のブロードキャストが返ってくることがある)
                // と非応答CONFIGは追跡しない
                if ptype == proto::TYPE_AUDIO
                    || (ptype == proto::TYPE_CONFIG
                        && flags & proto::CFG_FLAG_REPLY != 0)
                {
                    self.track_seq(seq);
                }
                None
            }
        }
    }

    /// 公開し終わったバッファを返してもらう。次の emit() で使い回す。
    ///
    /// 大きさが変わっている(モード切替)ときは捨てる。捨てても次で確保し直す
    /// だけなので害はない。
    pub fn recycle(&mut self, buf: Vec<u8>) {
        if buf.len() == self.fb.len() {
            self.spare = Some(buf);
        }
    }

    fn emit(&mut self) -> Option<CompletedFrame> {
        // --- YC8 なら、まず復調して fb を RGB に置き換える ---
        //
        // ここでやるのは、Y/C分離のコムが上下のラインを要るのでフィールドが
        // 揃ってからでないと計算できないため。欠損ラインの減衰・太らせは
        // この後に走るので、復調結果に対して掛かる(順序はこれで正しい)。
        //
        // ロックしなかったとき(バーストが無い信号など)は fb を触らないので、
        // ライン受信時に書いてあるYのグレースケールがそのまま残る。
        let (w, h) = (self.width, self.height);
        // 太らせるかどうかはインタレースの判定で決まる(finish_frame のコメント)
        self.update_interlace_measure();
        let woven = self.mode.as_ref().map_or(false, |m| m.mflags & 0x0001 != 0);
        let interlaced = woven || self.interlace_measured;
        // インターレースでは半分の行が来ないのが正常なので、減衰は別の値を使う
        let d = if interlaced { self.interlace_decay } else { self.decay };
        let decode = !self.raw.is_empty() && !self.raw_view;
        let total = (self.width * self.height).max(1);
        let fill_ratio = self.px_filled as f32 / total as f32;
        let frame_idx = self.cur_frame.unwrap_or(0);
        self.stats.frames += 1;
        self.cur_frame = None;
        self.px_filled = 0;

        // 常駐スレッドで復調するときは、届いた行を溜めて渡すだけ。結果は
        // take_decoded() で受け取る
        if decode && self.worker.is_some() {
            if self.pending.is_some() {
                // 前のフィールドがまだ渡せていない(復調が追いついていない)。
                // **新しい方だけを復調する。** 飛ばしたフィールドの行は前の
                // 復調結果が残る(送られなかった行と同じ扱い)。
                //
                // ★届いた行を足して1回にまとめると、インタレースでは2フィールド
                //   分(Pixel 10 の 480i で 524行)を復調することになり、1回が
                //   倍かかってさらに遅れる。実機で 46ms/回・22fps に落ちた。
                //   スレッドは空いた時点で次を始めるので、飛ばすフィールドの
                //   極性は偏らない
                self.stats.decode_skipped += 1;
            }
            if self.pending_seen.len() != h {
                self.pending_seen = vec![false; h];
            }
            self.pending_seen.copy_from_slice(&self.line_seen);
            for s in self.line_seen.iter_mut() {
                *s = false;
            }
            self.pending = Some(Pending { interlaced, decay: d, frame_idx, fill_ratio });
            self.field_end = true;
            self.dispatch();
            return None;
        }

        if decode {
            let dotclk = self.mode.as_ref().map(|m| m.dotclk_hz).unwrap_or(0);
            let hist = ntsc::History {
                p2: &self.raw_p2, p4: &self.raw_p4, hist_n: &self.hist_n,
            };
            let info = ntsc::decode_field(
                &self.raw, self.width, self.height, &self.line_seen, dotclk,
                &mut self.fb, self.comb3d.then_some(hist), self.adjust);
            self.ntsc_info = Some(info);
        }
        self.unfilled_rows = finish_frame(&mut self.fb, &mut self.line_seen, w, h,
                                          interlaced, d, self.field_view);
        for s in self.line_seen.iter_mut() {
            *s = false;
        }
        // 公開用のバッファは使い回す。
        //
        // 以前は self.fb.clone() で毎フレーム新規確保していた。31kHzなら
        // 1104x1136x4 = 4.8MB を 55回/秒、つまり毎秒263MBを確保+コピーする。
        // この大きさだとアロケータがOSから取り直し、初回書き込みでページ
        // フォールトも起きるので、単純なmemcpyよりずっと重い。その間
        // recv_from が呼ばれないので、OSのUDPバッファが溢れて取りこぼす
        // (実測: Windowsの低スペック機で lost 9.9%)。
        //
        // UIが使い終わったバッファを recycle() で戻してもらい、それへ
        // copy_from_slice する。確保もページフォールトも起きず、memcpy 1回で済む。
        // fb 自体は次フレームへ持ち越す必要がある(受信していない行は前の内容を
        // 保つ設計)ので、move ではなくコピーが要る。
        let need = self.fb.len();
        let mut out = match self.spare.take() {
            Some(v) if v.len() == need => v,
            _ => vec![0u8; need],
        };
        out.copy_from_slice(&self.fb);
        Some(CompletedFrame {
            frame_idx,
            width: self.width,
            height: self.height,
            rgba: out,
            fill_ratio,
            stamps: None,
        })
    }

    /// 復調を常駐スレッドへ移す。受信ループから使う(テストは同期のまま)。
    pub fn enable_decode_worker(&mut self) {
        if self.worker.is_none() {
            self.worker = Some(DecodeWorker::spawn());
        }
    }

    /// 直前の feed() でフィールドが閉じ、復調へ回したか(遅延計測の区切り用)
    pub fn take_field_end(&mut self) -> bool {
        std::mem::take(&mut self.field_end)
    }

    /// 常駐スレッドの復調が終わっていれば、そのフレームを返す。
    pub fn take_decoded(&mut self) -> Option<CompletedFrame> {
        let done = self.worker.as_mut()?.try_take()?;
        self.ntsc_info = Some(done.info);
        self.unfilled_rows = done.unfilled_rows;
        if let Some(wk) = self.worker.as_mut() {
            wk.free = done.job;
        }
        // 待っていた分があればすぐ渡す
        self.dispatch();
        Some(done.frame)
    }

    /// 溜めたフィールドを、スレッドが空いていれば材料を写して渡す。
    fn dispatch(&mut self) {
        let Some(wk) = self.worker.as_mut() else { return };
        if wk.busy {
            return;
        }
        let Some(p) = self.pending.take() else { return };
        if self.raw.is_empty() || self.raw_view {
            // 渡すまでの間に YC8 でなくなった
            return;
        }
        let mut job = std::mem::take(&mut wk.free);
        fn copy<T: Copy>(dst: &mut Vec<T>, src: &[T]) {
            dst.clear();
            dst.extend_from_slice(src);
        }
        job.width = self.width;
        job.height = self.height;
        job.dotclk_hz = self.mode.as_ref().map(|m| m.dotclk_hz).unwrap_or(0);
        copy(&mut job.raw, &self.raw);
        copy(&mut job.gray, &self.fb);
        copy(&mut job.seen, &self.pending_seen);
        job.comb3d = self.comb3d;
        if self.comb3d {
            copy(&mut job.p2, &self.raw_p2);
            copy(&mut job.p4, &self.raw_p4);
            copy(&mut job.hist_n, &self.hist_n);
        }
        job.adjust = self.adjust;
        job.interlaced = p.interlaced;
        job.decay = p.decay;
        job.field_view = self.field_view;
        job.frame_idx = p.frame_idx;
        job.fill_ratio = p.fill_ratio;
        job.out = self.spare.take().unwrap_or_default();
        for s in self.pending_seen.iter_mut() {
            *s = false;
        }
        wk.send(job);
    }
}

/// YCC422(2画素で Y0 Cb0 Y1 Cr0)を RGB888 の枠(Pr, Y, Pb)に展開する。
/// 色差は2画素で共有する(ボードは偶数画素の値を送っている)
fn expand_ycc422(px: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(px.len() / 2 * 3);
    for q in px.chunks_exact(4) {
        let (y0, cb, y1, cr) = (q[0], q[1], q[2], q[3]);
        out.extend_from_slice(&[cr, y0, cb, cr, y1, cb]);
    }
    out
}

/// BT.601 スタジオレンジの Y/Cb/Cr(8bit)を RGB に直す。
///
/// 係数は ITU-R BT.601 の逆変換を 256 倍した整数(Y 16〜235 → 0〜255、
/// 色差 16〜240 → ±0.5)。SDTV/EDTV(240p/480i/480p)は全部 601 の行列で良い。
fn ypbpr_to_rgb(y: i32, cb: i32, cr: i32) -> [u8; 3] {
    let y = 298 * (y - 16);
    let cb = cb - 128;
    let cr = cr - 128;
    let r = (y + 409 * cr + 128) >> 8;
    let g = (y - 100 * cb - 208 * cr + 128) >> 8;
    let b = (y + 516 * cb + 128) >> 8;
    [r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8]
}

/// フィールドを閉じるときの仕上げ(太らせ・欠損行の減衰・片フィールド表示)。
/// 戻り値は太らせても埋まらなかった行数。`seen` は太らせた行も立つ。
///
/// 組立側で復調するときと常駐スレッドで復調するときの両方で使う。
pub(crate) fn finish_frame(
    fb: &mut [u8],
    seen: &mut [bool],
    w: usize,
    h: usize,
    interlaced: bool,
    d: f32,
    field_view: u8,
) -> u32 {
    // 行位置は半ライン単位のスロットなので、プログレッシブでは1つ飛びに埋まる。
    // 空くスロットは「次のラインまでの間隔」ぶん太らせて埋める(ビームには
    // 太さがあるので物理的にも正しい)。インターレースでは間隔が1スロットに
    // なるので、この処理は何もしない。
    //
    // 埋めるのは「隙間がちょうど1スロットで両隣が埋まっている」場合だけ。
    // 構造的な空きは1つ飛びで規則的に現れるので必ずこの形になる。
    //
    // 織り込み済み(mflags bit0)のときは全スロットが埋まるので、この処理は本来
    // 発動しない。ところが1本だけドロップすると発動し、**別フィールドの行**を
    // 複製してしまう(隣のスロットは別フィールド)。実機で「ところどころ開始位置が
    // 他の行と合わない」形で出た。織り込み時は太らせない。
    //
    // ★判定は mflags ではなく**測定**で行う。mflags bit0 は「フレーム単位の
    //   織り込み」の意味で、VSYNCがフィールドごとに来る本物のインタレースでは
    //   0 になる。詳細は update_interlace_measure のコメント。
    if !interlaced {
        let seen0: Vec<bool> = seen.to_vec();
        for line in 1..h {
            let below = if line + 1 < h { seen0[line + 1] } else { true };
            if !seen0[line] && seen0[line - 1] && below {
                let (a, b) = fb.split_at_mut(line * w * 4);
                let src = &a[(line - 1) * w * 4..];
                b[..w * 4].copy_from_slice(&src[..w * 4]);
                seen[line] = true;
            }
        }
    }
    // このフレームで受信できなかったライン(=送信側でドロップ)を前値×decayで減衰。
    // 継続的に欠損するラインは 0.8^n で徐々に暗転し「しばらくすると消える」。
    // 1回だけの欠損は80%でほぼ気づかず、次に受信すれば満輝度へ復帰。
    if d < 1.0 {
        for line in 0..h {
            if !seen.get(line).copied().unwrap_or(true) {
                let row = &mut fb[line * w * 4..(line + 1) * w * 4];
                for px in row.chunks_exact_mut(4) {
                    px[0] = (px[0] as f32 * d) as u8;
                    px[1] = (px[1] as f32 * d) as u8;
                    px[2] = (px[2] as f32 * d) as u8;
                    // alpha(px[3])は不変
                }
            }
        }
    }
    // 片方のフィールドだけを見る(切り分け用)。選んだパリティの行を
    // 隣のスロットへ複製する = ラインダブラ。
    if field_view != 0 {
        let par = (field_view - 1) as usize;   // 1→0(偶数) / 2→1(奇数)
        let mut line = par;
        while line < h {
            let dst = line ^ 1;                     // 隣のスロット
            if dst < h {
                let s = line * w * 4;
                fb.copy_within(s..s + w * 4, dst * w * 4);
            }
            line += 2;
        }
    }
    // 太らせても埋まらなかった行を数える。0でないと前フレームの残りが減衰して
    // 薄い影として見える。診断用に統計へ出す
    seen.iter().filter(|s| !**s).count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::testutil::{pack_line, pack_mode};
    use crate::protocol::{Mode, PIXFMT_RGB555};

    fn test_mode() -> Mode {
        Mode {
            mode_id: 1, pixfmt: PIXFMT_RGB555, mflags: 0,
            hactive: 4, htotal: 5, vactive: 2, vtotal: 3,
            dotclk_hz: 450, hfreq_mhz_x1000: 90_000, vfreq_mhz_x1000: 30_000,
            seq: 0,
        }
    }

    fn rgb555(r8: u8, g8: u8, b8: u8) -> [u8; 2] {
        let v = ((r8 as u16 >> 3) << 10) | ((g8 as u16 >> 3) << 5) | (b8 as u16 >> 3);
        v.to_le_bytes()
    }

    #[test]
    fn assembles_full_frames_and_counts_losses() {
        let mut asm = FrameAssembler::new();
        let mut seq = 0u16;
        let mut send = |asm: &mut FrameAssembler, d: Vec<u8>| asm.feed(&d);

        assert!(send(&mut asm, pack_mode(&test_mode(), 0, seq)).is_none());
        for frame in 0..2u16 {
            for line in 0..2u16 {
                seq += 1;
                let px: Vec<u8> = (0..4)
                    .flat_map(|x| rgb555(x * 8 + frame as u8, 0, line as u8 * 8))
                    .collect();
                let got = send(&mut asm, pack_line(frame, seq, line, 0, PIXFMT_RGB555, 1, 0, &px));
                if frame == 1 && line == 0 {
                    // 次フレームの最初のLINEで前フレームが完成する
                    let f = got.expect("frame 0 should complete");
                    assert_eq!((f.frame_idx, f.width, f.height), (0, 4, 2));
                    assert_eq!(f.fill_ratio, 1.0);
                    // 先頭ピクセル: r8=0→0, b8=0→0 のビット複製
                    assert_eq!(&f.rgba[..4], &[0, 0, 0, 255]);
                    // x=3: r8=24→r5=3→(3<<3)|(3>>2)=24
                    assert_eq!(f.rgba[3 * 4], 24);
                } else {
                    assert!(got.is_none());
                }
            }
        }
        assert_eq!(asm.stats.lost_packets, 0);
        assert_eq!(asm.stats.orphan_lines, 0);

        // seqを2飛ばすと1ロス
        seq += 2;
        let px: Vec<u8> = (0..4).flat_map(|_| rgb555(0, 0, 0)).collect();
        send(&mut asm, pack_line(1, seq, 1, 0, PIXFMT_RGB555, 1, 0, &px));
        assert_eq!(asm.stats.lost_packets, 1);

        // 未知mode_idのLINEは迷子扱い
        seq += 1;
        send(&mut asm, pack_line(1, seq, 0, 0, PIXFMT_RGB555, 9, 0, &px));
        assert_eq!(asm.stats.orphan_lines, 1);
    }

    /// YC8(生8bit)は byte0(緑ch=CVBS/Y)をグレースケールに置く。
    /// コンポジットの波形をそのまま目で見るための表示なので、Yが3成分に
    /// 等しく入り、byte1(赤ch=C)は色として混ざらないことを確かめる。
    #[test]
    fn yc8_maps_y_to_grayscale() {
        use crate::protocol::PIXFMT_YC8;
        let mut asm = FrameAssembler::new();
        let mut m = test_mode();
        m.pixfmt = PIXFMT_YC8;
        assert!(asm.feed(&pack_mode(&m, 0, 0)).is_none());

        // Y = 0, 64, 128, 255 / C は全て 0xFF(混ざったら気づく値)
        let ys = [0u8, 64, 128, 255];
        let px: Vec<u8> = ys.iter().flat_map(|&y| [y, 0xFF]).collect();
        assert!(asm.feed(&pack_line(0, 1, 0, 0, PIXFMT_YC8, 1, 0, &px)).is_none());
        assert!(asm.feed(&pack_line(0, 2, 1, 0, PIXFMT_YC8, 1, 0, &px)).is_none());
        // 次フレームの先頭で前フレームが完成する
        let f = asm
            .feed(&pack_line(1, 3, 0, 0, PIXFMT_YC8, 1, 0, &px))
            .expect("frame 0 should complete");
        assert_eq!(f.fill_ratio, 1.0);
        for (x, &y) in ys.iter().enumerate() {
            assert_eq!(&f.rgba[x * 4..x * 4 + 3], &[y, y, y],
                       "x={x} は Y をそのまま3成分に置く");
        }
    }

    /// コンポーネントの基準色が RGB の基準色に戻ること(BT.601 の75%カラーバー)
    #[test]
    fn ypbpr_bars_map_to_rgb() {
        use super::ypbpr_to_rgb;
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() <= 2);
        assert_eq!(ypbpr_to_rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(ypbpr_to_rgb(235, 128, 128), [255, 255, 255]);
        // 75% 赤 / 緑 / 青(Y, Cb, Cr は BT.601 の表の値)
        assert!(near(ypbpr_to_rgb(65, 100, 212), [191, 0, 0]), "{:?}", ypbpr_to_rgb(65, 100, 212));
        assert!(near(ypbpr_to_rgb(112, 72, 58), [0, 191, 0]), "{:?}", ypbpr_to_rgb(112, 72, 58));
        assert!(near(ypbpr_to_rgb(35, 212, 114), [0, 0, 191]), "{:?}", ypbpr_to_rgb(35, 212, 114));
    }

    /// コンポーネントの黒が浮いていても、バックポーチを基準に (0,0,0) に戻ること。
    /// 実機の値(黒 Y=20 / Cb=126 / Cr=127)で確かめる
    #[test]
    fn ycbcr_black_follows_back_porch() {
        use crate::protocol::PIXFMT_RGB888;
        let mut asm = FrameAssembler::new();
        asm.ycbcr = true;
        let mut m = test_mode();
        m.pixfmt = PIXFMT_RGB888;
        m.hactive = 858;
        m.htotal = 858;
        assert!(asm.feed(&pack_mode(&m, 0, 0)).is_none());
        // byte0=Pr byte1=Y byte2=Pb。同期 0..62 は Y=0、以降は黒(20/126/127)、
        // 300 番だけ白(黒+219)
        let mut px = Vec::new();
        for x in 0..858usize {
            let y = if x < 62 { 0 } else if x == 300 { 20 + 219 } else { 20 };
            px.extend_from_slice(&[127, y, 126]);
        }
        let mut seq = 0;
        let mut last = None;
        for frame in 0..3u16 {
            for line in 0..2u16 {
                seq += 1;
                if let Some(f) = asm.feed(&pack_line(frame, seq, line, 0, PIXFMT_RGB888, 1, 0, &px)) {
                    last = Some(f);
                }
            }
        }
        let f = last.expect("フレームが完成しない");
        assert_eq!(&f.rgba[200 * 4..200 * 4 + 3], &[0, 0, 0], "黒が浮いている");
        assert_eq!(&f.rgba[300 * 4..300 * 4 + 3], &[255, 255, 255], "白が白にならない");
    }

    /// 送出範囲を固定した YCC422(112 から 724画素。バックポーチは終わりの
    /// 8サンプルだけ)でも、黒がバックポーチ基準で (0,0,0) に戻り、白が白になる。
    /// 1ラインを1パケットに収める構成そのもの
    #[test]
    fn ycc422_window_black_follows_back_porch() {
        use crate::protocol::PIXFMT_YCC422;
        let mut asm = FrameAssembler::new();
        asm.ycbcr = true;
        let mut m = test_mode();
        m.pixfmt = PIXFMT_YCC422;
        m.hactive = 858;
        m.htotal = 858;
        assert!(asm.feed(&pack_mode(&m, 0, 0)).is_none());
        // 黒は実機の値(Y=20 / Cb=126 / Cr=127)、300 番だけ白(黒+219)。
        // Y0 Cb Y1 Cr の並びで 112..836 を送る
        let (start, len) = (112u16, 724usize);
        let mut px = Vec::new();
        for x in (start as usize..start as usize + len).step_by(2) {
            let y = |x: usize| if x == 300 { 20 + 219 } else { 20 };
            px.extend_from_slice(&[y(x), 126, y(x + 1), 127]);
        }
        let mut seq = 0;
        let mut last = None;
        for frame in 0..4u16 {
            for line in 0..2u16 {
                seq += 1;
                if let Some(f) = asm.feed(&pack_line(frame, seq, line, start, PIXFMT_YCC422, 1, 0, &px)) {
                    last = Some(f);
                }
            }
        }
        let f = last.expect("フレームが完成しない");
        assert_eq!(&f.rgba[200 * 4..200 * 4 + 3], &[0, 0, 0], "黒が浮いている");
        assert_eq!(&f.rgba[300 * 4..300 * 4 + 3], &[255, 255, 255], "白が白にならない");
    }

    /// CVBS8(1B/px)は YC8 の緑ch と同じ絵になる(赤ch は0として扱う)
    #[test]
    fn cvbs8_matches_yc8() {
        use crate::protocol::{PIXFMT_CVBS8, PIXFMT_YC8};
        let ys = [0u8, 64, 128, 255];
        let run = |fmt: u8, px: &[u8]| {
            let mut asm = FrameAssembler::new();
            let mut m = test_mode();
            m.pixfmt = fmt;
            assert!(asm.feed(&pack_mode(&m, 0, 0)).is_none());
            assert!(asm.feed(&pack_line(0, 1, 0, 0, fmt, 1, 0, px)).is_none());
            assert!(asm.feed(&pack_line(0, 2, 1, 0, fmt, 1, 0, px)).is_none());
            asm.feed(&pack_line(1, 3, 0, 0, fmt, 1, 0, px)).expect("frame 0 should complete")
        };
        let yc8: Vec<u8> = ys.iter().flat_map(|&y| [y, 0]).collect();
        let a = run(PIXFMT_YC8, &yc8);
        let b = run(PIXFMT_CVBS8, &ys);
        assert_eq!(a.rgba, b.rgba);
        for (x, &y) in ys.iter().enumerate() {
            assert_eq!(&b.rgba[x * 4..x * 4 + 3], &[y, y, y]);
        }
    }

    /// 常駐スレッドで復調しても、組立側で復調したときと同じ絵になる。
    /// 欠けた行(減衰・前の結果の保持)と3次元コムの履歴も含めて比べる
    #[test]
    fn decode_worker_matches_inline() {
        use crate::protocol::PIXFMT_YC8;
        let mut m = test_mode();
        m.pixfmt = PIXFMT_YC8;
        m.hactive = 1820;
        m.htotal = 1820;
        m.vactive = 24;
        m.dotclk_hz = 28_636_364; // 8fsc
        // ntsc.rs の試験と同じ合成: 同期・バースト・2色の帯。隣の行と、
        // フレームごと(=1 NTSCフレームおき)に副搬送波を180°反転させる。
        // 帯の境目をフレームごとにずらして、3次元コムの動き検出にも差を出す
        let line_px = |frame: u16, line: u16| {
            let sps = m.dotclk_hz as f32;
            let at = |us: f32| (us * 1e-6 * sps) as usize;
            let (cpi, porch) = (0.78f32, 158.0f32);
            let flip = std::f32::consts::PI * (line as f32 + (frame / 2) as f32);
            let mut px = Vec::new();
            for n in 0..1820usize {
                let psi = 2.0 * std::f32::consts::PI * (n as f32 - at(5.4) as f32) / 8.0 - flip;
                let val = if n < at(4.7) {
                    porch - 40.0 * cpi
                } else if (at(5.4)..at(7.7)).contains(&n) {
                    porch + 20.0 * cpi * psi.cos()
                } else if n >= at(9.6) {
                    let red = (n + frame as usize * 16) % 600 < 300;
                    let (r, g, b) = if red { (0.8f32, 0.2, 0.2) } else { (0.2, 0.3, 0.8) };
                    let yy = 0.299 * r + 0.587 * g + 0.114 * b;
                    let c = (-0.493 * (b - yy) * psi.cos() + 0.877 * (r - yy) * psi.sin()) * 0.5;
                    porch + (yy * 100.0 + c * 100.0) * cpi
                } else {
                    porch
                };
                px.extend_from_slice(&[val.clamp(0.0, 255.0) as u8, 0]);
            }
            px
        };
        let run = |worker: bool| {
            let mut asm = FrameAssembler::new();
            asm.set_decay(0.5);
            if worker {
                asm.enable_decode_worker();
            }
            assert!(asm.feed(&pack_mode(&m, 0, 0)).is_none());
            let mut out = Vec::new();
            let mut seq = 0;
            for frame in 0..8u16 {
                for line in 0..24u16 {
                    // 3フレーム目は5行目を落とす(太らせの経路)
                    if frame == 3 && line == 5 {
                        continue;
                    }
                    seq += 1;
                    let px = line_px(frame, line);
                    let f = asm.feed(&pack_line(frame, seq, line, 0, PIXFMT_YC8, 1, 0, &px));
                    if let Some(f) = f {
                        out.push(f.rgba);
                    } else if asm.take_field_end() {
                        let t = std::time::Instant::now();
                        loop {
                            if let Some(f) = asm.take_decoded() {
                                out.push(f.rgba);
                                break;
                            }
                            assert!(t.elapsed().as_secs() < 5, "復調が返ってこない");
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                    }
                }
            }
            (out, asm.ntsc_info.map(|i| i.lines_locked), asm.stats.decode_skipped)
        };
        let (a, la, _) = run(false);
        let (b, lb, skipped) = run(true);
        assert_eq!(a.len(), 7);
        assert_eq!(skipped, 0);
        assert_eq!(la, lb);
        assert!(la.unwrap_or(0) > 0, "色がロックしていない(試験信号の問題)");
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!(x == y, "フレーム {i} が違う");
        }
    }
}
