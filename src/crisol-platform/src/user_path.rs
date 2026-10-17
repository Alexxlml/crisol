//! Windows implementation of [`UserPath`]: the user `PATH` in the registry (F4.4).
//!
//! The user `PATH` is the value `Path` of `HKEY_CURRENT_USER\Environment`; the machine one is
//! the value of the same name under `HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\
//! Session Manager\Environment`, which is only ever **read**. Changing the user value needs no
//! elevation. After a change, `WM_SETTINGCHANGE` is broadcast with the text `Environment`, which
//! is what makes Explorer, and so the terminals opened afterwards, pick the new value up.
//!
//! The root key is a parameter so that the tests can use a **private application hive**
//! (`RegLoadAppKeyW` over a file in the test's own temporary folder): it needs no privileges,
//! never touches the global registry, and disappears with the folder. The tests never write the
//! real `Environment`. A private hive does not broadcast.

use std::io;

use crisol_core::userpath::{PathValue, UserPath};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, LPARAM, WIN32_ERROR, WPARAM,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

const USER_ENVIRONMENT: &str = "Environment";
const SYSTEM_ENVIRONMENT: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const VALUE: &str = "Path";
/// How long to wait for each window to answer the broadcast.
const BROADCAST_TIMEOUT_MS: u32 = 5000;

/// The user `PATH` in the Windows registry.
#[derive(Debug, Clone)]
pub struct RegistryUserPath {
    root: isize,
    subkey: String,
    broadcast: bool,
}

impl Default for RegistryUserPath {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryUserPath {
    /// The real user `PATH`: `HKCU\Environment`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: HKEY_CURRENT_USER.0 as isize,
            subkey: USER_ENVIRONMENT.to_owned(),
            broadcast: true,
        }
    }

    /// A `PATH` stored under `subkey` of another root (a private hive), for tests. Does not
    /// broadcast.
    #[cfg(test)]
    fn private(root: HKEY, subkey: &str) -> Self {
        Self {
            root: root.0 as isize,
            subkey: subkey.to_owned(),
            broadcast: false,
        }
    }

    fn root(&self) -> HKEY {
        HKEY(self.root as *mut core::ffi::c_void)
    }
}

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn check(code: WIN32_ERROR) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(code.0).unwrap_or(i32::MAX),
        ))
    }
}

/// An open registry key, closed on drop.
pub(crate) struct Key(pub(crate) HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is closed exactly once, here.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

/// Opens `subkey` of `root` with `access`. `Ok(None)` if it does not exist.
pub(crate) fn open(root: HKEY, subkey: &str, access: u32) -> io::Result<Option<Key>> {
    let name = wide(subkey);
    let mut handle = HKEY::default();
    // SAFETY: `name` is NUL-terminated and outlives the call; `handle` is a valid out-parameter.
    let code = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(name.as_ptr()),
            None,
            windows::Win32::System::Registry::REG_SAM_FLAGS(access),
            &raw mut handle,
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(code)?;
    Ok(Some(Key(handle)))
}

/// Reads the string value `name` of `subkey`. `Ok(None)` if the key or the value is missing.
pub(crate) fn read_value(root: HKEY, subkey: &str, name: &str) -> io::Result<Option<PathValue>> {
    let Some(key) = open(root, subkey, KEY_QUERY_VALUE.0)? else {
        return Ok(None);
    };
    let wide_name = wide(name);
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0_u32;
    // SAFETY: a size query: no data buffer is passed; `kind` and `size` are valid out-parameters.
    let code = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(wide_name.as_ptr()),
            None,
            Some(&raw mut kind),
            None,
            Some(&raw mut size),
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(code)?;
    if kind != REG_SZ && kind != REG_EXPAND_SZ {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the `{name}` value is not a string (type {})", kind.0),
        ));
    }

    // The value can grow between the two calls: read again with room to spare if told to.
    let mut data = vec![0_u8; size as usize + 2];
    loop {
        let mut length = u32::try_from(data.len()).unwrap_or(u32::MAX);
        // SAFETY: `data` has `length` bytes and outlives the call.
        let code = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(wide_name.as_ptr()),
                None,
                Some(&raw mut kind),
                Some(data.as_mut_ptr()),
                Some(&raw mut length),
            )
        };
        if code == ERROR_MORE_DATA {
            data = vec![0_u8; data.len() * 2];
            continue;
        }
        check(code)?;
        data.truncate(length as usize);
        break;
    }
    let (pairs, rest) = data.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the `{name}` value is not UTF-16 text (odd length)"),
        ));
    }
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    // The stored text ends with NUL (sometimes several); that is not part of the value.
    let end = units.iter().rposition(|u| *u != 0).map_or(0, |i| i + 1);
    let text = String::from_utf16(&units[..end])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(PathValue {
        text,
        expandable: kind == REG_EXPAND_SZ,
    }))
}

