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
| U2 | **U14** | `adc_aux.adc` | — |
| C7 | **C75** | `adc_aux.bulk[0]` | 10µF ±10% |
| C8 | **C76** | `adc_aux.bulk[1]` | 10µF ±10% |
| C9 | **C69** | `adc_aux.c_in_l` | 1µF ±10% |
| C10 | **C70** | `adc_aux.c_in_r` | 1µF ±10% |
| C11 | **C71** | `adc_aux.c_vref_a` | 10µF ±10% |
| C12 | **C72** | `adc_aux.c_vref_b` | 100nF ±10% |
| C77 | **C73** | `adc_aux.dec[0]` | 100nF ±10% |
| C78 | **C74** | `adc_aux.dec[1]` | 100nF ±10% |
| U1 | **U13** | `adc_dsub.adc` | — |
| C75 | **C67** | `adc_dsub.bulk[0]` | 10µF ±10% |
| C76 | **C68** | `adc_dsub.bulk[1]` | 10µF ±10% |
| C1 | **C61** | `adc_dsub.c_in_l` | 1µF ±10% |
| C2 | **C62** | `adc_dsub.c_in_r` | 1µF ±10% |
| C3 | **C63** | `adc_dsub.c_vref_a` | 10µF ±10% |
| C4 | **C64** | `adc_dsub.c_vref_b` | 100nF ±10% |
| C5 | **C65** | `adc_dsub.dec[0]` | 100nF ±10% |
| C6 | **C66** | `adc_dsub.dec[1]` | 100nF ±10% |
| J7 | **J4** | `aux` | — |
| U3 | **U2** | `buf_sync` | — |
| U17 | **U8** | `buf_sync2` | — |
| C13 | **C60** | `c_5va` | 10µF ±10% |
| C14 | **C43** | `c_buf` | 100nF ±10% |
| C93 | **C41** | `c_buf2` | 100nF ±10% |
| C15 | **C44** | `c_filt1` | 100nF ±10% |
| C16 | **C45** | `c_filt2` | 4.7nF ±10% |
| C51 | **C58** | `c_led` | 100nF ±10% |
| C17 | **C46** | `c_osc` | 100nF ±10% |
|  | **C51** | `c_sog1` | 100nF ±10% |
|  | **C52** | `c_sog1_aa` | 33pF ±5% |
| C86 | **C39** | `c_sog2` | 100nF ±10% |
| C55 | **C40** | `c_sog2_aa` | 33pF ±5% |
| C18 | **C31** | `c_sog3` | 100nF ±10% |
| C85 | **C32** | `c_sog3_aa` | 33pF ±5% |
| C19 | **C77** | `c_spdif` | 100nF ±10% |
| C20 | **C42** | `c_vs` | 1nF ±10% |
| C21 | **C59** | `c_xo_audio` | 100nF ±10% |
| C79 | **C30** | `ch_b.c_aa` | 33pF ±5% |
| C22 | **C29** | `ch_b.c_ac` | 100nF ±10% |
| R12 | **R9** | `ch_b.r_aa` | 220Ω ±1% |
| R1 | **R8** | `ch_b.r_term` | 75Ω ±1% |
| C87 | **C38** | `ch_b2.c_aa` | 33pF ±5% |
| C88 | **C37** | `ch_b2.c_ac` | 100nF ±10% |
| R27 | **R16** | `ch_b2.r_aa` | 220Ω ±1% |
| R28 | **R15** | `ch_b2.r_term` | 75Ω ±1% |
|  | **C50** | `ch_c.c_aa` | 33pF ±5% |
|  | **C49** | `ch_c.c_ac` | 100nF ±10% |
|  | **R26** | `ch_c.r_aa` | 220Ω ±1% |
|  | **R25** | `ch_c.r_term` | 75Ω ±1% |
| C80 | **C28** | `ch_g.c_aa` | 33pF ±5% |
| C23 | **C27** | `ch_g.c_ac` | 100nF ±10% |
| R13 | **R7** | `ch_g.r_aa` | 220Ω ±1% |
| R2 | **R6** | `ch_g.r_term` | 75Ω ±1% |
| C89 | **C36** | `ch_g2.c_aa` | 33pF ±5% |
| C90 | **C35** | `ch_g2.c_ac` | 100nF ±10% |
| R29 | **R14** | `ch_g2.r_aa` | 220Ω ±1% |
| R30 | **R13** | `ch_g2.r_term` | 75Ω ±1% |
| C81 | **C26** | `ch_r.c_aa` | 33pF ±5% |
| C24 | **C25** | `ch_r.c_ac` | 100nF ±10% |
| R20 | **R5** | `ch_r.r_aa` | 220Ω ±1% |
| R3 | **R4** | `ch_r.r_term` | 75Ω ±1% |
| C91 | **C34** | `ch_r2.c_aa` | 33pF ±5% |
| C92 | **C33** | `ch_r2.c_ac` | 100nF ±10% |
| R32 | **R12** | `ch_r2.r_aa` | 220Ω ±1% |
| R33 | **R11** | `ch_r2.r_term` | 75Ω ±1% |
|  | **C48** | `ch_y.c_aa` | 33pF ±5% |
|  | **C47** | `ch_y.c_ac` | 100nF ±10% |
|  | **R24** | `ch_y.r_aa` | 220Ω ±1% |
|  | **R23** | `ch_y.r_term` | 75Ω ±1% |
| D7 | **D7** | `d_led` | — |
| C25 | **C11** | `dec_caps[0]` | 100nF ±10% |
| C35 | **C21** | `dec_caps[10]` | 100nF ±10% |
| C36 | **C22** | `dec_caps[11]` | 100nF ±10% |
| C37 | **C23** | `dec_caps[12]` | 100nF ±10% |
| C38 | **C24** | `dec_caps[13]` | 100nF ±10% |
| C26 | **C12** | `dec_caps[1]` | 100nF ±10% |
| C27 | **C13** | `dec_caps[2]` | 100nF ±10% |
| C28 | **C14** | `dec_caps[3]` | 100nF ±10% |
| C29 | **C15** | `dec_caps[4]` | 100nF ±10% |
| C30 | **C16** | `dec_caps[5]` | 100nF ±10% |
| C31 | **C17** | `dec_caps[6]` | 100nF ±10% |
| C32 | **C18** | `dec_caps[7]` | 100nF ±10% |
| C33 | **C19** | `dec_caps[8]` | 100nF ±10% |
| C34 | **C20** | `dec_caps[9]` | 100nF ±10% |
| C39 | **C78** | `dec_eeprom[0]` | 100nF ±10% |
| C40 | **C79** | `dec_eeprom[1]` | 100nF ±10% |
| U14 | **U10** | `drgb.buf[0]` | — |
| U15 | **U11** | `drgb.buf[1]` | — |
| U16 | **U12** | `drgb.buf[2]` | — |
| C82 | **C55** | `drgb.dec[0]` | 100nF ±10% |
| C83 | **C56** | `drgb.dec[1]` | 100nF ±10% |
| C84 | **C57** | `drgb.dec[2]` | 100nF ±10% |
| D4 | **D5** | `drgb.esd[0]` | — |
| D5 | **D6** | `drgb.esd[1]` | — |
| R21 | **R32** | `drgb.pd[0]` | 10kΩ ±1% |
| R22 | **R33** | `drgb.pd[1]` | 10kΩ ±1% |
| R23 | **R34** | `drgb.pd[2]` | 10kΩ ±1% |
| R24 | **R35** | `drgb.pd[3]` | 10kΩ ±1% |
| R25 | **R36** | `drgb.pd[4]` | 10kΩ ±1% |
| R26 | **R37** | `drgb.pd[5]` | 10kΩ ±1% |
| U4 | **U15** | `eeprom_mac0` | — |
| U5 | **U16** | `eeprom_mac1` | — |
| D1 | **D8** | `esd_audio` | — |
| D2 | **D1** | `esd_rgb` | — |
|  | **D4** | `esd_svideo` | — |
| D3 | **D3** | `esd_sync` | — |
| D6 | **D2** | `esd_sync2` | — |
|  | **D9** | `esd_usb` | — |
| C41 | **C82** | `eth.bridge_caps[0]` | 4.7nF ±10% |
| C42 | **C83** | `eth.bridge_caps[1]` | 4.7nF ±10% |
| C61 | **C84** | `eth.bridge_caps[2]` | 4.7nF ±10% |
| C62 | **C85** | `eth.bridge_caps[3]` | 4.7nF ±10% |
| C43 | **C80** | `eth.ct_caps[0]` | 100nF ±10% |
| C44 | **C81** | `eth.ct_caps[1]` | 100nF ±10% |
| J2 | **J11** | `eth.jack` | — |
| J11 | **J12** | `eth.jack2` | — |
| R16 | **R43** | `eth.r_led_g` | 220Ω ±1% |
| R18 | **R45** | `eth.r_led_g2` | 220Ω ±1% |
| R17 | **R44** | `eth.r_led_y` | 220Ω ±1% |
| R19 | **R46** | `eth.r_led_y2` | 220Ω ±1% |
| FB1 | **FB3** | `fb_audio` | — |
| FB2 | **FB1** | `fb_avdd` | — |
| FB3 | **FB2** | `fb_pll` | — |
|  | **FID1** | `fid[0]` | — |
|  | **FID2** | `fid[1]` | — |
|  | **FID3** | `fid[2]` | — |
| C46 | **C9** | `ft2232.c_osci` | 22pF ±5% |
| C63 | **C10** | `ft2232.c_osco` | 22pF ±5% |
|  | **C6** | `ft2232.c_vcc` | 100nF ±10% |
|  | **C7** | `ft2232.c_vcc_bulk` | 10µF ±10% |
|  | **C8** | `ft2232.c_vio` | 100nF ±10% |
|  | **U7** | `ft2232.ch` | — |
| R15 | **R3** | `ft2232.r_reset` | 10kΩ ±1% |
| X3 | **X2** | `ft2232.xtal` | — |
| J5 | **J9** | `j11_argus` | — |
| J4 | **J7** | `j_dbg` | — |
| J13 | **J6** | `j_drgb` | — |
| J3 | **J10** | `j_oled` | — |
| J1 | **J3** | `jtag_hdr` | — |
| LED1 | **LED1** | `led_status` | — |
| H1 | **H1** | `mount[0]` | — |
| H2 | **H2** | `mount[1]` | — |
| H3 | **H3** | `mount[2]` | — |
| H4 | **H4** | `mount[3]` | — |
| H5 | **H5** | `mount[4]` | — |
|  | **C53** | `nc_bin1` | 10nF ±10% |
|  | **C54** | `nc_gin4` | 10nF ±10% |
| X1 | **X1** | `osc` | — |
| PG1 | **PG1** | `pogo_tck` | — |
| PG2 | **PG3** | `pogo_tdi` | — |
| PG3 | **PG4** | `pogo_tdo` | — |
| PG4 | **PG2** | `pogo_tms` | — |
| R4 | **R1** | `r_cc1` | 5.1kΩ ±1% |
| R5 | **R2** | `r_cc2` | 5.1kΩ ±1% |
| R34 | **R19** | `r_cs_bot` | 1.5kΩ ±1% |
| R35 | **R17** | `r_cs_term` | 75Ω ±1% |
| R36 | **R18** | `r_cs_top` | 1.5kΩ ±1% |
| R6 | **R22** | `r_filt` | 1.5kΩ ±1% |
| R7 | **R28** | `r_i2ca` | 2.2kΩ ±1% |
| R38 | **R38** | `r_led` | 220Ω ±1% |
|  | **R39** | `r_mix_aux_l` | 1.5kΩ ±1% |
|  | **R40** | `r_mix_aux_r` | 1.5kΩ ±1% |
|  | **R41** | `r_mix_sv_l` | 1.5kΩ ±1% |
|  | **R42** | `r_mix_sv_r` | 1.5kΩ ±1% |
| R8 | **R31** | `r_rst` | 2.2kΩ ±1% |
| R9 | **R30** | `r_scl` | 2.2kΩ ±1% |
| R10 | **R29** | `r_sda` | 2.2kΩ ±1% |
|  | **R27** | `r_sog1` | 220Ω ±1% |
| R37 | **R20** | `r_sog2` | 220Ω ±1% |
| R31 | **R10** | `r_sog3` | 220Ω ±1% |
| R11 | **R21** | `r_vs` | 220Ω ±1% |
| JP3 | **JP1** | `sj_ext5v` | — |
| U11 | **U9** | `sodimm` | — |
| J8 | **J8** | `spdif` | — |
| C56 | **C1** | `supply.bulk_caps[0]` | 10µF ±10% |
| C57 | **C2** | `supply.bulk_caps[1]` | 10µF ±10% |
| C58 | **C3** | `supply.bulk_caps[2]` | 10µF ±10% |
| C59 | **C4** | `supply.bulk_caps[3]` | 10µF ±10% |
| C60 | **C5** | `supply.bulk_caps[4]` | 10µF ±10% |
| U6 | **U5** | `supply.ldo_a19` | — |
| U7 | **U4** | `supply.ldo_a33` | — |
| U8 | **U6** | `supply.ldo_d19` | — |
| U9 | **U3** | `supply.ldo_io` | — |
|  | **J5** | `svideo` | — |
| TP2 | **TP2** | `tp_led_do` | — |
| TP1 | **TP1** | `tp_ys` | — |
| U10 | **U1** | `tvp` | — |
| J9 | **J2** | `usbc` | — |
| J10 | **J1** | `vga` | — |
| X2 | **X3** | `xo_audio` | — |

合計 195個。V0から番号が変わったもの 167個、変わらなかったもの 13個、V0に無かった追加部品 15個。
