//! 描画遅延の計測。信号が来てから画面に出るまでを段階ごとに測る。
//!
//! ★**基準は「そのラインのパケットを受け取った時刻」。** ボードはラインを
//!   取り込んだ直後に送るので、受信時刻は「同じ信号を直結の液晶が受け取った
//!   時刻」と 0.1ms 程度しか違わない。液晶は走査に合わせてラインごとに出すが、
//!   こちらはフレームが揃うまで出せないので、**画面の上ほど遅れる**。そこで
//!   遅延は上端(最初のライン)・中央・下端(最後のライン)の3点で出す。
//!
//! 段階(時刻の並び):
//! ```text
//! first ─ 走査 ─ last ─ 待ち ─ trigger ─ 組立 ─ published ─ UI ─ picked ─ 描画 ─ submitted ─ 表示 ─ presented
//! (最初のライン) (最後のライン) (次フレームの先頭) (Sharedへ)      (UIが取得)   (present)            (画面に出た)
//! ```
//! - 走査: 最初から最後のラインまで。液晶との差の大半はここ(上端だけが被る)
//! - 待ち: 組立は**次のフレームの先頭ライン**で前のフレームを確定させるので、その分
//! - 組立: 復調・計測・Shared へ渡すまで
//! - UI: 描画側が拾うまで。ウィンドウ表示は60Hzで回るので 0〜16.7ms
//! - 描画: 拾ってから present を呼ぶまで(フルスクリーンのみ)
//! - 表示: present から実際に画面に出るまで(フルスクリーン + Metal のみ。
//!   MTLDrawable.presentedTime)
//!
//! ウィンドウ表示は eframe が present するので「描画」「表示」は測れない。
//! その先に macOS のコンポジタ(通常1フレーム前後)と液晶自身の遅延が乗る。

use std::time::{Duration, Instant};

/// 1フレームが経た時刻。受信側で埋めて CompletedFrame に載せる。
#[derive(Clone, Copy, Debug)]
pub struct FrameStamps {
    /// 最初のラインを受け取った時刻
    pub first: Instant,
    /// 最後のラインを受け取った時刻
    pub last: Instant,
    /// 確定のきっかけ(次フレームの先頭ライン)を受け取った時刻
    pub trigger: Instant,
    /// Shared へ渡した時刻
    pub published: Instant,
}

/// 受信側でラインの到着を追い、フレーム確定時に FrameStamps を作る。
#[derive(Default)]
pub struct LineClock {
    first: Option<Instant>,
    last: Option<Instant>,
}

impl LineClock {
    /// LINE パケットを受け取ったら呼ぶ。`completed` はこのパケットで前の
    /// フレームが確定したか(組立が返したか)。確定したら前フレームの
    /// (first, last, trigger) を返し、このパケットを次フレームの先頭とする。
    pub fn on_line(&mut self, rx: Instant, completed: bool)
        -> Option<(Instant, Instant, Instant)>
    {
        let done = if completed {
            match (self.first, self.last) {
                (Some(f), Some(l)) => Some((f, l, rx)),
                _ => None,
            }
        } else {
            None
        };
        if completed || self.first.is_none() {
            self.first = Some(rx);
        }
        self.last = Some(rx);
        done
    }
}

/// 段階ひとつ分の集計(平均と最大)
#[derive(Clone, Copy, Debug, Default)]
pub struct Stage {
    pub avg_ms: f32,
    pub max_ms: f32,
}

#[derive(Default, Clone, Copy)]
struct Acc {
    sum: f64,
    max: f64,
    n: u32,
}

impl Acc {
    fn add(&mut self, d: Duration) {
        let ms = d.as_secs_f64() * 1000.0;
        self.sum += ms;
        self.max = self.max.max(ms);
        self.n += 1;
    }
    fn stage(&self) -> Option<Stage> {
        (self.n > 0).then(|| Stage {
            avg_ms: (self.sum / self.n as f64) as f32,
            max_ms: self.max as f32,
        })
    }
}

/// 1秒ごとの集計結果。None の段階は測っていない(その表示方式では測れない)。
#[derive(Clone, Copy, Debug, Default)]
pub struct LatencySnapshot {
    /// 集計したフレーム数
    pub frames: u32,
    pub scan: Option<Stage>,
    pub wait: Option<Stage>,
    pub build: Option<Stage>,
    pub ui: Option<Stage>,
    pub draw: Option<Stage>,
    pub glass: Option<Stage>,
    /// 終点(画面に出た時刻。測れないときは測れた最後の段階)までの遅延
    pub top: Option<Stage>,
    pub mid: Option<Stage>,
    pub bottom: Option<Stage>,
    /// 終点が実際に画面に出た時刻か(false なら途中までの値)
    pub to_glass: bool,
}

/// 描画側が持つ集計器。フレームごとに `add` し、1秒ごとに `take` する。
#[derive(Default)]
pub struct LatencyMeter {
    scan: Acc,
    wait: Acc,
    build: Acc,
    ui: Acc,
    draw: Acc,
    glass: Acc,
    top: Acc,
    mid: Acc,
    bottom: Acc,
    to_glass: bool,
    frames: u32,
}

