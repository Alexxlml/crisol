//! Windows implementation of [`PrivateDirs`]: folders only the current user can write to.
//!
//! A new folder is created with an explicit, **protected** DACL (SDDL `D:P…`), so it does not
//! inherit what its parent grants. The DACL gives full control to the current user, `SYSTEM`
//! and the local administrators, with inheritance flags (`OICI`) so everything created
//! inside is private too. The user is named by their real SID rather than by "owner rights":
//! a folder made by an elevated process would otherwise be owned by the Administrators group
//! and be unreachable for the same person without elevation.
//!
//! Whether an existing folder is open to others is decided by reading its owner and DACL
//! back as SDDL text. The folder is *exposed* if its owner is anybody but the current user,
//! `SYSTEM` or the administrators, or if any *allow* entry gives a right to write to a
//! trustee that is not one of those (or the creator-owner placeholder). It is a list of who
//! may be trusted, not a list of groups to fear: another account's SID is as good as
//! `Everyone` for planting a DLL. The analysis ([`exposes_write`]) is plain Rust, turns every
//! right alias into its access mask first, and treats anything it does not recognise as
//! exposed. It is tested on literal strings; only the calls that fetch and release the text
//! use `unsafe`.
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
    GetTokenInformation, TokenUser, DACL_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
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
        Ok(exposes_write(
            &owner_and_dacl_as_sddl(dir)?,
            &current_user_sid()?,
        ))
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

/// The owner and the DACL of `path`, as SDDL text (`O:…D:…`).
fn owner_and_dacl_as_sddl(path: &Path) -> io::Result<String> {
    let wide_path = wide(path.as_os_str());
    let parts =
        OBJECT_SECURITY_INFORMATION(OWNER_SECURITY_INFORMATION.0 | DACL_SECURITY_INFORMATION.0);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `wide_path` is NUL-terminated and outlives the call; the other pointers are
    // either `None` or valid out-parameters. Windows allocates `descriptor`.
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide_path.as_ptr()),
            SE_FILE_OBJECT,
            parts,
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
            parts,
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

/// Trustees that may hold write access to a private folder: `SYSTEM`, the local
/// administrators, and the creator-owner and owner-rights placeholders (which only ever
/// resolve to whoever owns the item). The current user is added by the caller.
const TRUSTED: [&str; 7] = [
    "SY",
    "BA",
    "CO",
    "OW",
    "S-1-5-18",
    "S-1-5-32-544",
    "S-1-3-0",
];

/// Access-mask bits that are not read-only for a file or folder: write data / add file,
/// append / add subfolder, write extended attributes, delete child, write attributes,
/// delete, write permissions, take ownership, and the generic write and all rights.
const WRITE_BITS: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x1000_0000 | 0x4000_0000;

/// The access mask of an SDDL right alias. For folders, the "directory service" aliases
/// share their bits with the file rights: `DC` is *add file* (0x2), `LC` is *add
/// subfolder* (0x4), `DT` is *delete child* (0x40).
fn alias_mask(alias: &str) -> Option<u32> {
    Some(match alias {
        "CC" => 0x1,
        "DC" => 0x2,
        "LC" => 0x4,
        "SW" => 0x8,
        "RP" => 0x10,
        "WP" => 0x20,
        "DT" => 0x40,
        "LO" => 0x80,
        "CR" => 0x100,
        "SD" => 0x1_0000,
        "RC" => 0x2_0000,
        "WD" => 0x4_0000,
        "WO" => 0x8_0000,
        "GA" => 0x1000_0000,
        "GX" => 0x2000_0000,
        "GW" => 0x4000_0000,
        "GR" => 0x8000_0000,
        "FA" => 0x1F_01FF,
        "FR" => 0x12_0089,
        "FW" => 0x12_0116,
        "FX" => 0x12_00A0,
        _ => return None,
    })
}

/// The mask of a rights field: a hexadecimal number or a run of two-letter aliases. `None`
/// if any part is not understood.
fn rights_mask(rights: &str) -> Option<u32> {
    if let Some(hex) = rights
        .strip_prefix("0x")
        .or_else(|| rights.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).ok();
    }
    if rights.is_empty() || !rights.len().is_multiple_of(2) {
        return None;
    }
    rights.as_bytes().chunks(2).try_fold(0_u32, |mask, pair| {
        Some(mask | alias_mask(std::str::from_utf8(pair).ok()?)?)
    })
}

