# デジグネータ対応表

デジグネータは **main.ato の宣言順**に振ってある。上から下へ読むとほぼ機能ブロック順に
番号が並ぶ。振り直しは `tools/renumber_designators.py` を実行するだけ
(`ato build` 後に走らせる。`keep_designators`が既定trueなので以後は維持される)。

**手組みの試作機(V0)は「V0」列の番号**なので、実機の写真やオシロのメモを読むときは
この表で読み替えること。V0列が空欄のものは後から追加した部品。

## 仕組み

atopile は `keep_designators`(既定 true)のとき、`.kicad_pcb` の Reference プロパティを
正典としてデジグネータを読み込む(`faebryk/libs/app/designators.py` の
`load_kicad_pcb_designators`)。**PCB側でリネームすれば `ato build` はそれを維持する。**
重複があると `ato build` がエラーで止まるので安全側に倒れている。

各フットプリントの `atopile_address`(例 `ft2232.c_osci`, `dec_caps[6]`, `ch_g2.r_term`)の
パスを辿り、各段の宣言行番号を並べたタプルをソートキーにしている。配列は添字を数値で見る。

★**`.kicad_dru` の `memberOfFootprint` はデジグネータ直書き**なので、振り直したら
  必ず確認すること。現在の参照は `U1`(TVP7002) / `U7`(FT2232H) / `U10`(SO-DIMM) /
  `JP1`(5V給電ジャンパ) / `D*`(ESDアレイ、ワイルドカード)。忘れると微細ピッチの
  ファンアウト緩和が黙って無効になり clearance 違反が激増する(実測 0→401件)。

★**配列(`new X[n]`)の要素を減らすと、残った部品のネットだけが入れ替わる。**
  添字が `atopile_address` になるため、フットプリントと既存配線はそのままネットが
  変わって短絡する。実際に `nc_caps` で起きた(2026-08-14)。要素数が変わりうる
  ものは個別の名前にしておくこと。

