// RGB21(メス)→ ミニDIN8 ケーブル 変換基板。
// 部品は2つだけで、受動の素通し配線。分圧・終端・同期分離は入れない
// (FRAMEMEISTER 付属ケーブルも素通しで、レベル合わせは RetroCastX 側でやる)。
//
// 使うのは RGB/同期/音声の入力側だけ。モニタの出力用ピン(2,6,10)と
// 切替信号(11 AV制御, 12 Ym, 16 Ys)は RetroCastX では使わないので繋がない。
// 同期は 9番(コンポジット入力)に来る。RGB21 のゲーム機ケーブルは
// ここに CSYNC(またはコンポジット映像)を出す。
module rgb21_breakout (
);
    wire R;
    wire G;
    wire B;
    wire CSYNC;
    wire AUDIO_L;
    wire AUDIO_R;
    // 信号の GND はすべてまとめる(ミニDIN8 側は 4番の1本しかない)
    wire GND;
    // シェル(21番)とケーブルのシールド。GND とは分けておき、RetroCastX 側で
    // シェルと GND が繋がる(J13 のシェル = GND)。
    wire FG;

    (* id = "rgb21_female" *)
    RGB21_Female J1 (
        .AUDIO_L_IN(AUDIO_L),
        .AUDIO_L_OUT(),
        .AUDIO_IN_GND(GND),
        .AUDIO_OUT_GND(GND),
        .AUDIO_R_IN(AUDIO_R),
        .AUDIO_R_OUT(),
        .VIDEO_IN_GND(GND),
        .VIDEO_OUT_GND(GND),
        .VIDEO_IN(CSYNC),
        .VIDEO_OUT(),
        .AV_CTRL(),
        .YM(),
        .R_GND(GND),
        .YS_YM_GND(GND),
        .R(R),
        .YS(),
        .G_GND(GND),
        .B_GND(GND),
        .G(G),
        .B(B),
        .SHELL(FG)
    );
    (* id = "minidin8_cable" *)
    MiniDin8_Cable J2 (
        .AUDIO_R(AUDIO_R),
        .AUDIO_L(AUDIO_L),
        .CSYNC(CSYNC),
        .GND(GND),
        .NC5(),
        .B(B),
        .G(G),
        .R(R)
    );
    (* id = "fg_pad" *)
    ShieldWirePad J3 (
        .SHIELD(FG)
    );
endmodule
