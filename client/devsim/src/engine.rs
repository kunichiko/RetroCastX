//! 送受信のスレッド。実機のボードと同じ時刻の進み方で送る。
//!
//! - VSYNC 周期ごとに `Capture` で1周期分のラインを作り、各ラインを**そのラインが
//!   終わる時刻**に送る(実機は HSYNC でラインを押し出すので、1ライン遅れて出る)
//! - 測定値も実機と同じ窓で数える。fH と DATACLK は1秒窓なので、モードを切り替えた
//!   直後は前後の混ざった値になる。vtotal は4周期続けて同じ、2周期分の組は3回続けて
//!   同じになってから変わる。Viewer のモード切替の待ち(modeswitch.rs)はこの遅れに
//!   合わせて書かれているので、ここを省くと試験にならない

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::board::{Board, Input, SYS_CLK_HZ};
use crate::capture::{self, CapCfg, Capture, Period};
use crate::machines::Port;
use crate::pattern::{self, Picture};
use crate::protocol as proto;

/// 画面の「取り込んだ絵」用。スロットごとの最新のサンプル
#[derive(Default)]
pub struct Preview {
    pub serial: u64,
    pub width: u32,
    pub slots: Vec<Option<Arc<Vec<[u8; 3]>>>>,
}

#[derive(Default, Clone)]
pub struct Stats {
    pub pkts_per_s: u32,
    pub mbps: f64,
    pub lines_per_s: u32,
    pub send_errors: u64,
    pub last_error: String,
    pub dropped: u64,
    /// 周期の生成に掛かった時間の割合(1 を超えると間に合っていない)
    pub load: f64,
    /// 送出ループの周回数[/s]と、1周の最長[ms](眠りを除く)
    pub iters: u32,
    pub worst_ms: f64,
    pub would_block: u64,
    /// 直近1秒で、ラインを送るべき時刻から最も遅れた量[ms]。
    /// 1フレームを超えて増え続けるなら、送信(ループバックの sendto)が信号の
    /// パケットレートに追いついていない
    pub lag_ms: f64,
}

pub struct Shared {
    pub board: Mutex<Board>,
    pub preview: Mutex<Preview>,
    pub stats: Mutex<Stats>,
    pub stop: AtomicBool,
    pub local: Mutex<Option<SocketAddr>>,
}

impl Shared {
    pub fn new(board: Board) -> Arc<Shared> {
        Arc::new(Shared {
            board: Mutex::new(board),
            preview: Mutex::new(Preview::default()),
            stats: Mutex::new(Stats::default()),
            stop: AtomicBool::new(false),
            local: Mutex::new(None),
        })
    }
}

/// UDP を開く。
///
/// まず 0.0.0.0 で掴む(LAN の Viewer からのブロードキャストも受ける)。Viewer と
/// 同じ機械で、こちらが先に 34600 を掴めば Viewer は空きポートへ落ちて動く
/// (Viewer の receiver.rs)。
///
/// ★**Viewer が先に 34600 を掴んでいたら 127.0.0.1:34600 だけを掴む。** 実機を
///   見ている Viewer を閉じずにダミーを足せるようにするため。ワイルドカードより
///   具体的なアドレスの bind が優先されるので、127.0.0.1 宛のユニキャストは
///   こちらに届く(SO_REUSEADDR が要る)。Viewer は `--board 127.0.0.1` で繋ぐ。
///   ブロードキャストは受けられないので、発見(ボード一覧)には出ない
pub fn open(port: u16) -> std::io::Result<(UdpSocket, bool)> {
    use socket2::{Domain, Protocol, Socket, Type};
    let mk = |ip: [u8; 4], reuse: bool| -> std::io::Result<UdpSocket> {
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        let _ = s.set_send_buffer_size(8 << 20);
        let _ = s.set_recv_buffer_size(1 << 20);
        if reuse {
            s.set_reuse_address(true)?;
        }
        s.bind(&SocketAddr::from((ip, port)).into())?;
        s.set_broadcast(true)?;
        s.set_nonblocking(true)?;
        Ok(s.into())
    };
    match mk([0, 0, 0, 0], false) {
        Ok(s) => Ok((s, false)),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            mk([127, 0, 0, 1], true).map(|s| (s, true)).map_err(|_| e)
        }
        Err(e) => Err(e),
    }
}

