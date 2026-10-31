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

    fn owned_by_another_account(&self, path: &Path) -> io::Result<bool> {
        Ok(owned_by_other_user(
            &owner_as_sddl(path)?,
            &current_user_sid()?,
        ))
    }

    fn fix_command(&self, path: &Path) -> Option<String> {
        let sid = current_user_sid().ok()?;
        Some(crisol_core::paths::private_acl_fix_command(
            path,
            path.is_dir(),
            &sid,
        ))
    }

    fn is_writable_by_non_administrators(&self, path: &Path) -> io::Result<bool> {
        Ok(exposes_write_to_non_administrators(
            &owner_and_dacl_as_sddl(path)?,
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
pub(crate) fn current_user_sid() -> io::Result<String> {
    // SAFETY: the pseudo-handle of the current process is always valid.
    user_sid_of_process(unsafe { GetCurrentProcess() })
}

/// The SID of the user a process runs as, as text. `process` needs the right to query limited
/// information.
pub(crate) fn user_sid_of_process(process: HANDLE) -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: `process` is a valid handle (the caller's promise); `token` is a valid
    // out-parameter.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) }.map_err(io::Error::from)?;
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
    sddl_of(
        path,
        OBJECT_SECURITY_INFORMATION(OWNER_SECURITY_INFORMATION.0 | DACL_SECURITY_INFORMATION.0),
    )
}

/// Only the owner of `path`, as SDDL text (`O:…`). Cheaper than reading the whole DACL, which
/// matters to the shim.
pub(crate) fn owner_as_sddl(path: &Path) -> io::Result<String> {
    sddl_of(path, OWNER_SECURITY_INFORMATION)
}

/// The `parts` of the security descriptor of `path`, as SDDL text.
fn sddl_of(path: &Path, parts: OBJECT_SECURITY_INFORMATION) -> io::Result<String> {
    let wide_path = wide(path.as_os_str());
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
/// administrators (the group and the built-in Administrator account, which SDDL writes as `LA`
/// instead of its SID: the runner of GitHub Actions is that account), and the creator-owner and
/// owner-rights placeholders (which only ever resolve to whoever owns the item). The current
/// user is added by the caller.
const TRUSTED: [&str; 8] = [
    "SY",
    "BA",
    "LA",
    "CO",
    "OW",
    "S-1-5-18",
    "S-1-5-32-544",
    "S-1-3-0",
];

/// `NT SERVICE\TrustedInstaller`, which owns what Windows installs (and `Program Files`).
const TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// Whether `who` may write to something that runs with administrator rights: `SYSTEM`, the
/// administrators, the system's installer and the owner placeholders. Not the current user.
fn is_administrator_trustee(who: &str) -> bool {
    who == TRUSTED_INSTALLER || TRUSTED.contains(&who)
}

/// Whether `who` (a SID or an SDDL alias) is the user `user_sid`, `SYSTEM`, the administrators
/// or a placeholder for the owner. The one definition of "trusted" shared by the folder
/// check and the project-file owner check (ADR-0008, ADR-0010).
pub(crate) fn is_trusted_trustee(who: &str, user_sid: &str) -> bool {
    who == user_sid || TRUSTED.contains(&who)
}

/// Whether the owner in `sddl` is an ordinary account (`S-1-5-21-…`) that is not `user_sid`.
/// `SYSTEM`, the administrators and the system's installer are not "another account".
fn owned_by_other_user(sddl: &str, user_sid: &str) -> bool {
    owner_of(sddl).is_some_and(|owner| {
        owner.starts_with("S-1-5-21-") && !owner.eq_ignore_ascii_case(user_sid)
    })
}

/// The owner in SDDL text that starts with `O:`, up to the next section marker.
pub(crate) fn owner_of(sddl: &str) -> Option<&str> {
    let after = sddl.strip_prefix("O:")?;
    // A SID starts with `S-`, never `S:`, so the markers cannot be confused with it.
    let end = ["G:", "D:", "S:"]
        .iter()
        .filter_map(|marker| after.find(marker))
        .min()
        .unwrap_or(after.len());
    Some(&after[..end])
}

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
    exposes_write_unless(sddl, &|who| is_trusted_trustee(who, user_sid))
}

/// The stricter version for programs that run elevated: only `SYSTEM`, the administrators and
/// the system's installer may own or write the item. The current user is not among them: if
/// they could rewrite it, so could anything that runs as them.
fn exposes_write_to_non_administrators(sddl: &str) -> bool {
    exposes_write_unless(sddl, &is_administrator_trustee)
}

/// [`exposes_write`] with the list of who may be trusted given as a function.
fn exposes_write_unless(sddl: &str, may_write: &dyn Fn(&str) -> bool) -> bool {
    if owner_of(sddl).is_some_and(|owner| !may_write(owner)) {
        return true;
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
pub(crate) mod tests {
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

    /// Whether the tests run as the built-in Administrator account (RID 500, the runner of
    /// GitHub Actions). Its folders belong to `LA`, an administrator, so "a folder the user made
    /// is not fit for a privileged program" cannot hold for it: the user already is one.
    pub(crate) fn runs_as_builtin_administrator() -> bool {
        current_user_sid().is_ok_and(|sid| sid.starts_with("S-1-5-21-") && sid.ends_with("-500"))
    }

    #[test]
    fn the_built_in_administrator_account_written_as_la_is_an_administrator() {
        // SDDL writes the local Administrator account (RID 500) as `LA`, not as its SID, so
        // a user who is that account sees `LA` where their own SID is expected (the runner of
        // GitHub Actions is that account).
        let folder = "O:LAG:LAD:P(A;OICI;FA;;;LA)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
        assert!(!exposes_write(folder, ME));
        assert!(!exposes_write_to_non_administrators(folder));
        assert!(!owned_by_other_user(folder, ME));
        assert!(is_trusted_trustee("LA", ME));
        // The guest account is not an administrator.
        assert!(exposes_write("O:LGD:P(A;OICI;FA;;;LG)", ME));
    }

    #[test]
    fn what_runs_elevated_must_not_be_writable_by_the_user_either() {
        // The strict rule: the user's own SID is not among the trusted ones.
        let own = format!("O:{ME}G:{ME}D:P(A;OICI;FA;;;{ME})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
        assert!(!exposes_write(&own, ME), "fine for a private home");
        assert!(
            exposes_write_to_non_administrators(&own),
            "not for a privileged program"
        );
        assert!(exposes_write_to_non_administrators(&format!(
            "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FW;;;{ME})"
        )));
        // Owned by the user, even if the DACL is harmless: the owner can rewrite it.
        assert!(exposes_write_to_non_administrators(&format!(
            "O:{ME}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
        )));
        assert!(exposes_write_to_non_administrators("O:BAD:(A;;FA;;;AU)"));

        // What a default `Program Files\Crisol` looks like: owned by the system's installer,
        // administrators, SYSTEM and the installer with full control, users read and execute.
        let program_files = format!(
            "O:{TRUSTED_INSTALLER}D:PAI(A;;FA;;;{TRUSTED_INSTALLER})(A;CIIO;GA;;;{TRUSTED_INSTALLER})\
             (A;OICIIO;GA;;;SY)(A;;FA;;;SY)(A;OICIIO;GA;;;BA)(A;;FA;;;BA)\
             (A;OICIIO;GXGR;;;BU)(A;;0x1200a9;;;BU)(A;OICIIO;GXGR;;;S-1-15-2-1)\
             (A;OICIIO;GA;;;CO)"
        );
        assert!(
            !exposes_write_to_non_administrators(&program_files),
            "{program_files}"
        );

        // A file owned by the administrators group with no DACL changes passes; anything
        // unreadable does not.
        assert!(!exposes_write_to_non_administrators("O:BAD:"));
        assert!(exposes_write_to_non_administrators("O:BAD:(A;;ZZ;;;SY"));
        assert!(exposes_write_to_non_administrators("O:BAD:(A;;ZZ;;;BU)"));
    }

    /// The guard of ADR-0008 with real permissions, on a temporary home: a folder opened to
    /// Authenticated Users blocks, the message carries a command that works as printed (run
    /// here through `cmd`, on that folder only), and after it the home is accepted again.
    #[test]
    fn a_real_open_runtime_folder_blocks_and_the_printed_command_closes_it() {
        use crisol_core::paths::{require_private_home, CrisolPaths, HomeError};
        use std::os::windows::process::CommandExt;

        let tmp = tempfile::tempdir().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        let dirs = WindowsPrivateDirs::new();
        paths.ensure_private_layout(&dirs).unwrap();
        fs::create_dir_all(paths.bin_dir()).unwrap();
        require_private_home(&paths, &dirs).unwrap();

        let grant = Command::new("icacls")
            .arg(paths.bin_dir())
            .args(["/grant", "*S-1-5-11:(OI)(CI)M"])
            .output()
            .unwrap();
        assert!(grant.status.success());
        let HomeError::Exposed { path, fix } = require_private_home(&paths, &dirs).unwrap_err()
        else {
            panic!("expected an exposed folder");
        };
        assert_eq!(path, paths.bin_dir());
        assert!(
            fix.contains("(OI)(CI)F") && fix.starts_with("icacls "),
            "{fix}"
        );

        // The command, exactly as printed, in `cmd` (the folder is this test's own).
        let ran = Command::new("cmd")
            .arg("/C")
            .raw_arg(format!("\"{fix}\""))
            .output()
            .unwrap();
        assert!(
            ran.status.success(),
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );
        require_private_home(&paths, &dirs).unwrap();
    }

    #[test]
    fn the_real_folders_of_the_system_and_the_user_are_judged_apart() {
        let dirs = WindowsPrivateDirs::new();
        let system = crate::system_paths::system_directory().unwrap();
        assert!(!dirs.is_writable_by_non_administrators(&system).unwrap());
        // A folder made by a test: the user can write it, so it is not for a privileged program,
        // but it is private for the home rule.
        let tmp = tempfile::tempdir().unwrap();
        let private = tmp.path().join("private");
        assert!(dirs.create_private_dir(&private).unwrap());
        assert!(!dirs.is_writable_by_others(&private).unwrap());
        // See `runs_as_builtin_administrator`: for that account, its own folder is an
        // administrator's.
        if !runs_as_builtin_administrator() {
            assert!(dirs.is_writable_by_non_administrators(&private).unwrap());
        }
    }

    #[test]
    fn only_an_ordinary_account_other_than_the_user_counts_as_another_account() {
        let other = "O:S-1-5-21-1-2-3-1002G:S-1-5-21-1-2-3-1002D:P(A;OICI;FA;;;SY)";
        assert!(owned_by_other_user(other, ME));
        assert!(!owned_by_other_user(other, "S-1-5-21-1-2-3-1002"));
        let mine = format!("O:{ME}D:P(A;OICI;FA;;;SY)");
        assert!(!owned_by_other_user(&mine, ME));
        // What an installer or an elevated process makes is not somebody else's home.
        for owner in ["BA", "SY", "S-1-5-32-544", "S-1-5-80-956008885-3418522649"] {
            assert!(
                !owned_by_other_user(&format!("O:{owner}D:P(A;OICI;FA;;;SY)"), ME),
                "{owner}"
            );
        }
        // And a folder made by this very test is mine.
        let tmp = TempDir::new().unwrap();
        assert!(!WindowsPrivateDirs
            .owned_by_another_account(tmp.path())
            .unwrap());
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
    fn a_program_is_trusted_in_a_private_folder_and_not_in_one_opened_with_icacls() {
        // The rule for the programs that start on their own (review of phase 8, finding 3),
        // with the real judge on folders of this test's own temp dir.
        use crisol_core::programs::{require_trusted_program, ProgramError};

        let tmp = TempDir::new().unwrap();
        let private = tmp.path().join("private");
        assert!(WindowsPrivateDirs.create_private_dir(&private).unwrap());
        let program = private.join("crisold.exe");
        fs::write(&program, "MZ").unwrap();
        require_trusted_program(&program, &WindowsPrivateDirs).unwrap();

        let open = tmp.path().join("open");
        fs::create_dir(&open).unwrap();
        let granted = Command::new("icacls")
            .arg(&open)
            .args(["/grant", "*S-1-5-11:(OI)(CI)M"])
            .output()
            .unwrap();
        assert!(granted.status.success(), "icacls failed");
        let exposed = open.join("crisold.exe");
        fs::write(&exposed, "MZ").unwrap();
        match require_trusted_program(&exposed, &WindowsPrivateDirs) {
            Err(ProgramError::Exposed { fix, .. }) => assert!(fix.starts_with("icacls "), "{fix}"),
            other => panic!("{other:?}"),
        }
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

    /// A parent that, like a drive root, gives "modify" to Authenticated Users and passes it
    /// down to what is created inside.
    fn open_parent(tmp: &TempDir) -> std::path::PathBuf {
        let parent = tmp.path().join("drive-root");
        fs::create_dir(&parent).unwrap();
        let grant = Command::new("icacls")
            .arg(&parent)
            .args(["/grant", "*S-1-5-11:(OI)(CI)M"])
            .output()
            .unwrap();
        assert!(grant.status.success(), "icacls failed");
        parent
    }

    #[test]
    fn the_local_ca_is_made_in_a_private_certs_folder_even_inside_an_open_parent() {
        // Review of phase 6, finding 1: a home under `C:\` or `D:\` inherits write access for
        // everybody, and certs/ must not.
        use crisol_core::ca::{ensure_ca, load_ca};
        use crisol_core::paths::CrisolPaths;
        use std::time::SystemTime;

        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(open_parent(&tmp).join("home")).unwrap();
        let made = ensure_ca(&paths, &WindowsPrivateDirs, "test", SystemTime::now()).unwrap();
        assert!(made.created);
        for path in [
            paths.home().to_path_buf(),
            paths.certs_dir(),
            paths.ca_key(),
            paths.ca_cert(),
        ] {
            assert!(
                !WindowsPrivateDirs.is_writable_by_others(&path).unwrap(),
                "{}",
                path.display()
            );
        }
        assert!(load_ca(&paths, &WindowsPrivateDirs, SystemTime::now())
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_certs_folder_that_inherited_openness_is_refused() {
        use crisol_core::ca::{ensure_ca, CaError};
        use crisol_core::paths::CrisolPaths;
        use std::time::SystemTime;

        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(open_parent(&tmp).join("home")).unwrap();
        // Somebody (or an older Crisol) made the folders the plain way: they are open.
        fs::create_dir_all(paths.certs_dir()).unwrap();
        let error = ensure_ca(&paths, &WindowsPrivateDirs, "test", SystemTime::now())
            .err()
            .unwrap();
        assert!(matches!(error, CaError::Exposed { .. }), "{error}");
        assert!(!paths.ca_key().exists(), "no key was written into it");
    }
}
