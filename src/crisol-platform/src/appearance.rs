//! What a window program asks Windows about itself: whether the taskbar is light (to pick the
//! variant of a tray icon, ADR-0027), which languages the user prefers, and a message box for when there is no window yet. Nothing is written.

use std::io;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
use windows::Win32::System::Registry::{
    RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, REG_DWORD, REG_VALUE_TYPE,
};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

use crate::user_path::{check, open, wide};

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const TASKBAR_VALUE: &str = "SystemUsesLightTheme";

/// Reads a `DWORD`. `Ok(None)` if the key or the value is missing.
fn read_dword(root: HKEY, subkey: &str, name: &str) -> io::Result<Option<u32>> {
    let Some(key) = open(root, subkey, KEY_QUERY_VALUE.0)? else {
        return Ok(None);
    };
    let wide_name = wide(name);
    let mut kind = REG_VALUE_TYPE::default();
    let mut data = [0_u8; 4];
    let mut length = 4_u32;
    // SAFETY: `data` has `length` bytes and outlives the call; `kind` and `length` are valid
    // out-parameters.
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
    if code == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(code)?;
    Ok((kind == REG_DWORD && length == 4).then(|| u32::from_le_bytes(data)))
}

/// Whether the taskbar is light, which is what its icons must contrast with. `false` (dark)
/// when Windows does not say: that is how the oldest builds, which have no such setting, look.
#[must_use]
pub fn taskbar_uses_light_theme() -> bool {
    matches!(
        read_dword(HKEY_CURRENT_USER, PERSONALIZE, TASKBAR_VALUE),
        Ok(Some(value)) if value != 0
    )
}

/// The languages the user prefers for Windows itself, most wanted first (`es-MX`, `en-US`).
/// Empty when Windows does not say.
#[must_use]
pub fn ui_languages() -> Vec<String> {
    let mut count = 0_u32;
    let mut size = 0_u32;
    // SAFETY: a size query: no buffer is passed; `count` and `size` are valid out-parameters.
    let asked = unsafe {
        GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &raw mut count, None, &raw mut size)
    };
    if asked.is_err() || size == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0_u16; size as usize];
    // SAFETY: `buffer` has `size` elements and outlives the call.
    let read = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &raw mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &raw mut size,
        )
    };
    if read.is_err() {
        return Vec::new();
    }
    split_multistring(&buffer)
}

/// The strings of a double-NUL-terminated list.
fn split_multistring(buffer: &[u16]) -> Vec<String> {
    buffer
        .split(|unit| *unit == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Shows an error in a message box and waits for it to be closed: for what must be said when
/// there is no window of our own to say it in.
pub fn error_box(title: &str, text: &str) {
    let (title, text) = (wide(title), wide(text));
    // SAFETY: both texts are NUL-terminated and outlive the call; there is no owner window.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real registry is only read. Whatever this machine has, asking must not fail.
    #[test]
    fn a_multistring_is_cut_at_its_nuls() {
        let buffer: Vec<u16> = "es-MX\0en-US\0\0".encode_utf16().collect();
        assert_eq!(split_multistring(&buffer), ["es-MX", "en-US"]);
        assert_eq!(split_multistring(&[0, 0]), Vec::<String>::new());
        assert_eq!(split_multistring(&[]), Vec::<String>::new());
    }

    #[test]
    fn asking_this_machine_gives_an_answer() {
        let _ = taskbar_uses_light_theme();
        let _ = ui_languages();
    }

    #[test]
    fn a_key_that_is_not_there_has_no_value() {
        assert!(matches!(
            read_dword(HKEY_CURRENT_USER, r"Software\Crisol\NoSuchKey", "x"),
            Ok(None)
        ));
    }
}
