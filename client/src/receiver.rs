//! UDP receive thread: subscribe keepalive + frame reassembly.
//! Publishes the latest completed frame and stats through `Shared`.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::assembler::{CompletedFrame, FrameAssembler};
use crate::audio::AudioPlayer;
use crate::protocol::{self as proto, Packet};

/// スケジューラに「これは対話的な処理だ」と伝える。
///
/// ★実機で、**Macを操作している間だけ**フレームが落ちた(マウス移動やアプリ
///   切り替え)。描画は60Hz出ていてCPU側も0.7msなのに、受信〜組立のスレッドが
///   横取りされてキューが溢れる(queue drops 2007 / orphan lines 54049)。
///   既定のQoSだと、UI操作中の window server や他アプリに負ける。
///
/// 48000パケット/秒を落とさず捌くのは実質リアルタイム処理なので、
/// USER_INTERACTIVE を要求する。効かない環境では単に無視される。
#[cfg(target_os = "macos")]
fn raise_thread_qos() {
    // QOS_CLASS_USER_INTERACTIVE
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn raise_thread_qos() {}

pub struct Config {
    pub port: u16,
    /// 受信ソケットを縛るローカルアドレス。既定は 0.0.0.0(全IF)。
    ///
    /// ★**VPN 接続中でも指定は要らない**(2026-09-04以降)。かつては 0.0.0.0 からの
    ///   255.255.255.255 宛 SUBSCRIBE が point-to-point の utun に載ろうとして
    ///   `Can't assign requested address` で失敗するため `--bind` が必須だったが、
    ///   宛先を**NICごとのサブネット宛**にしたので既定経路を参照しなくなった
    ///   (詳細は `broadcast_targets`)。実測: VPN接続のまま `--bind` 無しで
    ///   644フレーム / 58.8Mbps / lost 0。
    ///
    ///   いまは「NICを1つに絞りたい」ときだけ指定する。指定するとブロードキャストの
    ///   宛先もそのNICだけになる。
    pub bind: String,
    /// SUBSCRIBE keepalive の宛先。None なら購読しない(sender_sim等の受け専用)。
    pub subscribe_to: Option<String>,
    /// 購読対象ボードのMAC。None ならワイルドカード(単一ボードLAN専用)。
    /// 複数ボード環境では discover で得たMACを指名する。
    pub target_mac: Option<[u8; 6]>,
    /// 欠損ライン減衰率(1.0=前フレーム保持, 0.8=毎フレーム80%へ暗転)。
    pub decay: f32,
    /// インターレース時の減衰率(1.0=減衰しない)。既定1.0。
    pub interlace_decay: f32,
    /// 音声再生の設定。
    pub audio: AudioOpts,
}

#[derive(Clone, Copy)]
pub struct AudioOpts {
    /// 再生するsource(0=RGB端子音声, 1=LINE入力, 2=S/PDIF)。None なら再生しない。
    pub source: Option<u8>,
    /// プリバッファ[ms]。小さいほど低遅延だが枯渇しやすい。
    pub prebuffer_ms: u32,
    /// バッファ上限[ms]。超えた分は古いサンプルを捨てて遅延の蓄積を防ぐ。
    pub max_ms: u32,
}

impl Default for AudioOpts {
    fn default() -> Self {
        // D-SUB15音声を既定に。80ms貯めて最大240msで頭打ち(GbE LANなら十分小さい)
        Self { source: Some(0), prebuffer_ms: 80, max_ms: 240 }
    }
}

#[derive(Default, Clone)]
pub struct StatsSnapshot {
    pub packets: u64,
    pub mbps: f32,
    pub fps: f32,
    pub lost_packets: u64,
    /// 受信スレッドのキューが満杯で捨てた数。lost の内訳を分ける
    pub queue_drops: u64,
    pub orphan_lines: u64,
    pub frames: u64,
    /// 暗部でフレーム間に値が変わった画素の割合[%](=点状ノイズの量)
    pub noise_flicker: f32,
    /// そのずれの平均振幅[/255]
    pub noise_level: f32,
    /// 有効映像(黒でない範囲)の外接矩形。pll_div/hs_offset/vbp を数値で合わせるため。
    /// 内容が無い(真っ暗な)フレームでは直前の値を保持する。
    pub active_x: u16,
    pub active_y: u16,
    pub active_w: u16,
    pub active_h: u16,
    /// 織り込みのずれ具合(小さいほど正しい)。インターレースの自動判定に使う
    pub weave_err: f32,
    /// weave_err を更新した回数。UIが「新しい測定が来たか」を判定するのに使う
    pub weave_n: u64,
    /// 水平方向の鋭さ(隣接サンプル差の二乗和を画素数で正規化)。
    /// 1ドット=1サンプルのとき最大になる。pll_divideの局所探索に使う
    pub sharp_h: f32,
    /// 水平スペクトルのうちエネルギーの99%が収まる帯域の割合(0..1)。
    /// k倍に過剰サンプルすると 1/k に近づくので、意図的に過剰サンプルしてから
    /// 割れば1:1の pll_divide が1回の測定で出る
    pub occ_h: f32,
    /// 上2つを更新した回数。UIが「新しい測定が来たか」を判定するのに使う
    pub tune_n: u64,
    /// 明るい画素の外接矩形(しきい値を超える画素が1つでもある行/列)。
    /// active_* より絵の広がりを正しく拾うので、管面の幾何にはこちらを使う
    pub span_x: u16,
    pub span_y: u16,
    pub span_w: u16,
    pub span_h: u16,
    /// 太らせても埋まらなかった行数(前フレームの残りが減衰して見える行)。
    /// 0でないと薄い影が出る
    pub unfilled_rows: u32,
    /// NTSC復調の状態(YC8のときだけ。0本ならロックしていない)
    pub ntsc_locked: u32,
    /// コムに使ったライン間隔(1 or 2)。0ならロックしていない
    pub ntsc_comb_step: u8,
    /// 隣接ライン(コム間隔ぶん)の位相差[度]。180°付近が正常
    pub ntsc_phase_deg: f32,
    /// 3次元(動き適応フレームコム)を使えた行数
    pub ntsc_lines_3d: u32,
    /// そのうち「動いている」と判定した画素の割合(0..1)
    pub ntsc_motion_frac: f32,
    /// 1フレーム前との副搬送波位相のズレ |ε| の中央値[度]。DATACLK は HSYNC に
    /// ロックしていて副搬送波にはロックしていないので歩く。フレームコムの
    /// 消し残し(= フレームごとに反転するドットクロール)は C·sin(ε/2)。
    /// 実測で 4.8°、これを補正している。大きくなったら基板側を疑う。
    pub ntsc_phase_drift_deg: f32,
    /// 赤ch(C)にバーストが載っていた = S端子として復調した。
    /// このときコムは一切使わない(クロスカラーもドットクロールも原理的に無い)
    pub ntsc_svideo: bool,
    /// フレーム差し替えでUIを待った時間の、直近区間での合計[ms]と最大[ms]。
    /// **落ちる原因の切り分け用。** ここが長いと生産側が詰まる
    pub publish_wait_ms: f32,
    pub publish_wait_max_ms: f32,
    /// 測定で「インタレースの1フィールドずつ来ている」と判定しているか。
    /// mflags では判定できないので、埋まる行のパリティの交互性で決めている
    pub interlace_measured: bool,
}

/// pll_divide と位相を自動で決めるための測定器。
///
/// 有効映像の中だけを見て、水平方向の鋭さとスペクトル占有率を出す。
/// 鋭さは局所探索(1:1の近傍で最大になる)、占有率は倍率の割り出しに使う。
/// 詳しい原理は host/python/retrocastx/pll_tune.py の autotune のコメント参照。
#[derive(Default)]
struct TuneMeter {
    nth: u32,
    pub sharp: f32,
    pub occ: f32,
    pub n: u64,
    re: Vec<f32>,
    im: Vec<f32>,
    pow: Vec<f32>,
    /// 測定に使った範囲。管面の幾何にも使う(ActiveBox より絵の広がりを正しく拾う)
    pub bx: u16,
    pub by: u16,
    pub bw: u16,
    pub bh: u16,
}

impl TuneMeter {
    /// その場でビット反転並べ替え + バタフライの radix-2 FFT。
    /// 依存を増やしたくないので自前。長さは2の冪であること。
    fn fft(re: &mut [f32], im: &mut [f32]) {
        let n = re.len();
        let mut j = 0usize;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2usize;
        while len <= n {
            let ang = -2.0 * std::f32::consts::PI / len as f32;
            let (wr, wi) = (ang.cos(), ang.sin());
            let mut i = 0;
            while i < n {
                let (mut cr, mut ci) = (1.0f32, 0.0f32);
                for k in 0..len / 2 {
                    let (ur, ui) = (re[i + k], im[i + k]);
                    let (vr, vi) = (
                        re[i + k + len / 2] * cr - im[i + k + len / 2] * ci,
                        re[i + k + len / 2] * ci + im[i + k + len / 2] * cr,
                    );
                    re[i + k] = ur + vr;
                    im[i + k] = ui + vi;
                    re[i + k + len / 2] = ur - vr;
                    im[i + k + len / 2] = ui - vi;
                    let nr = cr * wr - ci * wi;
                    ci = cr * wi + ci * wr;
                    cr = nr;
                }
                i += len;
            }
            len <<= 1;
        }
    }

    /// 測定に使う範囲を自分で決める。
    ///
    /// ActiveBox は「その列の20%以上の行が明るいか」で判定するため、細い線ばかりの
    /// 画面ではベタ塗りのブロックだけが残る(実機のCRTCHK 15kHz画面で幅125になり、
    /// 平坦な赤ベタの中だけをFFTしていて占有率が無意味な値になった)。
    /// こちらは「明るい画素が1つでもあるか」で見る。絵の広がりを取りたいだけなので
    /// これで十分で、しきい値の取り方に鈍い。
    fn bounds(rgba: &[u8], width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
        const TH: f32 = 40.0;
        let lum = |row: &[u8], x: usize| -> f32 {
            let i = x * 4;
            0.299 * row[i] as f32 + 0.587 * row[i + 1] as f32 + 0.114 * row[i + 2] as f32
        };
        let (mut x0, mut x1) = (usize::MAX, 0usize);
        let (mut y0, mut y1) = (usize::MAX, 0usize);
        for y in (0..height).step_by(2) {
            let row = &rgba[y * width * 4..(y + 1) * width * 4];
            let mut any = false;
            for x in 0..width {
                if lum(row, x) > TH {
                    if x < x0 { x0 = x }
                    if x > x1 { x1 = x }
                    any = true;
                }
            }
            if any {
                if y < y0 { y0 = y }
                if y > y1 { y1 = y }
            }
        }
        if x0 == usize::MAX || y0 == usize::MAX || x1 <= x0 + 64 || y1 <= y0 + 8 {
            return None;
        }
        Some((x0, y0, x1 - x0 + 1, y1 - y0 + 1))
    }

    fn feed(&mut self, rgba: &[u8], width: usize, height: usize) {
        self.nth += 1;
        if self.nth % 4 != 0 {
            return;             // 毎フレームやる必要はない(CPUを食うだけ)
        }
        let Some((bx, by, bw, bh)) = Self::bounds(rgba, width, height) else {
            return;
        };
        self.bx = bx as u16;
        self.by = by as u16;
        self.bw = bw as u16;
        self.bh = bh as u16;
        // 端4サンプルは外接矩形のぶれで落とす
        if bw < 72 || bh < 8 {
            return;
        }
        let (x0, w) = (bx + 4, bw.saturating_sub(8));
        // FFT用に2の冪へ切り出す(中央寄せ)
        let mut n = 1usize;
        while n * 2 <= w {
            n *= 2;
        }
        if n < 64 {
            return;
        }
        let xs = x0 + (w - n) / 2;
        if self.pow.len() != n {
            self.pow = vec![0.0; n];
            self.re = vec![0.0; n];
            self.im = vec![0.0; n];
        }
        self.pow.iter_mut().for_each(|v| *v = 0.0);
        // Hann窓。左右の端の段差が偽の高域になるのを防ぐ
        let win: Vec<f32> = (0..n)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
            .collect();
        let mut sharp = 0.0f64;
        let mut nsharp = 0u64;
        let mut nrow = 0u32;
        for y in (by..by + bh).step_by(2) {
            let row = &rgba[y * width * 4..(y + 1) * width * 4];
            let lum = |x: usize| -> f32 {
                let i = x * 4;
                0.299 * row[i] as f32 + 0.587 * row[i + 1] as f32 + 0.114 * row[i + 2] as f32
            };
            // 鋭さは切り出した範囲そのままで測る
            let mut prev = lum(x0);
            for x in x0 + 1..x0 + w {
                let v = lum(x);
                let d = (v - prev) as f64;
                sharp += d * d;
                prev = v;
            }
            nsharp += (w - 1) as u64;
            // スペクトルは行ごとにDCを抜いてから窓を掛ける
            let mean: f32 = (0..n).map(|i| lum(xs + i)).sum::<f32>() / n as f32;
            for i in 0..n {
                self.re[i] = (lum(xs + i) - mean) * win[i];
                self.im[i] = 0.0;
            }
            let (mut re, mut im) = (std::mem::take(&mut self.re), std::mem::take(&mut self.im));
            Self::fft(&mut re, &mut im);
            for i in 1..n / 2 {
                self.pow[i] += re[i] * re[i] + im[i] * im[i];
            }
            self.re = re;
            self.im = im;
            nrow += 1;
        }
        if nrow == 0 || nsharp == 0 {
            return;
        }
        // 信号帯域の上端を「ノイズ床の何倍か」で決める。
        //
        // 累積エネルギーが99%に達する点で測るとノイズに弱い。ノイズは帯域全体に
        // 平らに乗るので、上の方が全部ノイズでも累積は少しずつ増え続け、上端を
        // 過大評価する(実機の15kHzモードで倍率を2.3倍も取り違えた)。
        // 過剰サンプル中は帯域の上端が純粋なノイズなので、そこから床を推定できる。
        let half = n / 2;
        let tail = &self.pow[half - half / 10..half];
        let mut t: Vec<f32> = tail.to_vec();
        t.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let floor = t[t.len() / 2].max(1e-12);
        // 3点移動平均で平滑してから、床の4倍を超える最も高い周波数を探す
        let mut edge = 1usize;
        for i in 2..half - 1 {
            let sm = (self.pow[i - 1] + self.pow[i] + self.pow[i + 1]) / 3.0;
            if sm > floor * 4.0 {
                edge = i;
            }
        }
        self.occ = edge as f32 / (half - 1) as f32;
        self.sharp = (sharp / nsharp as f64) as f32;
        self.n += 1;
    }
}

/// 有効映像の外接矩形を求める。ノイズを拾わないよう、しきい値を超える画素が
/// 一定数ある行/列だけを「内容あり」とみなす。
#[derive(Default)]
struct ActiveBox {
    nth: u32,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    col: Vec<u16>,
}

impl ActiveBox {
    fn feed(&mut self, rgba: &[u8], width: usize, height: usize) {
        self.nth = self.nth.wrapping_add(1);
        if self.nth % 8 != 0 || width == 0 || height == 0 {
            return;
        }
        const TH: u8 = 24; // RGB555の3LSB相当。点状ノイズを拾わない程度
        // 「内容あり」とみなす条件は画素数の比で決める。以前は行・列とも一律4画素
        // だったが、512行のうち4行(0.8%)ではノイズを内容と誤認し、同じ絵で外接矩形が
        // 634→917 まで跳ねた。比を0.05〜0.5のどこに取っても結果が変わらないことを
        // 実測で確認している(host/python/retrocastx/pll_tune.py probe)。
        let col_min = (height / 5).max(4) as u16;   // 列: 全行の20%
        let row_min = (width / 20).max(4) as u16;   // 行: 全幅の5%
        self.col.clear();
        self.col.resize(width, 0);
        let (mut top, mut bot) = (u16::MAX, 0u16);
        for y in 0..height {
            let row = &rgba[y * width * 4..(y + 1) * width * 4];
            let mut n = 0u16;
            for (x, px) in row.chunks_exact(4).enumerate() {
                if px[0] > TH || px[1] > TH || px[2] > TH {
                    n += 1;
                    self.col[x] = self.col[x].saturating_add(1);
                }
            }
            if n >= row_min {
                if top == u16::MAX {
                    top = y as u16;
                }
                bot = y as u16;
            }
        }
        if top == u16::MAX {
            return; // 内容なし(真っ暗)。直前の値を保持する
        }
        let left = self.col.iter().position(|&c| c >= col_min);
        let right = self.col.iter().rposition(|&c| c >= col_min);
        if let (Some(l), Some(r)) = (left, right) {
            self.x = l as u16;
            self.w = (r - l + 1) as u16;
        }
        self.y = top;
        self.h = bot - top + 1;
    }
}

/// インターレースの織り込みのずれ具合を測る。小さいほど正しい。
///
/// 同一フィールドの上下(y-1, y+1)がよく一致している画素だけを見て、その間に
/// 挟まれた行(=別フィールド)が中間値からどれだけ外れるかを測る。上下が一致
/// している場所は縦方向に滑らかなので、正しく織り込めていれば間の行も中間値
/// 付近に来るはず。フィールドの割当を間違えると絵の別の位置が入るので外れる。
///
/// 単純な「隣接行の差 ÷ 1行飛ばしの差」も試したが、黒地に細い線という絵では
/// ノイズに埋もれて候補間の差が1%も出ず判別できなかった。上下が一致する場所に
/// 限ると1.6倍の差がつく(実機で確認)。絵の内容には依存しない。
#[derive(Default)]
struct WeaveMeter {
    nth: u32,
    pub err: f32,
    pub n: u64,
}

impl WeaveMeter {
    fn feed(&mut self, rgba: &[u8], width: usize, height: usize) {
        self.nth = self.nth.wrapping_add(1);
        if self.nth % 2 != 0 || width == 0 || height < 8 {
            return;
        }
        const AGREE: i32 = 8; // 上下が「一致している」とみなす差
        const ACTIVE: i32 = 20; // 暗すぎる所はノイズしか無いので除く
        let l = |y: usize, x: usize| -> i32 {
            let i = (y * width + x) * 4;
            // 輝度の近似。緑を重く見るだけで十分(比較にしか使わない)
            (rgba[i] as i32 + 2 * rgba[i + 1] as i32 + rgba[i + 2] as i32) / 4
        };
        let (mut sum, mut cnt) = (0f64, 0u64);
        for y in 1..height - 1 {
            // 負荷を抑えるため2画素飛ばし。統計量なので密に見る必要はない
            for x in (0..width).step_by(2) {
                let (up, dn) = (l(y - 1, x), l(y + 1, x));
                if (up - dn).abs() < AGREE && up.max(dn) > ACTIVE {
                    sum += ((2 * l(y, x) - up - dn).abs() as f64) / 2.0;
                    cnt += 1;
                }
            }
        }
        if cnt >= 500 {
            self.err = (sum / cnt as f64) as f32;
            self.n += 1;
        }
    }
}

/// 連続フレームの差分から暗部ノイズを測る。静止した絵は差分に出ないので、
/// 内容にほぼ依存せずノイズ量だけを拾える(host/retrocastx/noise_meter.py と同趣旨。
/// あちらは中央値からのずれ、こちらは前フレームとの差なので値は完全一致しない)。
/// Viewerを開いたまま測れるようにするために内蔵している。
#[derive(Default)]
struct NoiseMeter {
    prev: Vec<u8>,
    nth: u32,
    pub flicker: f32,
    pub level: f32,
}

impl NoiseMeter {
    /// RGBA8フレームを食わせる。負荷を抑えるため4フレームに1回、画素も4つ飛ばしで見る。
    fn feed(&mut self, rgba: &[u8]) {
        self.nth = self.nth.wrapping_add(1);
        if self.nth % 4 != 0 {
            return;
        }
        if self.prev.len() != rgba.len() {
            self.prev = rgba.to_vec();
            return;
        }
        const DARK: i32 = 24; // RGB555の3LSB相当(8bit展開で24)
        let (mut n, mut chg, mut sum) = (0u32, 0u32, 0u64);
        // 4画素(16バイト)おきにサンプル
        for i in (0..rgba.len()).step_by(16) {
            let (a, b) = (&rgba[i..i + 3], &self.prev[i..i + 3]);
            let mx = a.iter().chain(b.iter()).map(|&v| v as i32).max().unwrap_or(0);
            if mx > DARK {
                continue; // 明部は絵の変化と区別できないので暗部だけ見る
            }
            n += 1;
            let d = (0..3)
                .map(|c| (a[c] as i32 - b[c] as i32).abs())
                .max()
                .unwrap_or(0);
            if d > 0 {
                chg += 1;
                sum += d as u64;
            }
        }
        if n > 0 {
            self.flicker = chg as f32 * 100.0 / n as f32;
            self.level = if chg > 0 { sum as f32 / chg as f32 } else { 0.0 };
        }
        self.prev.copy_from_slice(rgba);
    }
}

#[derive(Clone)]
pub struct BoardInfo {
    pub addr: String,
    pub name: String,
    pub mac: [u8; 6],
    pub fw_version: u16,
    pub last_seen: Instant,
}

#[derive(Default)]
pub struct Shared {
    /// ドット復元(1タップ逆フィルタ)の係数 a ×1000。0 = 無効。
    /// UI から live に変えられるよう Atomic で持つ(詳細は assembler の dot_a_milli)。
    pub dot_a_milli: AtomicU32,
    pub frame: Mutex<Option<CompletedFrame>>,
    /// フレーム更新の世代カウンタ(UIはこれでテクスチャ再アップロードを判定)
    pub frame_gen: AtomicU64,
    pub mode: Mutex<Option<proto::Mode>>,
    pub stats: Mutex<StatsSnapshot>,
    pub boards: Mutex<HashMap<String, BoardInfo>>,
    pub stop: AtomicBool,
    /// 音声再生の統計(再生器が無い場合は既定値のまま)
    pub audio: Mutex<Option<Arc<crate::audio::AudioStats>>>,
    /// UI→受信スレッドへの切替要求。cpalのStreamは生成スレッドから動かせないので、
    /// UIは要求を置くだけにして、受信スレッドが再生器を作り直す。
    pub audio_request: Mutex<Option<AudioRequest>>,
    /// 現在の再生状態(UI表示用)
    pub audio_now: Mutex<AudioNow>,
    /// UI→ボードへ送るCONFIG要求(key, value)。受信スレッドが取り出して送信する。
    /// ソケットは受信スレッドが持っているので、UIから直接は送れない。
    pub config_queue: Mutex<Vec<(u16, u32)>>,
    /// ボードから返ってきた現在値(key→value)。UIはこれで自分の表示を実体に
    /// 合わせる。読み戻さないと、チェックボックスの表示とボードの状態が食い違い、
    /// 「チェックが外れているのに効いている」状態になる。
    pub config_state: Mutex<HashMap<u16, u32>>,
    /// UI→ボードへの現在値問い合わせ(key)。応答は config_state に入る。
    pub config_get_queue: Mutex<Vec<u16>>,
    /// 種別ごとの受信数 [LINE, AUDIO, MODE, INFO, CONFIG, その他]。
    /// 「何も来ない」ときに、どの種類が届いていないかで原因を絞る。
    /// ブロードキャストだけ届いてユニキャストが届かない、などが見える。
    pub kind_counts: [AtomicU64; 6],
    /// 受信スレッドが「キューが満杯」で捨てたパケット数。
    ///
    /// 組立が追いつかない量。lost(OSのUDPバッファ溢れ+ここで捨てた分)の
    /// 内訳を分けるために持つ。捨て場所を作ったなら見えるようにしておかないと、
    /// どちらが詰まっているのか判断できない。
    pub queue_drops: AtomicU64,
    /// 送信そのものが失敗しているときの説明。None なら送れている。
    ///
    /// **「何も映らない」を「送れていない」と区別するために持つ。** VPNが既定経路を
    /// 握っていると送信が EADDRNOTAVAIL で落ちるが、以前は `let _ =` で捨てていたので
    /// 症状が受信側の問題と見分けられなかった。
    pub net_error: Mutex<Option<String>>,
    /// いま実際に SUBSCRIBE / CONFIG を送っている宛先。UI表示用。
    ///
    /// 指定値そのものではない。ユニキャスト指定で何も返ってこない場合は
    /// ブロードキャストへ切り替えるので、そのときはここが変わる。
    pub sub_dest: Mutex<String>,
    /// インターレース時の残光(前フィールドの行をどれだけ残すか)。
    /// UIから実行時に変えられるよう Shared 経由で渡す。None なら起動時の値のまま。
    pub interlace_decay: Mutex<Option<f32>>,
    /// 復調せず、生のY(緑ch=CVBSそのもの)をグレースケールで見る。
    /// **切り分けの道具。** 「二重に見える」等がデコーダ由来か、それより前(信号や
    /// 送出側)かを、絵を見るだけで分けられる。None なら変更なし。
    pub raw_view: Mutex<Option<bool>>,
    /// 表示するフィールド。0=織り込み / 1=偶数スロット / 2=奇数スロット。
    /// 織り込みの影響を外して1枚だけ見るための切り分け用。None なら変更なし。
    pub field_view: Mutex<Option<u8>>,
    /// 見た目の調整(彩度・明るさ・コントラスト・色相)。None なら変更なし。
    pub adjust: Mutex<Option<crate::ntsc::Adjust>>,
}

#[derive(Clone, Default)]
pub struct AudioRequest {
    /// 出力デバイス名。None なら既定デバイス。
    pub device: Option<String>,
    /// 再生するsource。None なら再生停止。
    pub source: Option<u8>,
}

#[derive(Clone, Default)]
pub struct AudioNow {
    pub device: String,
    pub source: Option<u8>,
}

pub fn spawn(
    cfg: Config,
    shared: Arc<Shared>,
    repaint: impl Fn() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    // 500Mbps級のバーストに耐えるよう受信バッファを拡大(OSデフォルトは
    // 数十KBで、フレームクローン等で受信スレッドが一瞬停まるだけで溢れる)
    let raw = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    // 受信バッファ。500Mbps級のバーストに耐えるだけ確保したい。
    //
    // 要求しても OS が黙って上限で切るので、大きい方から順に試して
    // 「実際に確保できた値」を読み戻して表示する。macOS は
    // kern.ipc.maxsockbuf(既定8MB程度)で頭打ちになるが、Windows は
    // もっと積める。以前は 8MB を要求して結果を見ていなかった。
    //
    // 足りないと、受信スレッドが一瞬止まるだけで取りこぼす。実測: Windows の
    // 低スペック機で 280Mbps を受けて lost 9.9%。280Mbps なら 8MB は
    // 0.23秒ぶんしか無い。
    for mb in [64usize, 32, 16, 8, 4] {
        if raw.set_recv_buffer_size(mb << 20).is_ok() {
            break;
        }
    }
    match raw.recv_buffer_size() {
        Ok(n) => eprintln!("recv buffer: {:.1} MB", n as f64 / (1 << 20) as f64),
        Err(e) => eprintln!("recv buffer: 読めません ({e})"),
    }
    // ★SO_REUSEPORT で Viewer と `videoin capture` を共存させようとしたが**駄目だった**
    //   (2026-08-15)。macOS では bind は通るようになるものの、ブロードキャストは
    //   片方のソケットにしか配られず、capture が無言で0行になる。
    //   「Viewerを閉じてください」と明示的に失敗する方がまだ良いので、付けない。
    let bind_ip: std::net::Ipv4Addr = cfg.bind.parse().map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput,
                            format!("--bind {} を解釈できません ({e})", cfg.bind))
    })?;
    raw.bind(&std::net::SocketAddr::from((bind_ip, cfg.port)).into())?;
    let sock: UdpSocket = raw.into();
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    if cfg.subscribe_to.is_some() {
        sock.set_broadcast(true)?;
    }
    Ok(std::thread::spawn(move || run(cfg, sock, shared, repaint)))
}

