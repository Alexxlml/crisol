//! Windows implementation of [`Autostart`]: a value in the current user's `Run` key (F8.4).
//!
//! `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, value `Crisol`. Nothing
//! under `HKEY_LOCAL_MACHINE` and no administrator: the entry is the user's and runs with the
//! user's rights. Like [`crate::RegistryUserPath`], the root key is a parameter so the tests use
//! a **private application hive** in their own temporary folder and never write the real `Run`
//! key.

use std::io;

use crisol_core::autostart::Autostart;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ,
};

use crate::user_path::{check, open, read_value, wide, Key};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "Crisol";

/// The `Run` entry of the current user.
#[derive(Debug, Clone)]
pub struct RegistryAutostart {
    root: isize,
    subkey: String,
}

impl Default for RegistryAutostart {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryAutostart {
    /// The real `Run` key of the current user.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: HKEY_CURRENT_USER.0 as isize,
            subkey: RUN_KEY.to_owned(),
        }
    }

    /// The entry under `subkey` of another root (a private hive), for tests.
    #[cfg(test)]
    fn private(root: HKEY, subkey: &str) -> Self {
        Self {
            root: root.0 as isize,
            subkey: subkey.to_owned(),
        }
    }

    fn root(&self) -> HKEY {
        HKEY(self.root as *mut core::ffi::c_void)
    }
}

impl Autostart for RegistryAutostart {
    fn current(&self) -> io::Result<Option<String>> {
        Ok(read_value(self.root(), &self.subkey, VALUE)?.map(|v| v.text))
    }

    fn set(&self, command: &str) -> io::Result<()> {
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
                REG_SAM_FLAGS(KEY_SET_VALUE.0),
                None,
                &raw mut handle,
                None,
            )
        })?;
        let key = Key(handle);
        let name = wide(VALUE);
        let bytes: Vec<u8> = wide(command)
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        // SAFETY: `name` is NUL-terminated; `bytes` is the UTF-16 text with its terminator.
        check(unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&bytes)) })
    }

    fn remove(&self) -> io::Result<bool> {
        let Some(key) = open(self.root(), &self.subkey, KEY_SET_VALUE.0)? else {
            return Ok(false);
        };
        let name = wide(VALUE);
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let code = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
        if code == ERROR_FILE_NOT_FOUND {
            Ok(false)
        } else {
            check(code).map(|()| true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::autostart::{disable, enable, state, AutostartChange, AutostartState};
    use crisol_core::paths::fake::FakePrivateDirs;
    use windows::Win32::System::Registry::{RegLoadAppKeyW, KEY_ALL_ACCESS};

    /// A private application hive over a file in a temporary folder of its own, as in the tests
    /// of the user `PATH`: the real registry is never written.
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

        fn store(&self) -> RegistryAutostart {
            RegistryAutostart::private(self.hive.0, RUN_KEY)
        }
    }

    fn daemon() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("crisold.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        (dir, exe)
    }

    #[test]
    fn nothing_there_reads_as_none_and_removing_it_is_fine() {
        let scratch = Scratch::new();
        let store = scratch.store();
        assert_eq!(store.current().unwrap(), None);
        assert!(!store.remove().unwrap());
        assert_eq!(store.current().unwrap(), None, "reading created nothing");
    }

    #[test]
    fn the_entry_round_trips_with_unicode_and_is_removed_by_name() {
        let scratch = Scratch::new();
        let store = scratch.store();
        store.set(r#""D:\ünï\日本語\crisold.exe""#).unwrap();
        assert_eq!(
            store.current().unwrap().as_deref(),
            Some(r#""D:\ünï\日本語\crisold.exe""#)
        );
        assert!(store.remove().unwrap());
        assert_eq!(store.current().unwrap(), None);
        assert!(!store.remove().unwrap());
    }

    #[test]
    fn the_core_logic_works_over_the_registry_and_touches_only_its_own_value() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let (_dir, exe) = daemon();
        // Another program's entry in the same key.
        {
            let other = RegistryAutostart {
                root: store.root,
                subkey: store.subkey.clone(),
            };
            let subkey = wide(&other.subkey);
            let mut handle = HKEY::default();
            // SAFETY: as in `set`.
            check(unsafe {
                RegCreateKeyExW(
                    other.root(),
                    PCWSTR(subkey.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    REG_SAM_FLAGS(KEY_SET_VALUE.0),
                    None,
                    &raw mut handle,
                    None,
                )
            })
            .unwrap();
            let key = Key(handle);
            let name = wide("SomethingElse");
            let data: Vec<u8> = wide("x").iter().flat_map(|u| u.to_le_bytes()).collect();
            // SAFETY: as in `set`.
            check(unsafe {
                RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&data))
            })
            .unwrap();
        }

        assert_eq!(state(&store, &exe).unwrap(), AutostartState::Off);
        assert_eq!(
            enable(&store, &exe, &FakePrivateDirs::new()).unwrap(),
            AutostartChange::Enabled
        );
        assert_eq!(state(&store, &exe).unwrap(), AutostartState::On);
        assert_eq!(
            enable(&store, &exe, &FakePrivateDirs::new()).unwrap(),
            AutostartChange::Unchanged
        );
        assert!(disable(&store).unwrap());
        assert_eq!(state(&store, &exe).unwrap(), AutostartState::Off);
        // The other entry is still there.
        let still = read_value(store.root(), &store.subkey, "SomethingElse").unwrap();
        assert_eq!(still.map(|v| v.text).as_deref(), Some("x"));
    }

    #[test]
    fn the_real_run_key_is_the_users_own_and_is_only_read_here() {
        let real = RegistryAutostart::new();
        assert_eq!(real.subkey, RUN_KEY);
        assert_eq!(real.root, HKEY_CURRENT_USER.0 as isize);
        // Reading never fails for a missing value, and writes nothing.
        let _ = real.current().unwrap();
    }
}
