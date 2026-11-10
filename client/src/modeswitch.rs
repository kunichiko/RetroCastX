//! 映像のモードが切り替わる間、管面を黒にする。
//!
//! 実機のマルチスキャンモニタも、周波数が変わると偏向を切り替える間は絵を
//! 出さない。こちらも切り替わりの途中の絵(前のモードの PLL で取り込んだ
//! 崩れた絵)と、途中の帯域の表示を見せない。
//!
//! ★**ボードの fH は1秒窓で数えている**(gateware/retrocastx_capture.py)。
//!   MODE も1秒おきなので、切り替えた直後は
//!     1. 前のモードの値(最大1秒)
//!     2. 窓の途中で切り替わった混ざった値(15.98kHz と 31.5kHz の間で約24kHz)
//!     3. 正しい値
//!   の順に届く。以前はこれをそのまま帯域にしていたので、15→31kHz の途中で
//!   一瞬 24kHz の管面と PLL になり、31kHz なのに 15kHz と出ている時間があった。
//!
//! そこで:
//! - **帯域は、同じ帯域の MODE が2回続いたら確定する**(混ざった値は1回しか出ない)
//! - **黒にし始めるのは、届くライン数が変わった瞬間。** ボードはラインを今の
//!   信号のまま送ってくるので、1フレームに届いた最大のライン番号は MODE より
//!   ずっと早く変わる
//! - **解くのは、MODE が届いたラインと合っていて(古い MODE ではない)、帯域が
//!   確定し、ライン数が数フレーム続けて同じで、帯域を決めてから数フレーム経ち、
//!   MODE の htotal が書いた PLL と合ったとき**
//!
//! ★**htotal を待つ理由。** Viewer はフレームの幅と横位置を MODE の htotal で決める。
//!   1秒窓で測った htotal は PLL を書き換えてから最大1秒古く、15→31kHz で PLL を
//!   1104 にした後も 1820 が届いて絵が 0.61 倍に縮んだ(2026-11-10 実機)。
//!   gw-v1.5.0 以降は書いた PLL をそのまま htotal で送り、変わったらすぐ MODE を出す
//!   ので待ちはほぼ無い。古いゲートウェアでは最大1秒ほど黒が延びる。
//!
//! 時刻は引数で受ける(テストで時間を進めるため)。

use std::time::{Duration, Instant};

/// 帯域を確定するのに要る、同じ帯域の MODE の連続回数
const CONFIRM: u32 = 2;
/// ボードが1フレームごとの組で fH を出す(CAP_FRAME_CLOCKS)なら1回でよい
const CONFIRM_FRESH: u32 = 1;
/// ライン数がこのフレーム数続けて同じなら落ち着いたとみなす
const STABLE_FRAMES: u32 = 3;
/// 帯域を決めてから、これだけのフレームを受けてから解く。帯域が決まると PLL と
/// 位相を TVP へ書き直すので、TVP がロックし直すまでと、その前に取り込んで
/// 送り途中のフレームを捨てる分。時間で待つと fps によって足りなくなる
const SETTLE_FRAMES: u32 = 4;
/// これを超えたら黒をやめる(信号が抜けた等で永久に黒のままにならないように)
const TIMEOUT: Duration = Duration::from_secs(5);
/// 端子を替えてから、前の信号と同じ組の MODE を無視する時間。両方の端子に同じ
/// 信号が来ていると違う組は来ないので、これを過ぎたら同じ組でも信じる
const STALE_FOR: Duration = Duration::from_secs(1);

/// 1フレームの組の sys クロック数が同じ信号のものか。組は ppm 単位で揃うので、
/// 1/1024 あれば十分(X68000 15kHz と NTSC でも 2% 違う)
fn same_frame(a: u32, b: u32) -> bool {
    a.abs_diff(b) <= a.max(b) / 1024
}

/// ライン数が「同じ」とみなす幅。インターレースの半ライン違いや、フレームの
/// 最後の行を取りこぼした程度では切り替わりと見ない
fn near(a: u32, b: u32) -> bool {
    a.abs_diff(b) <= (a.max(b) / 16).max(4)
}

