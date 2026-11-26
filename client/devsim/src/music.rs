//! テスト音の代わりに流す曲。PSG(矩形波の音源)風の音色で民謡を鳴らす。
//!
//! ★**サイン波の単音は聞き苦しい**(アプリ内デモで流しっぱなしになる)。曲は
//!   日本の伝承曲「さくらさくら」で、作者の無い民謡なので著作権の問題が無い。
//!   音色はレトロ機らしく矩形波にし、琴のように撥いて減衰させる。
//! ★**系統ごとに音色を変える。** どの端子の音を聞いているかが耳で分かるように
//!   (系統 0 = 矩形波、1 = 三角波、3 = 細いパルス)。
//! 音はサンプル番号だけで決まる(状態は位置と低域通過の1つだけ)。パケットの
//! 送り方が詰まっても、曲が速くなったり遅くなったりはしない。

/// 音色
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Voice {
    /// 矩形波(デューティ 50%)
    Square,
    /// 三角波(柔らかい)
    Triangle,
    /// 細いパルス(デューティ 25%。鼻にかかった音)
    Pulse,
}

/// 休符
const R: i8 = -1;
// 音名(MIDI ノート番号)
const E4: i8 = 64;
const F4: i8 = 65;
const A4: i8 = 69;
const B4: i8 = 71;
const C5: i8 = 72;
const E5: i8 = 76;
const F5: i8 = 77;

/// さくらさくら。(音, 長さ[8分音符の数])。4/4 で1小節 8
const SAKURA: &[(i8, u8)] = &[
    // さくら さくら
    (A4, 2), (A4, 2), (B4, 4),
    (A4, 2), (A4, 2), (B4, 4),
    // やよいの そらは
    (A4, 2), (B4, 2), (C5, 2), (B4, 2),
    (A4, 2), (B4, 1), (A4, 1), (F4, 4),
    // みわたす かぎり
    (E5, 2), (C5, 2), (E5, 2), (F5, 2),
    (E5, 2), (E5, 1), (C5, 1), (B4, 4),
    // かすみか くもか
    (A4, 2), (B4, 2), (C5, 2), (B4, 2),
    (A4, 2), (B4, 1), (A4, 1), (F4, 4),
    // においぞ いずる
    (E5, 2), (C5, 2), (E5, 2), (F5, 2),
    (E5, 2), (E5, 1), (C5, 1), (B4, 4),
    // いざや いざや
    (A4, 2), (A4, 2), (B4, 4),
    (A4, 2), (A4, 2), (B4, 4),
    // みに ゆかん
    (E4, 2), (F4, 2), (B4, 1), (A4, 1), (F4, 2),
    (E4, 8),
    // 繰り返す前の一息
    (R, 8),
];

/// 4分音符のテンポ
const BPM: f64 = 80.0;

/// 1系統ぶんの演奏の状態
pub struct Tune {
    voice: Voice,
    /// 曲の頭からのサンプル数
    n: u64,
    /// 低域通過の状態(矩形波の角を丸めて耳に痛くないようにする)
    lp: f64,
}

impl Tune {
    pub fn new(voice: Voice) -> Tune {
        Tune { voice, n: 0, lp: 0.0 }
    }

    /// 次の1サンプル(-1.0..1.0 の範囲に収まる)
    pub fn next(&mut self, rate: u32) -> f64 {
        let rate = rate as f64;
        let eighth = rate * 60.0 / BPM / 2.0;
        let total: u64 = SAKURA.iter().map(|&(_, d)| d as u64).sum();
        let len = (total as f64 * eighth) as u64;
        let pos = self.n % len.max(1);
        self.n += 1;

        // いまの音符を探す(曲は短いので頭から数える)
        let mut start = 0.0;
        let mut cur: Option<(i8, f64, f64)> = None;
        for &(note, d) in SAKURA {
            let dur = d as f64 * eighth;
            if (pos as f64) < start + dur {
                cur = Some((note, start, dur));
                break;
            }
            start += dur;
        }
        let raw = match cur {
            Some((note, start, dur)) if note >= 0 => {
                let t = pos as f64 - start;
                let hz = 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0);
                // 撥いた弦のように減衰させる。立ち上がりと切れ目は数ms で丸めてプチ音を消す
                let attack = (t / (rate * 0.004)).min(1.0);
                let release = ((dur - t) / (rate * 0.008)).clamp(0.0, 1.0);
                let env = (-t / (rate * 0.55)).exp() * attack * release;
                let ph = (t * hz / rate).fract();
                // 1オクターブ下の三角波を薄く重ねて厚みを出す
                let ph_lo = (t * hz * 0.5 / rate).fract();
                let lead = wave(self.voice, ph);
                let low = wave(Voice::Triangle, ph_lo);
                env * (0.7 * lead + 0.3 * low)
            }
            _ => 0.0,
        };
        // 1次の低域通過(約 3kHz)
        let a = 1.0 - (-2.0 * std::f64::consts::PI * 3000.0 / rate).exp();
        self.lp += a * (raw - self.lp);
        self.lp
    }
}

fn wave(v: Voice, ph: f64) -> f64 {
    match v {
        Voice::Square => if ph < 0.5 { 1.0 } else { -1.0 },
        Voice::Pulse => if ph < 0.25 { 1.0 } else { -1.0 },
        Voice::Triangle => 4.0 * (ph - 0.5).abs() - 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 範囲に収まり、無音でなく、曲の長さで頭に戻る
    #[test]
    fn plays_and_loops() {
        let rate = 48_000;
        let mut t = Tune::new(Voice::Square);
        let mut peak: f64 = 0.0;
        let mut energy = 0.0;
        for _ in 0..rate * 4 {
            let v = t.next(rate);
            assert!(v.abs() <= 1.0, "範囲外 {v}");
            peak = peak.max(v.abs());
            energy += v * v;
        }
        assert!(peak > 0.3, "音が小さすぎる ({peak})");
        assert!(energy > 1000.0);
        // 2周目の頭は1周目の頭と同じ音(位置だけで決まる)
        let eighth = rate as f64 * 60.0 / BPM / 2.0;
        let total: u64 = SAKURA.iter().map(|&(_, d)| d as u64).sum();
        let len = (total as f64 * eighth) as u64;
        let mut a = Tune::new(Voice::Pulse);
        let mut b = Tune::new(Voice::Pulse);
        b.n = len;
        for _ in 0..1000 {
            assert_eq!(a.next(rate), b.next(rate));
        }
    }

    /// 曲は4/4 の小節に揃っている(拍がずれると繰り返しで崩れる)
    #[test]
    fn bars_are_whole() {
        let total: u32 = SAKURA.iter().map(|&(_, d)| d as u32).sum();
        assert_eq!(total % 8, 0, "8分音符 {total} 個は小節に揃わない");
    }
}