/// Whether the owner or the DACL in `sddl` (`O:…D:…`) lets an account other than the user
/// `user_sid`, `SYSTEM` or the administrators write to the folder or to what is created
/// inside it. Entries that only apply to children (`IO`) count: that is where new files get
/// their permissions. Deny entries are ignored: an unrelated deny does not make an allow for
/// a stranger safe to rely on. Whatever cannot be understood counts as exposed.
fn exposes_write(sddl: &str, user_sid: &str) -> bool {
    let may_write = |who: &str| who == user_sid || TRUSTED.contains(&who);

    if let Some(after) = sddl.strip_prefix("O:") {
        // The owner runs up to the next section marker (`G:`, `D:` or `S:`). A SID starts
        // with `S-`, never `S:`, so the markers cannot be confused with it.
        let end = ["G:", "D:", "S:"]
            .iter()
            .filter_map(|marker| after.find(marker))
            .min()
            .unwrap_or(after.len());
        if !may_write(&after[..end]) {
            return true;
        }
    }

    let Some(entries) = aces(sddl) else {
        return true; // unbalanced parentheses: not something to trust
    };
    entries.into_iter().any(|entry| {
        // At most seven fields; the seventh is the condition of a conditional entry, which
        // has semicolons and parentheses of its own.
        let fields: Vec<&str> = entry.splitn(7, ';').collect();
        let [kind, _flags, rights, _object, _inherited, trustee, ..] = fields.as_slice() else {
            return true; // an entry that does not look like one is not to be trusted
        };
        // Every kind of *allow* entry counts; a condition can only narrow who it applies to
        // at run time, and nothing here can evaluate it.
        if !matches!(*kind, "A" | "XA" | "OA" | "ZA") || may_write(trustee) {
            return false;
        }
        rights_mask(rights).is_none_or(|mask| mask & WRITE_BITS != 0)
    })
}

