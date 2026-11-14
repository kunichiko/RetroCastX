//! ランチャーの一覧のための発見。**映像は受けない。**
//!
//! ★**ANNOUNCE_ONLY だけを出す。** 通常の SUBSCRIBE を出すと、ボードの映像の
//!   送り先がランチャーに移り、そのボードを見ている Viewer の窓が奪われる。
//!   発見だけの問いかけなら送り先は動かない(protocol-v0.md の SUBSCRIBE)。
//! ★**受信スレッド(receiver.rs)を流用しない。** あちらは掴む・組み立てる・鳴らす
//!   までを1本で持っていて、一覧を出すだけのために再生器や組立器まで起きる。
//!   宛先の選び方と送り方だけを借りる。

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::protocol::{self as proto, Packet};
use crate::receiver::{self, BoardInfo};

/// 問いかけの間隔。ボードは問いかけごとに1回返す
const PROBE_EVERY: Duration = Duration::from_secs(2);
/// これだけ返事が無ければ居ない(問いかけ5回ぶん。受信スレッドと同じ)
const GONE_AFTER: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Discovery {
    /// 見つかっているボード。キーは MAC(receiver::Shared::boards と同じ理由)
    pub boards: Mutex<HashMap<[u8; 6], BoardInfo>>,
    /// 手入力のボードIP。ブロードキャストに**加えて**ここへも送る(iOS の逃げ道)
    pub extra_dest: Mutex<Option<String>>,
    /// いま問いかけている宛先(表示用)
    pub sub_dest: Mutex<String>,
    /// 送信が1つも通らなかったときの説明。None なら送れている
    pub net_error: Mutex<Option<String>>,
    stop: AtomicBool,
}

impl Discovery {
    /// 見つかっているボードを MAC 順で。**並びが毎回変わると選び間違える**
    pub fn list(&self) -> Vec<BoardInfo> {
        let mut v: Vec<BoardInfo> = self.boards.lock().unwrap().values().cloned().collect();
        v.sort_by_key(|b| b.mac);
        v
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 発見のスレッドを立てる。返した `Arc` を落とすと止まる(スレッドは弱参照で持つ)
pub fn spawn(bind: String, port: u16, repaint: impl Fn() + Send + 'static) -> Arc<Discovery> {
    let d = Arc::new(Discovery::default());
    let weak = Arc::downgrade(&d);
    std::thread::Builder::new()
        .name("discovery".into())
        .spawn(move || {
            let bind_ip: std::net::Ipv4Addr =
                bind.parse().unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
            // ★**ポートは空きを使う。** ボードは送信元ポートへ返すので 34600 は要らず、
            //   掴むと同じ機械の Viewer やホスト側の CLI と取り合う
            let sock = match UdpSocket::bind((bind_ip, 0)) {
                Ok(s) => s,
                Err(e) => {
                    if let Some(d) = weak.upgrade() {
                        *d.net_error.lock().unwrap() = Some(format!("UDP を開けません: {e}"));
                    }
                    return;
                }
            };
            let _ = sock.set_broadcast(true);
            let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
            let mut seq: u16 = 0;
            let mut last_probe: Option<Instant> = None;
            let mut buf = vec![0u8; 2048];
            loop {
                let Some(d) = weak.upgrade() else { return };
                if d.stop.load(Ordering::Relaxed) {
                    return;
                }
                if last_probe.map_or(true, |t| t.elapsed() >= PROBE_EVERY) {
                    last_probe = Some(Instant::now());
                    // NIC の増減(Wi-Fi の付け外し・VPN)に追従するよう毎回引き直す
                    let targets = receiver::broadcast_targets(&bind);
                    let extra = d.extra_dest.lock().unwrap().clone();
                    let pkt = proto::pack_subscribe(seq, true, &proto::WILDCARD_MAC);
                    seq = seq.wrapping_add(1);
                    let ok = receiver::send_all(&sock, port, &targets, extra.as_deref(), &pkt);
                    *d.net_error.lock().unwrap() = (ok == 0).then(|| {
                        format!("問いかけを送信できません(宛先 {})。\n{}",
                                targets.join(" "), receiver::SEND_FAIL_HINT)
                    });
                    *d.sub_dest.lock().unwrap() = targets.join(" ");
                }
                // 受けている間は d を握らない(落とせば次の周で止まれるように)
                drop(d);
                let got = sock.recv_from(&mut buf);
                let Some(d) = weak.upgrade() else { return };
                let mut changed = false;
                if let Ok((n, src)) = got {
                    if let Ok(Packet::Announce(a)) = proto::parse(&buf[..n]) {
                        let mut boards = d.boards.lock().unwrap();
                        let before = boards.get(&a.mac).map(|b| (b.name.clone(), b.addr.clone()));
                        receiver::upsert_board(&mut boards, &src.ip().to_string(), &a,
                                               Instant::now());
                        let b = &boards[&a.mac];
                        changed |= before != Some((b.name.clone(), b.addr.clone()));
                    }
                }
                {
                    let mut boards = d.boards.lock().unwrap();
                    let n = boards.len();
                    boards.retain(|_, b| b.last_seen.elapsed() < GONE_AFTER);
                    changed |= boards.len() != n;
                }
                if changed {
                    repaint();
                }
            }
        })
        .expect("discovery スレッドを起動できません");
    d
}
