//! タッチの身振りを「タップ」「スクロール」「ドラッグ」に振り分ける(モバイル)。
//!
//! ★**egui はタッチをマウスとして扱う。** 指を置いた瞬間に押下が飛ぶので、
//!   パネルをスクロールしようとして指がスライダーの上に乗ると、その場で値が
//!   変わる(押した位置へ飛ぶ)。数値欄(DragValue)も同じで、Pixel で LED の
//!   明るさを意図せず変えてボードへ送ってしまった。
//!
//! そこで egui に渡す前に、押下を少し預かって身振りを決める:
//!
//! - 動かずに離した → タップ。預かった押下と離しをそのまま渡す
//! - 最初に縦へ動いた → スクロール。押下は捨て、指の移動をホイールに変える
//!   (トラックパッドと同じ Start/Move/End の相を付けるので、egui の
//!   ホイール平滑化が挟まらず指に遅れずに付いてくる)
//! - 最初に横へ動いた → ドラッグ。預かった分を渡し、以後は素通し
//!   (スライダーは横に動かして合わせる)
//!
//! - 画面の左端から右へ払った → エッジスワイプ。押下は渡さず、払い終えたら
//!   `take_edge_swipe` で知らせる(パネルを閉じる。iOS の「戻る」と同じ身振り。
//!   Android のジェスチャーナビゲーションでは OS が先に取って「戻る」キーになる)
//!
//! スクロールの指を勢いよく離したら、慣性で滑らせる(iOS / Android の標準の
//! 振る舞い)。ホイールに変えているので egui の側には慣性が無く、指を離した
//! ところでぴたりと止まって使いづらかった。離す直前の速さで毎フレーム
//! ホイールを送り、指数的に減速させる。途中で画面に触れたら止める。
//!
//! マウスやトラックパッド(iPad に繋いだもの)には手を出さない。Touch イベントが
//! 来ている間の押下だけを預かる。

use eframe::egui::{Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2};

/// 身振りを決めるまでの移動量[pt]。egui のクリック判定(6pt)より少し大きくして、
/// タップのつもりの指のぶれをスクロールと取り違えないようにする
const DECIDE_DIST: f32 = 10.0;
/// 左端からこの幅[pt]の中で触れ始めたらエッジスワイプの候補
const EDGE_WIDTH: f32 = 20.0;
/// エッジスワイプとみなす右への移動量[pt]
const EDGE_SWIPE_DIST: f32 = 60.0;
/// 離す直前の速さを測る区間[秒]
const VELOCITY_WINDOW: f64 = 0.08;
/// 最後に動いてからこれ以上経って離したら、止めてから離したとみなす[秒]
const FLING_STALE: f64 = 0.05;
/// これより遅く離したら滑らせない、滑らせている速さがこれを切ったら止める[pt/秒]
const FLING_MIN: f32 = 60.0;
/// 減速の時定数[秒]。iOS の標準(1ms ごとに 0.998 倍 = 約0.5秒)に合わせる
const FLING_TAU: f32 = 0.5;

#[derive(Default)]
pub struct TouchGesture {
    /// 指が画面に触れている(Touch の Start〜End/Cancel)
    touching: bool,
    state: State,
    /// いまのフレームの時刻[秒]
    now: f64,
    /// 滑っているのを止めるために触れた。その指を動かさずに離しても
    /// タップにはしない(下のボタンが押されないように)
    stopped_fling: bool,
    /// エッジスワイプを払い終えた(take_edge_swipe で取る)
    edge_swiped: bool,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    /// 押下を預かって、身振りが決まるのを待っている
    Pending { origin: Pos2, held: Vec<Event> },
    /// `recent` は直近の (時刻, 位置)。離したときの速さを測る
    Scrolling { last: Pos2, recent: Vec<(f64, Pos2)> },
    Dragging,
    /// 左端から右へ払っている。`origin` は触れ始めた位置
    EdgeSwipe { origin: Pos2, last: Pos2 },
    /// 指を離したあとの慣性。`at` は最後に進めた時刻、`pos` はスクロールさせる
    /// 位置(ScrollArea はポインタの下にあるものを回す)
    Flinging { vel: Vec2, at: f64, pos: Pos2 },
}

