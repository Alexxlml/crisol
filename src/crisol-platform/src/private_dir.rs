//! Windows implementation of [`PrivateDirs`]: folders only the current user can write to.
//!
//! A new folder is created with an explicit, **protected** DACL (SDDL `D:P…`), so it does not
//! inherit what its parent grants. The DACL gives full control to the current user, `SYSTEM`
//! and the local administrators, with inheritance flags (`OICI`) so everything created
//! inside is private too. The user is named by their real SID rather than by "owner rights":
//! a folder made by an elevated process would otherwise be owned by the Administrators group
//! and be unreachable for the same person without elevation.
//!
//! Whether an existing folder is open to others is decided by reading its DACL back as SDDL
//! text and looking for *allow* entries for `Everyone`, `Authenticated Users` or `Users`
//! that include any write right. The text analysis ([`exposes_write`]) is plain Rust and is
//! tested on literal strings; only the calls that fetch and release the text use `unsafe`.
//!
//! Nothing here needs elevation, and only the folder it is asked about is touched.

use std::ffi::OsStr;
use std::io;
use std::iter::once;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use crisol_core::paths::PrivateDirs;
use windows::core::{HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
    SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Creates private folders with Windows ACLs.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsPrivateDirs;

impl WindowsPrivateDirs {
    /// A new creator. It holds no state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PrivateDirs for WindowsPrivateDirs {
    fn create_private_dir(&self, dir: &Path) -> io::Result<bool> {
        let sddl = private_sddl(&current_user_sid()?);
        let wide_sddl = wide(OsStr::new(&sddl));

        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide_sddl` is a NUL-terminated UTF-16 string that outlives the call;
        // `descriptor` is a valid out-parameter. On success Windows allocates the descriptor,
        // which is released with `LocalFree` below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide_sddl.as_ptr()),
                SDDL_REVISION_1,
                &raw mut descriptor,
                None,
            )
        }
        .map_err(io::Error::from)?;

        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let wide_dir = wide(dir.as_os_str());
        // SAFETY: `wide_dir` is NUL-terminated and `attributes` points to the descriptor made
        // above; both outlive the call.
        let created =
            unsafe { CreateDirectoryW(PCWSTR(wide_dir.as_ptr()), Some(&raw const attributes)) };
        // SAFETY: `descriptor.0` was allocated by Windows for us and is no longer used.
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };

        match created {
            Ok(()) => Ok(true),
            Err(e) if e.code() == HRESULT::from_win32(ERROR_ALREADY_EXISTS.0) => Ok(false),
            Err(e) => Err(io::Error::from(e)),
        }
    }

    fn is_writable_by_others(&self, dir: &Path) -> io::Result<bool> {
        Ok(exposes_write(&dacl_as_sddl(dir)?))
    }
}

/// UTF-16, NUL-terminated.
fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(once(0)).collect()
}

/// The DACL for a private folder owned by the user with SID `sid`.
fn private_sddl(sid: &str) -> String {
    // D:P = protected DACL (no inheritance from the parent); OICI = inherited by files and
    // subfolders; FA = full control; SY = SYSTEM; BA = built-in Administrators.
    format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
}

/// The SID of the user this process runs as, as text (`S-1-5-21-…`).
fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle of the current process is always valid; `token` is a valid
    // out-parameter.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
        .map_err(io::Error::from)?;
    // SAFETY: `token` is a valid handle that nobody else owns.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let handle = HANDLE(token.as_raw_handle());

    let mut length = 0_u32;
    // SAFETY: a size query: no buffer is passed. It is expected to fail with "insufficient
    // buffer" and write the needed size into `length`.
    let _ = unsafe { GetTokenInformation(handle, TokenUser, None, 0, &raw mut length) };
    if length == 0 {
        return Err(io::Error::other("the token has no user information"));
    }
    // 8-byte aligned storage, as `TOKEN_USER` contains pointers.
    let mut buffer = vec![0_u64; (length as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` has at least `length` bytes and outlives the call.
    unsafe {
        GetTokenInformation(
            handle,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &raw mut length,
        )
    }
    .map_err(io::Error::from)?;

    // SAFETY: the call above filled `buffer` with a `TOKEN_USER`, correctly aligned.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = PWSTR::null();
    // SAFETY: `user.User.Sid` points into `buffer`, which is alive; `text` is a valid
    // out-parameter. The string Windows allocates is released below.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut text) }.map_err(io::Error::from)?;
    // SAFETY: on success `text` is a valid NUL-terminated UTF-16 string.
    let sid = unsafe { text.to_string() };
    // SAFETY: allocated by Windows for us and not used afterwards.
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    sid.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The DACL of `path`, as SDDL text.
fn dacl_as_sddl(path: &Path) -> io::Result<String> {
    let wide_path = wide(path.as_os_str());
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `wide_path` is NUL-terminated and outlives the call; the other pointers are
    // either `None` or valid out-parameters. Windows allocates `descriptor`.
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide_path.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            &raw mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status.0).unwrap_or(i32::MAX),
        ));
    }

    let mut text = PWSTR::null();
    // SAFETY: `descriptor` is the valid descriptor obtained above; `text` is a valid
    // out-parameter whose string Windows allocates.
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &raw mut text,
            None,
        )
    };
    let result = match converted {
        // SAFETY: on success `text` is a valid NUL-terminated UTF-16 string.
        Ok(()) => {
            unsafe { text.to_string() }.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) => Err(io::Error::from(e)),
    };
    // SAFETY: both were allocated by Windows for us and are not used afterwards.
    unsafe {
        if !text.is_null() {
            LocalFree(Some(HLOCAL(text.0.cast())));
        }
        LocalFree(Some(HLOCAL(descriptor.0)));
    }
    result
}