/// 受信専用スレッド。`recv_from` だけを回して、他は一切やらない。
///
/// 以前は1本のスレッドで「受信 → パース → 画素書き込み → フレーム完成時の
/// コピー → 計測4種」を直列にやっていた。その間 recv_from が呼ばれないので、
/// OSのUDPバッファが溢れて取りこぼす。300Mbps・27000パケット/秒では、
/// 受信バッファを64MBにしてもフレーム完成のたびに空く時間が効いて、
/// Windowsの低スペック機で lost が3%残った(確保をやめても変わらなかったので、
/// 原因はコピー量ではなく「直列にやっていること」そのもの)。
///
/// 受信側が何にも待たされなくなるので、コアが2つ以上あれば組立と並列に動く。
/// キューが満杯なら捨てる。捨てた分は組立側のseq追跡で lost に出るので、
/// 黙って消えることはない。
fn rx_thread(
    sock: UdpSocket,
    stop: Arc<Shared>,
    tx: std::sync::mpsc::SyncSender<(Vec<u8>, usize, std::net::IpAddr)>,
    free_rx: std::sync::mpsc::Receiver<Vec<u8>>,
) {
    // バッファは使い回す。1パケットは MTU 以下なので 2048 で足りる
    let mut spare: Vec<Vec<u8>> = Vec::new();
    while !stop.stop.load(Ordering::Relaxed) {
        // 組立側が返してきたバッファを回収
        while let Ok(b) = free_rx.try_recv() {
            spare.push(b);
        }
        let mut buf = spare.pop().unwrap_or_else(|| vec![0u8; 2048]);
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => {
                if let Err(e) = tx.try_send((buf, n, addr.ip())) {
                    // キューが満杯 = 組立側が遅れている。捨てて受信を続ける。
                    // ここで待つと受信が止まり、OSのバッファを溢れさせてしまう。
                    stop.queue_drops.fetch_add(1, Ordering::Relaxed);
                    // 捨てたバッファは戻して使い回す(満杯が続くときに
                    // 毎パケット確保しないため)
                    let (b, ..) = match e {
                        std::sync::mpsc::TrySendError::Full(v) => v,
                        std::sync::mpsc::TrySendError::Disconnected(v) => v,
                    };
                    spare.push(b);
                }
            }
            Err(_) => spare.push(buf), // タイムアウト。バッファは戻す
        }
    }
}