/// MODE が届いたラインと合っているか(古い MODE ではないか)。
///
/// 1フレームに届くライン数は MODE の vactive(スロット数)に収まり、その半分は
/// 超える。15kHz の MODE(vactive 520)のまま 31kHz のライン(〜1135)が来ていれば
/// 合わない。逆も同じ(31kHz の 1136 に 15kHz の 519 は半分に届かない)。
///
/// ★**両側に 1/32 の幅を持たせる。** どちらも黒が5秒のタイムアウトまで解けなかった
///   (2026-11-10 実機の X68000 15kHz):
///   - VSYNC がフィールド2枚に1回しか来ない(vactive 1042)とき、届くラインは 519
///     まで。ちょうど半分に 4 足りない
///   - インターレース(1フィールド 260.5 ライン)では vactive 522 に対し、第2フィールド
///     が半ライン下に入るので 525 スロットまで届く
fn mode_fits(lines: u32, vactive: u32) -> bool {
    let tol = vactive / 32;
    vactive > 0 && lines <= vactive + tol && lines * 2 + tol >= vactive
}

#[derive(Debug, Default)]
pub struct ModeSwitch {
    /// 確定した帯域。None は未確定(起動直後)
    band: Option<u32>,
    /// 落ち着いていたときの1フレームのライン数
    lines: Option<u32>,
    /// 直近のライン数と、それが続いているフレーム数
    cand: (u32, u32),
    /// 直近の MODE の vactive
    vactive: u32,
    /// 黒にし始めた時刻
    since: Option<Instant>,
    /// 黒にしてから、ラインと合う MODE で帯域が確定したか
    confirmed_since: bool,
    /// 帯域を決めてから受けたフレーム数
    settle: u32,
    /// ボードが1フレームごとの組で fH を出すか(CAP_FRAME_CLOCKS)
    fresh: bool,
    /// 直近の MODE の1フレームの組の sys クロック数(0 = 付いていない)
    frame_clocks: u32,
    /// 端子を替えたときの組と、それを無視する期限(`restart`)
    stale: Option<(u32, Instant)>,
}

impl ModeSwitch {
    /// 確定した帯域[kHz]。follow_band はこれを使う(途中の値を採らない)
    pub fn band(&self) -> Option<u32> {
        self.band
    }

    /// ボードが CAP_FRAME_CLOCKS を持つか。持てば、ラインと合う MODE を1回で信用する
    pub fn set_fresh(&mut self, fresh: bool) {
        self.fresh = fresh;
    }

    /// 直近のライン数と、それが続いているフレーム数(ログ用)
    pub fn cand(&self) -> (u32, u32) {
        self.cand
    }

    /// 切り替わりの最中か(管面を黒にする)
    pub fn blanking(&self) -> bool {
        self.since.is_some()
    }

    fn start(&mut self, now: Instant) {
        if self.since.is_none() {
            self.since = Some(now);
            self.confirmed_since = false;
        }
    }

    /// 入力の端子を替えた。ライン数や帯域が同じでも、絵は別の信号なので黒にし、
    /// 替えた後の MODE で確かめ直す(X68000 15kHz → コンポジットは帯域もライン数も
    /// ほぼ同じで、ライン数の変化では黒にならない)。
    ///
    /// ★**替えた時点の組と同じ MODE は、前の信号のものとして採らない。** 替えてから
    ///   200ms ほどは、届くラインも MODE もまだ前の信号で、両者が合ってしまう。
    ///   これで解いていたので、前の端子の帯域と管面で3〜6フレーム映ってから黒に
    ///   戻っていた(2026-11-10 実機: X68000 31kHz → コンポジット)
    pub fn restart(&mut self, now: Instant) {
        self.since = Some(now);
        self.confirmed_since = false;
        self.settle = 0;
        self.stale = (self.frame_clocks != 0).then_some((self.frame_clocks, now + STALE_FOR));
    }

    /// 1フレーム届いた。`lines` はそのフレームの最大のライン番号 + 1
    pub fn on_frame(&mut self, lines: u32, now: Instant) {
        if lines == 0 {
            return;
        }
        self.settle = self.settle.saturating_add(1);
        if near(lines, self.cand.0) {
            self.cand.1 = self.cand.1.saturating_add(1);
        } else {
            self.cand = (lines, 1);
        }
        match self.lines {
            Some(s) if !near(lines, s) && self.band.is_some() => self.start(now),
            None if self.cand.1 >= STABLE_FRAMES && self.since.is_none() => {
                self.lines = Some(self.cand.0)
            }
            _ => {}
        }
    }

