# データシート置き場(ADCフロントエンド)

メーカー配布のPDFは**再配布が許諾されていないためコミットしていません**(`.gitignore` 済み)。
必要なときは各自で入手して、このディレクトリに下表の名前で置いてください。
`main.ato` のコメントはこの名前で参照しています。

多くは可否の記載自体がありませんが(=許諾なし)、Everlight PLR135/T は
データシート本文で明示的に禁止しています。

> These specification sheets include materials protected under copyright of
> EVERLIGHT corporation. Please don't reproduce or cause anyone to reproduce
> them without EVERLIGHT's consent.

**新しい部品を追加するときは、データシートPDFをコミットしないこと。**
この表に1行足して、入手元だけ残してください。

| ファイル | 内容 | 入手元 |
|---|---|---|
| `HR911130A_HanRun_MagJack.pdf` | HanRun HR911130A(1000BASE-T MagJack、トランス・チョーク・Bob-Smith内蔵RJ45) | LCSC [C54408](https://www.lcsc.com/product-detail/C54408.html) |
| `PJ-327C-4A_HOOYA_3.5mmJack.pdf` | PJ-327C-4A(3.5mm ステレオジャック、SMD 4P) | LCSC [C145813](https://www.lcsc.com/product-detail/C145813.html) |
| `emzt6.8e.pdf` | ROHM EMZT6.8ET2R(4ch コモンアノード ESDアレイ、EMD5 / SC-75A)。D1〜D6, D8 | LCSC [C510333](https://www.lcsc.com/product-detail/C510333.html) / ROHM 製品ページで型番検索 |
| `plr135-t.pdf` | Everlight PLR135/T(TOSLINK 光受信モジュール)。J8。文書番号 DPL-0000018_Rev.4 | 秋月電子 [109595](https://akizukidenshi.com/catalog/g/g109595/) |

LCSC の製品ページ内「Datasheet」リンク、または秋月の商品ページからPDFを取得できます。

**注意(PLR135)**: v0.9.0 に実装するのは **PLR135/T**(秋月
[109595](https://akizukidenshi.com/catalog/g/g109595/)、文書 DPL-0000018_Rev.4)で、
上表の `plr135-t.pdf` がその資料です。

以前は **PLR135/T10**(秋月 [109597](https://akizukidenshi.com/catalog/g/g109597/)、
別文書 DPL-0000031_Rev.4)を選定していましたが、DigiKey で廃品種・在庫0になったため
2026-08-19 に T へ切り替えました(取付穴が φ1.15/5.08mm → φ3.20/7.62mm と違うので
ランドも作り直しています)。`parts/_mech/PLR135_T10.kicad_mod` は履歴として
残していますが**使っていません**。経緯は `parts/_mech/mech.ato` の
`SpdifRxModule` の docstring を参照。

## メモ

これらの資料から確認した内容(ピン配置・内蔵終端の構成など)は `main.ato` の
コメントに書き出しています。データシートが手元になくても設計の根拠は追えるように
しているので、再ダウンロードは実際にピン配置を検証し直すときだけで足ります。

なお TVP7002(TI SLES206C)は `docs/datasheets/` 側に置く運用です。
