//! Frame reassembly (mirror of `host/python/retrocastx/receiver.py` FrameAssembler).
//! Output is RGBA8 so the frame can go straight to a GPU texture.

use crate::protocol::{self as proto, Packet};

pub struct CompletedFrame {
    pub frame_idx: u16,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub fill_ratio: f32,
}

#[derive(Default)]
pub struct Stats {
    pub lost_packets: u64,
    pub orphan_lines: u64,
    pub packets: u64,
    pub bytes: u64,
    pub frames: u64,
}

pub struct FrameAssembler {
    pub mode: Option<proto::Mode>,
    pub stats: Stats,
    fb: Vec<u8>, // RGBA
    width: usize,
    height: usize,
    cur_frame: Option<u16>,
    px_filled: usize,
    last_seq: Option<u16>,
    line_seen: Vec<bool>, // このフレームで各lineを受信したか
    /// 太らせても埋まらなかった行数(診断用)
    pub unfilled_rows: u32,
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
    /// 公開用バッファの使い回し先。UIが使い終わったものを recycle() で戻す。
    /// 毎フレームの確保をなくすためだけのもので、中身に意味は無い。
    spare: Option<Vec<u8>>,
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self {
            mode: None,
            stats: Stats::default(),
            fb: Vec::new(),
            spare: None,
            width: 0,
            height: 0,
            cur_frame: None,
            px_filled: 0,
            last_seq: None,
            line_seen: Vec::new(),
            unfilled_rows: 0,
            decay: 0.8,
            interlace_decay: 1.0,
        }
    }

    /// 欠損ライン減衰率を設定(1.0で従来の前フレーム保持)。
    pub fn set_decay(&mut self, d: f32) {
        self.decay = d;
    }

    /// インターレース時の減衰率を設定(1.0=減衰しない)。
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
                    self.line_seen = vec![false; self.height];
                    self.cur_frame = None;
                    self.px_filled = 0;
                }
                // 諸元(dotclk/hfreq/vfreq/htotal/vtotal)はボードが毎秒実測して送るので
                // mode_idが同じでも必ず取り込む。以前はmode_id変化時のみ更新していたため
                // 起動後に測定値が更新されても表示が古いままだった。
                self.mode = Some(m);
                None
            }
            Packet::Line(l) => {
                self.track_seq(l.seq);
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
                        completed = Some(self.emit());
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
                }
                let base = (l.line as usize * self.width + off) * 4;
                match l.pixfmt {
                    proto::PIXFMT_RGB888 => {
                        for (i, px) in l.pixels.chunks_exact(3).enumerate().take(fit) {
                            let o = base + i * 4;
                            self.fb[o..o + 3].copy_from_slice(px);
                        }
                    }
                    proto::PIXFMT_RGB555 => {
                        for (i, px) in l.pixels.chunks_exact(2).enumerate().take(fit) {
                            let v = u16::from_le_bytes([px[0], px[1]]);
                            let (r5, g5, b5) = ((v >> 10) & 0x1F, (v >> 5) & 0x1F, v & 0x1F);
                            let o = base + i * 4;
                            // 5bit→8bit はビット複製(受信リファレンスと同一)
                            self.fb[o] = ((r5 << 3) | (r5 >> 2)) as u8;
                            self.fb[o + 1] = ((g5 << 3) | (g5 >> 2)) as u8;
                            self.fb[o + 2] = ((b5 << 3) | (b5 >> 2)) as u8;
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

    fn emit(&mut self) -> CompletedFrame {
        // このフレームで受信できなかったライン(=送信側でドロップ)を前値×decayで減衰。
        // 継続的に欠損するラインは 0.8^n で徐々に暗転し「しばらくすると消える」。
        // 1回だけの欠損は80%でほぼ気づかず、次に受信すれば満輝度へ復帰。
        let (w, h) = (self.width, self.height);
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
        let interlaced = self.mode.as_ref().map_or(false, |m| m.mflags & 0x0001 != 0);
        // インターレースでは半分の行が来ないのが正常なので、減衰は別の値を使う
        let d = if interlaced { self.interlace_decay } else { self.decay };
        if !interlaced {
            let seen: Vec<bool> = self.line_seen.clone();
            for line in 1..h {
                let below = if line + 1 < h { seen[line + 1] } else { true };
                if !seen[line] && seen[line - 1] && below {
                    let (a, b) = self.fb.split_at_mut(line * w * 4);
                    let src = &a[(line - 1) * w * 4..];
                    b[..w * 4].copy_from_slice(&src[..w * 4]);
                    self.line_seen[line] = true;
                }
            }
        }
        if d < 1.0 {
            for line in 0..h {
                if !self.line_seen.get(line).copied().unwrap_or(true) {
                    let row = &mut self.fb[line * w * 4..(line + 1) * w * 4];
                    for px in row.chunks_exact_mut(4) {
                        px[0] = (px[0] as f32 * d) as u8;
                        px[1] = (px[1] as f32 * d) as u8;
                        px[2] = (px[2] as f32 * d) as u8;
                        // alpha(px[3])は不変
                    }
                }
            }
        }
        // 太らせても埋まらなかった行を数える。0でないと前フレームの残りが減衰して
        // 薄い影として見える。診断用に統計へ出す
        self.unfilled_rows = self.line_seen.iter().filter(|s| !**s).count() as u32;
        for s in self.line_seen.iter_mut() {
            *s = false;
        }
        let total = (self.width * self.height).max(1);
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
        let f = CompletedFrame {
            frame_idx: self.cur_frame.unwrap_or(0),
            width: self.width,
            height: self.height,
            rgba: out,
            fill_ratio: self.px_filled as f32 / total as f32,
        };
        self.stats.frames += 1;
        self.cur_frame = None;
        self.px_filled = 0;
        f
    }
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
}
