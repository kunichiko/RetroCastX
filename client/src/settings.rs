//! Viewerの設定を保存/復元する(音量など、次回起動時に引き継ぎたいもの)。
//!
//! eframeのpersistence機能はserde+ronを引き込むので、数個のスカラのために
//! 依存を増やさず自前の `key = value` テキストにする。壊れていても既定値で
//! 起動できるよう、読めない行は黙って無視する。
//!
//! 置き場所は環境変数を順に見て決める:
//!
//! ```text
//! $XDG_CONFIG_HOME/retrocastx/viewer.conf
//! $HOME/.config/retrocastx/viewer.conf          Unix
//! %APPDATA%\retrocastx\viewer.conf              Windows
//! %USERPROFILE%\AppData\Roaming\retrocastx\...   同(APPDATA未設定時)
//! $HOME/Library/Application Support/retrocastx/  iOS(アプリのコンテナ)
//! <内部データ領域>/retrocastx/                    Android(android_main が渡す)
//! ```
//!
//! 起動時にパスを1行表示して、「どこかに勝手に作られた忘れられる設定」に
//! ならないようにしている。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::profiles::{self, Port};

/// 環境変数より優先する置き場所。Android は環境変数に何も無いので、入口
/// (android_main)がアプリの内部データ領域を入れる
static BASE_DIR: OnceLock<PathBuf> = OnceLock::new();

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn set_base_dir(dir: PathBuf) {
    let _ = BASE_DIR.set(dir);
}

/// iOS の HOME はアプリのコンテナで、その直下は書けない(書けるのは
/// Documents / Library / tmp)。設定はユーザーに見せるものではないので Library 側
#[cfg(target_os = "ios")]
fn platform_base() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
}

#[cfg(not(target_os = "ios"))]
fn platform_base() -> Option<PathBuf> {
    None
}
// ★**「いまの設定ファイル」をグローバルに持たない。** 1プロセスで複数の
//   ウィンドウを開くようになったので、プロセスに1つでは足りない
//   (2つ目のウィンドウが1つ目の設定を上書きする)。番号は呼び側が持つ。

/// ウィンドウがどのボードに繋がるか。
///
/// ★**「自動」と「未選択」を分ける。** ボードを1枚しか持っていない人には
///   「勝手に繋がる」のが正しいが、3画面で使う人には「この窓はこの基板」が
///   固定されていないと並びが毎回入れ替わる。同じ設定で両方を満たせない。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BoardSel {
    #[default]
    /// 空いているボードを見つけ次第つなぐ(既定)
    Auto,
    /// この基板だけ。居なければ未選択のまま待つ
    Mac([u8; 6]),
    /// つながない
    None,
}

impl BoardSel {
    pub fn parse(v: &str) -> Option<BoardSel> {
        match v.trim() {
            "auto" => Some(BoardSel::Auto),
            "none" | "" => Some(BoardSel::None),
            hex => {
                let b: Vec<u8> = hex
                    .split(|c| c == ':' || c == '-')
                    .filter_map(|p| u8::from_str_radix(p, 16).ok())
                    .collect();
                (b.len() == 6).then(|| BoardSel::Mac(b.try_into().unwrap()))
            }
        }
    }

    pub fn to_text(self) -> String {
        match self {
            BoardSel::Auto => "auto".into(),
            BoardSel::None => "none".into(),
            BoardSel::Mac(m) => m.map(|x| format!("{x:02x}")).join(":"),
        }
    }
}

/// ドット復元の係数 a の既定値 ×1000。
///
/// ★**この係数は「1サンプル=1ドット」でしか意味を持たない。** 一次ローパスの
///   逆演算なので、オーバーサンプリングしているとモデルが成立しない。
///   合わせる前に pll_divide を必ず合わせること
///   (X68000 の512ドット系グラフィックなら 736)。
///
/// 2026-09-03 に RGB888 伝送・pll_div 736(1サンプル=1ドット)で実測:
///
///     a      1px縞  4px縞  1px/4px  飽和
///     0.00   195    233     84%     0.00%
///     0.20   234    233    100%     0.00%   ← 採用
///     0.28   255    255    100%    16.84%   過補正
///
/// **0.20 で 1px が白レベルに完全到達し、飽和ゼロ。**
///
/// ※以前の既定 0.28 は RGB555 時代に pll_div=1104(1.5倍オーバーサンプリング)
///   で詰めた値で、前提が崩れた状態での調整だった。
fn default_dot_a() -> u32 { 200 }

/// 音声の溜めの既定値[ms]。
///
/// ★**macOS は 10ms。** 実機(複数出力装置)で 遅延 ≈ 16ms(溜め6〜9 + 出力9.8)、
///   GUI で聴いて途切れなし・遅れを感じないことを確かめた(2026-10-12)。
///   80ms では合計約95msで、聞いて分かる遅れがあった。
///   Windows は WASAPI の出力側の遅延も、小さいデバイスバッファを受け付けるかも
///   確かめていないので、実績のある 80ms のまま。
#[cfg(target_os = "macos")]
pub const DEFAULT_AUDIO_BUFFER_MS: u32 = 10;
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_AUDIO_BUFFER_MS: u32 = 80;