/// ブロードキャスト送信の宛先を、**NICごとのサブネット宛**で列挙する。
///
/// ★**`255.255.255.255`(限定ブロードキャスト)を使ってはいけない。**
///   限定ブロードキャストはサブネット経路を持たないので既定経路に載る。既定経路が
///   point-to-point の VPN トンネル(utun*)だと送信元アドレスを割り当てられず、
///   `Can't assign requested address` で送信そのものが失敗する。VPNに繋いだまま
///   Viewer を起動するとボードが見つからない、という形で出る(2026-09-04)。
///
///   サブネット宛(例 192.168.11.255)なら経路が「そのNICのサブネット経路」に
///   決まるので、既定経路を一切参照しない。実測でも、bind を 0.0.0.0 のまま
///   192.168.11.255 へ送ればVPN接続中でもレジスタの読み書きが通った
///   (255.255.255.255 は同条件で EADDRNOTAVAIL)。
///
///   **ボードと別サブネットでも届く。** ボード 192.168.10.50 / PC 192.168.11.24 で
///   確認済み。同じL2にいればL2ブロードキャストで届き、返りはボード側の
///   `retrocastx_net.py`(受信パケットから相手のMACを学習する層)が処理する。
///
/// `bind` に具体的なアドレスが指定されていれば、そのNICだけに絞る(明示指定を尊重)。
/// 候補が1つも作れないときだけ、最後の手段として限定ブロードキャストを返す。
fn broadcast_targets(bind: &str) -> Vec<String> {
    let pinned: Option<std::net::Ipv4Addr> = bind
        .parse::<std::net::Ipv4Addr>()
        .ok()
        .filter(|a| !a.is_unspecified());
    let ifs = if_addrs::get_if_addrs().unwrap_or_default();
    pick_targets(
        ifs.into_iter().filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some((v4.ip, v4.broadcast)),
            _ => None,
        }),
        pinned,
    )
}

