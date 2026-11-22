//! ボードとのつながり(後勝ちの排他と、切断の検出)。
//!
//! ★**ボードの映像送り先は1つだけ。** 以前は2秒ごとの SUBSCRIBE が毎回送り先を
//!   奪ったので、2台の Viewer が同じボードを見ると交互に奪い合い、どちらも
//!   まともに映らなかった。いまは次のように分ける:
//!   - **奪う**のは通常の SUBSCRIBE。起動時・ボードを選んだとき・「接続」を
//!     押したときだけ出す(後勝ち)。
//!   - **延長**は RENEW。ボードは送信元がいまの送り先のときだけ受け付けるので、
//!     奪われた側の定期送信では奪い返せない。
//! ★**「奪われた」と「ボードが消えた」は映像が止まるだけでは見分けられない。**
//!   ボードは ANNOUNCE の flags で「送信中か」「宛先が送り先か」を返すので、
//!   それで分ける。奪われたら止まったまま(押すまで繋ぎ直さない)、消えたら
//!   画面を消して、戻ってきて誰も使っていなければ繋ぎ直す。
//! ★**「あなた宛でない」を1つ受けただけでは奪われたことにしない。** 古い
//!   ゲートウェアは宛先と「あなた宛」を別の時点で決めていたので、他の Viewer の
//!   発見の問いかけがその間に挟まると、こちら宛なのに「あなた宛でない」が届いた
//!   (実機で一度、使用中に止まった)。疑いにしておき、映像が途絶えたときだけ
//!   奪われたとする。本当に奪われたなら映像はすぐ止まり、誤報なら来続ける。
//! ★**古いゲートウェアは flags を返さない**(OWNER_INFO が立たない)。その相手には
//!   RENEW が通常の SUBSCRIBE として効いてしまうので、以前どおり通常の
//!   SUBSCRIBE で延長する(奪い合いは残るが、切断の表示だけは効く)。

use std::time::{Duration, Instant};

use crate::protocol as proto;

/// 延長の間隔。ボードは最後の SUBSCRIBE から10秒で購読を切る。
pub const RENEW_EVERY: Duration = Duration::from_secs(2);
/// 映像が来なくなってから「切断」にするまで。
///
/// ボードは購読中なら入力が無くても MODE を毎秒送るので、これだけ来なければ
/// 送り先から外れているかボードが居ない。
pub const SILENCE: Duration = Duration::from_secs(3);
/// 奪った直後に届いた ANNOUNCE で「奪われた」と判定しない時間。
///
/// 奪う前にボードが出した定期 ANNOUNCE が、奪ったあとに届くことがある。
const TAKE_GRACE: Duration = Duration::from_secs(1);
/// 「あなた宛でない」を受けてから、映像が来なければ奪われたとするまで。
///
/// ボードは購読中なら入力が無くても MODE を毎秒送るので、それより長くする。
const TAKEN_CONFIRM: Duration = Duration::from_millis(1500);
/// 疑ってからこれより後に届いた映像だけを「まだこちらが送り先」の証拠にする。
/// 奪われる前に出た映像が遅れて届くことがあるため。
const IN_FLIGHT: Duration = Duration::from_millis(100);
/// 繋ぎ直し(通常の SUBSCRIBE)を出す最短の間隔。
const RETAKE_MIN: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum State {
    /// 繋ぐボードが無い
    #[default]
    Idle,
    /// SUBSCRIBE を出して映像を待っている
    Connecting,
    /// 映像が来ている
    Live,
    /// 映像が途絶えた(ボードの電源断・ケーブル抜け)。戻れば繋ぎ直す
    Lost,
    /// 他の Viewer に奪われた。「接続」を押すまで何も出さない
    Taken,
}

/// 受信ループが出すべきもの。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// 通常の SUBSCRIBE(送り先を奪う)
    Take,
    /// RENEW(いまの送り先なら延長する。奪わない)
    Renew,
}

#[derive(Default)]
pub struct Link {
    pub state: State,
    /// 繋いでいるボード。違うボードを選び直したら Taken は解ける
    mac: Option<[u8; 6]>,
    take_pending: bool,
    last_take: Option<Instant>,
    last_send: Option<Instant>,
    last_stream: Option<Instant>,
    /// ボードが送り先の情報を返すか(新しいゲートウェア)
    owner_aware: bool,
    /// 「送信中・あなた宛でない」を受けた時刻。映像が来続ければ取り消す
    suspect: Option<Instant>,
}

impl Link {
    /// ボードを掴んだ。**同じボードを奪われたまま掴み直しただけなら止まったまま。**
    ///
    /// ボードが一覧から一度消えて戻ると掴み直しになるが、それは利用者の操作では
    /// ないので、奪い返してはいけない。
    pub fn on_claim(&mut self, mac: [u8; 6], now: Instant) {
        if self.state == State::Taken && self.mac == Some(mac) {
            return;
        }
        self.mac = Some(mac);
        self.connect(now);
    }