impl LatencyMeter {
    /// 1フレーム分を足す。`picked` は描画側が拾った時刻、`submitted` は
    /// present を呼んだ時刻、`presented` は実際に画面に出た時刻(測れたときだけ)。
    pub fn add(&mut self, s: &FrameStamps, picked: Instant,
               submitted: Option<Instant>, presented: Option<Instant>)
    {
        let d = |a: Instant, b: Instant| b.saturating_duration_since(a);
        self.scan.add(d(s.first, s.last));
        self.wait.add(d(s.last, s.trigger));
        self.build.add(d(s.trigger, s.published));
        self.ui.add(d(s.published, picked));
        if let Some(t) = submitted {
            self.draw.add(d(picked, t));
            if let Some(p) = presented {
                self.glass.add(d(t, p));
            }
        }
        let end = presented.or(submitted).unwrap_or(picked);
        self.to_glass = presented.is_some();
        self.top.add(d(s.first, end));
        self.bottom.add(d(s.last, end));
        // 中央のラインは最初と最後の中間に届く
        let mid = s.first + d(s.first, s.last) / 2;
        self.mid.add(d(mid, end));
        self.frames += 1;
    }

    /// 集計を取り出して空にする。
    pub fn take(&mut self) -> LatencySnapshot {
        let s = LatencySnapshot {
            frames: self.frames,
            scan: self.scan.stage(),
            wait: self.wait.stage(),
            build: self.build.stage(),
            ui: self.ui.stage(),
            draw: self.draw.stage(),
            glass: self.glass.stage(),
            top: self.top.stage(),
            mid: self.mid.stage(),
            bottom: self.bottom.stage(),
            to_glass: self.to_glass,
        };
        *self = Self::default();
        s
    }
}

impl LatencySnapshot {
    /// 端末・パネル用の2行。`frame_ms` は1フレームの長さ(フレーム換算用)。
    pub fn describe(&self, frame_ms: Option<f32>) -> String {
        let f = |s: Option<Stage>| match s {
            Some(s) => format!("{:.1}", s.avg_ms),
            None => "-".into(),
        };
        let fr = |s: Option<Stage>| match (s, frame_ms) {
            (Some(s), Some(fm)) if fm > 0.0 => format!(" ({:.1}f)", s.avg_ms / fm),
            _ => String::new(),
        };
        let end = if self.to_glass { "画面" } else { "UI取得" };
        format!(
            "遅延→{end}: 上 {}{} / 中 {}{} / 下 {}{} ms\n\
             内訳 走査 {} 待ち {} 組立 {} UI {} 描画 {} 表示 {} ms",
            f(self.top), fr(self.top), f(self.mid), fr(self.mid),
            f(self.bottom), fr(self.bottom),
            f(self.scan), f(self.wait), f(self.build), f(self.ui),
            f(self.draw), f(self.glass))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★**受信の並びどおりに呼んで、前フレームの時刻が取れること。**
    ///   確定のきっかけは次フレームの先頭ラインなので、そのラインを前フレームの
    ///   最後として数えてはいけない(数えると走査が1行ぶん長く、待ちが0になる)。
    #[test]
    fn line_clock_splits_frames_at_the_trigger_line() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut c = LineClock::default();
        assert!(c.on_line(ms(0), false).is_none());   // フレームNの先頭
        assert!(c.on_line(ms(15), false).is_none());  // フレームNの最後
        // 帰線を挟んで N+1 の先頭が来て N が確定
        let (f, l, t) = c.on_line(ms(18), true).unwrap();
        assert_eq!((f, l, t), (ms(0), ms(15), ms(18)));
        // N+1 の先頭はきっかけのライン
        assert!(c.on_line(ms(33), false).is_none());
        let (f, l, _) = c.on_line(ms(36), true).unwrap();
        assert_eq!((f, l), (ms(18), ms(33)));
    }

    /// 上端は走査ぶん下端より遅れる。終点が無い段階は None のまま
    #[test]
    fn meter_reports_top_mid_bottom() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let s = FrameStamps { first: ms(0), last: ms(16), trigger: ms(18), published: ms(19) };
        let mut m = LatencyMeter::default();
        m.add(&s, ms(25), None, None);
        let r = m.take();
        assert_eq!(r.frames, 1);
        assert!((r.top.unwrap().avg_ms - 25.0).abs() < 0.01);
        assert!((r.mid.unwrap().avg_ms - 17.0).abs() < 0.01);
        assert!((r.bottom.unwrap().avg_ms - 9.0).abs() < 0.01);
        assert!(r.draw.is_none() && r.glass.is_none() && !r.to_glass);
        // フルスクリーン: 画面に出た時刻まで
        m.add(&s, ms(20), Some(ms(21)), Some(ms(30)));
        let r = m.take();
        assert!(r.to_glass);
        assert!((r.glass.unwrap().avg_ms - 9.0).abs() < 0.01);
        assert!((r.top.unwrap().avg_ms - 30.0).abs() < 0.01);
    }
}