/// The entries of an SDDL ACL: the text inside each top-level pair of parentheses. Nested
/// parentheses (the condition of an `XA` entry) stay inside their entry. `None` if the
/// parentheses do not balance.
fn aces(sddl: &str) -> Option<Vec<&str>> {
    let mut entries = Vec::new();
    let (mut depth, mut start) = (0_usize, 0_usize);
    for (index, c) in sddl.char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    start = index + 1;
                }
                depth += 1;
            }
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    entries.push(&sddl[start..index]);
                }
            }
            _ => {}
        }
    }
    (depth == 0).then_some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    const ME: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn sddl_text_that_lets_others_write_is_recognised() {
        // What `C:\Crisol` really inherits on a default Windows install (see the review).
        let inherited = "D:PAI(A;OICIID;FA;;;BA)(A;OICIID;FA;;;SY)(A;OICIIOID;GA;;;CO)\
            (A;OICIID;0x1200a9;;;BU)(A;CIID;LC;;;BU)(A;CIID;DC;;;BU)(A;OICIIOID;SDGXGWGR;;;AU)\
            (A;CIID;0x1301bf;;;AU)";
        assert!(exposes_write(inherited, ME));

        // Each of these alone is enough: creating a file is enough to plant a DLL.
        for exposed in [
            "D:(A;;FA;;;WD)",
            "D:(A;OICI;0x1301bf;;;AU)",
            "D:(A;OICI;FW;;;BU)",
            "D:(A;CIID;DC;;;BU)", // create files
            "D:(A;CIID;LC;;;BU)", // create folders
            "D:(A;CIID;DT;;;BU)", // delete children
            "D:(A;OICIIO;GW;;;S-1-5-11)",
            "D:(A;OICI;FA;;;S-1-5-21-1-2-3-1002)", // another account, by its own SID
            "D:(A;;0xZZ;;;WD)",                    // unreadable mask
            "D:(A;;ZZ;;;WD)",                      // unknown alias
            "D:(A;;FA)",                           // malformed entry
        ] {
            assert!(exposes_write(exposed, ME), "{exposed}");
        }
    }

    #[test]
    fn conditional_and_object_allows_count_as_allows() {
        // A conditional entry has parentheses and semicolons of its own.
        for exposed in [
            "D:(XA;OICI;FA;;;WD;(@User.x == 1))",
            "D:(XA;;DC;;;BU;(Member_of {SID(BA)} && @Resource.y > 2))",
            "D:(OA;;FA;;;AU)",
            "D:(ZA;;FA;;;WD)",
        ] {
            assert!(exposes_write(exposed, ME), "{exposed}");
        }
        // The same conditional entry for somebody trusted, or read-only, is fine.
        assert!(!exposes_write("D:(XA;OICI;FA;;;SY;(@User.x == 1))", ME));
        assert!(!exposes_write("D:(XA;;GRGX;;;WD;(@User.x == 1))", ME));
        // And what cannot be split into entries is not trusted.
        assert!(exposes_write("D:(A;;GR;;;WD", ME));
        assert!(exposes_write("D:A;;GR;;;WD)", ME));
    }

    #[test]
    fn aces_are_split_by_nesting_depth() {
        assert_eq!(
            aces("D:P(A;;FA;;;SY)(XA;;GR;;;WD;(a;(b)))"),
            Some(vec!["A;;FA;;;SY", "XA;;GR;;;WD;(a;(b))"])
        );
        assert_eq!(aces("D:P"), Some(Vec::new()));
        assert_eq!(aces("(A"), None);
        assert_eq!(aces(")("), None);
    }

    #[test]
    fn sddl_text_that_only_lets_trusted_accounts_write_is_not_flagged() {
        let own = format!("O:{ME}G:{ME}D:P(A;OICI;FA;;;{ME})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
        assert!(!exposes_write(&own, ME));
        assert!(!exposes_write(
            "O:BAD:P(A;OICI;FA;;;SY)(A;OICIIO;GA;;;CO)",
            ME
        ));
        assert!(!exposes_write("O:SYD:", ME));

        // Reading and executing is fine for anybody; so are denies.
        for fine in [
            "D:(A;OICI;0x1200a9;;;BU)",
            "D:(A;OICI;GRGX;;;WD)",
            "D:(A;OICI;CC;;;BU)", // list folder
            "D:(A;OICI;FRFX;;;AU)",
            "D:(D;OICI;FA;;;WD)",
            "D:P",
            "",
        ] {
            assert!(!exposes_write(fine, ME), "{fine}");
        }
    }

    #[test]
    fn a_folder_owned_by_somebody_else_is_exposed_whatever_its_acl_says() {
        // Somebody else created `C:\Crisol` first and granted the user full control: the
        // DACL looks harmless, but the owner can rewrite it at any time.
        let planted = format!("O:S-1-5-21-1-2-3-1002G:S-1-5-21-1-2-3-1002D:P(A;OICI;FA;;;{ME})");
        assert!(exposes_write(&planted, ME));
        assert!(!exposes_write(&format!("O:{ME}D:P(A;OICI;FA;;;{ME})"), ME));
    }

    #[test]
    fn right_aliases_are_turned_into_masks() {
        assert_eq!(alias_mask("DC"), Some(0x2));
        assert_eq!(
            rights_mask("SDGXGWGR"),
            Some(0x1_0000 | 0x2000_0000 | 0x4000_0000 | 0x8000_0000)
        );
        assert_eq!(rights_mask("0x1200a9"), Some(0x12_00A9));
        assert_eq!(rights_mask("FRFX"), Some(0x12_0089 | 0x12_00A0));
        assert_eq!(rights_mask(""), None);
        assert_eq!(rights_mask("F"), None);
        assert_eq!(rights_mask("ZZ"), None);
    }

    #[test]
    fn the_private_descriptor_is_protected_and_names_the_user() {
        let sddl = private_sddl(ME);
        assert!(sddl.starts_with("D:P("), "protected: {sddl}");
        assert!(sddl.contains(&format!("FA;;;{ME}")));
        assert!(!exposes_write(&sddl, ME));
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
    fn a_folder_where_users_can_only_create_files_is_exposed() {
        // What `C:\` leaves for `Users` (`(A;CIID;DC;;;BU)`): no "modify", just "create
        // files", which is already enough to plant a DLL. `icacls` calls it `WD`.
        let tmp = TempDir::new().unwrap();
        let open = tmp.path().join("create-only");
        fs::create_dir(&open).unwrap();
        let grant = Command::new("icacls")
            .arg(&open)
            .args(["/grant", "*S-1-5-32-545:(OI)(CI)(WD)"])
            .output()
            .unwrap();
        assert!(grant.status.success(), "icacls failed");

        assert!(WindowsPrivateDirs.is_writable_by_others(&open).unwrap());
    }

    #[test]
    fn a_folder_granted_to_another_account_by_sid_is_exposed() {
        // Not a broad group: a concrete SID that is neither the user nor an administrator.
        let tmp = TempDir::new().unwrap();
        let open = tmp.path().join("other-account");
        fs::create_dir(&open).unwrap();
        let grant = Command::new("icacls")
            .arg(&open)
            .args(["/grant", "*S-1-5-32-546:(OI)(CI)M"])
            .output()
            .unwrap();
        assert!(grant.status.success(), "icacls failed");

        assert!(WindowsPrivateDirs.is_writable_by_others(&open).unwrap());
    }

    #[test]
    fn a_missing_folder_is_an_error_not_an_answer() {
        let tmp = TempDir::new().unwrap();
        assert!(WindowsPrivateDirs
            .is_writable_by_others(&tmp.path().join("nope"))
            .is_err());
    }
}