impl TouchGesture {
    /// 1フレーム分の入力を書き換える(`raw_input_hook` から呼ぶ)。
    /// `now` はそのフレームの時刻[秒]
    pub fn filter(&mut self, events: &mut Vec<Event>, now: f64) {
        self.now = now;
        let input = std::mem::take(events);
        for ev in input {
            self.step(ev, events);
        }
        self.advance_fling(events);
    }

    /// 左端からのエッジスワイプを払い終えたか(取ると消える)
    pub fn take_edge_swipe(&mut self) -> bool {
        std::mem::take(&mut self.edge_swiped)
    }

    /// 慣性で滑っている。呼ぶ側は次のフレームを描かせる(入力が無くても進める)
    pub fn flinging(&self) -> bool {
        matches!(self.state, State::Flinging { .. })
    }

    fn advance_fling(&mut self, out: &mut Vec<Event>) {
        let State::Flinging { vel, at, pos } = self.state else { return };
        let dt = (self.now - at).max(0.0) as f32;
        if dt <= 0.0 {
            return;
        }
        // 減速ぶんを積分して進める(フレームの間隔が揺れても進む量が変わらない)
        let k = (-dt / FLING_TAU).exp();
        let step = vel * FLING_TAU * (1.0 - k);
        let vel = vel * k;
        out.push(Event::PointerMoved(pos));
        out.push(wheel(step, TouchPhase::Move));
        if vel.length() < FLING_MIN {
            out.push(wheel(Vec2::ZERO, TouchPhase::End));
            out.push(Event::PointerGone);
            self.state = State::Idle;
        } else {
            self.state = State::Flinging { vel, at: self.now, pos };
        }
    }

