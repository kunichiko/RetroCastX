# データシート置き場(ADCフロントエンド)

メーカー配布のPDFは**再配布可否が不明なのでコミットしていません**(`.gitignore` 済み)。
必要なときは各自で入手して、このディレクトリに下表の名前で置いてください。
`main.ato` のコメントはこの名前で参照しています。

| ファイル | 内容 | 入手元 |
|---|---|---|
| `HR911130A_HanRun_MagJack.pdf` | HanRun HR911130A(1000BASE-T MagJack、トランス・チョーク・Bob-Smith内蔵RJ45) | LCSC [C54408](https://www.lcsc.com/product-detail/C54408.html) |
| `PJ-327C-4A_HOOYA_3.5mmJack.pdf` | PJ-327C-4A(3.5mm ステレオジャック、SMD 4P) | LCSC [C145813](https://www.lcsc.com/product-detail/C145813.html) |

LCSC の製品ページ内「Datasheet」リンクからPDFを取得できます。

## メモ

これらの資料から確認した内容(ピン配置・内蔵終端の構成など)は `main.ato` の
コメントに書き出しています。データシートが手元になくても設計の根拠は追えるように
しているので、再ダウンロードは実際にピン配置を検証し直すときだけで足ります。

なお TVP7002(TI SLES206C)は `docs/datasheets/` 側に置く運用です。
