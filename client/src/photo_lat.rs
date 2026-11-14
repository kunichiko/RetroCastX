//! 表示遅延の計測(フォトトランジスタ)。ボード側の実体は
//! gateware/retrocastx_latency.py。
//!
//! ボードが映像を白/黒のフレームに差し替え、白いフレームを送り始めてから
//! 画面に貼ったセンサーが光るまでを数える。こちらは開始/停止を書き、
//! 集計(回数・最小・最大・合計)を読み返して出すだけ。
//!
//! ★**latency.rs とは測っているものが違う。** あちらは Viewer の中の時刻
//!   (受信〜present)で、macOS のコンポジタと液晶自身の遅延は入らない。
//!   こちらは画面の光そのものなので、それも含めた端から端までの値になる。
//!
//! ⌘L(Windows/Linux は Ctrl+L)で開始/停止する。低遅延全画面(fullscreen.rs)
//! にはパネルが無いので、全画面に入ってからキーで始め、抜ける前に止める。
//! 集計はボードが持っているので、ウィンドウ表示に戻ればパネルで読める。

use std::time::{Duration, Instant};

use eframe::egui;

use crate::protocol;
use crate::receiver::Shared;
use crate::theme;

const STAT_KEYS: [u16; 6] = [
    protocol::CFG_KEY_PLAT_STATE,
    protocol::CFG_KEY_PLAT_COUNT,
    protocol::CFG_KEY_PLAT_LAST,
    protocol::CFG_KEY_PLAT_MIN,
    protocol::CFG_KEY_PLAT_MAX,
    protocol::CFG_KEY_PLAT_SUM,
];

pub struct PhotoLat {
    /// 何回計ったら止めるか
    pub target: u32,
    /// センサーが光ると High(比較器付きの基板など)。既定は光ると Low
    pub active_high: bool,
    /// こちらが開始を書いてから、止めるまで
    running: bool,
    /// いまの状態に入った時刻(白待ちが長いときの手掛かり)
    state_since: Option<(u32, Instant)>,
    polled: Option<Instant>,
}

impl Default for PhotoLat {
    fn default() -> Self {
        Self { target: 50, active_high: false, running: false, state_since: None, polled: None }
    }
}

/// コンポジット/S端子は白い絵を作れない(差し替えると同期ごと消える)
fn rgb_input(pixfmt: Option<u8>) -> bool {
    !pixfmt.is_some_and(|f| f == protocol::PIXFMT_YC8 || f == protocol::PIXFMT_CVBS8)
}

/// ボードの集計。読めていない値があれば None
struct Stats {
    state: u32,
    count: u32,
    timeouts: u32,
    last_us: u32,
    min_us: u32,
    max_us: u32,
    sum_us: u32,
}

impl Stats {
    fn read(shared: &Shared) -> Option<Stats> {
        let st = shared.config_state.lock().unwrap();
        let g = |k: u16| st.get(&k).copied();
        let count = g(protocol::CFG_KEY_PLAT_COUNT)?;
        Some(Stats {
            state: g(protocol::CFG_KEY_PLAT_STATE)?,
            count: count & 0xFFFF,
            timeouts: count >> 16,
            last_us: g(protocol::CFG_KEY_PLAT_LAST)?,
            min_us: g(protocol::CFG_KEY_PLAT_MIN)?,
            max_us: g(protocol::CFG_KEY_PLAT_MAX)?,
            sum_us: g(protocol::CFG_KEY_PLAT_SUM)?,
        })
    }
}

fn ms(us: u32) -> String {
    format!("{:.2} ms", us as f64 / 1000.0)
}

fn ctl(run: bool, active_high: bool) -> u32 {
    run as u32 | (active_high as u32) << 1
}

/// 開始を書く。0→1 でボードが集計をクリアする。前の結果を出さないよう手元の写しも消す
pub fn send_start(shared: &Shared, active_high: bool) {
    shared.config_queue.lock().unwrap().extend([
        (protocol::CFG_KEY_PLAT_CTL, ctl(false, active_high)),
        (protocol::CFG_KEY_PLAT_CTL, ctl(true, active_high)),
    ]);
    let mut st = shared.config_state.lock().unwrap();
    for k in STAT_KEYS {
        st.remove(&k);
    }
}

pub fn send_stop(shared: &Shared, active_high: bool) {
    shared.config_queue.lock().unwrap().push((protocol::CFG_KEY_PLAT_CTL, ctl(false, active_high)));
}