/// 測定の窓
#[derive(Default)]
struct Meter {
    win_start: Option<Instant>,
    win_lines: u64,
    win_clocks: u64,
    win8_start: Option<Instant>,
    win8_vsyncs: u32,
    vt_cand: u32,
    vt_count: u32,
    prev_period: Option<(f64, u32)>,
    pair_cand: Option<(u32, u32)>,
    pair_count: u32,
    last_vsync: Option<Instant>,
}

/// 進行中の周期
struct Current {
    period: Period,
    t0: Instant,
    next: usize,
    cfg: CapCfg,
}

const AUDIO_SOURCES: [(u8, Port, f64); 3] =
    [(0, Port::Rgb15, 440.0), (1, Port::Rgb21, 554.37), (3, Port::Video, 659.25)];
const AUDIO_N: usize = 240;
const AUDIO_RATE: u32 = 48_000;

pub fn run(sock: UdpSocket, sh: Arc<Shared>) {
    raise_thread_priority();
    let local = sock.local_addr().ok();
    *sh.local.lock().unwrap() = local;
    let mut cap = Capture::default();
    let mut seq: u16 = 0;
    let mut frame: u16 = 0;
    let mut cur: Option<Current> = None;
    let mut meter = Meter::default();
    let mut pic: Option<(u64, Picture)> = None;
    let mut last_input_id: Option<String> = None;
    let mut ann_at = Instant::now();
    let mut mode_at = Instant::now();
    let mut mode_sent: Option<(u32, u32)> = None;
    let mut audio_at = Instant::now();
    let mut audio_phase = [0.0f64; 3];
    let mut buf = vec![0u8; 2048];
    let mut out_pkts: Vec<Vec<u8>> = Vec::new();
    // 1秒ごとの統計
    let mut st_at = Instant::now();
    let (mut st_pkts, mut st_bytes, mut st_lines) = (0u32, 0u64, 0u32);
    let mut gen_time = Duration::ZERO;
    let mut sent_time = Duration::ZERO;

    let (mut iters, mut worst, mut lag) = (0u32, Duration::ZERO, Duration::ZERO);
    while !sh.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        iters += 1;
        // --- 受信 ---
        loop {
            match sock.recv_from(&mut buf) {
                // ★**自分のアドレスから来たものは捨てる。** 127.0.0.1:34600 に落ちたとき、
                //   34600 を掴んでいる Viewer の送信元も 127.0.0.1:34600 になり、その返信は
                //   (より具体的な bind の)こちらに戻ってくる。応答し続けると自分と会話する
                Ok((_, from)) if Some(from) == local => {}
                Ok((n, from)) => {
                    let replies = sh.board.lock().unwrap().handle(&buf[..n], from, &mut seq, now);
                    for (to, p) in replies {
                        send(&sock, &sh, &p, to, &mut st_pkts, &mut st_bytes);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break, // Windows は ICMP 到達不能を recv のエラーで返す。無視する
            }
        }

        // --- 時刻の来たラインを送る ---
        // ★**周期を切り替える前に送る。** 周期の最後のラインは周期の終わりちょうどに
        //   時刻が来るので、切り替えを先にすると末尾の数十ライン(1ms 分)を捨てていた。
        //   Viewer には1フレームのライン数が毎回揺れて見え、モード切替と誤認された
        let (dest, subscribed) = {
            let b = sh.board.lock().unwrap();
            (b.sub, b.subscribed(now))
        };
        if let Some(c) = cur.as_mut() {
            while c.next < c.period.rows.len() {
                let row = &c.period.rows[c.next];
                let due = c.t0 + Duration::from_secs_f64(row.at_line as f64 * c.period.line_s);
                // ★**1周で送るのは 1ms 分まで。** ループバックの sendto は1つ 15µs ほど
                //   掛かり、31kHz(6万 pkt/s)では送っている間に次のラインの時刻が来続けて
                //   1周期まるごと抜けなかった。その間は CONFIG に答えられず(往復 9ms)、
                //   音声も止まる。残りは次の周回で(眠らずに)続ける
                if due > now || now.elapsed() > Duration::from_millis(1) {
                    break;
                }
                // 送るべき時刻からの遅れ。送信が信号の速さに追いつかないと積み上がる
                lag = lag.max(now.saturating_duration_since(due));
                c.next += 1;
                frame = row.frame;
                if !subscribed {
                    continue;
                }
                out_pkts.clear();
                capture::packetize(row, &c.cfg, &mut seq, &mut out_pkts);
                st_lines += 1;
                let drop = sh.board.lock().unwrap().drop_line();
                if drop {
                    sh.stats.lock().unwrap().dropped += out_pkts.len() as u64;
                    continue;
                }
                for p in &out_pkts {
                    send(&sock, &sh, p, dest.unwrap(), &mut st_pkts, &mut st_bytes);
                }
            }
        }

        // --- 周期の切り替え ---
        // ★**送り残しがあるうちは切り替えない。** 上で 1ms 分ずつしか送らないので、
        //   周期の終わりの時刻を過ぎても末尾のラインが残っていることがある。捨てると
        //   Viewer には1フレームのライン数が揺れて見え、モード切替と誤認される。
        //   次の周期の t0 は元の終わりの時刻なので、待っても時間はずれない
        let period_over = cur.as_ref().is_none_or(|c| {
            now >= c.t0 + Duration::from_secs_f64(c.period.lines as f64 * c.period.line_s)
                && c.next >= c.period.rows.len()
        });
        if period_over {
            // 前の周期を測定に入れる。信号が無いあいだの待ち(vtotal 0)は VSYNC では
            // ないので数えない(数えると fH 50Hz・vtotal 1 という嘘の MODE が出た)
            if let Some(c) = cur.as_ref().filter(|c| c.period.vtotal != 0) {
                let dur = c.period.lines as f64 * c.period.line_s;
                let mut b = sh.board.lock().unwrap();
                account(&mut meter, &mut b, Some((dur, c.period.lines, c.period.vtotal, c.period.il_frame)),
                        c.cfg.pll_divide, c.t0 + Duration::from_secs_f64(dur));
            }
            let t0 = cur.as_ref().map_or(now, |c| {
                let end = c.t0 + Duration::from_secs_f64(c.period.lines as f64 * c.period.line_s);
                // 大きく遅れたら(スリープ明けなど)今に合わせ直す
                if now.duration_since(end) > Duration::from_millis(200) { now } else { end }
            });
            let (input, cfg, opt, pic_serial, noise) = {
                let b = sh.board.lock().unwrap();
                let mut opt = b.picture.clone();
                let inp = b.tvp_input().cloned();
                if !opt.caption.is_empty() {
                    opt.caption = inp.as_ref().map_or(String::new(), Input::caption);
                }
                (inp, b.cap_cfg(), opt, b.pic_gen, b.noise)
            };
            let id = input.as_ref().map(|i| format!("{}/{}", i.machine_key, i.mode.name));
            if id != last_input_id {
                last_input_id = id.clone();
                sh.board.lock().unwrap().log(match &id {
                    Some(s) => format!("TVP の信号: {s}"),
                    None => "TVP の信号: なし".into(),
                });
            }
            match input {
                Some(inp) => {
                    let started = Instant::now();
                    let src = inp.source();
                    let (w, h) = src.timing.picture_size();
                    let anim = if opt.moving { cap_frame_hint(frame) } else { 0 };
                    let key = pic_serial.wrapping_mul(1_000_003) ^ anim ^ hash_str(id.as_deref().unwrap_or(""));
                    if pic.as_ref().is_none_or(|(k, _)| *k != key) {
                        pic = Some((key, pattern::render(w, h, src.color_bits, &opt, anim as u32)));
                    }
                    let p = &pic.as_ref().unwrap().1;
                    let native = capture::native_invert(&inp.machine_key);
                    let period = cap.next_period(&src, native, p, key, &cfg, noise);
                    gen_time += started.elapsed();
                    sent_time += Duration::from_secs_f64(period.lines as f64 * period.line_s);
                    update_preview(&sh, &period, &cfg);
                    cur = Some(Current { period, t0, next: 0, cfg });
                }
                None => {
                    // 信号が無い。HSYNC も VSYNC も来ないので何も送らない(MODE だけ)
                    cur = Some(Current {
                        period: Period { lines: 1, line_s: 0.02, vtotal: 0, il_frame: false, rows: Vec::new() },
                        t0, next: 0, cfg,
                    });
                    let mut b = sh.board.lock().unwrap();
                    account(&mut meter, &mut b, None, cfg.pll_divide, now);
                }
            }
        }

        // --- ANNOUNCE(1秒ごと、最後の SUBSCRIBE の送信元へ) ---
        if now >= ann_at {
            ann_at = now + Duration::from_secs(1);
            let p = {
                let b = sh.board.lock().unwrap();
                b.ann_dest.map(|d| (d, b.announce(d, &mut seq, now)))
            };
            if let Some((d, p)) = p {
                send(&sock, &sh, &p, d, &mut st_pkts, &mut st_bytes);
            }
        }

        // --- MODE(購読中だけ。購読したとき・1秒ごと・htotal/vactive が変わったとき) ---
        if subscribed {
            let mut b = sh.board.lock().unwrap();
            let pll = b.reg(proto::CFG_KEY_PLL_DIVIDE).clamp(200, 2304);
            let vact = 2 * b.meas.vtotal.saturating_sub(b.reg(proto::CFG_KEY_VBP)).min(1024);
            let key = (pll, vact);
            if b.mode_now || now >= mode_at || mode_sent != Some(key) {
                b.mode_now = false;
                mode_at = now + Duration::from_secs(1);
                mode_sent = Some(key);
                let p = b.mode_packet(frame, &mut seq);
                drop(b);
                send(&sock, &sh, &p, dest.unwrap(), &mut st_pkts, &mut st_bytes);
            }
        }

        // --- AUDIO(5ms ごと、有効な系統ぶん) ---
        // ★**時刻が来た分だけまとめて送る。** 周回ごとに1つだと、ループが詰まった
        //   (OS が sleep を引き延ばした)ときにレートがそのまま落ち、Viewer の再生が
        //   枯れる。実機の ADC は止まらないので、遅れても数は揃える
        if now.duration_since(audio_at) > Duration::from_millis(100) {
            audio_at = now;
        }
        while now >= audio_at {
            audio_at += Duration::from_micros(5_000);
            if !subscribed {
                continue;
            }
            let (mask, tone, ports) = {
                let b = sh.board.lock().unwrap();
                let ports: Vec<bool> = AUDIO_SOURCES.iter().map(|s| b.input(s.1).is_some()).collect();
                (b.reg(0x0001), b.tone, ports)
            };
            for (i, &(src, _, hz)) in AUDIO_SOURCES.iter().enumerate() {
                if mask & (1 << src) == 0 {
                    continue;
                }
                let on = tone && ports[i];
                let p = audio_packet(src, frame, &mut seq, &mut audio_phase[i], hz, on);
                send(&sock, &sh, &p, dest.unwrap(), &mut st_pkts, &mut st_bytes);
            }
        }

        // --- 統計 ---
        if now.duration_since(st_at) >= Duration::from_secs(1) {
            let secs = now.duration_since(st_at).as_secs_f64();
            st_at = now;
            let mut s = sh.stats.lock().unwrap();
            s.pkts_per_s = (st_pkts as f64 / secs) as u32;
            s.mbps = st_bytes as f64 * 8.0 / secs / 1e6;
            s.lines_per_s = (st_lines as f64 / secs) as u32;
            s.load = if sent_time.is_zero() { 0.0 } else { gen_time.as_secs_f64() / sent_time.as_secs_f64() };
            (st_pkts, st_bytes, st_lines) = (0, 0, 0);
            (gen_time, sent_time) = (Duration::ZERO, Duration::ZERO);
            s.iters = iters;
            s.worst_ms = worst.as_secs_f64() * 1e3;
            s.lag_ms = lag.as_secs_f64() * 1e3;
            (iters, worst, lag) = (0, Duration::ZERO, Duration::ZERO);
        }

        // 次にすることまで眠る(最長 1ms。Windows の sleep は粗いが、その分まとめて送る)
        let next_due = cur.as_ref().and_then(|c| {
            c.period.rows.get(c.next).map(|r| c.t0 + Duration::from_secs_f64(r.at_line as f64 * c.period.line_s))
        });
        let wake = next_due.unwrap_or(now + Duration::from_millis(1)).min(now + Duration::from_millis(1));
        let d = wake.saturating_duration_since(Instant::now());
        worst = worst.max(now.elapsed());
        if !d.is_zero() {
            std::thread::sleep(d);
        }
    }
}

/// 送出のスレッドを対話的な処理として扱わせる。
///
/// ★**既定の優先度だと、macOS は 1ms の sleep をまとめて引き延ばすことがある。**
///   送出ループが1周期(16ms)に1回しか回らなくなり、音声が毎秒 200 → 約 60 に
///   落ちた(VGA のモードで断続的に再現)。Viewer の受信スレッドと同じ扱い
///   (client/src/receiver.rs の raise_thread_qos)にする。
///   Windows は sleep の刻みが既定 15.6ms なので、1ms に細かくする
fn raise_thread_priority() {
    #[cfg(target_vendor = "apple")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Media::timeBeginPeriod;
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };
        timeBeginPeriod(1);
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
    }
}

