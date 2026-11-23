// RGB21 → ミニDIN8 変換基板の部品(リーフモジュール)。
// フットプリントは rgb21_breakout.pretty(tools/gen_footprints.py が生成)。
// RGB21(JP21)メス。ケーブル側コネクタの半田端子で基板の端を挟む。
// 信号名はモニタ(受け側)から見た名前。ゲーム機の RGB21 ケーブル(オス)を
// 挿すので、ゲーム機の出力は「入力」のピンに来る。この基板から見ると信号が
// 出てくるピンなので、使う入力ピンの etype は output にしてある。
// 出典: ja.wikipedia「RGB21ピン」。retrogamingcables の JP21 表と R/G/B/同期/音声が一致。
(* footprint = "rgb21_breakout:RGB21_CardEdge", prefix = "J" *)
module RGB21_Female (
    (* pad = "1", etype = "output" *) inout AUDIO_L_IN,
    (* pad = "2", etype = "passive" *) inout AUDIO_L_OUT,
    (* pad = "3", etype = "passive" *) inout AUDIO_IN_GND,
    (* pad = "4", etype = "passive" *) inout AUDIO_OUT_GND,
    (* pad = "5", etype = "output" *) inout AUDIO_R_IN,
    (* pad = "6", etype = "passive" *) inout AUDIO_R_OUT,
    (* pad = "7", etype = "passive" *) inout VIDEO_IN_GND,
    (* pad = "8", etype = "passive" *) inout VIDEO_OUT_GND,
    (* pad = "9", etype = "output" *) inout VIDEO_IN,
    (* pad = "10", etype = "passive" *) inout VIDEO_OUT,
    (* pad = "11", etype = "passive" *) inout AV_CTRL,
    (* pad = "12", etype = "passive" *) inout YM,
    (* pad = "13", etype = "passive" *) inout R_GND,
    (* pad = "14", etype = "passive" *) inout YS_YM_GND,
    (* pad = "15", etype = "output" *) inout R,
    (* pad = "16", etype = "passive" *) inout YS,
    (* pad = "17", etype = "passive" *) inout G_GND,
    (* pad = "18", etype = "passive" *) inout B_GND,
    (* pad = "19", etype = "output" *) inout G,
    (* pad = "20", etype = "output" *) inout B,
    (* pad = "21", etype = "passive" *) inout SHELL
);
endmodule

// ミニDIN8 ケーブル(オス — RetroCastX の J13 に挿す)を切った側の芯線8本。
// ピン配置は FRAMEMEISTER の RGB IN と同じ(hardware/adc-frontend の MiniDin8Rgb):
//   1=音声R 2=音声L 3=CSYNC 4=GND 5=未使用 6=B 7=G 8=R(シールドは ShieldWirePad)
// 5番は使わないのでどこにも繋がないが、芯線の端を固定する場所としてパッドはある。
// 信号は RetroCastX へ入るので input。GND の基準は RetroCastX 側に
// あるので power_out にしてある(kinema の undriven-net 検査のため)。
(* footprint = "rgb21_breakout:MiniDin8_WirePads", prefix = "J" *)
module MiniDin8_Cable (
    (* pad = "1", etype = "input" *) inout AUDIO_R,
    (* pad = "2", etype = "input" *) inout AUDIO_L,
    (* pad = "3", etype = "input" *) inout CSYNC,
    (* pad = "4", etype = "power_out" *) inout GND,
    (* pad = "5", etype = "no_connect" *) inout NC5,
    (* pad = "6", etype = "input" *) inout B,
    (* pad = "7", etype = "input" *) inout G,
    (* pad = "8", etype = "input" *) inout R
);
endmodule

// ミニDIN8 ケーブルのシールド(編組)を付けるパッド。芯線のパッドとは別部品にして
// 離して置けるようにした。基準は RetroCastX 側(J13 のシェル)なので power_out。
(* footprint = "rgb21_breakout:FG_WirePad", prefix = "J" *)
module ShieldWirePad (
    (* pad = "1", etype = "power_out" *) inout SHIELD
);
endmodule