/// 書いた要求が受信スレッドに拾われるまで待つ(最大 `limit`)。
/// ★**プロセスを終える前に呼ぶ。** 全画面の出入りは起動し直しなので、待たないと
///   停止が送られないまま終わり、切り替えのあいだも白黒の点滅が続く
pub fn flush(shared: &Shared, limit: Duration) {
    let t0 = Instant::now();
    while !shared.config_queue.lock().unwrap().is_empty() && t0.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 集計を読み直して1行にする(全画面の stderr 用)。読めなければ None
pub fn summary(shared: &Shared) -> Option<String> {
    let s = Stats::read(shared)?;
    if s.count == 0 {
        return Some(format!("計れた回数 0(時間切れ {})", s.timeouts));
    }
    let avg = (s.sum_us as f64 / s.count as f64).round() as u32;
    Some(format!("{} 回  平均 {}  最小 {}  最大 {}  時間切れ {}",
                 s.count, ms(avg), ms(s.min_us), ms(s.max_us), s.timeouts))
}

/// 集計の読み出しを頼む
pub fn request_stats(shared: &Shared) {
    shared.config_get_queue.lock().unwrap().extend(STAT_KEYS);
}

impl PhotoLat {
    pub fn running(&self) -> bool {
        self.running
    }

    fn start(&mut self, shared: &Shared) {
        send_start(shared, self.active_high);
        self.running = true;
        self.state_since = None;
    }

    pub fn stop(&mut self, shared: &Shared) {
        send_stop(shared, self.active_high);
        self.running = false;
        // 止めた後の値をもう一度読む
        self.polled = None;
    }

    /// ⌘L。対応していないゲートウェアや RGB 以外の入力では始めない
    pub fn toggle(&mut self, shared: &Shared, caps: u16, pixfmt: Option<u8>) {
        if self.running {
            self.stop(shared);
        } else if caps & protocol::CAP_PHOTO_LAT != 0 && rgb_input(pixfmt) {
            self.start(shared);
        }
    }

    /// `pixfmt` はいま届いている形式(MODE)、`vfreq_hz` はソースのフレームレート。
    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &Shared, caps: u16,
              pixfmt: Option<u8>, vfreq_hz: Option<f32>) {
        if caps & protocol::CAP_PHOTO_LAT == 0 {
            ui.weak("このゲートウェアは対応していません");
            return;
        }
        let rgb = rgb_input(pixfmt);

        // 計測中は速めに、止まっているあいだもセンサーの明暗が見えるように読む
        let every = Duration::from_millis(if self.running { 250 } else { 500 });
        if self.polled.map_or(true, |t| t.elapsed() >= every) {
            self.polled = Some(Instant::now());
            shared.config_get_queue.lock().unwrap().extend(STAT_KEYS);
        }
        ui.ctx().request_repaint_after(every);

        let s = Stats::read(shared);
        if let Some(s) = &s {
            if self.running && s.count >= self.target {
                self.stop(shared);
            }
        }

        ui.horizontal(|ui| {
            theme::label_col(ui, "回数");
            ui.add_enabled(
                !self.running,
                egui::DragValue::new(&mut self.target).range(1..=1000),
            );
            if self.running {
                if ui.button("停止").clicked() {
                    self.stop(shared);
                }
            } else if ui.add_enabled(rgb, egui::Button::new("開始")).clicked() {
                self.start(shared);
            }
        });
        ui.weak(format!("{} で開始/停止(低遅延全画面でも効く)",
                        if cfg!(target_os = "macos") { "⌘L" } else { "Ctrl+L" }));
        ui.add_enabled(
            !self.running,
            egui::Checkbox::new(&mut self.active_high, "センサーは光ると High"),
        )
        .on_hover_text(
            "フォトトランジスタをピンとGNDの間に入れただけなら外したまま(光ると Low)。\n\
             比較器付きのセンサー基板のように、光ると High を出すものなら入れる",
        );
        if !rgb {
            ui.weak("RGB の入力で測ります(コンポジット/S端子では白い絵を作れません)");
        }

        let Some(s) = s else {
            ui.weak("ボードの応答待ち");
            return;
        };
        let state = s.state & 3;
        let lit = s.state & 0x10 != 0;
        let now = Instant::now();
        if self.state_since.map_or(true, |(st, _)| st != state) {
            self.state_since = Some((state, now));
        }
        let held = self.state_since.map_or(Duration::ZERO, |(_, t)| now - t);

        ui.horizontal(|ui| {
            theme::label_col(ui, "センサー");
            if lit {
                ui.colored_label(theme::OK, "● 光っている");
            } else {
                ui.label("○ 暗い");
            }
        })
        .response
        .on_hover_text(
            "デバッグ端子 J7 の pin9(dbg7)。黒い画面で「暗い」、白い画面で\n\
             「光っている」になれば配線と極性は合っています",
        );
        let label = ["停止", "暗転待ち", "白待ち", "計測中"][state as usize];
        theme::kv(ui, "状態", label);
        if s.state & 0x20 != 0 {
            ui.colored_label(theme::AMBER, "映像の購読が切れたので止まりました");
        } else if state == 1 && lit && held >= Duration::from_secs(1) {
            ui.colored_label(theme::AMBER, "黒い画面なのにセンサーが光ったままです。\n\
                                            貼る位置か極性を確かめてください");
        } else if state == 2 && held >= Duration::from_secs(1) {
            ui.colored_label(theme::AMBER, "白いフレームを送り出せません(映像の入力が無い?)");
        }

        theme::kv(ui, "回数", if s.timeouts > 0 {
            format!("{} / {}  時間切れ {}", s.count, self.target, s.timeouts)
        } else {
            format!("{} / {}", s.count, self.target)
        });
        if s.count > 0 {
            let avg = (s.sum_us as f64 / s.count as f64).round() as u32;
            theme::kv(ui, "平均", ms(avg));
            theme::kv(ui, "最小", ms(s.min_us));
            theme::kv(ui, "最大", ms(s.max_us));
            theme::kv(ui, "直近", ms(s.last_us));
        }
        let frame = vfreq_hz.filter(|v| *v > 1.0).map(|v| 1000.0 / v);
        ui.weak(match frame {
            Some(f) => format!("起点はフレームの先頭(VSYNC直後)。1フレーム {f:.2} ms"),
            None => "起点はフレームの先頭(VSYNC直後)".to_string(),
        })
        .on_hover_text(
            "ボードが白いフレームの最初のラインを送り始めてから、センサーが光るまで。\n\
             画面全体を白にするので、センサーが画面の下にあるほど液晶の走査の分だけ\n\
             遅く出ます(上端に貼ると最小)。直結のモニタならそのラインが届いた瞬間に\n\
             出るので、比べるときはセンサーの位置ぶんの走査時間を差し引いてください。\n\
             黒→白の切り替えは毎回 100〜165ms の乱数で待つので、モニタの\n\
             リフレッシュとの位相は毎回ばらけます(最小と最大の幅がその分)",
        );
    }
}