    fn step(&mut self, ev: Event, out: &mut Vec<Event>) {
        if let Event::Touch { phase, .. } = &ev {
            match phase {
                TouchPhase::Start => {
                    self.touching = true;
                    // 滑っている途中で触れたら止める(押さえて止める、と同じ)
                    self.stopped_fling = false;
                    if let State::Flinging { .. } = self.state {
                        out.push(wheel(Vec2::ZERO, TouchPhase::End));
                        self.state = State::Idle;
                        self.stopped_fling = true;
                    }
                }
                TouchPhase::End | TouchPhase::Cancel => self.touching = false,
                TouchPhase::Move => {}
            }
            out.push(ev);
            return;
        }
        let state = std::mem::take(&mut self.state);
        self.state = match (state, ev) {
            (State::Idle, ev @ Event::PointerButton {
                pressed: true, button: PointerButton::Primary, pos, ..
            }) if self.touching => State::Pending { origin: pos, held: vec![ev] },

            (State::Pending { origin, mut held }, Event::PointerMoved(p)) => {
                held.push(Event::PointerMoved(p));
                let d = p - origin;
                if d.length() < DECIDE_DIST {
                    State::Pending { origin, held }
                } else if origin.x <= EDGE_WIDTH && d.x > d.y.abs() {
                    // 押下は渡さない(左端のスライダーやボタンを動かさない)
                    State::EdgeSwipe { origin, last: p }
                } else if d.y.abs() > d.x.abs() {
                    // ScrollArea はポインタの下にあるものを回すので、位置を先に渡す
                    out.push(Event::PointerMoved(p));
                    out.push(wheel(Vec2::ZERO, TouchPhase::Start));
                    out.push(wheel(d, TouchPhase::Move));
                    State::Scrolling { last: p, recent: vec![(self.now, p)] }
                } else {
                    out.extend(held);
                    State::Dragging
                }
            }
            (State::Pending { held, .. }, ev @ Event::PointerButton { pressed: false, .. }) => {
                if !std::mem::take(&mut self.stopped_fling) {
                    out.extend(held);
                    out.push(ev);
                }
                State::Idle
            }

            (State::Scrolling { last, mut recent }, Event::PointerMoved(p)) => {
                out.push(Event::PointerMoved(p));
                out.push(wheel(p - last, TouchPhase::Move));
                recent.push((self.now, p));
                recent.retain(|(t, _)| self.now - t <= VELOCITY_WINDOW);
                State::Scrolling { last: p, recent }
            }
            (State::Scrolling { last, recent }, Event::PointerButton { pressed: false, .. }) => {
                // 離す直前の区間の平均の速さ。最後に動いてから時間が経っていれば
                // 指を止めてから離したので滑らせない
                let vel = match (recent.first(), recent.last()) {
                    (Some(&(t0, p0)), Some(&(t1, p1)))
                        if t1 > t0 && self.now - t1 <= FLING_STALE =>
                    {
                        (p1 - p0) / (t1 - t0) as f32
                    }
                    _ => Vec2::ZERO,
                };
                // 縦のスクロールなので縦だけを滑らせる
                let vel = Vec2::new(0.0, vel.y);
                if vel.length() >= FLING_MIN {
                    State::Flinging { vel, at: self.now, pos: last }
                } else {
                    out.push(wheel(Vec2::ZERO, TouchPhase::End));
                    State::Idle
                }
            }

            (State::EdgeSwipe { origin, .. }, Event::PointerMoved(p)) => {
                State::EdgeSwipe { origin, last: p }
            }
            (State::EdgeSwipe { origin, last }, Event::PointerButton { pressed: false, .. }) => {
                let d = last - origin;
                self.edge_swiped = d.x >= EDGE_SWIPE_DIST && d.x > d.y.abs();
                State::Idle
            }

            (State::Dragging, ev @ Event::PointerButton { pressed: false, .. }) => {
                out.push(ev);
                State::Idle
            }

            // 指を離すと PointerGone が来るが、滑っている間はポインタの位置で
            // ScrollArea を選ぶので、ポインタを消さずにおく(止まったときに消す)
            (state @ State::Flinging { .. }, Event::PointerGone) => state,

            // 指が取り消された(着信など)。押下を渡していないなら何も起きなかったことにする
            (state, Event::PointerGone) => {
                if let State::Scrolling { .. } = state {
                    out.push(wheel(Vec2::ZERO, TouchPhase::Cancel));
                }
                out.push(Event::PointerGone);
                State::Idle
            }

            (state, ev) => {
                out.push(ev);
                state
            }
        };
    }
}