impl UserPath for RegistryUserPath {
    fn read(&self) -> io::Result<Option<PathValue>> {
        read_value(self.root(), &self.subkey, VALUE)
    }

    fn write(&self, value: Option<&PathValue>) -> io::Result<()> {
        let name = wide(VALUE);
        let Some(value) = value else {
            let Some(key) = open(self.root(), &self.subkey, KEY_SET_VALUE.0)? else {
                return Ok(());
            };
            // SAFETY: `name` is NUL-terminated and outlives the call.
            let code = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
            return if code == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                check(code)
            };
        };

        let subkey = wide(&self.subkey);
        let mut handle = HKEY::default();
        // SAFETY: `subkey` is NUL-terminated; `handle` is a valid out-parameter. Opens the key,
        // creating it if it is missing; nothing else is created.
        check(unsafe {
            RegCreateKeyExW(
                self.root(),
                PCWSTR(subkey.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                windows::Win32::System::Registry::REG_SAM_FLAGS(KEY_SET_VALUE.0),
                None,
                &raw mut handle,
                None,
            )
        })?;
        let key = Key(handle);
        let bytes: Vec<u8> = wide(&value.text)
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let kind = if value.expandable {
            REG_EXPAND_SZ
        } else {
            REG_SZ
        };
        // SAFETY: `name` is NUL-terminated; `bytes` is the UTF-16 text with its terminator.
        check(unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, kind, Some(&bytes)) })
    }

    fn system_path(&self) -> io::Result<Option<String>> {
        Ok(read_value(HKEY_LOCAL_MACHINE, SYSTEM_ENVIRONMENT, VALUE)?.map(|v| v.text))
    }

    fn expand(&self, text: &str) -> String {
        let source = wide(text);
        // SAFETY: `source` is NUL-terminated. A call without a buffer returns the size needed.
        let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None) };
        if needed == 0 {
            return text.to_owned();
        }
        let mut buffer = vec![0_u16; needed as usize];
        // SAFETY: `buffer` has `needed` units, as asked for.
        let written =
            unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(&mut buffer)) };
        if written == 0 || written as usize > buffer.len() {
            return text.to_owned();
        }
        let end = buffer.iter().position(|u| *u == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    fn notify_changed(&self) -> io::Result<()> {
        if !self.broadcast {
            return Ok(());
        }
        let setting = wide(USER_ENVIRONMENT);
        // SAFETY: `setting` is NUL-terminated and outlives the call; the message is only a
        // notification, and `SMTO_ABORTIFHUNG` stops a hung window from blocking us.
        let _ = unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(setting.as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                BROADCAST_TIMEOUT_MS,
                None,
            )
        };
        // A window that does not answer is not our failure: the value is already stored.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::userpath::{add_shims, remove_shims, PathChange};
    use std::path::Path;
    use windows::Win32::System::Registry::{RegLoadAppKeyW, KEY_ALL_ACCESS};

    /// A private application hive over a file in a temporary folder of its own. The global
    /// registry is never written by a test; the hive is unloaded when this is dropped, before
    /// the folder is removed.
    struct Scratch {
        hive: Key,
        _dir: tempfile::TempDir,
    }

    impl Scratch {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let file = wide(&dir.path().join("hive.dat").to_string_lossy());
            let mut handle = HKEY::default();
            // SAFETY: `file` is NUL-terminated and outlives the call; `handle` is a valid
            // out-parameter. The file is created if it does not exist.
            check(unsafe {
                RegLoadAppKeyW(
                    PCWSTR(file.as_ptr()),
                    &raw mut handle,
                    KEY_ALL_ACCESS.0,
                    0,
                    None,
                )
            })
            .unwrap();
            Self {
                hive: Key(handle),
                _dir: dir,
            }
        }

        fn store(&self) -> RegistryUserPath {
            RegistryUserPath::private(self.hive.0, USER_ENVIRONMENT)
        }
    }

    fn shims() -> &'static Path {
        Path::new(r"C:\Crisol\shims")
    }

    #[test]
    fn a_missing_key_or_value_reads_as_none() {
        let scratch = Scratch::new();
        assert_eq!(scratch.store().read().unwrap(), None);
        // And removing from nothing is fine, without creating anything.
        scratch.store().write(None).unwrap();
        assert_eq!(scratch.store().read().unwrap(), None);
    }

    #[test]
    fn values_round_trip_with_their_kind() {
        let scratch = Scratch::new();
        let store = scratch.store();
        for expandable in [true, false] {
            let value = PathValue {
                text: "C:\\Tools;%USERPROFILE%\\bin;D:\\ünï\\日本語;".to_owned(),
                expandable,
            };
            store.write(Some(&value)).unwrap();
            assert_eq!(store.read().unwrap(), Some(value));
        }
        store.write(None).unwrap();
        assert_eq!(store.read().unwrap(), None);
    }

    #[test]
    fn a_very_long_value_survives() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let text = (0..400)
            .map(|i| format!(r"C:\Some\Long\Folder\Name\Number{i}"))
            .collect::<Vec<_>>()
            .join(";");
        assert!(text.len() > 10_000);
        store
            .write(Some(&PathValue {
                text: text.clone(),
                expandable: true,
            }))
            .unwrap();
        assert_eq!(store.read().unwrap().unwrap().text, text);
    }

    #[test]
    fn add_then_remove_leaves_the_registry_value_identical() {
        // F4.4's acceptance, against a private hive that mimics `Environment`.
        let scratch = Scratch::new();
        let store = scratch.store();
        for (original, expandable) in [
            (Some(r"C:\Tools;%USERPROFILE%\bin;"), true),
            (Some(r"C:\Tools;D:\More"), false),
            (None, true),
        ] {
            let before = original.map(|text| PathValue {
                text: text.to_owned(),
                expandable,
            });
            store.write(before.as_ref()).unwrap();

            assert_eq!(add_shims(&store, shims()).unwrap(), PathChange::Added);
            let during = store.read().unwrap().unwrap();
            assert!(during.text.starts_with(r"C:\Crisol\shims"), "{during:?}");
            assert_eq!(remove_shims(&store, shims()).unwrap(), PathChange::Removed);

            assert_eq!(store.read().unwrap(), before, "started from {original:?}");
        }
    }

    #[test]
    fn the_real_user_path_is_only_read() {
        // Read only: whatever the user has must parse, and nothing is written.
        let real = RegistryUserPath::new();
        let value = real.read().unwrap();
        if let Some(value) = value {
            assert_ne!(value.text, "");
        }
        assert!(real.broadcast, "the real one announces its changes");
        assert!(!RegistryUserPath::private(HKEY_CURRENT_USER, "x").broadcast);
    }

    #[test]
    fn shadowing_programs_are_found_on_this_machine_without_touching_anything() {
        // Read only: the real PATH and the real file system, to see what Crisol would warn about.
        let shadows = crisol_core::userpath::shadowing(
            &RegistryUserPath::new(),
            Path::new(r"C:\Crisol\shims"),
            &|path: &Path| path.is_file(),
        )
        .unwrap();
        for shadow in &shadows {
            eprintln!(
                "{} -> {} ({}, {})",
                shadow.program,
                shadow.found.display(),
                shadow.suite.unwrap_or("unknown tool"),
                if shadow.in_system_path {
                    "machine PATH"
                } else {
                    "user PATH"
                }
            );
        }
        assert!(shadows.iter().all(|s| s.found.is_file()));
    }

    #[test]
    fn the_machine_path_can_be_read() {
        let system = RegistryUserPath::new().system_path().unwrap().unwrap();
        assert!(system.to_lowercase().contains("system32"), "{system}");
    }

    #[test]
    fn variables_are_expanded_like_windows_does() {
        let store = RegistryUserPath::new();
        let root = std::env::var("SystemRoot").unwrap();
        assert_eq!(
            store.expand(r"%SystemRoot%\system32"),
            format!(r"{root}\system32")
        );
        assert_eq!(store.expand("no variables here"), "no variables here");
        assert_eq!(
            store.expand("%A_VARIABLE_NOBODY_DEFINED%"),
            "%A_VARIABLE_NOBODY_DEFINED%"
        );
    }
}
