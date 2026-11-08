//! 取りこぼしが続いたら間引きを 1/2 にする判定。
//!
//! ★**原因を問わず、症状で決める。** iPhone が熱を持ってクロックが落ちたとき、
//!   バッテリーが減って性能が抑えられたとき、Wi-Fi が途切れるとき、遅い PC の
//!   どれでも「パケットを落とす」「復調が追いつかずフィールドを飛ばす」として
//!   現れる。iPhone 15 Pro で 8fsc のコンポジットを10分ほど受けると、発熱で
//!   復調が 15ms → 30ms になり、毎秒 6,500〜10,600 パケットを落として音が
//!   ブチブチ切れた。1/2(対モード)にすると欠落も音切れもほぼ 0 になった。
//!
//! ★**自動では戻さない。** 1/2 にすると負荷が下がって取りこぼしが消えるので、
//!   「戻してよいか」は測れない。試しに戻すと行き来(ピンポン)になるので、
//!   自動で落としていることを表示し、戻すのはユーザーに任せる。

use std::time::{Duration, Instant};

/// 1回の判定に使う区間
const WINDOW: Duration = Duration::from_secs(5);
/// 落としたパケットの割合がこれ以上なら「取りこぼし」
const LOST_RATIO: f64 = 0.02;
/// 復調で飛ばしたフィールドの割合がこれ以上なら「取りこぼし」
const SKIPPED_RATIO: f64 = 0.10;
/// 何区間続いたら落とすか。一瞬の途切れ(モード切替など)では落とさない
const STREAK: u32 = 2;

/// 統計の累積値(StatsSnapshot から写す)
#[derive(Clone, Copy, Default)]
pub struct Counts {
    pub packets: u64,
    pub lost: u64,
    pub fields: u64,
    pub skipped: u64,
}

#[derive(Default)]
pub struct Detector {
    start: Option<(Instant, Counts)>,
    streak: u32,
}

impl Detector {
    /// 判定をやり直す(受けていないとき、手で間引きを変えたときなど)
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// 累積値を渡す。取りこぼしが STREAK 区間続いたら true
    pub fn feed(&mut self, now: Instant, c: Counts) -> bool {
        let Some((t0, c0)) = self.start else {
            self.start = Some((now, c));
            return false;
        };
        if now.duration_since(t0) < WINDOW {
            return false;
        }
        self.start = Some((now, c));
        // 累積値が戻った(受信のやり直し)区間は数えない
        if c.packets < c0.packets || c.lost < c0.lost || c.fields < c0.fields
            || c.skipped < c0.skipped
        {
            self.streak = 0;
            return false;
        }
        let (p, l) = ((c.packets - c0.packets) as f64, (c.lost - c0.lost) as f64);
        let (f, s) = ((c.fields - c0.fields) as f64, (c.skipped - c0.skipped) as f64);
        let lossy = p + l > 0.0 && l / (p + l) >= LOST_RATIO;
        let behind = f > 0.0 && s / f >= SKIPPED_RATIO;
        if lossy || behind {
            self.streak += 1;
        } else {
            self.streak = 0;
        }
        self.streak >= STREAK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5秒ごとに、毎秒 pps パケット・lps 欠落・60 フィールド・sps 飛ばしを足す
    fn run(d: &mut Detector, windows: &[(u64, u64, u64)]) -> Vec<bool> {
        let t0 = Instant::now();
        let mut c = Counts::default();
        let mut out = vec![d.feed(t0, c)];
        for (i, &(pps, lps, sps)) in windows.iter().enumerate() {
            c.packets += pps * 5;
            c.lost += lps * 5;
            c.fields += 60 * 5;
            c.skipped += sps * 5;
            out.push(d.feed(t0 + WINDOW * (i as u32 + 1), c));
        }
        out
    }

    #[test]
    fn healthy_stream_never_triggers() {
        let mut d = Detector::default();
        let r = run(&mut d, &[(32_000, 1, 0); 10]);
        assert!(r.iter().all(|x| !x));
    }

    #[test]
    fn sustained_loss_triggers_on_second_window() {
        let mut d = Detector::default();
        // iPhone が熱を持ったときの実測(毎秒 24,000 受けて 7,000 落とす)
        let r = run(&mut d, &[(32_000, 1, 0), (24_000, 7_000, 0), (24_000, 7_000, 0)]);
        assert_eq!(r, vec![false, false, false, true]);
    }

    #[test]
    fn sustained_decode_skips_trigger() {
        let mut d = Detector::default();
        // 欠落は無いが、復調が毎秒 13 フィールド追いつかない(有線の iPhone の実測)
        let r = run(&mut d, &[(32_000, 1, 13), (32_000, 1, 13)]);
        assert_eq!(r, vec![false, false, true]);
    }

    #[test]
    fn single_bad_window_does_not_trigger() {
        let mut d = Detector::default();
        // モード切替などの一瞬の途切れ
        let r = run(&mut d, &[(32_000, 1, 0), (20_000, 12_000, 30), (32_000, 1, 0),
                              (20_000, 12_000, 30), (32_000, 1, 0)]);
        assert!(r.iter().all(|x| !x));
    }

    #[test]
    fn small_loss_is_ignored() {
        let mut d = Detector::default();
        // Wi-Fi の Pixel 程度(毎秒約45)は気にしない
        let r = run(&mut d, &[(32_000, 45, 2); 6]);
        assert!(r.iter().all(|x| !x));
    }
}