    /// ボードを手放した(未選択にした・ボードが一覧から消えた)。
    pub fn on_release(&mut self) {
        if self.state != State::Taken {
            self.state = State::Idle;
        }
        self.take_pending = false;
        self.last_stream = None;
        self.suspect = None;
    }

    /// 利用者が「接続」を押した。奪われていても奪い返す。
    pub fn connect(&mut self, now: Instant) {
        if self.mac.is_none() {
            return;
        }
        self.state = State::Connecting;
        self.take_pending = true;
        self.last_stream = None;
        self.suspect = None;
        let _ = now;
    }

    /// 宛先を変えたので、次の周で通常の SUBSCRIBE を出し直す(奪われていれば出さない)。
    pub fn retake(&mut self) {
        if matches!(self.state, State::Connecting | State::Live | State::Lost) {
            self.take_pending = true;
            self.last_take = None;
        }
    }

    /// 映像の流れ(MODE/LINE/AUDIO)が届いた。送り先でなければ来ないものだけ渡す。
    pub fn on_stream(&mut self, now: Instant) {
        if self.state == State::Taken || self.mac.is_none() {
            return;
        }
        self.last_stream = Some(now);
        self.state = State::Live;
        if self.suspect.is_some_and(|t| now.duration_since(t) >= IN_FLIGHT) {
            self.suspect = None;
        }
    }

    /// 掴んでいるボードの ANNOUNCE。奪われたかは疑うだけで、決めるのは tick。
    pub fn on_announce(&mut self, mac: [u8; 6], flags: u8, now: Instant) {
        if self.mac != Some(mac) || self.state == State::Taken || self.state == State::Idle {
            return;
        }
        self.owner_aware = flags & proto::ANN_FLAG_OWNER_INFO != 0;
        if !self.owner_aware || flags & proto::ANN_FLAG_YOURS != 0 {
            return;
        }
        if flags & proto::ANN_FLAG_STREAMING != 0 {
            if self.last_take.map_or(false, |t| now.duration_since(t) < TAKE_GRACE) {
                return;
            }
            self.suspect.get_or_insert(now);
            return;
        }
        // 誰も受けていない(ボードの再起動・購読の失効)。こちらから繋ぎ直す
        self.take_pending = true;
    }

    /// 毎周呼ぶ。戻り値の .1 が true なら画面を消す。
    pub fn tick(&mut self, now: Instant) -> (Action, bool) {
        if self.suspect.is_some_and(|t| now.duration_since(t) >= TAKEN_CONFIRM) {
            self.suspect = None;
            self.state = State::Taken;
            self.take_pending = false;
            return (Action::None, true);
        }
        let mut blank = false;
        if self.state == State::Live
            && self.last_stream.map_or(true, |t| now.duration_since(t) >= SILENCE)
        {
            self.state = State::Lost;
            blank = true;
        }
        let active = matches!(self.state, State::Connecting | State::Live | State::Lost);
        if !active || self.mac.is_none() {
            return (Action::None, blank);
        }
        if self.take_pending
            && self.last_take.map_or(true, |t| now.duration_since(t) >= RETAKE_MIN)
        {
            self.take_pending = false;
            self.last_take = Some(now);
            self.last_send = Some(now);
            return (Action::Take, blank);
        }
        if self.last_send.map_or(true, |t| now.duration_since(t) >= RENEW_EVERY) {
            self.last_send = Some(now);
            // 古いゲートウェアには RENEW が通常の SUBSCRIBE として効くので、
            // 区別せず以前どおりの通常 SUBSCRIBE にしておく
            return (if self.owner_aware { Action::Renew } else { Action::Take }, blank);
        }
        (Action::None, blank)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0xd8, 0x47, 0x8f, 0xcc, 0xf7, 0x6f];
    const OWNED: u8 = proto::ANN_FLAG_OWNER_INFO | proto::ANN_FLAG_STREAMING | proto::ANN_FLAG_YOURS;
    const OTHER: u8 = proto::ANN_FLAG_OWNER_INFO | proto::ANN_FLAG_STREAMING;
    const FREE: u8 = proto::ANN_FLAG_OWNER_INFO;

