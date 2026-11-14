//! ボードとの往復時間(ターンアラウンド)。
//!
//! CONFIG の GET はボードのハードウェアがそのまま返す(CPU を通らない)ので、
//! 送ってから応答が届くまでが、ネットワークと OS の受信経路の往復になる。
//! 受信スレッドが一定間隔で build_id(0x0009。読み取り専用で値が変わらない)を
//! 問い合わせ、`rx_t`(受信スレッドが受け取った時刻)との差を数える。
//!
//! ★**応答は seq を返さない**(ボードは 0 を入れる)ので、同時に飛ばすのは1つだけ
//!   にしてキーで突き合わせる。ボードの CONFIG 応答は1枠しかなく、他の問い合わせ
//!   (表示遅延の集計など)と重なると片方が返らないことがある。返らなかったものは
//!   時間切れとして数え、値には入れない。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 問い合わせの間隔
pub const EVERY: Duration = Duration::from_millis(100);
/// これより遅い応答は返らなかったとみなす
pub const TIMEOUT: Duration = Duration::from_millis(500);
/// 集計に残す直近の回数
const KEEP: usize = 100;

#[derive(Default)]
pub struct Rtt {
    /// 応答待ちの問い合わせを送った時刻
    sent: Option<Instant>,
    last_sent: Option<Instant>,
    /// 直近の往復時間 [µs]
    samples: VecDeque<u32>,
    pub timeouts: u64,
}

impl Rtt {
    /// 送る頃合いなら true を返し、送ったものとして覚える。
    /// 応答が来ないまま TIMEOUT を過ぎたものは時間切れに数えて次を送る
    pub fn due(&mut self, now: Instant) -> bool {
        if let Some(t) = self.sent {
            if now.saturating_duration_since(t) < TIMEOUT {
                return false;
            }
            self.timeouts += 1;
            self.sent = None;
        }
        if self.last_sent.is_some_and(|t| now.saturating_duration_since(t) < EVERY) {
            return false;
        }
        self.sent = Some(now);
        self.last_sent = Some(now);
        true
    }

    /// build_id の応答が届いた。こちらの問い合わせへの応答なら true
    pub fn on_reply(&mut self, rx_t: Instant) -> bool {
        let Some(t) = self.sent.take() else { return false };
        let us = rx_t.saturating_duration_since(t).as_micros().min(u32::MAX as u128) as u32;
        if self.samples.len() >= KEEP {
            self.samples.pop_front();
        }
        self.samples.push_back(us);
        true
    }

    /// 繋がっていない間は前の値を残さない
    pub fn reset(&mut self) {
        *self = Self { timeouts: 0, ..Default::default() };
    }

    /// 直近の (回数, 最小, 中央, 最大) [µs]
    pub fn stats(&self) -> Option<(usize, u32, u32, u32)> {
        let mut v: Vec<u32> = self.samples.iter().copied().collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        Some((v.len(), v[0], v[v.len() / 2], v[v.len() - 1]))
    }

    /// 1行の説明(パネルと全画面の stderr 用)
    pub fn describe(&self) -> Option<String> {
        let (n, min, med, max) = self.stats()?;
        let ms = |us: u32| us as f64 / 1000.0;
        Some(format!("往復 最小 {:.2} / 中央 {:.2} / 最大 {:.2} ms(直近 {n} 回、時間切れ {})",
                     ms(min), ms(med), ms(max), self.timeouts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_probe_at_a_time_and_timeouts() {
        let mut r = Rtt::default();
        let t0 = Instant::now();
        assert!(r.due(t0));
        // 応答待ちの間は送らない
        assert!(!r.due(t0 + Duration::from_millis(200)));
        assert!(r.on_reply(t0 + Duration::from_micros(350)));
        // 頼んでいない応答は数えない
        assert!(!r.on_reply(t0 + Duration::from_millis(1)));
        // 間隔が空くまで送らない
        assert!(!r.due(t0 + Duration::from_millis(50)));
        assert!(r.due(t0 + Duration::from_millis(100)));
        // 返らないまま TIMEOUT を過ぎたら時間切れにして次を送る
        assert!(r.due(t0 + Duration::from_millis(100) + TIMEOUT));
        assert_eq!(r.timeouts, 1);
        assert_eq!(r.stats(), Some((1, 350, 350, 350)));
    }
}
