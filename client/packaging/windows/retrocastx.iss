; ===========================================================================
; RetroCast X Windows installer (Inno Setup 6)
; ---------------------------------------------------------------------------
; zip 版と中身は同じ(RetroCastX.exe + README-first.txt)。**固定パスへ入る**
; ことが唯一にして最大の違いで、そこに意味がある:
;
;   Windows のファイアウォール/ローカルネットワークの許可は**実行ファイルの
;   パス単位**なので、zip を展開する場所を変えたり exe を置き換えたりすると
;   そのたびに許可を聞かれ、許可するまで映像が来ない(数秒間まったく反応が
;   無いように見える)。インストーラなら次回以降その入れ替わりが起きない。
;
; ローカルビルド(client/ から):
;   cargo build --release
;   mkdir ..\dist\RetroCastX
;   copy target\release\retrocastx-viewer.exe ..\dist\RetroCastX\RetroCastX.exe
;   copy packaging\windows\README-first.txt   ..\dist\RetroCastX\
;   "%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe" /DMyAppVersion=0.1.0 ^
;     packaging\windows\retrocastx.iss
;
; CI:
;   .github/workflows/viewer-release.yml の Windows ジョブが、zip 用に組んだ
;   ステージング(dist\RetroCastX)をそのまま入力にして ISCC を実行する。
;   **zip とインストーラで中身が食い違わない**ようにするため、materials は
;   1か所で作って両方から使う。
;
; 注意:
;   - AppId は一度決めたら変更しない(アップグレード判定とアンインストールの
;     識別が壊れる)。MimicX とは別の GUID を使うこと。
;   - 無署名なので SmartScreen の警告は出る。回避には EV 証明書か
;     Azure Trusted Signing が要る。README-first.txt に案内がある。
; ===========================================================================

#define MyAppName       "RetroCast X"
#define MyAppPublisher  "Kunihiko Ohnaka"
#define MyAppURL        "https://github.com/kunichiko/RetroCastX"
#define MyAppExeName    "RetroCastX.exe"
#define IconFile        "AppIcon.ico"

; CI から /DMyAppVersion=1.2.3 で渡す。ローカル単発ビルド用に既定値も置く。
#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

; 入れるファイルの置き場所。.iss からの相対
; (packaging\windows → client → リポジトリルート → dist\RetroCastX)。
; CI は絶対パスを /DStageDir=... で渡す。
#ifndef StageDir
  #define StageDir "..\..\..\dist\RetroCastX"
#endif

#ifndef OutDir
  #define OutDir "..\..\..\dist"
#endif

; VersionInfoVersion は X.X.X.X 形式の純粋な数値しか受け付けないので、
; pre-release suffix(例 "0.2.0-rc1")があれば落として "0.2.0" にする。
; AppVersion / OutputBaseFilename はハイフン入りをそのまま使ってよい。
#if Pos("-", MyAppVersion) > 0
  #define MyAppVersionNumeric Copy(MyAppVersion, 1, Pos("-", MyAppVersion) - 1)
#else
  #define MyAppVersionNumeric MyAppVersion
#endif

[Setup]
AppId={{3D35BD9C-FC37-431F-822D-8D1DE81440F5}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
VersionInfoVersion={#MyAppVersionNumeric}
; 管理者権限を要求せず Per-user / Per-machine をユーザーに選ばせる。
; admin なら {autopf} = Program Files、非 admin なら LocalAppData\Programs。
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutDir}
OutputBaseFilename=RetroCastX_windows-setup-{#MyAppVersion}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\{#MyAppExeName}
; 起動中なら自動的に閉じてから上書きインストールする。
; ファイアウォールの許可を保つため、パスは変えない。
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
    GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; zip と同じステージングをそのまま入れる(RetroCastX.exe と README-first.txt)。
Source: "{#StageDir}\*"; DestDir: "{app}"; \
    Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; \
    Tasks: desktopicon

[Run]
; NIC の受信バッファーの設定が要るので、最初に README を読ませる。
; これをやらないとパケットを1〜6%落として映像に穴が開く。
Filename: "{app}\README-first.txt"; \
    Description: "最初に読む注意書きを開く (NIC の設定が要ります)"; \
    Flags: shellexec postinstall skipifsilent
Filename: "{app}\{#MyAppExeName}"; \
    Description: "{cm:LaunchProgram,{#MyAppName}}"; \
    Flags: nowait postinstall skipifsilent