| V0 | 現在 | 回路上の位置 | 値 |
|---|---|---|---|
| U10 | **U1** | `tvp` | — |
| J10 | **J1** | `vga` | — |
| U3 | **U2** | `buf_sync` | — |
| X1 | **X1** | `osc` | — |
| U9 | **U3** | `supply.ldo_io` | — |
| U7 | **U4** | `supply.ldo_a33` | — |
| U6 | **U5** | `supply.ldo_a19` | — |
| U8 | **U6** | `supply.ldo_d19` | — |
| C56 | **C1** | `supply.bulk_caps[0]` | 10µF ±10% |
| C57 | **C2** | `supply.bulk_caps[1]` | 10µF ±10% |
| C58 | **C3** | `supply.bulk_caps[2]` | 10µF ±10% |
| C59 | **C4** | `supply.bulk_caps[3]` | 10µF ±10% |
| C60 | **C5** | `supply.bulk_caps[4]` | 10µF ±10% |
| J9 | **J2** | `usbc` | — |
| R4 | **R1** | `r_cc1` | 5.1kΩ ±1% |
| R5 | **R2** | `r_cc2` | 5.1kΩ ±1% |
| U13 | **U7** | `ft2232.ft` | — |
| FB4 | **FB1** | `ft2232.fb_phy` | — |
| FB5 | **FB2** | `ft2232.fb_pll` | — |
| C64 | **C6** | `ft2232.c_vccio[0]` | 100nF ±10% |
| C65 | **C7** | `ft2232.c_vccio[1]` | 100nF ±10% |
| C66 | **C8** | `ft2232.c_vccio[2]` | 100nF ±10% |
| C67 | **C9** | `ft2232.c_vccio[3]` | 100nF ±10% |
| C72 | **C10** | `ft2232.c_vregin` | 100nF ±10% |
| C71 | **C11** | `ft2232.c_vregin_bulk` | 10µF ±10% |
| C74 | **C12** | `ft2232.c_vregout` | 100nF ±10% |
| C73 | **C13** | `ft2232.c_vregout_bulk` | 1µF ±10% |
| C68 | **C14** | `ft2232.c_vcore` | 100nF ±10% |
| C69 | **C15** | `ft2232.c_vphy` | 100nF ±10% |
| C70 | **C16** | `ft2232.c_vpll` | 100nF ±10% |
| R14 | **R3** | `ft2232.r_ref` | 12kΩ ±1% |
| R15 | **R4** | `ft2232.r_reset` | 10kΩ ±1% |
| X3 | **X2** | `ft2232.xtal` | — |
| C46 | **C17** | `ft2232.c_osci` | 33pF ±5% |
| C63 | **C18** | `ft2232.c_osco` | 33pF ±5% |
| U12 | **U8** | `ft2232.ee` | — |
| C45 | **C19** | `ft2232.c_ee` | 100nF ±10% |
| PG1 | **PG1** | `pogo_tck` | — |
| PG4 | **PG2** | `pogo_tms` | — |
| PG2 | **PG3** | `pogo_tdi` | — |
| PG3 | **PG4** | `pogo_tdo` | — |
| J1 | **J3** | `jtag_hdr` | — |
| JP3 | **JP1** | `sj_ext5v` | — |
| FB2 | **FB3** | `fb_avdd` | — |
| FB3 | **FB4** | `fb_pll` | — |
| C25 | **C20** | `dec_caps[0]` | 100nF ±10% |
| C26 | **C21** | `dec_caps[1]` | 100nF ±10% |
| C27 | **C22** | `dec_caps[2]` | 100nF ±10% |
| C28 | **C23** | `dec_caps[3]` | 100nF ±10% |
| C29 | **C24** | `dec_caps[4]` | 100nF ±10% |
| C30 | **C25** | `dec_caps[5]` | 100nF ±10% |
| C31 | **C26** | `dec_caps[6]` | 100nF ±10% |
| C32 | **C27** | `dec_caps[7]` | 100nF ±10% |
| C33 | **C28** | `dec_caps[8]` | 100nF ±10% |
| C34 | **C29** | `dec_caps[9]` | 100nF ±10% |
| C35 | **C30** | `dec_caps[10]` | 100nF ±10% |
| C36 | **C31** | `dec_caps[11]` | 100nF ±10% |
| C37 | **C32** | `dec_caps[12]` | 100nF ±10% |
| C38 | **C33** | `dec_caps[13]` | 100nF ±10% |
| R3 | **R5** | `ch_r.r_term` | 75Ω ±1% |
| C24 | **C34** | `ch_r.c_ac` | 100nF ±10% |
| R20 | **R6** | `ch_r.r_aa` | 220Ω ±1% |
| C81 | **C35** | `ch_r.c_aa` | 33pF ±5% |
| R2 | **R7** | `ch_g.r_term` | 75Ω ±1% |
| C23 | **C36** | `ch_g.c_ac` | 100nF ±10% |
| R13 | **R8** | `ch_g.r_aa` | 220Ω ±1% |
| C80 | **C37** | `ch_g.c_aa` | 33pF ±5% |
| R1 | **R9** | `ch_b.r_term` | 75Ω ±1% |
| C22 | **C38** | `ch_b.c_ac` | 100nF ±10% |
| R12 | **R10** | `ch_b.r_aa` | 220Ω ±1% |
| C79 | **C39** | `ch_b.c_aa` | 33pF ±5% |
| D2 | **D1** | `esd_rgb` | — |
| C18 | **C40** | `c_sog3` | 100nF ±10% |
| R31 | **R11** | `r_sog3` | 220Ω ±1% |
| C85 | **C41** | `c_sog3_aa` | 33pF ±5% |
| J7 | **J4** | `aux` | — |
| R33 | **R12** | `ch_r2.r_term` | 75Ω ±1% |
| C92 | **C42** | `ch_r2.c_ac` | 100nF ±10% |
| R32 | **R13** | `ch_r2.r_aa` | 220Ω ±1% |
| C91 | **C43** | `ch_r2.c_aa` | 33pF ±5% |
| R30 | **R14** | `ch_g2.r_term` | 75Ω ±1% |
| C90 | **C44** | `ch_g2.c_ac` | 100nF ±10% |
| R29 | **R15** | `ch_g2.r_aa` | 220Ω ±1% |
| C89 | **C45** | `ch_g2.c_aa` | 33pF ±5% |
| R28 | **R16** | `ch_b2.r_term` | 75Ω ±1% |
| C88 | **C46** | `ch_b2.c_ac` | 100nF ±10% |
| R27 | **R17** | `ch_b2.r_aa` | 220Ω ±1% |
| C87 | **C47** | `ch_b2.c_aa` | 33pF ±5% |
| R35 | **R18** | `r_cs_term` | 75Ω ±1% |
| R36 | **R19** | `r_cs_top` | 1.5kΩ ±1% |
| R34 | **R20** | `r_cs_bot` | 1.5kΩ ±1% |
| C86 | **C48** | `c_sog2` | 100nF ±10% |
| R37 | **R21** | `r_sog2` | 220Ω ±1% |
| C55 | **C49** | `c_sog2_aa` | 33pF ±5% |
| D6 | **D2** | `esd_sync2` | — |
| U17 | **U9** | `buf_sync2` | — |
| C93 | **C50** | `c_buf2` | 100nF ±10% |
| TP1 | **TP1** | `tp_ys` | — |
| D3 | **D3** | `esd_sync` | — |
| R11 | **R22** | `r_vs` | 220Ω ±1% |
| C20 | **C51** | `c_vs` | 1nF ±10% |
| C14 | **C52** | `c_buf` | 100nF ±10% |
| R6 | **R23** | `r_filt` | 1.5kΩ ±1% |
| C15 | **C53** | `c_filt1` | 100nF ±10% |
| C16 | **C54** | `c_filt2` | 4.7nF ±10% |
| C17 | **C55** | `c_osc` | 100nF ±10% |
| — | **J5** | `svideo` | — |
| — | **R24** | `ch_y.r_term` | 75Ω ±1% |
| — | **C56** | `ch_y.c_ac` | 100nF ±10% |
| — | **R25** | `ch_y.r_aa` | 220Ω ±1% |
| — | **C57** | `ch_y.c_aa` | 33pF ±5% |
| — | **R26** | `ch_c.r_term` | 75Ω ±1% |
| — | **C58** | `ch_c.c_ac` | 100nF ±10% |
| — | **R27** | `ch_c.r_aa` | 220Ω ±1% |
| — | **C59** | `ch_c.c_aa` | 33pF ±5% |
| — | **D4** | `esd_svideo` | — |
| — | **C60** | `c_sog1` | 100nF ±10% |
| — | **R28** | `r_sog1` | 220Ω ±1% |
| — | **C61** | `c_sog1_aa` | 33pF ±5% |
| — | **C62** | `nc_bin1` | 10nF ±10% |
| — | **C63** | `nc_gin4` | 10nF ±10% |
| R7 | **R29** | `r_i2ca` | 2.2kΩ ±1% |
| R10 | **R30** | `r_sda` | 2.2kΩ ±1% |
| R9 | **R31** | `r_scl` | 2.2kΩ ±1% |
| R8 | **R32** | `r_rst` | 2.2kΩ ±1% |
| U11 | **U10** | `sodimm` | — |
| D4 | **D5** | `drgb.esd[0]` | — |
| D5 | **D6** | `drgb.esd[1]` | — |
| R21 | **R33** | `drgb.pd[0]` | 10kΩ ±1% |
| R22 | **R34** | `drgb.pd[1]` | 10kΩ ±1% |
| R23 | **R35** | `drgb.pd[2]` | 10kΩ ±1% |
| R24 | **R36** | `drgb.pd[3]` | 10kΩ ±1% |
| R25 | **R37** | `drgb.pd[4]` | 10kΩ ±1% |
| R26 | **R38** | `drgb.pd[5]` | 10kΩ ±1% |
| U14 | **U11** | `drgb.buf[0]` | — |
| U15 | **U12** | `drgb.buf[1]` | — |
| U16 | **U13** | `drgb.buf[2]` | — |
| C82 | **C64** | `drgb.dec[0]` | 100nF ±10% |
| C83 | **C65** | `drgb.dec[1]` | 100nF ±10% |
| C84 | **C66** | `drgb.dec[2]` | 100nF ±10% |
| J13 | **J6** | `j_drgb` | — |
| J4 | **J7** | `j_dbg` | — |
| LED1 | **LED1** | `led_status` | — |
| D7 | **D7** | `d_led` | — |
| C51 | **C67** | `c_led` | 100nF ±10% |
| R38 | **R39** | `r_led` | 220Ω ±1% |
| TP2 | **TP2** | `tp_led_do` | — |
| X2 | **X3** | `xo_audio` | — |
| C21 | **C68** | `c_xo_audio` | 100nF ±10% |
| FB1 | **FB5** | `fb_audio` | — |
| C13 | **C69** | `c_5va` | 10µF ±10% |
| U1 | **U14** | `adc_dsub.adc` | — |
| C1 | **C70** | `adc_dsub.c_in_l` | 1µF ±10% |
| C2 | **C71** | `adc_dsub.c_in_r` | 1µF ±10% |
| C3 | **C72** | `adc_dsub.c_vref_a` | 10µF ±10% |
| C4 | **C73** | `adc_dsub.c_vref_b` | 100nF ±10% |
| C5 | **C74** | `adc_dsub.dec[0]` | 100nF ±10% |
| C6 | **C75** | `adc_dsub.dec[1]` | 100nF ±10% |
| C75 | **C76** | `adc_dsub.bulk[0]` | 10µF ±10% |
| C76 | **C77** | `adc_dsub.bulk[1]` | 10µF ±10% |
| U2 | **U15** | `adc_aux.adc` | — |
| C9 | **C78** | `adc_aux.c_in_l` | 1µF ±10% |
| C10 | **C79** | `adc_aux.c_in_r` | 1µF ±10% |
| C11 | **C80** | `adc_aux.c_vref_a` | 10µF ±10% |
| C12 | **C81** | `adc_aux.c_vref_b` | 100nF ±10% |
| C77 | **C82** | `adc_aux.dec[0]` | 100nF ±10% |
| C78 | **C83** | `adc_aux.dec[1]` | 100nF ±10% |
| C7 | **C84** | `adc_aux.bulk[0]` | 10µF ±10% |
| C8 | **C85** | `adc_aux.bulk[1]` | 10µF ±10% |
| D1 | **D8** | `esd_audio` | — |
| J8 | **J8** | `spdif` | — |
| C19 | **C86** | `c_spdif` | 100nF ±10% |
| J5 | **J9** | `j11_argus` | — |
| H1 | **H1** | `mount[0]` | — |
| H2 | **H2** | `mount[1]` | — |
| H3 | **H3** | `mount[2]` | — |
| H4 | **H4** | `mount[3]` | — |
| H5 | **H5** | `mount[4]` | — |
| J3 | **J10** | `j_oled` | — |
| U4 | **U16** | `eeprom_mac0` | — |
| U5 | **U17** | `eeprom_mac1` | — |
| C39 | **C87** | `dec_eeprom[0]` | 100nF ±10% |
| C40 | **C88** | `dec_eeprom[1]` | 100nF ±10% |
| J2 | **J11** | `eth.jack` | — |
| J11 | **J12** | `eth.jack2` | — |
| C43 | **C89** | `eth.ct_caps[0]` | 100nF ±10% |
| C44 | **C90** | `eth.ct_caps[1]` | 100nF ±10% |
| R16 | **R40** | `eth.r_led_g` | 220Ω ±1% |
| R17 | **R41** | `eth.r_led_y` | 220Ω ±1% |
| R18 | **R42** | `eth.r_led_g2` | 220Ω ±1% |
| R19 | **R43** | `eth.r_led_y2` | 220Ω ±1% |
| C41 | **C91** | `eth.bridge_caps[0]` | 4.7nF ±10% |
| C42 | **C92** | `eth.bridge_caps[1]` | 4.7nF ±10% |
| C61 | **C93** | `eth.bridge_caps[2]` | 4.7nF ±10% |
| C62 | **C94** | `eth.bridge_caps[3]` | 4.7nF ±10% |

合計 195個。V0から番号が変わったもの 167個、変わらなかったもの 13個、V0に無かった追加部品 15個。