#[derive(Clone, Debug)]
pub struct Settings {
    pub volume: f32,
    pub muted: bool,
    /// 再生する音声source。None は再生しない
    pub audio_source: Option<u8>,
    /// 出力デバイス名。None はシステム既定
    pub audio_device: Option<String>,
    /// 音声の溜め[ms]。小さいほど低遅延だが途切れやすい
    pub audio_buffer_ms: u32,
    /// 画枠パラメータ(ボードへCONFIGで送る値)
    pub tune_vbp: i32,
    pub tune_hs_offset: i32,
    pub tune_pll_divide: i32,
    /// TVPのアナログ映像帯域。0=最大 / 15=最小
    pub tune_video_bw: u8,
    /// サンプリング位相 0..31。ドット周期の1/32刻み
    pub tune_phase: u8,
    /// 1ラインまるごと送る(非黒範囲の最適化を切る)
    pub tune_full_line: bool,
    /// フレーム間引き 0=毎フレーム / 1=2フレームに1回 …
    pub tune_frame_skip: u8,
    /// 間引きをフィールド単位にする(インターレースのとき)。既定は false =
    /// 2フィールドずつ(対モード。CONFIG 0x35 bit4)。フィールド単位は fps が
    /// 倍になる代わりに片方の極性しか届かず、織り込みが崩れる
    pub frame_skip_fields: bool,
    /// コンポジットの3次元コム。既定は使う(モバイルでは重くて追いつかないことがある)
    pub ntsc_comb3d: bool,
    /// 選んでいる入力端子
    pub input_port: Port,
    /// 端子ごとに選んでいる機種(プロファイルのキー)。空文字は「汎用」
    /// (機種を決めずに広く探す)。端子を切り替えて戻ってきたとき、前の機種に戻る。
    pub port_profiles: BTreeMap<Port, String>,
    /// 画面回転 0/1/2/3 = 時計回りに 0/90/180/270 度(縦画面のゲーム用)
    pub rotate: u32,
    /// 管面(表示領域)の縦横比 幅/高さ。0 なら有効映像の比をそのまま使う。
    /// 実際のCRTは「何ドットか」を知らず、偏向で決まった管面いっぱいに描く。
    /// ここを 4/3 にすれば 512x256 でも 768x512 でも同じ形で表示される。
    pub tube_aspect: f32,
    /// 見た目の調整。**復調の校正とは別**で、好みで動かす値。
    /// 既定(0,1,0,1)は「信号どおり」を指す
    pub adj_hue_deg: f32,
    pub adj_saturation: f32,
    pub adj_brightness: f32,
    pub adj_contrast: f32,
    /// ガンマ。1.0 = 信号どおり。2.2 にすると符号化済みの値を二重符号化した
    /// 見え方(明るい液晶向け)になる
    pub adj_gamma: f32,
    /// 右の操作パネルを表示するか
    pub show_panel: bool,
    /// NIC受信バッファーの警告ダイアログを二度と出さない。
    /// 管理者権限が無くて直せない人に毎回出すのは敵対的なので、逃げ道を用意する
    /// (パネル内の表示は消さない)
    pub netcheck_muted: bool,
    /// 手入力のボードIP(モバイル)。空ならブロードキャストだけで探す。
    /// iOS はブロードキャストを送れないことがあるので、その逃げ道
    /// (receiver::Shared::extra_dest)
    pub board_ip: String,
    /// モニタの枠(ベゼル)の名前。空文字は枠なし
    pub bezel: String,
    /// 枠を一時的に隠す(bezel の選択は保つ)
    pub bezel_off: bool,
    /// 管面の外(余白)の描き方。`letterbox::Style::key()` の値
    pub letterbox: String,
    /// 補間 0=ニアレスト 1=バイリニア 2=sharp-bilinear
    pub filter: u32,
    /// ドット復元(1タップ逆フィルタ)の係数 a ×1000。0 = 無効。
    /// R/G/B 独立に掛かる(劣化も3経路で独立に起きているため)。
    /// 既定値の根拠は default_dot_a() を参照。
    pub dot_a_milli: u32,
    /// インターレース時の残光。1.0=前フィールドの行をそのまま残す(既定、チラつき無し)。
    /// 下げるとCRTの残光に近づくが、面全体がフィールドレートでちらつく。
    pub interlace_decay: f32,
    /// 表示する切り出し範囲[画素]。w か h が 0 なら切り出さない(全体を表示)
    pub crop_x: u32,
    pub crop_y: u32,
    pub crop_w: u32,
    pub crop_h: u32,
    /// 管面が映す時間窓 [h0,h1,v0,v1](ラインとフレームに対する割合)。
    /// tube_time_based=false のときだけ使う(旧方式)。
    pub window: [f32; 4],
    /// 管面を時間ベースで決める(実CRTと同じ挙動)
    pub tube_time_based: bool,
    /// いまの帯域のモニタプロファイル [H幅, H位置, V幅, V位置]。
    /// pll_divide と位相は帯域ごとに別途持つ(mon_pll)。
    ///
    /// 3モードディスプレイの偏向は「1HSYNC周期でブラウン管の左右をちょうど掃引する」
    /// ように周波数ごとに速度が切り替わる。だから管面の横幅は 1/fH そのもので、
    /// 掃引時間は自由パラメータではない。ここで持つのは「その周期のうち実際に
    /// 管面へ出る割合」と「位置」だけ。1.0なら周期全体が見えるので何も切れない。
    pub mon: [f32; 4],
    /// 機種 × 帯域ごとのモニタプロファイル(CZ-612Dのような3モードディスプレイに相当)。
    /// キーは (`profiles::geo_key`, 帯域[kHz])。
    ///
    /// ★**帯域だけをキーにしていた頃は、同じ帯域の別機種が値を共有していた。**
    ///   31kHz で X68000 に合わせた管面が、31kHz の FM-TOWNS にもそのまま使われ、
    ///   横位置や幅が大きくずれた。機種を足すたびに起きるので、機種もキーに入れる。
    pub mon_bands: BTreeMap<(String, u32), [f32; 4]>,
    /// 機種 × 帯域ごとの [pll_divide, 位相]。キーは `mon_bands` と同じ。
    ///
    /// pll_divide はモードごとに正解が違い(31kHz 768x512 は1104、15kHz 512x512 は
    /// 1216 など)、位相も帯域ごとに最適値が変わる(ケーブルとTVP内部の遅延は固定[ns]
    /// だが、位相レジスタはドット周期の1/32刻みなのでドットクロックが変われば
    /// 同じ遅延が別の目盛りになる)。1つだけ持つと帯域を切り替えるたびに合わせ直しに
    /// なるので、帯域ごとに覚える。
    pub band_pll: BTreeMap<(String, u32), [u32; 2]>,
    /// 管面の位置合わせ(実CRTのH位置/V位置つまみ)

