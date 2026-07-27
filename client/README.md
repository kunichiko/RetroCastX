# RetroCastX Viewer(クライアントアプリ雛形)

Rust + eframe/egui(wgpuバックエンド)のクロスプラットフォーム・ビューア。
Mac/Windows/Linux を単一コードベースでカバーする。

```sh
cargo run --release                     # SUBSCRIBEをブロードキャストして実機ボードを受信
cargo run --release -- --board 192.168.10.50   # ボードIP指定
cargo run --release -- --no-subscribe   # 受け専用(sender_sim相手はこれ)
cargo run --release -- --fullscreen     # 低遅延フルスクリーン(下記、ESCで終了)
cargo run --release -- --headless 5 --no-subscribe  # GUIなしで受信統計のみ(検証用)
cargo test                              # プロトコル/アセンブラのユニットテスト
```

sender_sim を相手にした動作確認:

```sh
(cd ../host/python && python3 -m retrocastx.sender_sim --pixfmt rgb555) &
cargo run --release -- --no-subscribe
```

## 構成

- `src/protocol.rs` — プロトコルv0のパース(リファレンスは `host/python/retrocastx/protocol.py`)
- `src/assembler.rs` — LINE→フレーム再構成(RGBA8出力、ロス/迷子ライン統計)
- `src/receiver.rs` — UDP受信スレッド + SUBSCRIBEキープアライブ(2秒ごと)
- `src/main.rs` — eframe UI(映像表示 + モード/統計/発見ボードのサイドパネル)

## 設計メモ

- v0.1 は egui 標準のテクスチャAPIで毎フレーム全面アップロード(512×512なら余裕)。
  **パレット吸着・CRTシェーダ・整数拡大以外のフィルタは egui_wgpu の paint callback
  (自前WGSLシェーダ)へ移行して実装する**(次段)

### VRR / 非標準リフレッシュ(55.46Hz等)の追従 — 検証結果(2026-07-27, Mac mini M4 Pro + 60Hz外部モニタ)

`examples/pace_probe.rs`(専用スレッドからwgpuサーフェスへ直接present)による実測:

| 構成 | present間隔 |
|---|---|
| eframe(ウィンドウ、vsync/no-vsyncとも) | **60Hzに量子化**(winitの再描画スケジューリングがディスプレイリンク駆動) |
| 専用スレッド + Immediate、ウィンドウ表示 | **60Hzに量子化**(コンポジタ律速。latency増でも変わらず) |
| 専用スレッド + Immediate、**フルスクリーン** | **ターゲット追従**: 90Hz指定→11.1ms/89.8Hz、55.46Hz指定→18.03ms/55.4Hz(σ~2.5ms) |

結論: **フルスクリーン(direct-to-display)+専用presentスレッド+Immediate** が
55.46Hz追従の実装形。レトロゲーム全画面用途と合致する。ウィンドウ表示時は
60Hz vsyncのジャダーを許容(それでも実用上は十分)。
再現: `cargo run --release --example pace_probe -- 55.46 --latency 3 --fullscreen`

この構成は `--fullscreen` モード(`src/fullscreen.rs`)として本実装済み:
フレーム到着駆動でpresentし、sender_sim 512×512@55.46Hz RGB555 に対して
**present間隔 18.03ms σ0.7ms = 55.46Hz追従・ロス0** を実測確認(2026-07-27)。
プライマリモニタに全画面表示(不可視ディスプレイ対策でモニタ明示)、
ニアレスト+整数拡大レターボックス、ESC/クローズで終了、統計はstderr。
タイムスタンプ駆動のジッタ平滑化(到着ゆらぎをドットクロック時刻で吸収)は次段。

残る確認は**パネルが実際に追従するか**で、これはVRR対応表示器が必要
(ProMotion搭載MacBookの内蔵ディスプレイ、またはAdaptive-Sync対応モニタのDP接続)。
このマシンの60Hz固定モニタではpresentは55.46Hzで打てるが表示は60Hzサンプリングになる。
Windows側は wgpu Immediate が DXGI allow-tearing にマップされるので FreeSync で同様の構成が取れる見込み。
- 音声(AUDIO パケット, protocol-v0.md 参照)の再生は未実装。cpal クレートで
  リングバッファ再生を追加予定
- CONFIG パケット(音声ソース選択・ArgusX入力切替)のUIも未実装(プロトコル定義済み)
- インターレース: 現状の実装で自然に **weave 合成**になる(プロトコルの規約により
  LINE.line がフルフレーム行のため。検証済み)。**bob 表示**(フィールド縦2倍・
  フィールドレート表示、コーミングなし)は将来の表示オプション
