//! UDP receive thread: subscribe keepalive + frame reassembly.
//! Publishes the latest completed frame and stats through `Shared`.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::assembler::{CompletedFrame, FrameAssembler};
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
}

#[derive(Default, Clone)]
pub struct StatsSnapshot {
    pub packets: u64,
    pub mbps: f32,
    pub fps: f32,
    pub lost_packets: u64,
    pub orphan_lines: u64,
    pub frames: u64,
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
    let mut asm = FrameAssembler::new();
    asm.set_decay(cfg.decay);
    let mut buf = vec![0u8; 65536];
    let mut sub_seq: u16 = 0;
    let mut last_subscribe: Option<Instant> = None;
    let mut last_report = Instant::now();
    let mut bytes_since = 0u64;
    let mut frames_since = 0u32;

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

        let (n, addr) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => {
                tick_stats(&shared, &asm, &mut last_report, &mut bytes_since, &mut frames_since);
                continue;
            }
        };
        bytes_since += n as u64;

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
            *shared.mode.lock().unwrap() = asm.mode.clone();
            *shared.frame.lock().unwrap() = Some(frame);
            shared.frame_gen.fetch_add(1, Ordering::Release);
            repaint();
        }
        tick_stats(&shared, &asm, &mut last_report, &mut bytes_since, &mut frames_since);
    }
}

fn tick_stats(
    shared: &Shared,
    asm: &FrameAssembler,
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
    };
    *last_report = Instant::now();
    *bytes_since = 0;
    *frames_since = 0;
}