    /// モードごとの設定。キーは "fH[100Hz単位]_vtotal_htotal"。
    /// 同じ31kHzでも 768x512 と 256x256 は同期信号が同一なので、htotal
    /// (= pll_divide)まで含めないと区別できない。pll_divide を合わせた時点で
    /// モードが確定するので、それ以降は自動で復元される。
    /// 値は crop_x,crop_y,crop_w,crop_h,rotate,phase
    /// (古い設定ファイルは phase を持たないので、その場合は既定16を入れる)
    pub modes: BTreeMap<String, [u32; 6]>,
    /// 前回のウィンドウ内寸。次回起動時に復元する
    /// 接続先。番号ごとに覚える(3画面の並びが再起動で入れ替わらないように)
    pub board_sel: BoardSel,
    /// このウィンドウを最後に使っていた時刻(UNIX秒)。**復元の判断に使う。**
    ///
    /// ★**「閉じたら false」では駄目だった。** ウィンドウは別プロセスなので、
    ///   3つ開いて順に閉じると、最後の1つ以外が「個別に閉じられた」ことになり、
    ///   次の起動で1つしか戻らない。閉じた**順序**で結果が変わってしまう。
    /// ★**時刻なら順序に依らない。** 起動時に「最後に使われた時刻」から一定
    ///   時間内に生きていたウィンドウを戻す。3つまとめて終了すれば3つ戻り、
    ///   1つを閉じてしばらく使い続けてから終了すれば、その1つは戻らない。
    ///   クラッシュしても最後の心拍が残るので、そのまま復元できる。
    pub last_used: u64,
    /// ウィンドウ位置。**大きさだけでは並びが戻らない。**
    /// 未知(初回)は None にして OS に任せる
    pub window_x: Option<f32>,
    pub window_y: Option<f32>,
    pub window_w: f32,
    pub window_h: f32,
    /// 配信用クリーン出力ウィンドウを開くか(次回起動時に復元する)
    pub clean_open: bool,
    /// クリーン出力ウィンドウの内寸。**モード切替では変えない**。
    /// 変えると OBS 側のソースサイズが動いて配信が破綻する。
    pub clean_size: [f32; 2],
    /// S/PDIF(デジタル音声入力)を主系統へ混ぜるか。
    pub spdif_mix: bool,
    /// S/PDIF の音量(主系統とは独立)。
    pub spdif_volume: f32,
    /// 低遅延全画面で present をリフレッシュに合わせるか(`--vsync`)
    pub fullscreen_vsync: bool,
    /// ボードのLEDを Viewer から設定するか。false ならボードの既定に任せる
    pub led_control: bool,
    /// ボードのLEDの設定。**ボードには残らない**(電源で既定に戻る)ので、
    /// 繋ぐたびに Viewer が送り直す
    pub leds: crate::leds::LedSettings,
    /// 音声入力を映像ソースに追従させるか。
    /// 既定は on(入力を切り替えたら音声も付いてくるのが自然なので)。
    pub audio_auto: bool,
    /// 詳細表示。調査用の数値と細かい設定を出すか。
    /// 既定は off(常用時に見たいものだけにする)。
    pub show_advanced: bool,
    /// クリーン出力ウィンドウのタイトルバーを消すか。
    /// OBS のウィンドウキャプチャはタイトルバーごと取り込むので既定で消す。
    pub clean_undecorated: bool,
}

fn default_clean_size() -> [f32; 2] {
    crate::cleanout::DEFAULT_SIZE
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
            audio_source: Some(0),
            audio_device: None,
            audio_buffer_ms: DEFAULT_AUDIO_BUFFER_MS,
            adj_hue_deg: 0.0,
            adj_saturation: 1.0,
            adj_brightness: 0.0,
            adj_contrast: 1.0,
            adj_gamma: 1.0,
            tune_vbp: 0,
            tune_hs_offset: 152,
            tune_pll_divide: 1104,
            tune_video_bw: 15,
            tune_phase: 16,
            tune_full_line: false,
            tune_frame_skip: 0,
            frame_skip_fields: false,
            ntsc_comb3d: true,
            // 既定は D-SUB15 の汎用。配っている基板で一番多く使われる端子で、
            // 機種を選ばなくても X68000 / VGA / PC-9801 のどれかとして探せる
            input_port: Port::Rgb15,
            port_profiles: BTreeMap::new(),
            rotate: 0,
            window: [0.22, 0.94, 0.07, 0.98],
            tube_time_based: true,

            mon: [1.0, 0.0, 1.0, 0.0],
            mon_bands: BTreeMap::new(),
            band_pll: BTreeMap::new(),
            modes: BTreeMap::new(),
            tube_aspect: 0.0,
            show_panel: true,
            netcheck_muted: false,
            board_ip: String::new(),
            bezel: String::new(),
            bezel_off: false,
            letterbox: "ruler".into(),
            clean_open: false,
            clean_size: default_clean_size(),
            clean_undecorated: true,
            show_advanced: false,
            audio_auto: true,
            spdif_mix: false,
            spdif_volume: 1.0,
            fullscreen_vsync: false,
            led_control: true,
            leds: Default::default(),
            filter: 2,
            dot_a_milli: default_dot_a(),
            interlace_decay: 1.0,
            crop_x: 0,
            crop_y: 0,
            crop_w: 0,
            crop_h: 0,
            // 既定は「自動」。ボードを1枚しか持っていない人が大半なので、
            // 何も設定しなくても繋がるのが正しい
            board_sel: BoardSel::Auto,
            last_used: 0,
            window_x: None,
            window_y: None,
            window_w: 1160.0,
            window_h: 820.0,
        }
    }
}

