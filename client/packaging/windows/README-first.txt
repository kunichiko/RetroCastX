RetroCastX Viewer (Windows)
===========================

retrocastx-viewer.exe を実行するだけで使えます。インストールは不要です。
起動すると SUBSCRIBE をブロードキャストしてボードを探し、映像を受け取ります。


■ 最初に1回だけやってほしい設定(これをやらないとパケットを落とします)

NIC の「受信バッファー」(受信記述子リング)の既定値 256 は、RetroCastX の
パケットレート(31kHz 等倍で約 35,000 パケット/秒)に足りません。そのままだと
1〜6% のパケットが NIC ドライバの段階で捨てられ、映像に穴が開き音が途切れます。

管理者権限の PowerShell で(アダプタ名は環境に合わせてください):

    Get-NetAdapter | ft Name,Status,LinkSpeed
    Set-NetAdapterAdvancedProperty -Name 'イーサネット' -RegistryKeyword '*ReceiveBuffers' -RegistryValue 2048

実測(同じ機械・同じ配線):

    受信バッファー 256   : lost 1.7%    音が途切れる
    受信バッファー 2048  : lost 0.0013% 音は途切れない(316Mbps, 55.8fps)

うまくいっているかは Viewer 右側の Stats の "lost" で分かります。増え続けるなら
次を見てください。errors がほぼ 0 なのに discards だけ増えるなら、上の設定です。

    Get-NetAdapterStatistics -Name 'イーサネット' | fl ReceivedDiscardedPackets,ReceivedPacketErrors

「省電力イーサネット」を切っておくと、長時間動かしたときのバースト落ちも防げます。


■ 初回起動時の許可ダイアログ

Windows は新しい実行ファイルに対して「ローカルネットワーク通信を許可しますか」
「ファイアウォールの許可」を尋ねます。許可するまでボードからの映像は届きません
(数秒間まったく反応が無いように見えます)。許可は実行ファイル単位なので、
exe を置き換えたり別のフォルダへ移したりすると再び尋ねられます。

このアプリは署名していないため、SmartScreen が警告を出すことがあります。
「詳細情報」→「実行」で起動できます。


■ 有線接続で使ってください

Wi-Fi と有線が同時に有効だと、探索のブロードキャストが Wi-Fi 側から出てボードに
届かないことがあります(映像が出ない/出るまで待たされる)。

詳細は docs/design-notes.md(リポジトリ)にあります。