    /// MODE が1つ届いた。`repeats` は同じ帯域が続いた回数(この MODE を含む)
    pub fn on_mode(&mut self, band: u32, repeats: u32, vactive: u32, frame_clocks: u32,
                   now: Instant) {
        self.frame_clocks = frame_clocks;
        if let Some((fc, until)) = self.stale {
            if now < until && same_frame(frame_clocks, fc) {
                return;
            }
            self.stale = None;
        }
        self.vactive = vactive;
        let fits = mode_fits(self.cand.0, vactive);
        let need = if self.fresh { CONFIRM_FRESH } else { CONFIRM };
        if repeats < need {
            // 帯域が変わった最初の MODE(混ざった値の可能性がある)
            if self.band.is_some_and(|b| b != band) {
                self.start(now);
            }
            return;
        }
        // ★**帯域が変わった MODE は、ラインと照合せずに採る。** 新しい帯域を言う
        //   MODE は、前のモードの古い MODE ではあり得ない。照合で止めていた頃は、
        //   ラインと vactive が少し食い違うだけ(インターレースで 525 / 522)で帯域を
        //   決められず、5秒のタイムアウトで前の帯域のまま解き、すぐ新しい帯域で
        //   黒にし直してもう5秒かかった(2026-11-10 実機)
        if self.band != Some(band) {
            if self.band.is_some() {
                self.start(now);
            }
            self.band = Some(band);
            self.settle = 0;
            if self.since.is_some() {
                self.confirmed_since = true;
            }
            return;
        }
        // 帯域が同じなら、届いたラインと合う MODE でしか解く根拠にしない(前のモードの
        // 古い MODE を聞いて、切り替えの途中で解いてしまわないように)
        if self.since.is_some() && fits {
            self.confirmed_since = true;
        }
    }