/// 動く絵のフレーム番号(周期ごとに1つ進む)
fn cap_frame_hint(frame: u16) -> u64 {
    frame as u64 + 1
}

fn hash_str(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn send(sock: &UdpSocket, sh: &Shared, p: &[u8], to: SocketAddr, pkts: &mut u32, bytes: &mut u64) {
    loop {
        match sock.send_to(p, to) {
            Ok(_) => {
                *pkts += 1;
                *bytes += p.len() as u64 + 42;
                return;
            }
            // 送信バッファが一杯。少し待って同じものを送り直す(実機も詰まれば待つ)
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                sh.stats.lock().unwrap().would_block += 1;
                std::thread::sleep(Duration::from_micros(100));
            }
            Err(e) => {
                let mut s = sh.stats.lock().unwrap();
                s.send_errors += 1;
                s.last_error = format!("{to}: {e}");
                return;
            }
        }
    }
}

/// 終わった周期を測定に入れる。`period` が None なら信号なし(VSYNC が来ない)
fn account(m: &mut Meter, b: &mut Board, period: Option<(f64, u32, u32, bool)>, pll: u32, now: Instant) {
    if let Some((dur, lines, vtotal, il_frame)) = period {
        m.win_lines += lines as u64;
        m.win_clocks += lines as u64 * pll as u64;
        m.win8_vsyncs += 1;
        m.last_vsync = Some(now);
        // vtotal: 4回続けて同じ(±1 は同じとみなして大きい方を採る)
        if lines.abs_diff(m.vt_cand) <= 1 {
            m.vt_cand = m.vt_cand.max(lines);
            m.vt_count += 1;
        } else {
            m.vt_cand = lines;
            m.vt_count = 1;
        }
        if m.vt_count >= 4 && b.meas.vtotal != vtotal.max(m.vt_cand) {
            b.meas.vtotal = vtotal.max(m.vt_cand);
            b.meas.il_frame = il_frame;
            b.mode_now = true;
        }
        // 2周期分の組: 3回続けて同じになったら出す
        if let Some((pdur, plines)) = m.prev_period {
            let clocks = ((pdur + dur) * SYS_CLK_HZ as f64).round() as u32;
            let flines = plines + lines;
            let same = m.pair_cand.is_some_and(|(c, l)| c.abs_diff(clocks) <= c / 64 && l.abs_diff(flines) <= 1);
            if same {
                m.pair_count += 1;
            } else {
                m.pair_cand = Some((clocks, flines));
                m.pair_count = 1;
            }
            if m.pair_count >= 3 {
                let fresh = b.meas.frame_clocks == 0
                    || b.meas.frame_clocks.abs_diff(clocks) > b.meas.frame_clocks / 1024;
                b.meas.frame_clocks = clocks;
                b.meas.frame_lines = flines as u16;
                if fresh {
                    b.mode_now = true;
                }
            }
        }
        m.prev_period = Some((dur, lines));
    } else if m.last_vsync.is_none_or(|t| now.duration_since(t) > Duration::from_millis(1490)) {
        // VSYNC が約1.5秒来なければ組は 0 に戻る
        b.meas.frame_clocks = 0;
        b.meas.frame_lines = 0;
        m.prev_period = None;
        m.pair_cand = None;
    }
    // 1秒窓(fH と DATACLK)
    let ws = *m.win_start.get_or_insert(now);
    if now.duration_since(ws) >= Duration::from_secs(1) {
        let secs = now.duration_since(ws).as_secs_f64();
        b.meas.hfreq = (m.win_lines as f64 / secs).round() as u32;
        b.meas.dotclk = (m.win_clocks as f64 / secs).round() as u32;
        b.meas.htotal_window = pll;
        m.win_start = Some(now);
        m.win_lines = 0;
        m.win_clocks = 0;
    }
    // 8秒窓(fV)。125mHz 刻み
    let ws8 = *m.win8_start.get_or_insert(now);
    if now.duration_since(ws8) >= Duration::from_secs(8) {
        let secs = now.duration_since(ws8).as_secs_f64();
        b.meas.vfreq_mhz = ((m.win8_vsyncs as f64 / secs * 8.0).round() as u32) * 125;
        m.win8_start = Some(now);
        m.win8_vsyncs = 0;
    }
    // 起動直後は窓が埋まるまで待たない(実機も電源投入から最初の窓までは0だが、
    // 待つ意味が無いので初回だけ周期から出す)
    if b.meas.hfreq == 0 {
        if let Some((dur, lines, ..)) = period {
            b.meas.hfreq = (lines as f64 / dur).round() as u32;
            b.meas.dotclk = (lines as f64 * pll as f64 / dur).round() as u32;
            b.meas.htotal_window = pll;
        }
    }
    if b.meas.vfreq_mhz == 0 {
        if let Some((dur, ..)) = period {
            b.meas.vfreq_mhz = ((8.0 / dur).round() as u32) * 125;
        }
    }
}

