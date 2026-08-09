//! RetroCastX protocol v0 (little-endian).
//! Mirror of `host/python/retrocastx/protocol.py` — that file is the reference.

pub const MAGIC: u8 = 0x52;
pub const VERSION: u8 = 0;
pub const DEFAULT_PORT: u16 = 34600;

pub const TYPE_LINE: u8 = 0;
pub const TYPE_MODE: u8 = 1;
#[allow(dead_code)]
pub const TYPE_AUDIO: u8 = 2;
pub const TYPE_INFO: u8 = 3;
pub const TYPE_SUBSCRIBE: u8 = 4;
pub const TYPE_CONFIG: u8 = 5;

pub const PIXFMT_RGB888: u8 = 0;
pub const PIXFMT_RGB555: u8 = 1;
pub const PIXFMT_RGB565: u8 = 2;

pub fn bytes_per_px(pixfmt: u8) -> Option<usize> {
    match pixfmt {
        PIXFMT_RGB888 => Some(3),
        PIXFMT_RGB555 | PIXFMT_RGB565 => Some(2),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    pub mode_id: u8,
    pub pixfmt: u8,
    pub mflags: u16,
    pub hactive: u16,
    pub htotal: u16,
    pub vactive: u16,
    pub vtotal: u16,
    pub dotclk_hz: u32,
    pub hfreq_mhz_x1000: u32,
    pub vfreq_mhz_x1000: u32,
    pub seq: u16,
}

#[derive(Debug)]
pub struct Line<'a> {
    pub frame: u16,
    pub seq: u16,
    #[allow(dead_code)]
    pub flags: u8,
    pub line: u16,
    pub offset_px: u16,
    pub count_px: u16,
    pub pixfmt: u8,
    pub mode_id: u8,
    #[allow(dead_code)]
    pub timestamp: u32,
    pub pixels: &'a [u8],
}

/// AUDIO(type=2): s16le L/R interleaved。timestamp は映像LINEと同一の
/// ドットクロックカウンタ(A/V同期用)。
#[derive(Debug)]
pub struct Audio<'a> {
    pub source: u8,          // 0=RGB端子音声, 1=LINE入力, 2=S/PDIF
    pub format: u8,          // 0=PCM16
    pub nsamples: u16,       // サンプルフレーム数(L+Rで1)
    pub rate_hz: u32,
    pub timestamp: u32,
    pub samples: &'a [u8],
    pub seq: u16,
}

pub const AUDIO_FMT_PCM16: u8 = 0;

#[derive(Debug, Clone)]
pub struct Announce {
    pub mac: [u8; 6],
    pub udp_port: u16,
    pub fw_version: u16,
    pub caps: u16,
    pub name: String,
    pub seq: u16,
}

