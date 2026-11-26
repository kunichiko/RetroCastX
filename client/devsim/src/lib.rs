//! ダミーデバイスの中身(信号を作り、実機のボードと同じパケットで送る)。
//!
//! 単体のダミーデバイス(main.rs の GUI / `--headless`)と、Viewer のアプリ内デモ
//! (`embed`)の両方がこれを使う。

pub mod board;
pub mod capture;
pub mod embed;
pub mod engine;
pub mod machines;
pub mod music;
pub mod pattern;
pub mod signal;

/// プロトコルの定数とパーサは Viewer のものをそのまま使う(版ずれを防ぐ)
#[allow(dead_code)]
#[path = "../../src/protocol.rs"]
pub mod protocol;

/// デジタルRGB の色の表も Viewer のものを使う(Viewer の展開と1対1にするため)
#[allow(dead_code)]
#[path = "../../src/palette.rs"]
pub mod palette;