/// `broadcast_targets` の選び方だけを切り出したもの。NICの列挙結果を受ける。
///
/// 実機のNIC構成に依存せずに規則を試験できるように分けてある。
fn pick_targets(
    ifs: impl Iterator<Item = (std::net::Ipv4Addr, Option<std::net::Ipv4Addr>)>,
    pinned: Option<std::net::Ipv4Addr>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (ip, bcast) in ifs {
        if ip.is_loopback() {
            continue;
        }
        // point-to-point(VPNトンネル等)は broadcast を持たないので自然に落ちる
        let Some(b) = bcast else { continue };
        if pinned.is_some_and(|p| p != ip) {
            continue;
        }
        let b = b.to_string();
        if !out.contains(&b) {
            out.push(b);
        }
    }
    if out.is_empty() {
        // NICを列挙できない環境向けの保険。VPN下では失敗するが、
        // 失敗したことは net_error で見えるようにしてある
        out.push("255.255.255.255".to_string());
    }
    out
}

/// 宛先すべてへ送り、成功した数を返す。0 なら1つも出せていない。
fn send_all(sock: &UdpSocket, port: u16, targets: &[String], pkt: &[u8]) -> usize {
    let mut ok = 0usize;
    for t in targets {
        if sock.send_to(pkt, (t.as_str(), port)).is_ok() {
            ok += 1;
        }
    }
    ok
}