/// `mon` の値(H幅, H位置, V幅, V位置)
pub fn parse_mon(v: &str) -> Option<[f32; 4]> {
    let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    (n.len() >= 4).then(|| [n[0], n[1], n[2], n[3]])
}

/// `bandpll` の値(pll_divide, 位相)
pub fn parse_pll(v: &str) -> Option<[u32; 2]> {
    let n: Vec<u32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    (n.len() >= 2).then(|| [n[0], n[1].min(31)])
}

/// これだけ離れていたら「別のとき」。1つ閉じてしばらく使い続けたなら、その
/// ウィンドウは前回のセッションに含まれない ─ という線引き。
const RESTORE_GRACE: u64 = 120;

/// 心拍の時刻から「前回いっしょに使われていたウィンドウ」を選ぶ。
///
/// ★**閉じた順序に依らないこと。** 「閉じたら開いていない印を付ける」方式だと、
///   3つ開いて順に閉じたときに最後の1つ以外が個別に閉じられた扱いになり、
///   次の起動で1つしか戻らない。時刻を見れば、まとめて終了したのか、1つだけ
///   閉じてから使い続けたのかを区別できる。
pub fn restore_from_stamps(self_id: u32, stamps: &[(u32, u64)]) -> Vec<u32> {
    let newest = stamps.iter().map(|(_, t)| *t).max().unwrap_or(0);
    if newest == 0 {
        return Vec::new();
    }
    stamps
        .iter()
        .filter(|(id, t)| *id != self_id && *t > 0 && t + RESTORE_GRACE >= newest)
        .map(|(id, _)| *id)
        .collect()
}

impl Settings {
    /// ウィンドウ番号ごとの設定ファイル。
    ///
    /// ★**番号0は従来どおり `viewer.conf`。** ボードを1枚しか持っていない人は
    ///   ずっと0番を使うので、ファイル名が変わって設定が消えたように見えるのは
    ///   避ける。
    /// ★**2つ同時に開くと同じファイルを取り合う。** 後から保存した方が勝つので、
    ///   片方のウィンドウで詰めた画枠パラメータがもう片方に消される。
    /// ★**そもそも画枠パラメータは接続先ごとの値。** 別の機械が繋がっている
    ///   のだから pll_divide も位相もゲインも違って当たり前。
    pub fn path_for_slot(id: u32) -> PathBuf {
        let p = Self::path();
        if id == 0 {
            p
        } else {
            p.with_file_name(format!("viewer-{id}.conf"))
        }
    }

    /// 番号 `id` の設定を読む(いま使っている設定とは別に覗く。復元の判断用)。
    pub fn peek_slot(id: u32) -> Self {
        Self::parse(&std::fs::read_to_string(Self::path_for_slot(id)).unwrap_or_default())
    }

    pub fn now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 前回いっしょに使われていたウィンドウ番号(自分は除く)。
    ///
    /// 「最後に使われた時刻」から `GRACE` 秒以内に生きていたものを、同じ
    /// セッションだったとみなす。
    pub fn slots_to_restore(self_id: u32, max: u32) -> Vec<u32> {
        let stamps: Vec<(u32, u64)> =
            (0..max).map(|id| (id, Self::peek_slot(id).last_used)).collect();
        restore_from_stamps(self_id, &stamps)
    }