fn wheel(delta: Vec2, phase: TouchPhase) -> Event {
    Event::MouseWheel { unit: MouseWheelUnit::Point, delta, phase, modifiers: Modifiers::NONE }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{TouchDeviceId, TouchId, pos2};

    fn touch(phase: TouchPhase) -> Event {
        Event::Touch {
            device_id: TouchDeviceId(0), id: TouchId(0), phase, pos: pos2(0.0, 0.0), force: None,
        }
    }
    fn press(x: f32, y: f32, pressed: bool) -> Event {
        Event::PointerButton {
            pos: pos2(x, y), button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE,
        }
    }
    fn run(g: &mut TouchGesture, evs: Vec<Event>) -> Vec<Event> {
        run_at(g, evs, 0.0)
    }
    fn run_at(g: &mut TouchGesture, evs: Vec<Event>, t: f64) -> Vec<Event> {
        let mut v = evs;
        g.filter(&mut v, t);
        v
    }
    fn presses(v: &[Event]) -> usize {
        v.iter().filter(|e| matches!(e, Event::PointerButton { pressed: true, .. })).count()
    }
    fn wheel_y(v: &[Event]) -> f32 {
        v.iter()
            .map(|e| match e {
                Event::MouseWheel { delta, .. } => delta.y,
                _ => 0.0,
            })
            .sum()
    }

    /// ★**縦のスワイプでスライダーに押下が届かないこと。** これが本題
    #[test]
    fn vertical_swipe_scrolls_without_pressing() {
        let mut g = TouchGesture::default();
        let mut out = run(&mut g, vec![touch(TouchPhase::Start), press(100.0, 300.0, true)]);
        out.extend(run(&mut g, vec![Event::PointerMoved(pos2(101.0, 280.0))]));
        out.extend(run(&mut g, vec![Event::PointerMoved(pos2(102.0, 250.0))]));
        out.extend(run(&mut g, vec![press(102.0, 250.0, false), touch(TouchPhase::End)]));
        assert_eq!(presses(&out), 0, "押下が渡っている: {out:?}");
        assert!(!out.iter().any(|e| matches!(e, Event::PointerButton { .. })));
        // 指が上へ50pt = 中身も上へ50pt(負)
        assert!((wheel_y(&out) + 50.0).abs() < 1e-3, "{}", wheel_y(&out));
    }

    /// 横に動かしたらスライダーを操作できる(押下も移動もそのまま届く)
    #[test]
    fn horizontal_drag_passes_through() {
        let mut g = TouchGesture::default();
        let mut out = run(&mut g, vec![touch(TouchPhase::Start), press(100.0, 300.0, true)]);
        assert_eq!(presses(&out), 0, "決まる前に押下が出ている");
        out.extend(run(&mut g, vec![Event::PointerMoved(pos2(130.0, 303.0))]));
        assert_eq!(presses(&out), 1);
        out.extend(run(&mut g, vec![Event::PointerMoved(pos2(160.0, 305.0))]));
        out.extend(run(&mut g, vec![press(160.0, 305.0, false)]));
        assert_eq!(wheel_y(&out), 0.0);
        assert!(out.iter().any(|e| matches!(e, Event::PointerButton { pressed: false, .. })));
    }

    /// 動かさずに離したらタップ(押下と離しが順に届く)
    #[test]
    fn tap_is_delivered_on_release() {
        let mut g = TouchGesture::default();
        let mut out = run(&mut g, vec![touch(TouchPhase::Start), press(50.0, 50.0, true)]);
        out.extend(run(&mut g, vec![Event::PointerMoved(pos2(52.0, 51.0))]));
        out.extend(run(&mut g, vec![press(52.0, 51.0, false), touch(TouchPhase::End)]));
        let buttons: Vec<bool> = out
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { pressed, .. } => Some(*pressed),
                _ => None,
            })
            .collect();
        assert_eq!(buttons, vec![true, false]);
    }

    /// マウス(Touch イベントが無い押下)には手を出さない
    #[test]
    fn mouse_is_untouched() {
        let mut g = TouchGesture::default();
        let out = run(&mut g, vec![press(10.0, 10.0, true), Event::PointerMoved(pos2(10.0, 60.0))]);
        assert_eq!(presses(&out), 1);
        assert_eq!(wheel_y(&out), 0.0);
    }

    /// 縦に勢いよく払って離したら、離したあとも同じ向きに滑り、やがて止まる
    #[test]
    fn flick_keeps_scrolling_and_stops() {
        let mut g = TouchGesture::default();
        let mut out = run_at(&mut g, vec![touch(TouchPhase::Start), press(100.0, 400.0, true)], 0.0);
        // 60Hz で毎フレーム 20pt 上へ(= 1200pt/秒)
        for i in 1..=5 {
            let y = 400.0 - 20.0 * i as f32;
            out.extend(run_at(&mut g, vec![Event::PointerMoved(pos2(100.0, y))], i as f64 / 60.0));
        }
        out.extend(run_at(&mut g, vec![press(100.0, 300.0, false), touch(TouchPhase::End),
                                       Event::PointerGone], 5.0 / 60.0));
        assert!(g.flinging(), "離したあと滑っていない");
        let during_finger = wheel_y(&out);
        let mut after = 0.0;
        let mut frames = 0;
        while g.flinging() && frames < 600 {
            frames += 1;
            after += wheel_y(&run_at(&mut g, vec![], (5 + frames) as f64 / 60.0));
        }
        assert!(!g.flinging(), "止まらない");
        // 指と同じ向き(上 = 負)に、速さ×時定数 ≒ 600pt ほど進む
        assert!(during_finger < 0.0 && after < -300.0 && after > -700.0, "{after}");
        assert_eq!(presses(&out), 0);
    }

    /// 指を止めてから離したら滑らない
    #[test]
    fn slow_release_does_not_fling() {
        let mut g = TouchGesture::default();
        run_at(&mut g, vec![touch(TouchPhase::Start), press(100.0, 400.0, true)], 0.0);
        run_at(&mut g, vec![Event::PointerMoved(pos2(100.0, 300.0))], 0.1);
        run_at(&mut g, vec![press(100.0, 300.0, false), touch(TouchPhase::End)], 0.5);
        assert!(!g.flinging());
    }

    /// 滑っている途中で触れたら止まる
    #[test]
    fn touch_stops_fling() {
        let mut g = TouchGesture::default();
        run_at(&mut g, vec![touch(TouchPhase::Start), press(100.0, 400.0, true)], 0.0);
        for i in 1..=5 {
            let y = 400.0 - 20.0 * i as f32;
            run_at(&mut g, vec![Event::PointerMoved(pos2(100.0, y))], i as f64 / 60.0);
        }
        run_at(&mut g, vec![press(100.0, 300.0, false), touch(TouchPhase::End)], 5.0 / 60.0);
        assert!(g.flinging());
        let mut out = run_at(&mut g, vec![touch(TouchPhase::Start), press(100.0, 200.0, true)], 0.2);
        assert!(!g.flinging());
        out.extend(run_at(&mut g, vec![press(100.0, 200.0, false), touch(TouchPhase::End)], 0.3));
        assert!(!out.iter().any(|e| matches!(e, Event::PointerButton { .. })),
                "止めるための触れがタップになった: {out:?}");
        // 次のタップはふつうに届く
        let mut out = run_at(&mut g, vec![touch(TouchPhase::Start), press(50.0, 50.0, true)], 1.0);
        out.extend(run_at(&mut g, vec![press(50.0, 50.0, false), touch(TouchPhase::End)], 1.1));
        assert_eq!(presses(&out), 1);
    }

    /// 左端から右へ払ったらエッジスワイプ。途中の押下も移動も渡さない
    #[test]
    fn edge_swipe_is_reported_and_swallowed() {
        let mut g = TouchGesture::default();
        let mut out = run(&mut g, vec![touch(TouchPhase::Start), press(5.0, 400.0, true)]);
        out.extend(run(&mut g, vec![Event::PointerMoved(pos2(30.0, 405.0))]));
        out.extend(run(&mut g, vec![Event::PointerMoved(pos2(120.0, 410.0))]));
        out.extend(run(&mut g, vec![press(120.0, 410.0, false), touch(TouchPhase::End)]));
        assert!(g.take_edge_swipe());
        assert!(!g.take_edge_swipe(), "一度取ったら消える");
        assert!(!out.iter().any(|e| matches!(e, Event::PointerButton { .. })), "{out:?}");
        assert_eq!(wheel_y(&out), 0.0);
    }

    /// 左端でも、払いが短ければエッジスワイプにしない。端から離れた横の動きは
    /// ふつうのドラッグ(スライダー)
    #[test]
    fn short_or_inner_swipe_is_not_edge_swipe() {
        let mut g = TouchGesture::default();
        run(&mut g, vec![touch(TouchPhase::Start), press(5.0, 400.0, true)]);
        run(&mut g, vec![Event::PointerMoved(pos2(40.0, 400.0))]);
        run(&mut g, vec![press(40.0, 400.0, false), touch(TouchPhase::End)]);
        assert!(!g.take_edge_swipe());

        let mut out = run(&mut g, vec![touch(TouchPhase::Start), press(100.0, 400.0, true)]);
        out.extend(run(&mut g, vec![Event::PointerMoved(pos2(200.0, 400.0))]));
        out.extend(run(&mut g, vec![press(200.0, 400.0, false), touch(TouchPhase::End)]));
        assert!(!g.take_edge_swipe());
        assert_eq!(presses(&out), 1);
    }
}