fn run(cfg: Config, sock: UdpSocket, shared: Arc<Shared>, repaint: impl Fn()) {
    // 受信専用スレッドへ渡す。送信(SUBSCRIBE/CONFIG)はこちらのスレッドが
    // 複製したソケットで行う。同じソケットなので、ボードが覚える送信元は変わらない。
    let send_sock = match sock.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ソケットを複製できません: {e}");
            return;
        }
    };
    // ★深さは**実測のパケットレート**で決めること。コメントが古くなっていた。
    //   YC8(全ライン)では実測 48000パケット/秒 なので、8192段では 0.17秒しか
    //   吸収できない(「27000/秒に対し0.3秒」と書いてあった)。
    //   実機で、Macを操作している間だけ組立が横取りされてここが溢れた
    //   (queue drops 2007 / orphan lines 54049)。16384段 = 約0.34秒にする。
    let (tx, rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, usize, std::net::IpAddr)>(16384);
    let (free_tx, free_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            raise_thread_qos();
            rx_thread(sock, shared, tx, free_rx)
        });
    }
    // 組立スレッド(この関数)も上げる
    raise_thread_qos();

    // 音声再生器(デバイスが開けなければ再生なしで続行)
    let mut audio_dev: Option<String> = None;
    let mut audio = open_audio(&cfg, &shared, cfg.audio.source, None);
    let mut asm = FrameAssembler::new();
    asm.set_decay(cfg.decay);
    asm.set_interlace_decay(cfg.interlace_decay);
    let mut sub_seq: u16 = 0;
    let mut last_subscribe: Option<Instant> = None;
    let mut last_geom: Option<Instant> = None;
    let mut last_report = Instant::now();
    let mut bytes_since = 0u64;
    let mut frames_since = 0u32;
    // フレーム差し替えでUIを待った時間(合計と最大)。統計で出す
    let mut publish_wait_us = 0u64;
    let mut publish_wait_max_us = 0u64;
    let mut noise = NoiseMeter::default();
    let mut abox = ActiveBox::default();
    let mut tune = TuneMeter::default();
    let mut weave = WeaveMeter::default();
    // モード変化ログ用の直近キー
    let mut mode_key: Option<(u16, u16, u16, u16, u32, u32, u32)> = None;
    // 実際の送信先。ユニキャストを指定されても、経路が無い環境では何も返って
    // こない(Macを再起動して en0 のエイリアスが消えた状態がこれ)。その場合は
    // ブロードキャストへ落とす。ボードは SUBSCRIBE の送信元へ返すので、
    // 別サブネットでも同じL2にいれば届く(実測: フル帯域で受信できた)。
    // 逆にユニキャストへ「上げる」ことはしない。宛先サブネットへの経路が
    // 必要になり、エイリアス無しでは送れなくなるため。
    let mut dest = cfg.subscribe_to.clone();
    let broadcast = "255.255.255.255".to_string();
    let mut got_any = false;
    let started = Instant::now();
    let mut fell_back = false;
    // ブロードキャスト時の実際の宛先(NICごとのサブネット宛)。NICは後から増減する
    // (Wi-Fiに繋ぐ、ケーブルを差す、VPNを上げ下げする)ので定期的に作り直す。
    let mut targets: Vec<String> = Vec::new();
    let mut targets_at: Option<Instant> = None;
    if let Some(d) = dest.clone() {
        *shared.sub_dest.lock().unwrap() = d;
    }

    while !shared.stop.load(Ordering::Relaxed) {
        // 宛先の実体を決める。ユニキャスト指定ならその1つ、ブロードキャストなら
        // NICごとのサブネット宛(理由は broadcast_targets のコメント)。
        let bcast = dest.as_deref() == Some(&broadcast);
        if bcast {
            if targets_at.map_or(true, |t| t.elapsed() >= Duration::from_secs(10)) {
                targets_at = Some(Instant::now());
                let fresh = broadcast_targets(&cfg.bind);
                if fresh != targets {
                    *shared.sub_dest.lock().unwrap() = fresh.join(" ");
                    targets = fresh;
                }
            }
        } else if let Some(d) = &dest {
            if targets.len() != 1 || targets[0] != *d {
                targets = vec![d.clone()];
                *shared.sub_dest.lock().unwrap() = d.clone();
            }
        } else {
            targets.clear();
        }
        // 購読キープアライブ(ボードは10秒で失効させる)
        // ユニキャスト指定で3秒間何も来なければブロードキャストへ落とす
        if !got_any && !fell_back && dest.is_some() && dest.as_deref() != Some(&broadcast)
            && started.elapsed() >= Duration::from_secs(3)
        {
            eprintln!("{} から応答が無いのでブロードキャストで探します",
                      dest.as_deref().unwrap_or("?"));
            dest = Some(broadcast.clone());
            fell_back = true;
            *shared.sub_dest.lock().unwrap() = broadcast.clone();
            last_subscribe = None;   // すぐ送り直す
        }
        if !targets.is_empty() {
            let due = last_subscribe.map_or(true, |t| t.elapsed() >= Duration::from_secs(2));
            if due {
                let mac = cfg.target_mac.unwrap_or(proto::WILDCARD_MAC);
                let pkt = proto::pack_subscribe(sub_seq, false, &mac);
                // ★**送信失敗を黙って捨てない。** 以前は `let _ =` で捨てていたので、
                //   VPNで送れないときの症状が「何も映らない」だけになり、原因を
                //   突き止めるのに何度も時間を使った。1つも出せていないなら言う。
                let ok = send_all(&send_sock, cfg.port, &targets, &pkt);
                *shared.net_error.lock().unwrap() = if ok == 0 {
                    Some(format!(
                        "SUBSCRIBE を送信できません(宛先 {})。\n\
                         VPN が既定経路を握っていると限定ブロードキャストが送れません。\n\
                         有線/Wi-Fi のNICが有効か、ボードと同じL2にいるかを確認してください",
                        targets.join(" ")))
                } else {
                    None
                };
                sub_seq = sub_seq.wrapping_add(1);
                last_subscribe = Some(Instant::now());
            }
        }

        // UIからのCONFIG要求をボードへ送る(画枠パラメータの実行時調整)
        {
            let mut q = shared.config_queue.lock().unwrap();
            if !q.is_empty() {
                if targets.is_empty() {
                    q.clear();
                } else {
                    let mac = cfg.target_mac.unwrap_or(proto::WILDCARD_MAC);
                    for (key, value) in q.drain(..) {
                        let pkt = proto::pack_config(sub_seq, 0, 0, key, value, &mac);
                        send_all(&send_sock, cfg.port, &targets, &pkt);
                        sub_seq = sub_seq.wrapping_add(1);
                    }
                }
            }
        }

        // 画枠パラメータ(vbp/hs_offset/pll_divide)は表示の幾何に必要なので、
        // UIの有無に関わらず受信側で持っておく。フルスクリーンにはUIが無い。
        {
            let due = last_geom.map_or(true, |t: Instant| t.elapsed() >= Duration::from_secs(2));
            if due {
                last_geom = Some(Instant::now());
                let st = shared.config_state.lock().unwrap();
                let want: Vec<u16> = [proto::CFG_KEY_VBP, proto::CFG_KEY_HS_OFFSET]
                    .into_iter()
                    .filter(|k| !st.contains_key(k))
                    .collect();
                drop(st);
                if !want.is_empty() {
                    shared.config_get_queue.lock().unwrap().extend(want);
                }
            }
        }

        // UIからの現在値問い合わせ(GET)。起動直後にボードの実体へ表示を合わせる
        {
            let mut q = shared.config_get_queue.lock().unwrap();
            if !q.is_empty() {
                if targets.is_empty() {
                    q.clear();
                } else {
                    let mac = cfg.target_mac.unwrap_or(proto::WILDCARD_MAC);
                    for key in q.drain(..) {
                        let pkt = proto::pack_config(sub_seq, 0, 1, key, 0, &mac);
                        send_all(&send_sock, cfg.port, &targets, &pkt);
                        sub_seq = sub_seq.wrapping_add(1);
                    }
                }
            }
        }

        // UIからの音声切替要求(デバイス/source)を反映する
        if let Some(req) = shared.audio_request.lock().unwrap().take() {
            audio_dev = req.device.clone();
            audio = open_audio(&cfg, &shared, req.source, audio_dev.as_deref());
        }

        // 受信スレッドから受け取る。ここでパース以降を全部やるが、受信側は
        // 待たされないので取りこぼしにはつながらない。
        let (buf, n, src_ip) = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(v) => {
                got_any = true;
                v
            }
            Err(_) => {
                tick_stats(&shared, &asm, &noise, &abox, &tune, &weave, &mut last_report,
                   &mut bytes_since, &mut frames_since,
                   &mut publish_wait_us, &mut publish_wait_max_us);
                continue;
            }
        };
        bytes_since += n as u64;
        if n >= 3 {
            let k = match buf[2] {
                proto::TYPE_LINE => 0,
                proto::TYPE_AUDIO => 1,
                proto::TYPE_MODE => 2,
                proto::TYPE_INFO => 3,
                proto::TYPE_CONFIG => 4,
                _ => 5,
            };
            shared.kind_counts[k].fetch_add(1, Ordering::Relaxed);
        }

        // CONFIG応答(ボードが受理した現在値)を端末に出す。設定が届いているか、
        // どの値が入ったかを確認できるようにする。
        if n >= 24 && buf[2] == proto::TYPE_CONFIG
            && buf[3] & proto::CFG_FLAG_REPLY != 0
        {
            let key = u16::from_le_bytes([buf[18], buf[19]]);
            let val = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
            let name = match key {
                proto::CFG_KEY_VBP => "vbp",
                proto::CFG_KEY_HS_OFFSET => "hs_offset",
                proto::CFG_KEY_PLL_DIVIDE => "pll_divide",
                proto::CFG_KEY_INTERLACE => "interlace",
                proto::CFG_KEY_F2_ROW => "f2_row",
                proto::CFG_KEY_FIELD_SWAP => "field_swap",
                _ => "key",
            };
            eprintln!("config reply: {name}(0x{key:04x}) = {val}");
            shared.config_state.lock().unwrap().insert(key, val);
        }

        // モード変化を端末にログする(モード表を作る調査用)。ボードが毎秒送る
        // 実測値なので、X68000のモードを切り替えると1行出る。Viewerを開いたまま
        // 操作できるように、専用ツールではなくここで記録する。
        if n >= 3 && buf[2] == proto::TYPE_MODE {
            if let Ok(Packet::Mode(m)) = proto::parse(&buf[..n]) {
                // 実測値は測定分解能のぶん常に揺れる(fHは1秒窓の計数なので±1Hz、
                // fVは8秒積算で0.125Hz刻み)。量子化では境界で必ずばたつくので、
                // 相対誤差のしきい値で「本当にモードが変わったか」を判定する。
                let changed = match mode_key {
                    None => true,
                    Some(p) => {
                        let rel = |a: u32, b: u32, tol: f64| {
                            let (a, b) = (a as f64, b as f64);
                            (a - b).abs() > b.max(1.0) * tol
                        };
                        p.0 != m.hactive || p.1 != m.vactive
                            || p.2 != m.htotal || p.3 != m.vtotal
                            || rel(m.dotclk_hz, p.4, 0.01)
                            || rel(m.hfreq_mhz_x1000, p.5, 0.01)
                            || rel(m.vfreq_mhz_x1000, p.6, 0.02)
                    }
                };
                if changed {
                    mode_key = Some((
                        m.hactive, m.vactive, m.htotal, m.vtotal,
                        m.dotclk_hz, m.hfreq_mhz_x1000, m.vfreq_mhz_x1000,
                    ));
                    eprintln!(
                        "mode: active {}x{}  total {}x{}  dotclk {:.4}MHz  \
                         h {:.3}kHz  v {:.2}Hz",
                        m.hactive, m.vactive, m.htotal, m.vtotal,
                        m.dotclk_hz as f64 / 1e6,
                        m.hfreq_mhz_x1000 as f64 / 1e6,
                        m.vfreq_mhz_x1000 as f64 / 1e3
                    );
                }
            }
        }

        // 音声は先に振り分ける(映像は数万パケット/秒来るので、typeバイトだけ見る
        // 安価な判定で音声を取りこぼさないようにする)
        if n >= 3 && buf[2] == proto::TYPE_AUDIO {
            if let Some(player) = &audio {
                if let Ok(Packet::Audio(a)) = proto::parse(&buf[..n]) {
                    if a.source == player.source {
                        player.push(a.samples);
                    }
                }
            }
        }

        // 発見情報はアセンブラと別に集計(送信元アドレスが正)
        if let Ok(Packet::Announce(a)) = proto::parse(&buf[..n]) {
            let mut boards = shared.boards.lock().unwrap();
            boards.insert(
                src_ip.to_string(),
                BoardInfo {
                    addr: src_ip.to_string(),
                    name: a.name.clone(),
                    mac: a.mac,
                    fw_version: a.fw_version,
                    last_seen: Instant::now(),
                },
            );
        }

        // UIから残光が変わっていたら取り込む(フレーム完成時だけで十分)
        if let Some(d) = shared.interlace_decay.lock().unwrap().take() {
            asm.set_interlace_decay(d);
        }
        if let Some(v) = shared.raw_view.lock().unwrap().take() {
            asm.set_raw_view(v);
        }
        if let Some(v) = shared.field_view.lock().unwrap().take() {
            asm.set_field_view(v);
        }
        if let Some(a) = shared.adjust.lock().unwrap().take() {
            asm.set_adjust(a);
        }
        asm.dot_a_milli = shared.dot_a_milli.load(Ordering::Relaxed);
        if let Some(frame) = asm.feed(&buf[..n]) {
            frames_since += 1;
            noise.feed(&frame.rgba);
            abox.feed(&frame.rgba, frame.width, frame.height);
            tune.feed(&frame.rgba, frame.width, frame.height);
            weave.feed(&frame.rgba, frame.width, frame.height);
            *shared.mode.lock().unwrap() = asm.mode.clone();
            // 差し替えて、UIが使い終わった前のバッファを組立側へ返す。
            // ロックはUIがテクスチャ転送を終えるまで取れないので、返した時点で
            // もう誰も参照していない。これで毎フレームの4.8MB確保が消える。
            // 生産側がUIを待った時間。**落ちる原因を切り分けるために測る。**
            // ここが長いと、その間フレームを差し替えられず受信側が詰まる。
            let t_wait = Instant::now();
            let old = shared.frame.lock().unwrap().replace(frame);
            let us = t_wait.elapsed().as_micros() as u64;
            publish_wait_us = publish_wait_us.saturating_add(us);
            if us > publish_wait_max_us { publish_wait_max_us = us; }
            if let Some(old) = old {
                asm.recycle(old.rgba);
            }
            shared.frame_gen.fetch_add(1, Ordering::Release);
            repaint();
        }
        // 使い終わったバッファを受信スレッドへ返す。返さないと毎パケット
        // 確保することになり、27000回/秒のアロケーションで元の木阿弥になる。
        // 受信スレッドが終了していて送れなくても、単に落とすだけで害はない。
        let _ = free_tx.send(buf);

        tick_stats(&shared, &asm, &noise, &abox, &tune, &weave, &mut last_report,
                   &mut bytes_since, &mut frames_since,
                   &mut publish_wait_us, &mut publish_wait_max_us);
    }
}

