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
//! マウスやトラックパッド(iPad に繋いだもの)には手を出さない。Touch イベントが
//! 来ている間の押下だけを預かる。

use eframe::egui::{Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2};

/// 身振りを決めるまでの移動量[pt]。egui のクリック判定(6pt)より少し大きくして、
/// タップのつもりの指のぶれをスクロールと取り違えないようにする
const DECIDE_DIST: f32 = 10.0;

#[derive(Default)]
pub struct TouchGesture {
    /// 指が画面に触れている(Touch の Start〜End/Cancel)
    touching: bool,
    state: State,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    /// 押下を預かって、身振りが決まるのを待っている
    Pending { origin: Pos2, held: Vec<Event> },
    Scrolling { last: Pos2 },
    Dragging,
}

impl TouchGesture {
    /// 1フレーム分の入力を書き換える(`raw_input_hook` から呼ぶ)
    pub fn filter(&mut self, events: &mut Vec<Event>) {
        let input = std::mem::take(events);
        for ev in input {
            self.step(ev, events);
        }
    }

    fn step(&mut self, ev: Event, out: &mut Vec<Event>) {
        if let Event::Touch { phase, .. } = &ev {
            match phase {
                TouchPhase::Start => self.touching = true,
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
                } else if d.y.abs() > d.x.abs() {
                    // ScrollArea はポインタの下にあるものを回すので、位置を先に渡す
                    out.push(Event::PointerMoved(p));
                    out.push(wheel(Vec2::ZERO, TouchPhase::Start));
                    out.push(wheel(d, TouchPhase::Move));
                    State::Scrolling { last: p }
                } else {
                    out.extend(held);
                    State::Dragging
                }
            }
            (State::Pending { held, .. }, ev @ Event::PointerButton { pressed: false, .. }) => {
                out.extend(held);
                out.push(ev);
                State::Idle
            }

            (State::Scrolling { last }, Event::PointerMoved(p)) => {
                out.push(Event::PointerMoved(p));
                out.push(wheel(p - last, TouchPhase::Move));
                State::Scrolling { last: p }
            }
            (State::Scrolling { .. }, Event::PointerButton { pressed: false, .. }) => {
                out.push(wheel(Vec2::ZERO, TouchPhase::End));
                State::Idle
            }

            (State::Dragging, ev @ Event::PointerButton { pressed: false, .. }) => {
                out.push(ev);
                State::Idle
            }

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
        let mut v = evs;
        g.filter(&mut v);
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
}
