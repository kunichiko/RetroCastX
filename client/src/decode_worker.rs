//! NTSC 復調を受信ループから切り離す常駐スレッド。
//!
//! ★**受信・組立のスレッドで復調すると、その間パケットを読めない。** Pixel 10 で
//!   8fsc のコンポジットを復調すると1フィールド 13〜15ms かかり、受信ループの
//!   9割近くを占めて受信キューから溢れた。復調をこのスレッドへ移し、組立側は
//!   フィールドが揃ったら材料を写して渡すだけにする。
//!
//! 常駐させるのは、フィールドごとにスレッドを作ると、作った直後は遅いコアで
//! 走り出すため(スケジューラが速いコアへ移す前に終わってしまう)。Android では
//! さらにパフォーマンスヒントで締め切りを伝え、クロックを上げてもらう。
//!
//! 復調した fb はこのスレッドが持ち続ける。来なかった行は前の復調結果を残して
//! 減衰させる、という組立側と同じ扱いをするため。

use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use crate::assembler::{finish_frame, CompletedFrame};
use crate::ntsc;
use crate::perf_hint::Hint;

/// 復調1回分の材料。バッファは結果と一緒に戻ってきて、次の回で使い回す。
#[derive(Default)]
pub struct Job {
    pub width: usize,
    pub height: usize,
    pub dotclk_hz: u32,
    /// 生サンプル(2B/px)
    pub raw: Vec<u8>,
    /// ライン受信時に書いたYのグレースケール(RGBA)。ロックしなかった行はこれが出る
    pub gray: Vec<u8>,
    /// 届いた行
    pub seen: Vec<bool>,
    /// 3次元コム用の履歴(p2, p4, hist_n)。切っているときは使わない
    pub p2: Vec<u8>,
    pub p4: Vec<u8>,
    pub hist_n: Vec<u8>,
    pub comb3d: bool,
    pub adjust: ntsc::Adjust,
    pub interlaced: bool,
    pub decay: f32,
    pub field_view: u8,
    pub frame_idx: u16,
    pub fill_ratio: f32,
    /// 公開用バッファ(組立側が UI から戻してもらったもの)
    pub out: Vec<u8>,
}

pub struct Done {
    pub frame: CompletedFrame,
    pub info: ntsc::Info,
    pub unfilled_rows: u32,
    /// 使い終わった材料のバッファ
    pub job: Job,
}

pub struct DecodeWorker {
    tx: SyncSender<Job>,
    rx: Receiver<Done>,
    /// 渡した材料がまだ戻ってきていない
    pub busy: bool,
    /// 次の材料に使うバッファ
    pub free: Job,
}

impl DecodeWorker {
    pub fn spawn() -> Self {
        let (tx, job_rx) = mpsc::sync_channel::<Job>(1);
        let (done_tx, rx) = mpsc::channel::<Done>();
        std::thread::Builder::new()
            .name("ntsc-decode".into())
            .spawn(move || {
                crate::receiver::raise_thread_qos();
                let mut fb: Vec<u8> = Vec::new();
                // 1フィールド(約16.7ms)以内に終えたい、と OS へ伝える。Android
                // 以外では作れず何もしない
                let hint = Hint::for_current_thread(Duration::from_micros(16_000));
                // 組立側が落ちれば送り手が消えて recv が Err になり、抜ける
                while let Ok(job) = job_rx.recv() {
                    let t = Instant::now();
                    let done = run(&mut fb, job);
                    if let Some(h) = hint.as_ref() {
                        h.report(t.elapsed());
                    }
                    if done_tx.send(done).is_err() {
                        break;
                    }
                }
            })
            .expect("ntsc-decode スレッドを作れない");
        Self { tx, rx, busy: false, free: Job::default() }
    }

    pub fn send(&mut self, job: Job) {
        // 容量1で、busy の間は送らないので詰まらない
        if self.tx.send(job).is_ok() {
            self.busy = true;
        }
    }

    pub fn try_take(&mut self) -> Option<Done> {
        match self.rx.try_recv() {
            Ok(d) => {
                self.busy = false;
                Some(d)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.busy = false;
                None
            }
        }
    }
}

fn run(fb: &mut Vec<u8>, mut job: Job) -> Done {
    let (w, h) = (job.width, job.height);
    let need = w * h * 4;
    if fb.len() != need {
        *fb = vec![0u8; need];
    }
    // 届いた行だけグレースケールで上書きしてから復調する。組立側で fb に
    // 直接書いて復調していたときと同じ並び(ロックしない行は Y のまま残る)
    for y in 0..h {
        if job.seen.get(y).copied().unwrap_or(false) {
            let r = y * w * 4..(y + 1) * w * 4;
            fb[r.clone()].copy_from_slice(&job.gray[r]);
        }
    }
    let hist = job.comb3d.then(|| ntsc::History {
        p2: &job.p2, p4: &job.p4, hist_n: &job.hist_n,
    });
    let info = ntsc::decode_field(&job.raw, w, h, &job.seen, job.dotclk_hz, fb, hist, job.adjust);
    let unfilled_rows = finish_frame(fb, &mut job.seen, w, h,
                                     job.interlaced, job.decay, job.field_view);
    let mut out = std::mem::take(&mut job.out);
    if out.len() != need {
        out = vec![0u8; need];
    }
    out.copy_from_slice(fb);
    Done {
        frame: CompletedFrame {
            frame_idx: job.frame_idx,
            width: w,
            height: h,
            rgba: out,
            fill_ratio: job.fill_ratio,
            stamps: None,
        },
        info,
        unfilled_rows,
        job,
    }
}