/// 音声再生器を開き、Sharedの表示用状態を更新する。source=None なら再生しない。
fn open_audio(
    cfg: &Config,
    shared: &Shared,
    source: Option<u8>,
    device: Option<&str>,
) -> Option<AudioPlayer> {
    let player = source.and_then(|src| {
        let p = AudioPlayer::new(src, cfg.audio.prebuffer_ms, cfg.audio.max_ms, device);
        if p.is_none() {
            eprintln!("audio: 出力デバイスを開けないため音声再生を無効化します");
        }
        p
    });
    *shared.audio.lock().unwrap() = player.as_ref().map(|p| p.stats.clone());
    *shared.audio_now.lock().unwrap() = AudioNow {
        device: player.as_ref().map(|p| p.device_name.clone()).unwrap_or_default(),
        source: player.as_ref().map(|p| p.source),
    };
    player
}

fn tick_stats(
    shared: &Shared,
    asm: &FrameAssembler,
    noise: &NoiseMeter,
    abox: &ActiveBox,
    tune: &TuneMeter,
    weave: &WeaveMeter,
    last_report: &mut Instant,
    bytes_since: &mut u64,
    frames_since: &mut u32,
    publish_wait_us: &mut u64,
    publish_wait_max_us: &mut u64,
) {
    let dt = last_report.elapsed();
    if dt < Duration::from_millis(500) {
        return;
    }
    let secs = dt.as_secs_f32();
    let mut stats = shared.stats.lock().unwrap();
    *stats = StatsSnapshot {
        packets: asm.stats.packets,
        mbps: (*bytes_since as f32 * 8.0) / secs / 1e6,
        fps: *frames_since as f32 / secs,
        lost_packets: asm.stats.lost_packets,
        queue_drops: shared.queue_drops.load(Ordering::Relaxed),
        orphan_lines: asm.stats.orphan_lines,
        frames: asm.stats.frames,
        noise_flicker: noise.flicker,
        noise_level: noise.level,
        active_x: abox.x,
        active_y: abox.y,
        weave_err: weave.err,
        weave_n: weave.n,
        sharp_h: tune.sharp,
        span_x: tune.bx,
        span_y: tune.by,
        span_w: tune.bw,
        span_h: tune.bh,
        unfilled_rows: asm.unfilled_rows,
        ntsc_locked: asm.ntsc_info.as_ref().map(|i| i.lines_locked).unwrap_or(0),
        ntsc_comb_step: asm.ntsc_info.as_ref().map(|i| i.comb_step as u8).unwrap_or(0),
        ntsc_phase_deg: asm.ntsc_info.as_ref().map(|i| i.phase_delta_deg).unwrap_or(0.0),
        ntsc_lines_3d: asm.ntsc_info.as_ref().map(|i| i.lines_3d).unwrap_or(0),
        ntsc_motion_frac: asm.ntsc_info.as_ref().map(|i| i.motion_frac).unwrap_or(0.0),
        ntsc_phase_drift_deg: asm.ntsc_info.as_ref().map(|i| i.phase_drift_deg).unwrap_or(0.0),
        ntsc_svideo: asm.ntsc_info.as_ref().map(|i| i.svideo).unwrap_or(false),
        publish_wait_ms: *publish_wait_us as f32 / 1000.0,
        publish_wait_max_ms: *publish_wait_max_us as f32 / 1000.0,
        interlace_measured: asm.interlace_measured,
        occ_h: tune.occ,
        tune_n: tune.n,
        active_w: abox.w,
        active_h: abox.h,
    };
    *last_report = Instant::now();
    *bytes_since = 0;
    *frames_since = 0;
    *publish_wait_us = 0;
    *publish_wait_max_us = 0;
}


