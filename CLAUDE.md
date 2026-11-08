# RetroCastX

基板(`hardware/`)・Viewer(`client/` `host/`)・ゲートウェア(`gateware/`)が
同居するリポジトリ。

## ブランチ運用

長く生きるブランチは `main` だけ。`develop` や `develop-pcb` のような
領域別の長寿命ブランチは作らない。

- 作業は `main` から `pcb/…` `viewer/…` `gw/…` `docs/…` の短命ブランチを切り、
  `main` に戻したら消す。
- リリースはタグで区別する: Viewer `vX.Y.Z`、ゲートウェア `gw-vX.Y.Z`、
  基板 `vX.Y.Z-pcb`。

詳細は [CONTRIBUTING.md](CONTRIBUTING.md) の「ブランチとリリースの運用」。

## コミット

- すべてのコミットに `Signed-off-by` を付ける(DCO の CI がある)。
  `.githooks/prepare-commit-msg` が自動で付けるので、`git config --get core.hooksPath`
  が `.githooks` でなければ、まず `git config core.hooksPath .githooks` を実行する。
- コミットメッセージは日本語で、既存の履歴の書き方に合わせる。