#[derive(Debug)]
pub enum Packet<'a> {
    Line(Line<'a>),
    Mode(Mode),
    Audio(Audio<'a>),
    Announce(Announce),
    /// CONFIG応答等(共通seq空間の追跡に必要な範囲のみ保持)
    Other { ptype: u8, flags: u8, seq: u16 },
}

fn u16le(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

fn u32le(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

pub fn parse(d: &[u8]) -> Result<Packet<'_>, &'static str> {
    if d.len() < 8 {
        return Err("short datagram");
    }
    if d[0] != MAGIC || d[1] != VERSION {
        return Err("bad magic/version");
    }
    let (ptype, flags, frame, seq) = (d[2], d[3], u16le(d, 4), u16le(d, 6));
    let body = &d[8..];
    match ptype {
        TYPE_LINE => {
            if body.len() < 12 {
                return Err("short LINE packet");
            }
            let count_px = u16le(body, 4);
            let pixfmt = body[6];
            let pixels = &body[12..];
            let bpp = bytes_per_px(pixfmt).ok_or("unknown pixfmt")?;
            if pixels.len() != count_px as usize * bpp {
                return Err("LINE payload size mismatch");
            }
            Ok(Packet::Line(Line {
                frame,
                seq,
                flags,
                line: u16le(body, 0),
                offset_px: u16le(body, 2),
                count_px,
                pixfmt,
                mode_id: body[7],
                timestamp: u32le(body, 8),
                pixels,
            }))
        }
        TYPE_MODE => {
            if body.len() < 24 {
                return Err("short MODE packet");
            }
            Ok(Packet::Mode(Mode {
                mode_id: body[0],
                pixfmt: body[1],
                mflags: u16le(body, 2),
                hactive: u16le(body, 4),
                htotal: u16le(body, 6),
                vactive: u16le(body, 8),
                vtotal: u16le(body, 10),
                dotclk_hz: u32le(body, 12),
                hfreq_mhz_x1000: u32le(body, 16),
                vfreq_mhz_x1000: u32le(body, 20),
                seq,
            }))
        }
        TYPE_AUDIO => {
            if body.len() < 12 {
                return Err("short AUDIO packet");
            }
            let format = body[1];
            let nsamples = u16le(body, 2);
            let samples = &body[12..];
            if format != AUDIO_FMT_PCM16 {
                return Err("unknown audio format");
            }
            // s16le × 2ch = 4B/フレーム
            if samples.len() != nsamples as usize * 4 {
                return Err("AUDIO payload size mismatch");
            }
            Ok(Packet::Audio(Audio {
                source: body[0],
                format,
                nsamples,
                rate_hz: u32le(body, 4),
                timestamp: u32le(body, 8),
                samples,
                seq,
            }))
        }
        TYPE_INFO => {
            if body.len() < 32 {
                return Err("short INFO packet");
            }
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&body[0..6]);
            let name_raw = &body[16..32];
            let name_end = name_raw.iter().position(|&b| b == 0).unwrap_or(16);
            Ok(Packet::Announce(Announce {
                mac,
                udp_port: u16le(body, 10),
                fw_version: u16le(body, 12),
                caps: u16le(body, 14),
                name: String::from_utf8_lossy(&name_raw[..name_end]).into_owned(),
                seq,
            }))
        }
        t => Ok(Packet::Other { ptype: t, flags, seq }),
    }
}

pub const CFG_FLAG_REPLY: u8 = 0x01;

/// 全ボードに向けるワイルドカード宛先MAC(発見用/単一ボードLAN専用)。
pub const WILDCARD_MAC: [u8; 6] = [0xFF; 6];

/// CONFIG(24B): 共通ヘッダ8B + 宛先MAC 6B + 予約2B + target/op/key 4B + value 4B。
/// op: 0=SET(値を書いて現在値を返す), 1=GET(現在値を返す)。
/// target 0 = ボード本体の設定。key は gateware 側と対応:
///   0x0001 音声ソース有効マスク / 0x0010 vbp / 0x0011 hs_offset / 0x0012 pll_divide
pub fn pack_config(
    seq: u16, target: u8, op: u8, key: u16, value: u32, mac: &[u8; 6],
) -> [u8; 24] {
    let mut p = [0u8; 24];
    p[0] = MAGIC;
    p[1] = VERSION;
    p[2] = TYPE_CONFIG;
    p[6..8].copy_from_slice(&seq.to_le_bytes());
    p[8..14].copy_from_slice(mac);
    p[16] = target;
    p[17] = op;
    p[18..20].copy_from_slice(&key.to_le_bytes());
    p[20..24].copy_from_slice(&value.to_le_bytes());
    p
}

pub const CFG_KEY_VBP: u16 = 0x0010;
pub const CFG_KEY_HS_OFFSET: u16 = 0x0011;
pub const CFG_KEY_PLL_DIVIDE: u16 = 0x0012;
/// インターレース(ウィーブ)。1回のVSYNCにフィールドが2枚入る信号を織り直す
pub const CFG_KEY_INTERLACE: u16 = 0x0013;
/// 第2フィールドが始まる row。0 なら vtotal/2 を使う
pub const CFG_KEY_F2_ROW: u16 = 0x0014;
/// フィールドの偶奇を入れ替える(どちらが偶数ラインかは信号から分からない)
pub const CFG_KEY_FIELD_SWAP: u16 = 0x0015;

/// SUBSCRIBE(16B): 共通ヘッダ8B + 宛先MAC 6B + 予約2B。
/// mac で宛先ボードを指名する(WILDCARD_MAC で全ボード)。
pub fn pack_subscribe(seq: u16, announce_only: bool, mac: &[u8; 6]) -> [u8; 16] {
    let mut p = [0u8; 16];
    p[0] = MAGIC;
    p[1] = VERSION;
    p[2] = TYPE_SUBSCRIBE;
    p[3] = if announce_only { 1 } else { 0 };
    p[6..8].copy_from_slice(&seq.to_le_bytes());
    p[8..14].copy_from_slice(mac);
    p
}

#[cfg(test)]
pub mod testutil {
    //! Packet builders for tests (mirror of the Python pack_* functions).
    use super::*;

    pub fn pack_line(
        frame: u16, seq: u16, line: u16, offset_px: u16, pixfmt: u8, mode_id: u8,
        timestamp: u32, pixels: &[u8],
    ) -> Vec<u8> {
        let bpp = bytes_per_px(pixfmt).unwrap();
        assert_eq!(pixels.len() % bpp, 0);
        let count_px = (pixels.len() / bpp) as u16;
        let mut p = vec![MAGIC, VERSION, TYPE_LINE, 0x01];
        p.extend_from_slice(&frame.to_le_bytes());
        p.extend_from_slice(&seq.to_le_bytes());
        p.extend_from_slice(&line.to_le_bytes());
        p.extend_from_slice(&offset_px.to_le_bytes());
        p.extend_from_slice(&count_px.to_le_bytes());
        p.push(pixfmt);
        p.push(mode_id);
        p.extend_from_slice(&timestamp.to_le_bytes());
        p.extend_from_slice(pixels);
        p
    }

    pub fn pack_mode(m: &Mode, frame: u16, seq: u16) -> Vec<u8> {
        let mut p = vec![MAGIC, VERSION, TYPE_MODE, 0];
        p.extend_from_slice(&frame.to_le_bytes());
        p.extend_from_slice(&seq.to_le_bytes());
        p.push(m.mode_id);
        p.push(m.pixfmt);
        p.extend_from_slice(&m.mflags.to_le_bytes());
        p.extend_from_slice(&m.hactive.to_le_bytes());
        p.extend_from_slice(&m.htotal.to_le_bytes());
        p.extend_from_slice(&m.vactive.to_le_bytes());
        p.extend_from_slice(&m.vtotal.to_le_bytes());
        p.extend_from_slice(&m.dotclk_hz.to_le_bytes());
        p.extend_from_slice(&m.hfreq_mhz_x1000.to_le_bytes());
        p.extend_from_slice(&m.vfreq_mhz_x1000.to_le_bytes());
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_roundtrip_shape() {
        let mac = [0x02, 0x52, 0x43, 0x58, 0x00, 0x01];
        let p = pack_subscribe(7, true, &mac);
        assert_eq!(&p[..4], &[MAGIC, VERSION, TYPE_SUBSCRIBE, 1]);
        assert_eq!(u16le(&p, 6), 7);
        assert_eq!(&p[8..14], &mac);
        // ワイルドカードも parse できる形であること(受信側では自身は判定しないが)
        let w = pack_subscribe(0, false, &WILDCARD_MAC);
        assert_eq!(&w[8..14], &[0xFF; 6]);
    }

    #[test]
    fn parse_line() {
        let pixels = [1u8, 2, 3, 4, 5, 6];
        let d = testutil::pack_line(9, 10, 11, 0, PIXFMT_RGB888, 1, 0xDEADBEEF, &pixels);
        match parse(&d).unwrap() {
            Packet::Line(l) => {
                assert_eq!((l.frame, l.seq, l.line, l.count_px), (9, 10, 11, 2));
                assert_eq!(l.timestamp, 0xDEADBEEF);
                assert_eq!(l.pixels, &pixels);
            }
            _ => panic!("wrong type"),
        }
    }

    #[test]
    fn parse_mode() {
        let m = Mode {
            mode_id: 1, pixfmt: PIXFMT_RGB555, mflags: 0,
            hactive: 512, htotal: 655, vactive: 512, vtotal: 542,
            dotclk_hz: 10_650_300, hfreq_mhz_x1000: 16_260_000,
            vfreq_mhz_x1000: 30_000, seq: 3,
        };
        let d = testutil::pack_mode(&m, 0, 3);
        match parse(&d).unwrap() {
            Packet::Mode(got) => assert_eq!(got, m),
            _ => panic!("wrong type"),
        }
    }
}
