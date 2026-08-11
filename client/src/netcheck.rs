//! Windows の NIC 受信バッファー(`*ReceiveBuffers`)が小さすぎないか調べる。
//!
//! **これが既定の 256 のままだとパケットを1〜6%落とし、映像に穴が開いて音が
//! 途切れる。** 実測(client/README.md): 256 → lost 1.7% / 2048 → lost 0.0013%。
//! 捨てているのはソケットバッファではなく**その手前のNICドライバのリング**なので、
//! `SO_RCVBUF` をいくら増やしても効かない。
//!
//! README-first.txt に書いてはあるが読まれないので、アプリから気付かせる。
//! **読むだけなので管理者権限は要らない**(直すには要るので、コマンドを見せて
//! コピーさせる)。
//!
//! ## 仕組み
//!
//! `Set-NetAdapterAdvancedProperty -RegistryKeyword '*ReceiveBuffers'` は名前どおり
//! レジストリに書くだけなので、同じ場所を読む:
//!
//! 1. ボードの IP から `GetBestInterfaceEx` で経路上のインターフェース番号を得る
//!    (「実際に受信に使われる NIC」を知りたいので、全アダプタを見てはいけない。
//!     Wi-Fi と有線が両方生きている機械で誤判定する)
//! 2. index → LUID → GUID(= `NetCfgInstanceId`)に変換する
//! 3. ネットワークアダプタのクラスキー配下を列挙し、`NetCfgInstanceId` が一致する
//!    ものの `*ReceiveBuffers` を読む
//!
//! 実機(Windows 11 26200 / Intel I219-LM)で確かめたこと:
//!   - 値は **REG_SZ の文字列**("2048")で入る。REG_DWORD 決め打ちでは読めない
//!   - クラスキーの配下には `0000` のような番号のほかに **`Properties` が混ざり、
//!     これは管理者でも開けない**。開けないサブキーは飛ばす必要がある
//!   - `0000` の ACL は `BUILTIN\Users: ReadKey` なので**読むだけなら管理者不要**
//!
//! ## 無いことを「小さい」と扱わない
//!
//! `*ReceiveBuffers` は NDIS の advanced property で、**ドライバが公開していなければ
//! 存在しない**(Realtek や Wi-Fi では無いことがある)。無い場合は `Unsupported` を
//! 返して黙る。ここを間違えると、問題の無い機械で嘘の警告を出すことになる。

/// 推奨値。実測で lost が 1.7% → 0.0013% になった値
pub const RECOMMENDED: u32 = 2048;

/// これ未満なら警告する。
///
/// 実測が取れているのは 256(悪)と 2048(良)の2点だけなので、間を断定しない
/// ように緩めにしてある。既定の 256 は確実に引っかかる。
pub const WARN_BELOW: u32 = 1024;

/// 受信バッファーの設定を調べた結果
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Buffers {
    /// 読めた。`adapter` は表示用の名前(「イーサネット」など)
    Known { adapter: String, value: u32 },
    /// このアダプタは `*ReceiveBuffers` を公開していない。
    /// **「小さい」ではない**ので警告しない
    Unsupported,
    /// 調べられなかった(Windows以外、経路が引けない、レジストリが読めない等)
    Unknown,
}

impl Buffers {
    /// 警告を出すべきか。読めて、かつ小さいときだけ true
    pub fn should_warn(&self) -> bool {
        matches!(self, Self::Known { value, .. } if *value < WARN_BELOW)
    }

    /// 表示用のアダプタ名(不明なら None)
    pub fn adapter(&self) -> Option<&str> {
        match self {
            Self::Known { adapter, .. } => Some(adapter.as_str()),
            _ => None,
        }
    }

    pub fn value(&self) -> Option<u32> {
        match self {
            Self::Known { value, .. } => Some(*value),
            _ => None,
        }
    }
}

/// 直し方。警告と一緒に出してコピーさせる
pub fn fix_command(adapter: Option<&str>) -> String {
    let name = adapter.unwrap_or("イーサネット");
    format!(
        "Set-NetAdapterAdvancedProperty -Name '{name}' \
         -RegistryKeyword '*ReceiveBuffers' -RegistryValue {RECOMMENDED}"
    )
}

