//! The `registry` Run PATH source on Windows (#26 D4).

use std::ffi::OsString;

/// Joins the machine and user `Path` values, machine first, as Explorer does (#26 D4). Either
/// may be absent or empty.
pub fn join_registry_paths(machine: Option<&str>, user: Option<&str>) -> String {
    [machine, user]
        .into_iter()
        .flatten()
        .map(|p| p.trim_matches(';'))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(";")
}

/// `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment\Path`, then
/// `HKCU\Environment\Path`, each `REG_EXPAND_SZ` expanded with `ExpandEnvironmentStringsW`,
/// joined machine first (#26 D4). A missing value counts as empty.
#[cfg(windows)]
pub fn registry_path() -> std::io::Result<OsString> {
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let machine = win::read_path_value(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    )?;
    let user = win::read_path_value(HKEY_CURRENT_USER, "Environment")?;
    let machine = machine.map(|w| String::from_utf16_lossy(&w));
    let user = user.map(|w| String::from_utf16_lossy(&w));
    let joined = join_registry_paths(machine.as_deref(), user.as_deref());
    Ok(OsString::from_wide(
        &joined.encode_utf16().collect::<Vec<_>>(),
    ))
}

/// There is no registry off Windows; `registry` is an invalid source there (#26 D1).
#[cfg(not(windows))]
pub fn registry_path() -> std::io::Result<OsString> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows_sys::Win32::System::Registry::{
        HKEY, REG_EXPAND_SZ, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The `Path` value under `subkey`, expanded, without its NUL; `None` when it is absent.
    pub(super) fn read_path_value(root: HKEY, subkey: &str) -> std::io::Result<Option<Vec<u16>>> {
        let subkey = wide(subkey);
        let name = wide("Path");
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut buf: Vec<u16> = vec![0; 2048];
        loop {
            let mut kind = 0u32;
            let mut bytes = u32::try_from(buf.len() * 2).unwrap_or(u32::MAX);
            // SAFETY: the key and value names are NUL-terminated UTF-16, `buf` holds `bytes`
            // bytes, and the out-pointers are valid for the call.
            let status = unsafe {
                RegGetValueW(
                    root,
                    subkey.as_ptr(),
                    name.as_ptr(),
                    flags,
                    &mut kind,
                    buf.as_mut_ptr().cast(),
                    &mut bytes,
                )
            };
            match status {
                ERROR_SUCCESS => {
                    buf.truncate(bytes as usize / 2);
                    while buf.last() == Some(&0) {
                        buf.pop();
                    }
                    return if kind == REG_EXPAND_SZ {
                        expand(&buf).map(Some)
                    } else {
                        Ok(Some(buf))
                    };
                }
                ERROR_MORE_DATA => buf.resize((bytes as usize).div_ceil(2) + 1, 0),
                ERROR_FILE_NOT_FOUND => return Ok(None),
                other => return Err(std::io::Error::from_raw_os_error(other as i32)),
            }
        }
    }

    fn expand(value: &[u16]) -> std::io::Result<Vec<u16>> {
        let src: Vec<u16> = value.iter().copied().chain(std::iter::once(0)).collect();
        let mut out: Vec<u16> = vec![0; src.len().max(256)];
        loop {
            let size = u32::try_from(out.len()).unwrap_or(u32::MAX);
            // SAFETY: `src` is NUL-terminated and `out` holds `size` UTF-16 units.
            let needed = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), out.as_mut_ptr(), size) };
            if needed == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if needed <= size {
                // `needed` counts the terminating NUL.
                out.truncate(needed as usize - 1);
                return Ok(out);
            }
            out.resize(needed as usize, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_first_then_user() {
        assert_eq!(
            join_registry_paths(
                Some(r"C:\Windows\system32;C:\Windows;"),
                Some(r"C:\Users\u\bin")
            ),
            r"C:\Windows\system32;C:\Windows;C:\Users\u\bin"
        );
        assert_eq!(join_registry_paths(None, Some(r"C:\u")), r"C:\u");
        assert_eq!(join_registry_paths(Some(r"C:\m"), None), r"C:\m");
        assert_eq!(join_registry_paths(Some(""), Some(";")), "");
        assert_eq!(join_registry_paths(None, None), "");
    }

    #[cfg(windows)]
    #[test]
    fn reads_the_registry_path() {
        // Every Windows install has a machine Path with the system dir in it.
        let path = registry_path().unwrap();
        let text = path.to_string_lossy().to_lowercase();
        assert!(text.contains("system32"), "machine Path not read");
        assert!(!text.contains("%systemroot%"), "REG_EXPAND_SZ not expanded");
    }
}