/// Whether an SDDL DACL has an *allow* entry for `Everyone`, `Authenticated Users` or
/// `Users` that grants any right to write, delete or change permissions. Entries that only
/// apply to children (`IO`) count: that is where new files get their permissions.
///
/// Deny entries are ignored: the answer is "could someone else write here", and an
/// unrelated deny does not make a broad allow safe to rely on.
fn exposes_write(sddl: &str) -> bool {
    // Rights that are not read-only: FILE_WRITE_DATA, FILE_APPEND_DATA, FILE_DELETE_CHILD,
    // DELETE, WRITE_DAC, WRITE_OWNER, GENERIC_WRITE, GENERIC_ALL.
    const WRITE_BITS: u32 = 0x0002
        | 0x0004
        | 0x0040
        | 0x0001_0000
        | 0x0004_0000
        | 0x0008_0000
        | 0x4000_0000
        | 0x1000_0000;
    const BROAD_TRUSTEES: [&str; 6] = ["WD", "AU", "BU", "S-1-1-0", "S-1-5-11", "S-1-5-32-545"];
    // Two-letter right aliases that include writing.
    const WRITE_ALIASES: [&str; 7] = ["FA", "FW", "GA", "GW", "SD", "WD", "WO"];

    sddl.split('(').skip(1).any(|entry| {
        let entry = entry.trim_end_matches(')');
        let fields: Vec<&str> = entry.split(';').collect();
        let [kind, _flags, rights, _object, _inherited, trustee, ..] = fields.as_slice() else {
            return false;
        };
        if *kind != "A" || !BROAD_TRUSTEES.contains(trustee) {
            return false;
        }
        if let Some(hex) = rights
            .strip_prefix("0x")
            .or_else(|| rights.strip_prefix("0X"))
        {
            return u32::from_str_radix(hex, 16).map_or(true, |mask| mask & WRITE_BITS != 0);
        }
        rights
            .as_bytes()
            .chunks(2)
            .filter_map(|pair| std::str::from_utf8(pair).ok())
            .any(|alias| WRITE_ALIASES.contains(&alias))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn sddl_text_with_broad_write_access_is_recognised() {
        // What `C:\Crisol` really inherits on a default Windows install (see the review).
        let inherited = "D:PAI(A;OICIID;FA;;;BA)(A;OICIID;FA;;;SY)(A;OICIIOID;GA;;;CO)\
            (A;OICIID;0x1200a9;;;BU)(A;CIID;LC;;;BU)(A;CIID;DC;;;BU)(A;OICIIOID;SDGXGWGR;;;AU)\
            (A;CIID;0x1301bf;;;AU)";
        assert!(exposes_write(inherited), "Authenticated Users can modify");

        assert!(exposes_write("D:(A;;FA;;;WD)"), "Everyone, full control");
        assert!(
            exposes_write("D:(A;OICI;0x1301bf;;;AU)"),
            "modify, as a mask"
        );
        assert!(exposes_write("D:(A;OICI;FW;;;BU)"), "Users can write files");
        assert!(
            exposes_write("D:(A;OICIIO;GW;;;S-1-5-11)"),
            "children only still counts"
        );
        assert!(
            exposes_write("D:(A;;0xZZ;;;WD)"),
            "an unreadable mask is not trusted"
        );
    }

    #[test]
    fn sddl_text_without_broad_write_access_is_not_flagged() {
        let own = "D:PAI(A;OICI;FA;;;S-1-5-21-1-2-3-1001)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
        assert!(!exposes_write(own));
        // Read and execute for everyone is fine; so are denies and other accounts' writes.
        assert!(!exposes_write("D:(A;OICI;0x1200a9;;;BU)"));
        assert!(!exposes_write("D:(A;OICI;GRGX;;;WD)"));
        assert!(!exposes_write("D:(D;OICI;FA;;;WD)"));
        assert!(!exposes_write("D:(A;OICI;FA;;;S-1-5-21-1-2-3-1002)"));
        assert!(!exposes_write("D:P"));
        assert!(!exposes_write(""));
    }

    #[test]
    fn the_private_descriptor_is_protected_and_names_the_user() {
        let sddl = private_sddl("S-1-5-21-1-2-3-1001");
        assert!(sddl.starts_with("D:P("), "protected: {sddl}");
        assert!(sddl.contains("FA;;;S-1-5-21-1-2-3-1001"));
        assert!(!exposes_write(&sddl));
    }

    #[test]
    fn the_current_user_sid_looks_like_a_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[test]
    fn a_new_private_folder_is_private_and_so_is_what_is_created_inside() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        assert!(WindowsPrivateDirs.create_private_dir(&home).unwrap());
        assert!(!WindowsPrivateDirs.is_writable_by_others(&home).unwrap());

        // The owner can still use it, and children inherit the same protection.
        let child = home.join("bin");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("php.exe"), "MZ").unwrap();
        assert!(!WindowsPrivateDirs.is_writable_by_others(&child).unwrap());
    }

    #[test]
    fn an_existing_folder_is_left_alone() {
        let tmp = TempDir::new().unwrap();
        let existing = tmp.path().join("mine");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("keep.txt"), "x").unwrap();

        assert!(!WindowsPrivateDirs.create_private_dir(&existing).unwrap());
        assert_eq!(fs::read_to_string(existing.join("keep.txt")).unwrap(), "x");
    }

    #[test]
    fn a_folder_opened_to_other_users_is_detected() {
        // Opens a folder of this test's own temp dir to "Authenticated Users" (S-1-5-11)
        // with `icacls`, the way a drive root would, and checks the report.
        let tmp = TempDir::new().unwrap();
        let open = tmp.path().join("open");
        fs::create_dir(&open).unwrap();
        let status = Command::new("icacls")
            .arg(&open)
            .args(["/grant", "*S-1-5-11:(OI)(CI)M"])
            .output()
            .unwrap();
        assert!(status.status.success(), "icacls failed");

        assert!(WindowsPrivateDirs.is_writable_by_others(&open).unwrap());
    }

    #[test]
    fn a_private_folder_does_not_inherit_the_openness_of_its_parent() {
        // The real case: `C:\` grants "modify" to Authenticated Users and inherits it down.
        // Reproduce it on a parent inside this test's temp dir, then create inside it.
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("drive-root");
        fs::create_dir(&parent).unwrap();
        let grant = Command::new("icacls")
            .arg(&parent)
            .args(["/grant", "*S-1-5-11:(OI)(CI)M"])
            .output()
            .unwrap();
        assert!(grant.status.success(), "icacls failed");

        // A plain folder made there inherits the openness...
        let plain = parent.join("plain");
        fs::create_dir(&plain).unwrap();
        assert!(WindowsPrivateDirs.is_writable_by_others(&plain).unwrap());

        // ...a private one does not, and neither does anything inside it.
        let private = parent.join("private");
        assert!(WindowsPrivateDirs.create_private_dir(&private).unwrap());
        assert!(!WindowsPrivateDirs.is_writable_by_others(&private).unwrap());
        let inside = private.join("bin");
        fs::create_dir(&inside).unwrap();
        assert!(!WindowsPrivateDirs.is_writable_by_others(&inside).unwrap());
    }

    #[test]
    fn a_missing_folder_is_an_error_not_an_answer() {
        let tmp = TempDir::new().unwrap();
        assert!(WindowsPrivateDirs
            .is_writable_by_others(&tmp.path().join("nope"))
            .is_err());
    }
}