/// `board` (ボードのIPv4文字列) への経路上にある NIC の受信バッファー設定を返す。
pub fn probe(board: &str) -> Buffers {
    match board.parse::<std::net::Ipv4Addr>() {
        Ok(ip) => imp::probe(ip),
        Err(_) => Buffers::Unknown,
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::Buffers;

    pub fn probe(_ip: std::net::Ipv4Addr) -> Buffers {
        // 受信バッファーの取りこぼしは Windows 固有の問題。macOS/Linux では
        // 既定のリングで足りている(実測でロス0)
        Buffers::Unknown
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::net::Ipv4Addr;

    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceIndexToLuid, ConvertInterfaceLuidToAlias, ConvertInterfaceLuidToGuid,
        GetBestInterfaceEx,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE,
        KEY_READ,
    };

    use super::Buffers;

    /// ネットワークアダプタのクラスキー。各アダプタが 0000, 0001, ... で並ぶ。
    /// `Set-NetAdapterAdvancedProperty -RegistryKeyword` が書くのもここ。
    const NET_CLASS_KEY: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";

    pub fn probe(ip: Ipv4Addr) -> Buffers {
        let Some(index) = best_interface(ip) else { return Buffers::Unknown };
        let Some((guid, alias)) = interface_identity(index) else { return Buffers::Unknown };
        match receive_buffers(&guid) {
            Some(Some(value)) => Buffers::Known { adapter: alias, value },
            // キーは見つかったが *ReceiveBuffers が無い = ドライバが公開していない
            Some(None) => Buffers::Unsupported,
            None => Buffers::Unknown,
        }
    }

    /// ボードへの経路が使うインターフェース番号。
    /// 「全アダプタを見る」のではなく経路で選ぶのが要点(Wi-Fiと有線が両方
    /// 生きている機械で誤判定しないため)。
    fn best_interface(ip: Ipv4Addr) -> Option<u32> {
        let mut sa: SOCKADDR_IN = unsafe { std::mem::zeroed() };
        sa.sin_family = AF_INET;
        sa.sin_addr.S_un.S_addr = u32::from_ne_bytes(ip.octets());
        let mut index: u32 = 0;
        let rc = unsafe {
            GetBestInterfaceEx(&sa as *const _ as *const _, &mut index)
        };
        (rc == NO_ERROR).then_some(index)
    }

    /// index → (NetCfgInstanceId の GUID 文字列, 表示名)
    fn interface_identity(index: u32) -> Option<(String, String)> {
        let mut luid = unsafe { std::mem::zeroed() };
        if unsafe { ConvertInterfaceIndexToLuid(index, &mut luid) } != NO_ERROR {
            return None;
        }
        let mut guid = unsafe { std::mem::zeroed() };
        if unsafe { ConvertInterfaceLuidToGuid(&luid, &mut guid) } != NO_ERROR {
            return None;
        }
        // 表示名(「イーサネット」など)。直し方のコマンドに埋めるので要る
        let mut alias = [0u16; 260];
        let alias = if unsafe {
            ConvertInterfaceLuidToAlias(&luid, alias.as_mut_ptr(), alias.len())
        } == NO_ERROR
        {
            from_wide(&alias)
        } else {
            String::new()
        };
        Some((format_guid(&guid), alias))
    }

    /// レジストリの GUID 表記(中括弧つき大文字)にする。`NetCfgInstanceId` と
    /// 文字列比較するので、書式を合わせる必要がある(比較は大文字小文字を無視する)
    fn format_guid(g: &windows_sys::core::GUID) -> String {
        format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            g.data1, g.data2, g.data3,
            g.data4[0], g.data4[1], g.data4[2], g.data4[3],
            g.data4[4], g.data4[5], g.data4[6], g.data4[7],
        )
    }

    /// クラスキーを列挙して `NetCfgInstanceId` が一致するものを探す。
    ///
    /// 戻り値: `None` = キーが見つからない/読めない、`Some(None)` = キーはあるが
    /// `*ReceiveBuffers` が無い(ドライバが公開していない)、`Some(Some(v))` = 値。
    fn receive_buffers(guid: &str) -> Option<Option<u32>> {
        let class = open_key(HKEY_LOCAL_MACHINE, NET_CLASS_KEY)?;
        let mut found = None;
        for name in subkeys(class) {
            // 配下には番号のキー(0000, 0001, ...)のほかに `Properties` が混ざる。
            // これは管理者でも開けないので、開けなかったものは黙って飛ばす
            // パスは class キーからの相対で渡す(HKLM からの絶対パスを渡すと
            // class の下をもう一段掘りに行って必ず外す)
            let Some(sub) = open_key(class, &name) else {
                continue;
            };
            let id = read_string(sub, "NetCfgInstanceId");
            let hit = id.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(guid));
            if hit {
                // 値は REG_SZ で入ることが多いが、ドライバによっては REG_DWORD。
                // どちらでも読めるようにする
                found = Some(read_u32_or_string(sub, "*ReceiveBuffers"));
            }
            unsafe { RegCloseKey(sub) };
            if hit {
                break;
            }
        }
        unsafe { RegCloseKey(class) };
        found
    }

    fn open_key(root: HKEY, path: &str) -> Option<HKEY> {
        let wide = to_wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        let rc = unsafe {
            RegOpenKeyExW(root, wide.as_ptr(), 0, KEY_READ, &mut key)
        };
        (rc == ERROR_SUCCESS).then_some(key)
    }

    fn subkeys(key: HKEY) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            let rc = unsafe {
                RegEnumKeyExW(
                    key, i, buf.as_mut_ptr(), &mut len,
                    std::ptr::null_mut(), std::ptr::null_mut(),
                    std::ptr::null_mut(), std::ptr::null_mut(),
                )
            };
            if rc != ERROR_SUCCESS {
                break;
            }
            out.push(from_wide(&buf[..len as usize]));
        }
        out
    }

    fn query(key: HKEY, name: &str) -> Option<(u32, Vec<u8>)> {
        let wide = to_wide(name);
        let mut ty: u32 = 0;
        let mut len: u32 = 0;
        let rc = unsafe {
            RegQueryValueExW(key, wide.as_ptr(), std::ptr::null(), &mut ty,
                             std::ptr::null_mut(), &mut len)
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let rc = unsafe {
            RegQueryValueExW(key, wide.as_ptr(), std::ptr::null(), &mut ty,
                             buf.as_mut_ptr(), &mut len)
        };
        (rc == ERROR_SUCCESS).then(|| {
            buf.truncate(len as usize);
            (ty, buf)
        })
    }

    fn read_string(key: HKEY, name: &str) -> Option<String> {
        let (_, buf) = query(key, name)?;
        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Some(from_wide(&wide))
    }

    /// REG_SZ でも REG_DWORD でも数値として読む
    fn read_u32_or_string(key: HKEY, name: &str) -> Option<u32> {
        let (ty, buf) = query(key, name)?;
        const REG_DWORD: u32 = 4;
        if ty == REG_DWORD && buf.len() >= 4 {
            return Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]));
        }
        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        from_wide(&wide).trim().parse().ok()
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// NUL 止め(または末尾まで)の UTF-16 を String にする
    fn from_wide(w: &[u16]) -> String {
        let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「公開していない」を「小さい」と扱わないこと。ここを間違えると
    /// 問題の無い機械で嘘の警告を出す
    #[test]
    fn unsupported_and_unknown_never_warn() {
        assert!(!Buffers::Unsupported.should_warn());
        assert!(!Buffers::Unknown.should_warn());
    }

    #[test]
    fn warns_only_below_threshold() {
        let at = |value| Buffers::Known { adapter: "イーサネット".into(), value };
        assert!(at(256).should_warn(), "既定の256は警告する");
        assert!(!at(WARN_BELOW).should_warn(), "閾値ちょうどは警告しない");
        assert!(!at(RECOMMENDED).should_warn());
    }

    /// 直し方のコマンドにアダプタ名が入ること(名前が違うとそのまま貼れない)
    #[test]
    fn fix_command_uses_adapter_name() {
        let cmd = fix_command(Some("イーサネット 2"));
        assert!(cmd.contains("-Name 'イーサネット 2'"), "{cmd}");
        assert!(cmd.contains("*ReceiveBuffers"), "{cmd}");
        assert!(cmd.contains("2048"), "{cmd}");
        // 名前が取れなかったときも貼れる形にしておく
        assert!(fix_command(None).contains("-Name 'イーサネット'"));
    }
}