#[cfg(test)]
mod tests {
    use super::pick_targets;
    use std::net::Ipv4Addr;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    /// ★**実在するNICがあるなら 255.255.255.255 を返してはいけない。**
    ///
    /// 限定ブロードキャストは既定経路に載るので、VPN接続中は送信自体が
    /// EADDRNOTAVAIL で落ちる。ここが崩れると「VPNを繋いだままだとボードが
    /// 見つからない」に戻る(2026-09-04 に .app で実際に踏んだ)。
    #[test]
    fn prefers_per_nic_directed_broadcast() {
        let ifs = vec![
            (ip("127.0.0.1"), Some(ip("127.255.255.255"))),  // ループバック
            (ip("192.168.11.24"), Some(ip("192.168.11.255"))), // 有線
            (ip("172.23.24.193"), None),                       // VPN(point-to-point)
        ];
        let got = pick_targets(ifs.into_iter(), None);
        assert_eq!(got, vec!["192.168.11.255"], "得られた宛先: {got:?}");
        assert!(!got.iter().any(|t| t == "255.255.255.255"));
    }

    /// 複数NICがあれば全部へ出す。どれがボードと同じL2かは分からないので、
    /// 絞らずに並べる(2秒ごとに数十バイトなので実害は無い)。
    #[test]
    fn lists_every_usable_nic() {
        let ifs = vec![
            (ip("192.168.11.24"), Some(ip("192.168.11.255"))),
            (ip("10.0.0.5"), Some(ip("10.0.0.255"))),
            (ip("127.0.0.1"), Some(ip("127.255.255.255"))),
        ];
        let got = pick_targets(ifs.into_iter(), None);
        assert_eq!(got, vec!["192.168.11.255", "10.0.0.255"]);
    }