    fn s(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    /// 起動して繋がるまで: 最初は奪い、以後は延長だけを出す。
    fn live(t0: Instant) -> Link {
        let mut l = Link::default();
        l.on_claim(MAC, t0);
        assert_eq!(l.tick(t0).0, Action::Take);
        l.on_announce(MAC, OWNED, s(t0, 10));
        l.on_stream(s(t0, 20));
        assert_eq!(l.state, State::Live);
        l
    }

    #[test]
    fn keepalive_renews_instead_of_taking() {
        let t0 = Instant::now();
        let mut l = live(t0);
        l.on_stream(s(t0, 1900));
        assert_eq!(l.tick(s(t0, 1000)).0, Action::None);
        assert_eq!(l.tick(s(t0, 2000)).0, Action::Renew);
    }

    /// ★**奪われたら止まり、勝手に奪い返さない。** これが崩れると取り合いに戻る。
    #[test]
    fn taken_stops_until_connect() {
        let t0 = Instant::now();
        let mut l = live(t0);
        l.on_stream(s(t0, 2990));
        l.on_announce(MAC, OTHER, s(t0, 3000));
        // 奪われる前に出た映像が遅れて届いても、まだこちらのものとは見ない
        l.on_stream(s(t0, 3050));
        // 疑っている間も延長(RENEW)は出す。RENEW では奪い返せない
        assert_eq!(l.tick(s(t0, 4000)), (Action::Renew, false));
        assert_ne!(l.state, State::Taken, "映像の途絶えを待たずに止まった");
        let (action, blank) = l.tick(s(t0, 4500));
        assert!(blank, "画面を消していない");
        assert_eq!(action, Action::None);
        assert_eq!(l.state, State::Taken);
        for ms in (4500..60_000).step_by(500) {
            assert_eq!(l.tick(s(t0, ms)).0, Action::None, "{ms}ms で何か出した");
        }
        // ボードが一覧から消えて戻っても止まったまま
        l.on_release();
        l.on_claim(MAC, s(t0, 61_000));
        assert_eq!(l.state, State::Taken);
        assert_eq!(l.tick(s(t0, 61_000)).0, Action::None);
        // 「接続」で奪い返す
        l.connect(s(t0, 62_000));
        assert_eq!(l.tick(s(t0, 62_000)).0, Action::Take);
    }

    /// 奪う前の ANNOUNCE が奪った直後に届いても、奪われたことにしない。
    #[test]
    fn stale_announce_right_after_take_is_ignored() {
        let t0 = Instant::now();
        let mut l = Link::default();
        l.on_claim(MAC, t0);
        assert_eq!(l.tick(t0).0, Action::Take);
        l.on_announce(MAC, OTHER, s(t0, 50));
        l.tick(s(t0, 2000));
        assert_ne!(l.state, State::Taken);
    }

    /// ★**こちら宛なのに「あなた宛でない」が1つ届いても止まらない。** 古い
    /// ゲートウェアは宛先とフラグを別の時点で決めていたので、他の Viewer の
    /// 発見の問いかけが挟まるとこうなった。映像が来続けていれば誤報。
    #[test]
    fn stray_not_yours_while_streaming_is_ignored() {
        let t0 = Instant::now();
        let mut l = live(t0);
        l.on_announce(MAC, OTHER, s(t0, 3000));
        for ms in (3000..10_000).step_by(16) {
            l.on_stream(s(t0, ms));
            assert_eq!(l.tick(s(t0, ms)).1, false, "{ms}ms で画面を消した");
        }
        assert_eq!(l.state, State::Live);
        // 入力が無く MODE が毎秒来るだけでも誤報と分かる
        l.on_announce(MAC, OTHER, s(t0, 10_000));
        l.on_stream(s(t0, 10_900));
        assert_eq!(l.tick(s(t0, 11_600)).1, false);
        assert_eq!(l.state, State::Live);
        // 疑いは消えているので、次の本物の横取りは改めて待ってから止まる
        l.on_announce(MAC, OTHER, s(t0, 12_000));
        assert_ne!(l.tick(s(t0, 13_000)).0, Action::Take);
        assert!(l.tick(s(t0, 13_500)).1);
        assert_eq!(l.state, State::Taken);
    }

    /// ★**電源断は切断表示、戻ってきたら繋ぎ直す。**
    #[test]
    fn lost_then_board_returns() {
        let t0 = Instant::now();
        let mut l = live(t0);
        let (_, blank) = l.tick(s(t0, 3100));
        assert!(blank, "途絶えたのに画面を消していない");
        assert_eq!(l.state, State::Lost);
        // 再起動したボードは誰にも送っていない
        l.on_announce(MAC, FREE, s(t0, 8000));
        assert_eq!(l.tick(s(t0, 8000)).0, Action::Take);
        l.on_stream(s(t0, 8100));
        assert_eq!(l.state, State::Live);
    }

    /// 古いゲートウェア(flags 無し)には以前どおり通常の SUBSCRIBE で延長する。
    #[test]
    fn legacy_gateware_keeps_plain_subscribe() {
        let t0 = Instant::now();
        let mut l = Link::default();
        l.on_claim(MAC, t0);
        assert_eq!(l.tick(t0).0, Action::Take);
        l.on_announce(MAC, 0, s(t0, 10));
        l.on_stream(s(t0, 20));
        l.on_stream(s(t0, 1990));
        assert_eq!(l.tick(s(t0, 2000)).0, Action::Take);
    }

    /// 別のボードを選び直したら、奪われていても繋ぎに行く。
    #[test]
    fn other_board_clears_taken() {
        let t0 = Instant::now();
        let mut l = live(t0);
        l.on_announce(MAC, OTHER, s(t0, 3000));
        l.tick(s(t0, 3100));
        l.tick(s(t0, 4600));
        assert_eq!(l.state, State::Taken);
        let other = [0x02, 0, 0, 0, 0, 1];
        l.on_release();
        l.on_claim(other, s(t0, 4000));
        assert_eq!(l.state, State::Connecting);
        assert_eq!(l.tick(s(t0, 4000)).0, Action::Take);
    }
}
