//! 他のアプリの中でダミーデバイスを動かす(Viewer のアプリ内デモ)。
//!
//! ★**127.0.0.1 の空きポートだけで待ち受ける。** LAN には出ないので、同じ LAN の
//!   別の Viewer の一覧に現れたり、実機のボードとポートを取り合ったりしない。
//!   埋め込んだアプリは、このアドレス(`Embedded::addr`)へ SUBSCRIBE を出して繋ぐ。
//!   ボードは SUBSCRIBE の送信元へ返すので、アプリ側の受信ポートは何でもよい。
//! ★**34600 には bind しない。** 同じプロセスの Viewer が 0.0.0.0:34600 を掴んで
//!   いると、127.0.0.1:34600 宛の返事がより具体的な bind のこちらへ戻ってしまい、
//!   自分と会話するだけになる(engine.rs の open のコメント)。

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::board::{Board, Input};
use crate::engine::{self, Shared};
use crate::machines::{self, Port};

/// 端子ごとに繋ぐ機種とモード(番号)。単体のダミーデバイスの既定と同じ並び
const INPUTS: [(Port, &str, usize); 4] = [
    (Port::Rgb15, "x68000", 0),
    (Port::Rgb21, "msx", 0),
    (Port::Video, "composite", 0),
    (Port::Digital, "pc6001mk2sr", 0),
];

/// 動いているダミーデバイス。落とすと止まる
pub struct Embedded {
    /// 待ち受けているアドレス(127.0.0.1:空きポート)
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Embedded {
    /// 名前と MAC を決めて送出のスレッドを立てる
    pub fn start(name: &str, mac: [u8; 6]) -> std::io::Result<Embedded> {
        let mut b = Board::new();
        b.set_name(name);
        b.mac = mac;
        // 左上に機種とモードを書く(いま何の信号を見ているかが分かる)
        b.picture.caption = "on".into();
        let ms = machines::machines();
        for (port, key, idx) in INPUTS {
            let Some(mc) = ms.iter().find(|m| m.key == key) else { continue };
            let Some(md) = mc.modes.get(idx) else { continue };
            b.inputs[port as usize] =
                Some(Input { machine_key: mc.key.to_string(), kind: mc.kind, mode: md.clone() });
        }
        let sock = engine::bind([127, 0, 0, 1], 0, false)?;
        let addr = sock.local_addr()?;
        let shared = Shared::new(b);
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("demo-board".into())
            .spawn(move || engine::run(sock, sh))?;
        Ok(Embedded { addr, shared, thread: Some(thread) })
    }
}

impl Drop for Embedded {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol as proto;
    use std::net::UdpSocket;
    use std::time::{Duration, Instant};

    /// SUBSCRIBE を出すと、指名した MAC で ANNOUNCE が返り、映像(LINE)が来る
    #[test]
    fn answers_on_loopback_and_streams() {
        let mac = [0x02, 0x52, 0x43, 0x58, 0xDE, 0x30];
        let dev = Embedded::start("デモ", mac).unwrap();
        assert!(dev.addr.ip().is_loopback());
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let (mut announced, mut lines) = (false, 0);
        let mut buf = [0u8; 2048];
        let until = Instant::now() + Duration::from_secs(5);
        let mut seq = 0u16;
        while Instant::now() < until && !(announced && lines > 100) {
            sock.send_to(&proto::pack_subscribe(seq, false, &mac), dev.addr).unwrap();
            seq = seq.wrapping_add(1);
            // 映像が流れ続けると受信が途切れないので、揃ったら抜ける
            while let (false, Ok((n, _))) = (announced && lines > 100, sock.recv_from(&mut buf)) {
                match proto::parse(&buf[..n]) {
                    Ok(proto::Packet::Announce(a)) if a.mac == mac => announced = true,
                    Ok(proto::Packet::Line(_)) => lines += 1,
                    _ => {}
                }
            }
        }
        assert!(announced, "ANNOUNCE が来ない");
        assert!(lines > 100, "LINE が来ない ({lines})");
    }
}
