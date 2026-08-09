//! UDP receive thread: subscribe keepalive + frame reassembly.
//! Publishes the latest completed frame and stats through `Shared`.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::assembler::{CompletedFrame, FrameAssembler};
use crate::audio::AudioPlayer;
use crate::protocol::{self as proto, Packet};

pub struct Config {
    pub port: u16,
    /// SUBSCRIBE keepalive の宛先。None なら購読しない(sender_sim等の受け専用)。
    pub subscribe_to: Option<String>,
    /// 購読対象ボードのMAC。None ならワイルドカード(単一ボードLAN専用)。
    /// 複数ボード環境では discover で得たMACを指名する。
    pub target_mac: Option<[u8; 6]>,
    /// 欠損ライン減衰率(1.0=前フレーム保持, 0.8=毎フレーム80%へ暗転)。
    pub decay: f32,
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
    raw.set_recv_buffer_size(8 << 20)?;
    raw.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], cfg.port)).into())?;
    let sock: UdpSocket = raw.into();
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    if cfg.subscribe_to.is_some() {
        sock.set_broadcast(true)?;
    }
    Ok(std::thread::spawn(move || run(cfg, sock, shared, repaint)))
}

fn run(cfg: Config, sock: UdpSocket, shared: Arc<Shared>, repaint: impl Fn()) {
    // 音声再生器(デバイスが開けなければ再生なしで続行)
    let mut audio_dev: Option<String> = None;
    let mut audio = open_audio(&cfg, &shared, cfg.audio.source, None);
    let mut asm = FrameAssembler::new();
    asm.set_decay(cfg.decay);
    let mut buf = vec![0u8; 65536];
    let mut sub_seq: u16 = 0;
    let mut last_subscribe: Option<Instant> = None;
    let mut last_report = Instant::now();
    let mut bytes_since = 0u64;
    let mut frames_since = 0u32;
    let mut noise = NoiseMeter::default();
    let mut abox = ActiveBox::default();
    // モード変化ログ用の直近キー
    let mut mode_key: Option<(u16, u16, u16, u16, u32, u32, u32)> = None;

    while !shared.stop.load(Ordering::Relaxed) {
        // 購読キープアライブ(ボードは10秒で失効させる)
        if let Some(dest) = &cfg.subscribe_to {
            let due = last_subscribe.map_or(true, |t| t.elapsed() >= Duration::from_secs(2));
            if due {
                let mac = cfg.target_mac.unwrap_or(proto::WILDCARD_MAC);
                let _ = sock.send_to(
                    &proto::pack_subscribe(sub_seq, false, &mac),
                    (dest.as_str(), cfg.port),
                );
                sub_seq = sub_seq.wrapping_add(1);
                last_subscribe = Some(Instant::now());
            }
        }

        // UIからのCONFIG要求をボードへ送る(画枠パラメータの実行時調整)
        {
            let mut q = shared.config_queue.lock().unwrap();
            if !q.is_empty() {
                if let Some(dest) = &cfg.subscribe_to {
                    let mac = cfg.target_mac.unwrap_or(proto::WILDCARD_MAC);
                    for (key, value) in q.drain(..) {
                        let pkt = proto::pack_config(sub_seq, 0, 0, key, value, &mac);
                        let _ = sock.send_to(&pkt, (dest.as_str(), cfg.port));
                        sub_seq = sub_seq.wrapping_add(1);
                    }
                } else {
                    q.clear();
                }
            }
        }

        // UIからの音声切替要求(デバイス/source)を反映する
        if let Some(req) = shared.audio_request.lock().unwrap().take() {
            audio_dev = req.device.clone();
            audio = open_audio(&cfg, &shared, req.source, audio_dev.as_deref());
        }

        let (n, addr) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => {
                tick_stats(&shared, &asm, &noise, &abox, &mut last_report, &mut bytes_since, &mut frames_since);
                continue;
            }
        };
        bytes_since += n as u64;

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
                _ => "key",
            };
            eprintln!("config reply: {name}(0x{key:04x}) = {val}");
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
                addr.ip().to_string(),
                BoardInfo {
                    addr: addr.ip().to_string(),
                    name: a.name.clone(),
                    mac: a.mac,
                    fw_version: a.fw_version,
                    last_seen: Instant::now(),
                },
            );
        }

        if let Some(frame) = asm.feed(&buf[..n]) {
            frames_since += 1;
            noise.feed(&frame.rgba);
            abox.feed(&frame.rgba, frame.width, frame.height);
            *shared.mode.lock().unwrap() = asm.mode.clone();
            *shared.frame.lock().unwrap() = Some(frame);
            shared.frame_gen.fetch_add(1, Ordering::Release);
            repaint();
        }
        tick_stats(&shared, &asm, &noise, &abox, &mut last_report, &mut bytes_since, &mut frames_since);
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
    last_report: &mut Instant,
    bytes_since: &mut u64,
    frames_since: &mut u32,
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
        orphan_lines: asm.stats.orphan_lines,
        frames: asm.stats.frames,
        noise_flicker: noise.flicker,
        noise_level: noise.level,
        active_x: abox.x,
        active_y: abox.y,
        active_w: abox.w,
        active_h: abox.h,
    };
    *last_report = Instant::now();
    *bytes_since = 0;
    *frames_since = 0;
}