    /// 同じサブネットに複数アドレスが付いていても宛先は1つにまとめる
    #[test]
    fn dedups_same_subnet() {
        let ifs = vec![
            (ip("192.168.11.24"), Some(ip("192.168.11.255"))),
            (ip("192.168.11.33"), Some(ip("192.168.11.255"))),
        ];
        assert_eq!(pick_targets(ifs.into_iter(), None), vec!["192.168.11.255"]);
    }

    /// --bind で明示指定したらそのNICだけに絞る(明示指定を尊重する)
    #[test]
    fn pinned_bind_restricts_to_that_nic() {
        let ifs = vec![
            (ip("192.168.11.24"), Some(ip("192.168.11.255"))),
            (ip("10.0.0.5"), Some(ip("10.0.0.255"))),
        ];
        assert_eq!(
            pick_targets(ifs.clone().into_iter(), Some(ip("10.0.0.5"))),
            vec!["10.0.0.255"]
        );
        // 指定したアドレスがどのNICにも無ければ候補ゼロ → 最後の手段
        assert_eq!(
            pick_targets(ifs.into_iter(), Some(ip("192.168.99.1"))),
            vec!["255.255.255.255"]
        );
    }

    /// 使えるNICが1つも無いときだけ限定ブロードキャストへ落ちる。
    /// VPN下では送れないが、送れないことは net_error でUIに出る。
    #[test]
    fn falls_back_only_when_nothing_usable() {
        let ifs = vec![
            (ip("127.0.0.1"), Some(ip("127.255.255.255"))),
            (ip("172.23.24.193"), None),
        ];
        assert_eq!(pick_targets(ifs.into_iter(), None), vec!["255.255.255.255"]);
    }
}