fn update_preview(sh: &Shared, period: &Period, cfg: &CapCfg) {
    if period.rows.is_empty() {
        return;
    }
    let mut pv = sh.preview.lock().unwrap();
    let width = cfg.pll_divide.min(capture::LINE_BUFFER_W);
    let max_slot = period.rows.iter().map(|r| r.slot as usize).max().unwrap_or(0);
    if pv.width != width || pv.slots.len() < max_slot + 1 || pv.slots.len() > (max_slot + 1) * 2 + 4 {
        pv.slots = vec![None; max_slot + 2];
        pv.width = width;
    }
    for r in &period.rows {
        pv.slots[r.slot as usize] = Some(r.samples.clone());
    }
    pv.serial += 1;
}

fn audio_packet(src: u8, frame: u16, seq: &mut u16, phase: &mut f64, hz: f64, on: bool) -> Vec<u8> {
    let mut p = vec![proto::MAGIC, proto::VERSION, proto::TYPE_AUDIO, 0];
    p.extend_from_slice(&frame.to_le_bytes());
    p.extend_from_slice(&seq.to_le_bytes());
    *seq = seq.wrapping_add(1);
    p.push(src);
    p.push(proto::AUDIO_FMT_PCM16);
    p.extend_from_slice(&(AUDIO_N as u16).to_le_bytes());
    p.extend_from_slice(&AUDIO_RATE.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    let step = 2.0 * std::f64::consts::PI * hz / AUDIO_RATE as f64;
    for _ in 0..AUDIO_N {
        let v = if on { (phase.sin() * 6000.0) as i16 } else { 0 };
        *phase = (*phase + step) % (2.0 * std::f64::consts::PI);
        p.extend_from_slice(&v.to_le_bytes());
        p.extend_from_slice(&v.to_le_bytes());
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::Packet;

    #[test]
    fn audio_packet_parses() {
        let mut seq = 0;
        let mut ph = 0.0;
        let p = audio_packet(1, 3, &mut seq, &mut ph, 440.0, true);
        let Ok(Packet::Audio(a)) = proto::parse(&p) else { panic!() };
        assert_eq!((a.source, a.nsamples, a.rate_hz), (1, 240, 48_000));
    }
}
