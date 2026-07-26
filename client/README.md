# RetroCastX Viewer(クライアントアプリ雛形)

Rust + eframe/egui(wgpuバックエンド)のクロスプラットフォーム・ビューア。
Mac/Windows/Linux を単一コードベースでカバーする。

```sh
cargo run --release                     # SUBSCRIBEをブロードキャストして実機ボードを受信
cargo run --release -- --board 192.168.10.50   # ボードIP指定
cargo run --release -- --no-subscribe   # 受け専用(sender_sim相手はこれ)
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
- **VRR / 非標準リフレッシュ(55.46Hz等)の追従**は eframe のフレームスケジューラの外に
  出る必要が出た時点で、presentation層のみネイティブAPI(macOS: CAMetalLayer +
  presentAtTime / Windows: DXGI + tearing flag)に差し替える。受信・再構成・UIは共通のまま
- 音声(AUDIO パケット, protocol-v0.md 参照)の再生は未実装。cpal クレートで
  リングバッファ再生を追加予定
- CONFIG パケット(音声ソース選択・ArgusX入力切替)のUIも未実装(プロトコル定義済み)