    /// 毎フレーム呼ぶ。解く条件がそろったら黒をやめる。
    /// `htotal_ok` は「MODE の htotal が、いま TVP に書いてある PLL と合っている」
    pub fn tick(&mut self, now: Instant, htotal_ok: bool) {
        let Some(since) = self.since else { return };
        let done = self.confirmed_since
            && self.cand.1 >= STABLE_FRAMES
            && mode_fits(self.cand.0, self.vactive)
            && self.settle >= SETTLE_FRAMES
            && htotal_ok;
        if done || now.duration_since(since) >= TIMEOUT {
            self.since = None;
            self.lines = Some(self.cand.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 55Hz 相当のフレームを n 枚送る
    fn frames(s: &mut ModeSwitch, t: &mut Instant, lines: u32, n: u32) {
        for _ in 0..n {
            *t += Duration::from_millis(18);
            s.on_frame(lines, *t);
            s.tick(*t, true);
        }
    }

    /// 15kHz(vactive 260)で落ち着いた状態を作る
    fn settled_15k() -> (ModeSwitch, Instant) {
        let mut s = ModeSwitch::default();
        let mut t = Instant::now();
        frames(&mut s, &mut t, 250, 5);
        s.on_mode(15, 1, 260, 0, t);
        frames(&mut s, &mut t, 250, 30);
        s.on_mode(15, 2, 260, 0, t);
        frames(&mut s, &mut t, 250, 30);
        assert_eq!(s.band(), Some(15));
        assert!(!s.blanking());
        (s, t)
    }

    /// 1秒窓の MODE で 15→31kHz: 古い値 → 混ざった値(24kHz)→ 正しい値 × 2。
    /// 24kHz は確定せず、黒は正しい帯域が確定してから解ける
    #[test]
    fn blanks_through_a_15_to_31_switch_and_skips_the_mixed_band() {
        let (mut s, mut t) = settled_15k();
        frames(&mut s, &mut t, 560, 1);
        assert!(s.blanking(), "ライン数が変わった瞬間に黒にする");
        // 古い MODE(15kHz、まだ続けて届く)。ラインと合わないので確定に使わない
        s.on_mode(15, 3, 260, 0, t);
        frames(&mut s, &mut t, 560, 50);
        assert!(s.blanking(), "古い MODE では解かない");
        s.on_mode(24, 1, 440, 0, t);
        frames(&mut s, &mut t, 560, 50);
        assert_eq!(s.band(), Some(15), "混ざった 24kHz は確定しない");
        assert!(s.blanking());
        s.on_mode(31, 1, 568, 0, t);
        frames(&mut s, &mut t, 560, 50);
        assert!(s.blanking(), "1回目の 31kHz ではまだ確定しない");
        s.on_mode(31, 2, 568, 0, t);
        assert_eq!(s.band(), Some(31));
        frames(&mut s, &mut t, 560, 3);
        assert!(s.blanking(), "確定してすぐは PLL を書き直す時間を待つ");
        frames(&mut s, &mut t, 560, 10);
        assert!(!s.blanking(), "確定して落ち着いたら解く");
    }

    /// 帯域の変わらないモード替え(31kHz の 768×512 ↔ 256×256)では黒にしない
    #[test]
    fn same_line_count_does_not_blank() {
        let (mut s, mut t) = settled_15k();
        frames(&mut s, &mut t, 252, 50);
        s.on_mode(15, 3, 260, 0, t);
        frames(&mut s, &mut t, 248, 50);
        assert!(!s.blanking());
    }

    /// 同じ帯域でライン数だけ変わる(VGA の 480 ↔ 400 ライン)。帯域は変わらないが、
    /// ラインと合う MODE が届けば解ける
    #[test]
    fn same_band_line_change_clears_on_a_fitting_mode() {
        let mut s = ModeSwitch::default();
        let mut t = Instant::now();
        frames(&mut s, &mut t, 1000, 5);
        s.on_mode(31, 2, 1050, 0, t);
        frames(&mut s, &mut t, 1000, 30);
        assert!(!s.blanking());
        frames(&mut s, &mut t, 830, 1);
        assert!(s.blanking());
        s.on_mode(31, 3, 1050, 0, t); // 古い MODE。830 は 1050 に収まるが、確定は続く
        frames(&mut s, &mut t, 830, 20);
        assert!(!s.blanking(), "ラインと合う同じ帯域の MODE で解ける");
    }

    /// 信号が抜けて MODE が来なくなっても、黒のままにならない
    #[test]
    fn gives_up_after_the_timeout() {
        let (mut s, mut t) = settled_15k();
        frames(&mut s, &mut t, 560, 1);
        assert!(s.blanking());
        frames(&mut s, &mut t, 560, 300);
        assert!(!s.blanking());
    }

    /// 新しいゲートウェア(CAP_FRESH_MODE)なら、ラインと合う MODE 1回で解ける。
    /// 古い値の MODE(ラインと合わない)では解かない
    #[test]
    fn fresh_board_clears_on_the_first_fitting_mode() {
        let (mut s, mut t) = settled_15k();
        s.set_fresh(true);
        frames(&mut s, &mut t, 560, 1);
        assert!(s.blanking());
        s.on_mode(15, 4, 260, 0, t);
        frames(&mut s, &mut t, 560, 20);
        assert!(s.blanking(), "ラインと合わない古い MODE では解かない");
        s.on_mode(31, 1, 568, 0, t);
        assert_eq!(s.band(), Some(31));
        frames(&mut s, &mut t, 560, 12);
        assert!(!s.blanking());
    }

    /// MODE の htotal が書いた PLL と合うまでは解かない(古いゲートウェアは htotal を
    /// 1秒窓で測るので、PLL を書き換えてから最大1秒古い)
    #[test]
    fn waits_until_htotal_matches_the_written_pll() {
        let (mut s, mut t) = settled_15k();
        s.set_fresh(true);
        frames(&mut s, &mut t, 560, 1);
        s.on_mode(31, 1, 568, 0, t);
        for _ in 0..20 {
            t += Duration::from_millis(18);
            s.on_frame(560, t);
            s.tick(t, false);
        }
        assert!(s.blanking(), "htotal が古いうちは解かない");
        t += Duration::from_millis(18);
        s.on_frame(560, t);
        s.tick(t, true);
        assert!(!s.blanking());
    }

    /// VSYNC がフィールド2枚に1回(vactive 1042)で、届くラインが 519 までのとき。
    /// ちょうど半分に足りないが合う MODE として扱う。古い 31kHz の MODE とは区別する
    #[test]
    fn half_rate_vsync_still_fits() {
        assert!(mode_fits(519, 1042));
        assert!(!mode_fits(519, 1136), "31kHz の MODE に 15kHz のラインは合わない");
        assert!(!mode_fits(1135, 520), "15kHz の MODE に 31kHz のラインは合わない");
        let (mut s, mut t) = settled_15k();
        s.set_fresh(true);
        frames(&mut s, &mut t, 1135, 1);
        s.on_mode(31, 1, 1136, 0, t);
        frames(&mut s, &mut t, 1135, 10);
        assert!(!s.blanking());
        frames(&mut s, &mut t, 519, 1);
        assert!(s.blanking());
        s.on_mode(15, 1, 1042, 0, t);
        frames(&mut s, &mut t, 519, 10);
        assert!(!s.blanking(), "タイムアウトを待たずに解く");
    }

    /// 31→15kHz のインターレース。届くラインが vactive を少し超える(525 / 522)。
    /// 帯域はすぐ決まり、タイムアウトを待たずに解ける
    #[test]
    fn interlace_slots_past_vactive_still_clear() {
        assert!(mode_fits(525, 522));
        let (mut s, mut t) = settled_15k();
        s.set_fresh(true);
        frames(&mut s, &mut t, 1135, 1);
        s.on_mode(31, 1, 1136, 0, t);
        frames(&mut s, &mut t, 1135, 10);
        assert!(!s.blanking());
        frames(&mut s, &mut t, 525, 1);
        assert!(s.blanking());
        s.on_mode(15, 1, 1136, 0, t); // 最初の MODE の vactive はまだ前のモード
        assert_eq!(s.band(), Some(15), "帯域の変更はラインと照合せずに採る");
        s.on_mode(15, 2, 522, 0, t);
        frames(&mut s, &mut t, 525, 10);
        assert!(!s.blanking(), "タイムアウトを待たずに解く");
    }

    /// 端子を替えたら、帯域もライン数も同じでも黒にし、替えた後の MODE で解く
    #[test]
    fn port_change_blanks_even_with_the_same_band_and_lines() {
        let (mut s, mut t) = settled_15k();
        s.set_fresh(true);
        s.restart(t);
        assert!(s.blanking());
        frames(&mut s, &mut t, 250, 20);
        assert!(s.blanking(), "替えた後の MODE が来るまで解かない");
        s.on_mode(15, 5, 262, 0, t);
        frames(&mut s, &mut t, 250, 1);
        assert!(!s.blanking());
    }

    /// 端子を替えた直後に届く、前の信号と同じ組の MODE では解かない
    /// (X68000 31kHz → コンポジットで、前の帯域のまま数フレーム映っていた)
    #[test]
    fn port_change_ignores_modes_from_the_previous_signal() {
        const X68K: u32 = 1_467_158; // X68000 15kHz の2フィールド分
        const NTSC: u32 = 1_501_500;
        let (mut s, mut t) = settled_15k();
        s.set_fresh(true);
        s.on_mode(15, 6, 262, X68K, t);
        s.restart(t);
        s.on_mode(15, 7, 262, X68K + 3, t);
        frames(&mut s, &mut t, 250, 10);
        assert!(s.blanking(), "前の信号の組の MODE では解かない");
        s.on_mode(15, 8, 262, NTSC, t);
        frames(&mut s, &mut t, 250, 1);
        assert!(!s.blanking(), "組が変わった MODE で解く");
    }

    /// 両方の端子に同じ信号が来ていると組は変わらない。1秒過ぎたら同じ組でも信じる
    #[test]
    fn port_change_with_the_same_signal_clears_after_a_second() {
        const X68K: u32 = 1_467_158;
        let (mut s, mut t) = settled_15k();
        s.set_fresh(true);
        s.on_mode(15, 6, 262, X68K, t);
        s.restart(t);
        frames(&mut s, &mut t, 250, 30);
        s.on_mode(15, 7, 262, X68K, t);
        frames(&mut s, &mut t, 250, 5);
        assert!(s.blanking(), "1秒以内は同じ組を採らない");
        frames(&mut s, &mut t, 250, 30);
        s.on_mode(15, 8, 262, X68K, t);
        frames(&mut s, &mut t, 250, 1);
        assert!(!s.blanking(), "1秒を過ぎたら同じ組でも解く(5秒のタイムアウトは待たない)");
    }

    /// 起動直後(帯域が未確定)は黒にしない。最初の帯域は2回目の MODE で決まる
    #[test]
    fn startup_does_not_blank_and_confirms_on_the_second_mode() {
        let mut s = ModeSwitch::default();
        let mut t = Instant::now();
        frames(&mut s, &mut t, 560, 5);
        s.on_mode(31, 1, 568, 0, t);
        assert_eq!(s.band(), None);
        frames(&mut s, &mut t, 560, 30);
        s.on_mode(31, 2, 568, 0, t);
        assert_eq!(s.band(), Some(31));
        assert!(!s.blanking());
    }
}