    pub fn path() -> PathBuf {
        // Windows では XDG_CONFIG_HOME も HOME も無いことが多い。そのまま
        // フォールバックすると「.」= カレントディレクトリに作ってしまい、
        // 起動した場所ごとに設定が散らばる(実機のWindowsで確認)。
        // APPDATA(= C:\Users\<user>\AppData\Roaming)を先に見る。
        let base = BASE_DIR.get().cloned()
            .or_else(platform_base)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|h| PathBuf::from(h).join("AppData").join("Roaming"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("retrocastx").join("viewer.conf")
    }

    pub fn load_slot(id: u32) -> Self {
        Self::parse(&std::fs::read_to_string(Self::path_for_slot(id)).unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        // 端子を持つ前の設定ファイル(source_profile と帯域だけのキー)。読み終えてから
        // 新しい形へ移す(行の順序に依らないように)
        let mut legacy_profile: Option<String> = None;
        let mut has_port = false;
        let mut legacy_mon: BTreeMap<u32, [f32; 4]> = BTreeMap::new();
        let mut legacy_pll: BTreeMap<u32, [u32; 2]> = BTreeMap::new();
        let text = text.to_string();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "volume" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.volume = x.clamp(0.0, 1.5);
                    }
                }
                "muted" => s.muted = v == "true",
                "tune_vbp" => { if let Ok(x) = v.parse() { s.tune_vbp = x } }
                "tune_hs_offset" => { if let Ok(x) = v.parse() { s.tune_hs_offset = x } }
                "tune_pll_divide" => { if let Ok(x) = v.parse() { s.tune_pll_divide = x } }
                // 表示倍率まわりの設定は撤去した。表示の形は管面(tube_aspect と
                // モニタプロファイル)が決めるので、ドット数に対する倍率や整数倍の
                // 概念が要らない。枠線と外側の色も、幾何がGPU側へ移って
                // キャプチャ範囲ではなく管面の外周を指すようになったのでやめた。
                "integer_scale" | "show_border" | "backdrop" | "vscale" => {}
                // pll_divide の比例計算(target w)は撤去した。実測が信用できないと
                // 上限まで走るので、いまはスペクトルから求める「自動調整」を使う
                "tune_target_w" | "tune_snap8" => {}
                // インターレースの設定は撤去した(すべて測定から決まる)。
                // 古い設定ファイルにこれらの行が残っていても読み捨てる
                "tune_interlace" | "tune_f2_row" | "tune_field_swap"
                    | "tune_field_src" => {}
                "tune_video_bw" => { if let Ok(x) = v.parse() { s.tune_video_bw = x } }
                "tune_phase" => { if let Ok(x) = v.parse::<u8>() { s.tune_phase = x.min(31) } }
                // 古い版へ戻したときのために書き続けている。新しい版は input_port と
                // port_profile.* を正とし、これは端子を持たない設定からの移行にだけ使う
                "source_profile" => legacy_profile = Some(v.to_string()),
                "input_port" => {
                    if let Some(p) = Port::from_key(v) {
                        s.input_port = p;
                        has_port = true;
                    }
                }
                k if k.starts_with("port_profile.") => {
                    if let Some(p) = Port::from_key(&k[13..]) {
                        s.port_profiles.insert(p, v.to_string());
                    }
                }
                "tune_full_line" => s.tune_full_line = v == "true",
                "tune_frame_skip" => { if let Ok(x) = v.parse::<u8>() { s.tune_frame_skip = x.min(7) } }
                "frame_skip_fields" => s.frame_skip_fields = v == "true",
                "ntsc_comb3d" => s.ntsc_comb3d = v != "false",
                "rotate" => {
                    if let Ok(x) = v.parse::<u32>() {
                        s.rotate = if x < 4 { x } else { 0 };
                    }
                }
                k if k.starts_with("mode.") => {
                    let nums: Vec<u32> =
                        v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if nums.len() >= 5 {
                        let ph = if nums.len() >= 6 { nums[5].min(31) } else { 16 };
                        s.modes.insert(k[5..].to_string(),
                                       [nums[0], nums[1], nums[2], nums[3], nums[4], ph]);
                    }
                }
                "tube_time_based" => { s.tube_time_based = v == "true" }
                // 旧方式(掃引時間を絶対時間で持つ)のキーは読み捨てる。
                // 管面の横幅は 1/fH そのものなので、掃引時間は設定項目ではない。
                "tube_span_us" | "tube_span_ms" | "tube_link_v" | "tube_v_trim"
                    | "tube_fit_margin" | "tube_off_us" | "tube_off_ms" => {}
                k if k.starts_with("band.") => {}
                "mon" => {
                    let n: Vec<f32> =
                        v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if n.len() >= 4 { s.mon = [n[0], n[1], n[2], n[3]] }
                }
                // 機種 × 帯域: `bandpll@x68000.31` / `mon@auto-rgb15.24`
                k if k.starts_with("bandpll@") || k.starts_with("mon@") => {
                    let rest = &k[k.find('@').unwrap() + 1..];
                    let Some((geo, khz)) = rest.rsplit_once('.') else { continue };
                    let Ok(khz) = khz.parse::<u32>() else { continue };
                    if k.starts_with("mon@") {
                        if let Some(m) = parse_mon(v) {
                            s.mon_bands.insert((geo.to_string(), khz), m);
                        }
                    } else if let Some(p) = parse_pll(v) {
                        s.band_pll.insert((geo.to_string(), khz), p);
                    }
                }
                // 帯域だけのキー(旧形式)
                k if k.starts_with("bandpll.") => {
                    if let (Ok(khz), Some(p)) = (k[8..].parse::<u32>(), parse_pll(v)) {
                        legacy_pll.insert(khz, p);
                    }
                }
                k if k.starts_with("mon.") => {
                    if let (Ok(khz), Some(m)) = (k[4..].parse::<u32>(), parse_mon(v)) {
                        legacy_mon.insert(khz, m);
                    }
                }
                "window" => {
                    let v: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if v.len() == 4 { s.window = [v[0], v[1], v[2], v[3]] }
                }
                "tube_aspect" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.tube_aspect = if (0.0..=10.0).contains(&x) { x } else { 0.0 };
                    }
                }
                "bezel" => s.bezel = v.to_string(),
                "show_panel" => s.show_panel = v != "false",
                "netcheck_muted" => s.netcheck_muted = v == "true",
                "board_ip" => s.board_ip = v.to_string(),
                "bezel_off" => s.bezel_off = v == "true",
                "letterbox" => s.letterbox = v.to_string(),
                "clean_open" => s.clean_open = v == "true",
                "clean_undecorated" => s.clean_undecorated = v == "true",
                "show_advanced" => s.show_advanced = v == "true",
                "audio_auto" => s.audio_auto = v != "false",
                "spdif_mix" => s.spdif_mix = v == "true",
                "fullscreen_vsync" => s.fullscreen_vsync = v == "true",
                "led_control" => s.led_control = v != "false",
                "leds" => {
                    // led_idle が先に読まれていることもあるので、切断中は残す
                    if let Some(l) = crate::leds::LedSettings::parse(v) {
                        s.leds = crate::leds::LedSettings { idle: s.leds.idle, ..l };
                    }
                }
                // 小亀のLEDの切断中の動作。leds の行とは分けてある(leds.rs の format)
                "led_idle" => {
                    if let Some(i) = crate::leds::Idle::from_key(v) { s.leds.idle = i }
                }
                "spdif_volume" => {
                    if let Ok(x) = v.parse::<f32>() { s.spdif_volume = x.clamp(0.0, 1.5) }
                }
                "clean_size" => {
                    let a: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    // 極端な値で開くと画面外に出て閉じられなくなるので範囲を切る
                    if a.len() == 2 && (160.0..=8192.0).contains(&a[0])
                        && (120.0..=8192.0).contains(&a[1]) {
                        s.clean_size = [a[0], a[1]];
                    }
                }
                "filter" => { if let Ok(x) = v.parse::<u32>() { s.filter = x.min(2) } }
                "dot_a_milli" => { if let Ok(x) = v.parse::<u32>() { s.dot_a_milli = x.min(900) } }
                "interlace_decay" => {
                    if let Ok(x) = v.parse::<f32>() { s.interlace_decay = x.clamp(0.0, 1.0) }
                }
                "crop_x" => { if let Ok(x) = v.parse() { s.crop_x = x } }
                "crop_y" => { if let Ok(x) = v.parse() { s.crop_y = x } }
                "crop_w" => { if let Ok(x) = v.parse() { s.crop_w = x } }
                "crop_h" => { if let Ok(x) = v.parse() { s.crop_h = x } }
                "board_sel" => {
                    if let Some(x) = BoardSel::parse(v) { s.board_sel = x }
                }
                "last_used" => { if let Ok(x) = v.parse() { s.last_used = x } }
                "window_x" => { s.window_x = v.parse::<f32>().ok() }
                "window_y" => { s.window_y = v.parse::<f32>().ok() }
                "window_w" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.window_w = x.clamp(480.0, 8192.0);
                    }
                }
                "window_h" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.window_h = x.clamp(360.0, 8192.0);
                    }
                }
                "audio_source" => {
                    s.audio_source = if v == "off" { None } else { v.parse().ok() };
                }
                "audio_device" => {
                    s.audio_device = if v.is_empty() { None } else { Some(v.to_string()) };
                }
                "audio_buffer_ms" => {
                    if let Ok(x) = v.parse::<u32>() {
                        s.audio_buffer_ms = x.clamp(5, 500);
                    }
                }
                _ => {}
            }
        }
        // 端子を持たない設定からの移行。選んでいた機種の端子へ置く
        // (「自動」は D-SUB15 の汎用)
        if !has_port {
            if let Some(key) = legacy_profile {
                let port = profiles::by_key(&key).map_or(Port::Rgb15, |p| p.port);
                s.input_port = port;
                s.port_profiles.insert(port, key);
            }
        }
        // ★**旧形式の帯域ごとの値は、全部の機種へ写す。** どの機種で合わせた値かは
        //   ファイルに残っていないので、選んでいた機種にだけ渡すと、ほかの機種へ
        //   切り替えたときに合わせ直しになる。全部へ写せば、更新した直後は以前と
        //   同じ見え方で始まり、以後は機種ごとに分かれていく。
        //
        // ★**ただし漂った帯域は写さない。** 何も繋がっていない入力では fH が
        //   範囲外をゆっくり漂うので、`mon.40`〜`mon.75` のような実在しない帯域が
        //   既定値のまま大量に残っている(実際の設定ファイルで37帯域あった)。
        //   既定値の管面は写さなくても同じ既定値が使われるので、捨てても何も
        //   失わない。PLL は、意味のある管面を持つ帯域と標準の 15/24/31kHz だけ写す。
        //
        // ★**`input_port` のあるファイル(この版が書いたもの)では写さない。** 旧形式の
        //   値は古い版へ戻したとき用にいまの機種の分だけ書いているので、写すと
        //   その機種の値がほかの機種の空いている帯域へ毎回染み出す。
        legacy_mon.retain(|_, m| *m != Self::default().mon);
        legacy_pll.retain(|b, _| legacy_mon.contains_key(b) || [15, 24, 31].contains(b));
        if !has_port && (!legacy_mon.is_empty() || !legacy_pll.is_empty()) {
            let geos = profiles::PROFILES
                .iter()
                .map(|p| p.key.to_string())
                .chain(Port::ALL.into_iter().filter(|p| p.has_generic())
                           .map(|p| profiles::geo_key(p, "")));
            for g in geos {
                for (khz, m) in &legacy_mon {
                    s.mon_bands.entry((g.clone(), *khz)).or_insert(*m);
                }
                for (khz, p) in &legacy_pll {
                    s.band_pll.entry((g.clone(), *khz)).or_insert(*p);
                }
            }
        }
        s
    }

    /// いま選んでいる機種のキー。空文字は汎用
    pub fn source_profile(&self) -> &str {
        self.port_profiles
            .get(&self.input_port)
            .map_or(self.input_port.default_profile(), |s| s.as_str())
    }

    /// いまの機種の、帯域ごとの管面(低遅延全画面へ渡す)
    pub fn mon_bands_for_current(&self) -> BTreeMap<u32, [f32; 4]> {
        let geo = profiles::geo_key(self.input_port, self.source_profile());
        self.mon_bands
            .iter()
            .filter(|((g, _), _)| *g == geo)
            .map(|((_, k), v)| (*k, *v))
            .collect()
    }

    pub fn save_slot(&self, id: u32) {
        self.save_to(&Self::path_for_slot(id));
    }

    fn save_to(&self, path: &std::path::Path) {
        if let Some(dir) = path.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                return;
            }
        }
        let body = format!(
            "# RetroCast X settings (自動生成。消しても既定値で起動する)\n\
             volume = {:.3}\n\
             muted = {}\n\
             audio_source = {}\n\
             audio_device = {}\n\
             audio_buffer_ms = {}\n\
             tune_vbp = {}\n\
             tune_hs_offset = {}\n\
             tune_pll_divide = {}\n\
             tune_video_bw = {}\n\
             tune_phase = {}\n\
             source_profile = {}\n\
             tune_full_line = {}\n\
             tune_frame_skip = {}\n\
             frame_skip_fields = {}\n\
             ntsc_comb3d = {}\n\
             tube_time_based = {}\n\
             mon = {:.4},{:.4},{:.4},{:.4}\n\
             rotate = {}\n\
             window = {:.4},{:.4},{:.4},{:.4}\n\
             tube_aspect = {:.4}\n\
             bezel = {}\n\
             show_panel = {}\n\
             netcheck_muted = {}\n\
             board_ip = {}\n\
             bezel_off = {}\n\
             letterbox = {}\n\
             filter = {}\n\
             dot_a_milli = {}\n\
             interlace_decay = {}\n\
             crop_x = {}\n\
             crop_y = {}\n\
             crop_w = {}\n\
             crop_h = {}\n\
             board_sel = {}\n\
             last_used = {}\n\
             window_x = {}\n\
             window_y = {}\n\
             window_w = {:.0}\n\
             window_h = {:.0}\n\
             clean_open = {}\n\
             clean_size = {:.0},{:.0}\n\
             clean_undecorated = {}\n\
             show_advanced = {}\n\
             audio_auto = {}\n\
             spdif_mix = {}\n\
             spdif_volume = {:.3}\n\
             fullscreen_vsync = {}\n\
             led_control = {}\n\
             leds = {}\n\
             led_idle = {}\n",
            self.volume,
            self.muted,
            self.audio_source.map(|v| v.to_string()).unwrap_or_else(|| "off".into()),
            self.audio_device.clone().unwrap_or_default(),
            self.audio_buffer_ms,
            self.tune_vbp,
            self.tune_hs_offset,
            self.tune_pll_divide,
            self.tune_video_bw,
            self.tune_phase,
            self.source_profile(),
            self.tune_full_line,
            self.tune_frame_skip,
            self.frame_skip_fields,
            self.ntsc_comb3d,
            self.tube_time_based,
            self.mon[0], self.mon[1], self.mon[2], self.mon[3],
            self.rotate,
            self.window[0],
            self.window[1],
            self.window[2],
            self.window[3],
            self.tube_aspect,
            self.bezel,
            self.show_panel,
            self.netcheck_muted,
            self.board_ip,
            self.bezel_off,
            self.letterbox,
            self.filter,
            self.dot_a_milli,
            self.interlace_decay,
            self.crop_x,
            self.crop_y,
            self.crop_w,
            self.crop_h,
            self.board_sel.to_text(),
            self.last_used,
            self.window_x.map(|v| format!("{v:.0}")).unwrap_or_default(),
            self.window_y.map(|v| format!("{v:.0}")).unwrap_or_default(),
            self.window_w,
            self.window_h,
            self.clean_open,
            self.clean_size[0], self.clean_size[1],
            self.clean_undecorated,
            self.show_advanced,
            self.audio_auto,
            self.spdif_mix,
            self.spdif_volume,
            self.fullscreen_vsync,
            self.led_control,
            self.leds.format(),
            self.leds.idle.key(),
        );
        let mut body = body;
        body.push_str(&format!("input_port = {}\n", self.input_port.key()));
        for (port, key) in &self.port_profiles {
            body.push_str(&format!("port_profile.{} = {key}\n", port.key()));
        }
        for ((geo, khz), v) in &self.band_pll {
            body.push_str(&format!("bandpll@{geo}.{khz} = {},{}\n", v[0], v[1]));
        }
        for ((geo, khz), v) in &self.mon_bands {
            body.push_str(&format!(
                "mon@{geo}.{khz} = {},{},{},{}\n", v[0], v[1], v[2], v[3]));
        }
        // ★**古い版の形でも、いまの機種の分だけ書く。** 0.14 以前の Viewer は
        //   `mon@…` を知らず、無いと管面を全帯域とも既定値(1,0,1,0)で始めて
        //   そのまま保存する(実機で発生: この版を試したあと入れてある版に戻したら、
        //   X68000 の管面が消えた)。読むときは `mon@…` が優先なので害は無い
        let geo = profiles::geo_key(self.input_port, self.source_profile());
        for ((g, khz), v) in &self.band_pll {
            if *g == geo {
                body.push_str(&format!("bandpll.{khz} = {},{}\n", v[0], v[1]));
            }
        }
        for (khz, v) in self.mon_bands_for_current() {
            body.push_str(&format!("mon.{khz} = {},{},{},{}\n", v[0], v[1], v[2], v[3]));
        }
        for (k, v) in &self.modes {
            body.push_str(&format!(
                "mode.{k} = {},{},{},{},{},{}\n",
                v[0], v[1], v[2], v[3], v[4], v[5]));
        }
        // 書けなくても致命的ではないので黙って諦める(次回は既定値で起動する)
        if let Ok(mut f) = std::fs::File::create(path) {
            let _ = f.write_all(body.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★**まとめて終了したら全部戻る。** 3画面筐体では並びそのものが設定で、
    ///   毎回開き直すのでは使い物にならない。
    #[test]
    fn restores_windows_closed_together() {
        let t = 1_800_000_000u64;
        // 3つを数秒差で閉じた
        let stamps = [(0, t), (1, t - 3), (2, t - 7)];
        assert_eq!(restore_from_stamps(0, &stamps), vec![1, 2]);
        // ★**閉じた順序で結果が変わらないこと。** どれが最後でも同じ3つ。
        let stamps = [(0, t - 7), (1, t), (2, t - 3)];
        assert_eq!(restore_from_stamps(0, &stamps), vec![1, 2]);
    }

    /// 1つ閉じてしばらく使い続けたなら、その1つは戻らない。
    #[test]
    fn does_not_restore_a_window_closed_long_ago() {
        let t = 1_800_000_000u64;
        let stamps = [(0, t), (1, t - RESTORE_GRACE - 1), (2, t - 10)];
        assert_eq!(restore_from_stamps(0, &stamps), vec![2]);
    }

    /// 使ったことのない番号(心拍0)は戻さない。
    #[test]
    fn ignores_never_used_slots() {
        let t = 1_800_000_000u64;
        let stamps = [(0, t), (1, 0), (2, 0)];
        assert!(restore_from_stamps(0, &stamps).is_empty());
        assert!(restore_from_stamps(0, &[(0, 0), (1, 0)]).is_empty());
    }

    #[test]
    fn board_sel_roundtrips() {
        for v in [
            BoardSel::Auto,
            BoardSel::None,
            BoardSel::Mac([0x74, 0xc9, 0x0f, 0x7c, 0x84, 0x29]),
        ] {
            assert_eq!(BoardSel::parse(&v.to_text()), Some(v), "{v:?}");
        }
        // 読めない値は既定へ落とさず None を返す(呼び側が既定を保つ)
        assert_eq!(BoardSel::parse("zzz"), None);
    }

    /// 端子を持つ前の設定ファイルを読んだとき、選んでいた機種の端子へ移り、
    /// 帯域だけの管面・PLL が全部の機種へ写ること。
    #[test]
    fn migrates_band_only_settings_to_every_profile() {
        let s = Settings::parse(
            "source_profile = msx\nmon.15 = 0.9,0.1,0.95,0\nbandpll.31 = 1104,12\n");
        assert_eq!(s.input_port, Port::Rgb21);
        assert_eq!(s.source_profile(), "msx");
        for geo in ["msx", "x68000", "pc98", "auto-rgb15", "auto-rgb21"] {
            assert_eq!(s.mon_bands.get(&(geo.into(), 15)), Some(&[0.9, 0.1, 0.95, 0.0]), "{geo}");
            assert_eq!(s.band_pll.get(&(geo.into(), 31)), Some(&[1104, 12]), "{geo}");
        }
        // 「自動」は D-SUB15 の汎用
        let s = Settings::parse("source_profile = \n");
        assert_eq!((s.input_port, s.source_profile()), (Port::Rgb15, ""));
    }

    /// 未接続で漂ってできた帯域(既定値の管面)は写さない。実在の帯域は写す
    #[test]
    fn migration_drops_drifted_bands() {
        let s = Settings::parse(
            "source_profile = composite\n\
             mon.31 = 0.697,-0.086,0.788,-0.022\nmon.47 = 1,0,1,0\nmon.1000 = 0.8,0,1,0\n\
             bandpll.31 = 1104,16\nbandpll.47 = 1104,16\nbandpll.15 = 1820,10\n");
        assert_eq!(s.input_port, Port::Video);
        assert!(s.mon_bands.contains_key(&("x68000".into(), 31)));
        assert!(s.mon_bands.contains_key(&("composite".into(), 1000)));
        assert!(s.band_pll.contains_key(&("msx".into(), 15)));
        assert!(!s.mon_bands.keys().any(|(_, b)| *b == 47), "漂った帯域の管面が残った");
        assert!(!s.band_pll.keys().any(|(_, b)| *b == 47), "漂った帯域の PLL が残った");
    }

    /// 新しい形は書いて読み直しても同じになる。機種ごとの値が混ざらないこと
    #[test]
    fn port_and_per_profile_geometry_roundtrip() {
        let mut s = Settings::default();
        s.input_port = Port::Video;
        s.port_profiles.insert(Port::Video, "svideo".into());
        s.port_profiles.insert(Port::Rgb15, "pc98".into());
        s.mon_bands.insert(("x68000".into(), 31), [0.9, -0.05, 0.95, 0.0]);
        s.mon_bands.insert(("vga".into(), 31), [1.0, 0.0, 1.0, 0.0]);
        s.band_pll.insert(("auto-rgb15".into(), 24), [848, 7]);
        let dir = std::env::temp_dir().join(format!("rcx-settings-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("viewer.conf");
        s.save_to(&path);
        let r = Settings::parse(&std::fs::read_to_string(&path).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r.input_port, Port::Video);
        assert_eq!(r.source_profile(), "svideo");
        assert_eq!(r.port_profiles.get(&Port::Rgb15).map(|s| s.as_str()), Some("pc98"));
        assert_eq!(r.mon_bands, s.mon_bands);
        assert_eq!(r.band_pll, s.band_pll);
    }

    /// 古い版へ戻しても管面が残るよう旧形式も書く。読み直しても他機種へ染み出さない
    #[test]
    fn writes_legacy_keys_for_downgrade_without_spilling() {
        let mut s = Settings::default();
        s.input_port = Port::Rgb15;
        s.port_profiles.insert(Port::Rgb15, "x68000".into());
        s.mon_bands.insert(("x68000".into(), 31), [0.697, -0.086, 0.788, -0.022]);
        s.band_pll.insert(("x68000".into(), 31), [1104, 16]);
        let dir = std::env::temp_dir().join(format!("rcx-legacy-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("viewer.conf");
        s.save_to(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.contains("\nmon.31 = 0.697,-0.086,0.788,-0.022\n"));
        assert!(text.contains("\nbandpll.31 = 1104,16\n"));
        let r = Settings::parse(&text);
        assert_eq!(r.mon_bands, s.mon_bands);
        assert_eq!(r.band_pll, s.band_pll);
    }

    /// ★**番号0は従来どおり `viewer.conf`。** ボードを1枚しか持っていない人は
    ///   ずっと0番なので、ファイル名が変わって設定が消えたように見えるのを防ぐ。
    /// 小亀のLEDの切断中の動作は leds の行と別のキー。どちらが先に来ても残ること
    #[test]
    fn led_idle_survives_either_order() {
        use crate::leds::Idle;
        let line = "leds = solid,ff0000,4000,50,3,64,32";
        let a = Settings::parse(&format!("led_idle = off\n{line}\n"));
        assert_eq!((a.leds.idle, a.leds.count), (Idle::Off, 3));
        let b = Settings::parse(&format!("{line}\nled_idle = breathe\n"));
        assert_eq!((b.leds.idle, b.leds.count), (Idle::Breathe, 3));
        // 無ければ「そのまま」
        assert_eq!(Settings::parse(&format!("{line}\n")).leds.idle, Idle::Keep);
    }

    #[test]
    fn slot_zero_keeps_the_original_filename() {
        assert_eq!(Settings::path_for_slot(0), Settings::path());
        assert_eq!(
            Settings::path_for_slot(2).file_name().unwrap(),
            "viewer-2.conf"
        );
    }
}
