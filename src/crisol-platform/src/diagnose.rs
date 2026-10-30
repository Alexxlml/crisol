//! What `doctor` can tell about why a program does not start on Windows (F11.1): the meaning
//! of the exit codes of a program the loader refused, whether the Visual C++ runtime is there,
//! whether `::1` works, and what to do when Microsoft Defender slows the databases down.
//!
//! Everything here only reads. The Defender exclusion is given as a command for an
//! administrator to run, never run by Crisol (ADR-0021).

use std::io;
use std::net::{Ipv6Addr, SocketAddr, TcpListener};
use std::path::Path;

use crisol_protocol::{Check, CheckStatus};
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

use crate::pe_imports::needs_vc_runtime;
use crate::system_paths::system_directory;
use crate::user_path::read_value;

/// Where the installer of the Visual C++ runtime (x64) writes its version.
const VC_RUNTIME_KEY: &str = r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\X64";

/// The files of the Visual C++ runtime that PHP and the databases load.
const VC_RUNTIME_DLLS: [&str; 3] = ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"];

/// Where Microsoft gives the runtime.
pub const VC_RUNTIME_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

/// `STATUS_DLL_NOT_FOUND`.
const DLL_NOT_FOUND: u32 = 0xC000_0135;
/// `STATUS_ENTRYPOINT_NOT_FOUND`.
const ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;
/// `STATUS_ORDINAL_NOT_FOUND`.
const ORDINAL_NOT_FOUND: u32 = 0xC000_0138;
/// `STATUS_INVALID_IMAGE_FORMAT`.
const INVALID_IMAGE_FORMAT: u32 = 0xC000_007B;
/// `STATUS_DLL_INIT_FAILED`.
const DLL_INIT_FAILED: u32 = 0xC000_0142;

/// What an exit code means when the Windows loader refused to run the program. The exit code
/// of a process is a `u32` kept as an `i32`: the bits are the same.
#[must_use]
pub fn explain_exit(code: i32) -> Option<String> {
    #[allow(clippy::cast_sign_loss)]
    let status = code as u32;
    let runtime = || {
        if vc_runtime_present() {
            String::new()
        } else {
            format!("; the Visual C++ runtime is not installed, get it from {VC_RUNTIME_URL}")
        }
    };
    let text = match status {
        DLL_NOT_FOUND => format!(
            "Windows could not find a DLL the program needs{}",
            runtime()
        ),
        ENTRYPOINT_NOT_FOUND | ORDINAL_NOT_FOUND => format!(
            "a DLL it loaded is older than the program needs, often an old Visual C++ runtime \
             or another copy of a DLL found in the PATH; install the current runtime from \
             {VC_RUNTIME_URL}"
        ),
        INVALID_IMAGE_FORMAT => "a DLL it loaded is not a 64-bit one, often another copy of a \
                                 DLL found in the PATH"
            .to_owned(),
        DLL_INIT_FAILED => "a DLL it loaded could not start".to_owned(),
        _ => return None,
    };
    Some(format!("{text} (code 0x{status:08X})"))
}

/// The version of the Visual C++ runtime its installer recorded, if any.
fn vc_runtime_version() -> Option<String> {
    read_value(HKEY_LOCAL_MACHINE, VC_RUNTIME_KEY, "Version")
        .ok()
        .flatten()
        .map(|value| value.text)
}

/// Whether the DLLs of the runtime are where every program finds them.
fn vc_runtime_dlls_in(system: &Path) -> bool {
    VC_RUNTIME_DLLS.iter().all(|dll| system.join(dll).is_file())
}

fn vc_runtime_present() -> bool {
    vc_runtime_version().is_some() || system_directory().is_ok_and(|dir| vc_runtime_dlls_in(&dir))
}

/// Whether the programs installed in `install_dir` (its `.exe` files and those of its `bin`)
/// import the Visual C++ runtime while it is not installed: then the reason and what to do.
/// PHP and the database servers do; Nginx, Node, Mailpit and Redis do not.
#[must_use]
pub fn missing_runtime(install_dir: &Path) -> Option<(String, String)> {
    if vc_runtime_present() || !installation_needs_vc_runtime(install_dir) {
        return None;
    }
    Some((
        "it needs the Microsoft Visual C++ runtime (x64), which is not installed".to_owned(),
        format!("install it from {VC_RUNTIME_URL}, then start it again"),
    ))
}

/// Whether any `.exe` in `install_dir` or in its `bin` imports the Visual C++ runtime.
fn installation_needs_vc_runtime(install_dir: &Path) -> bool {
    [install_dir.to_path_buf(), install_dir.join("bin")]
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        })
        .any(|path| crate::pe_imports::imports(&path).is_ok_and(|dlls| needs_vc_runtime(&dlls)))
}

/// The line of `doctor` about the Visual C++ runtime: PHP and the database servers do not
/// start without it, and they say so only with an exit code.
#[must_use]
pub fn vc_runtime_check() -> Check {
    let name = "Visual C++ runtime".to_owned();
    if let Some(version) = vc_runtime_version() {
        return Check {
            name,
            status: CheckStatus::Ok,
            detail: format!("{version} (x64) is installed"),
            fix: None,
        };
    }
    if system_directory().is_ok_and(|dir| vc_runtime_dlls_in(&dir)) {
        return Check {
            name,
            status: CheckStatus::Ok,
            detail: "its DLLs are in the system folder (no installer recorded a version)"
                .to_owned(),
            fix: None,
        };
    }
    Check {
        name,
        status: CheckStatus::Warning,
        detail: "the Microsoft Visual C++ runtime (x64) is not installed: PHP and the database \
                 servers cannot start without it"
            .to_owned(),
        fix: Some(format!("install it from {VC_RUNTIME_URL}")),
    }
}

/// Whether a program can listen on `::1`: Nginx listens there as well as on `127.0.0.1`, so a
/// computer with IPv6 turned off entirely cannot run it.
fn ipv6_loopback_works() -> io::Result<()> {
    TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, 0))).map(drop)
}

/// The line of `doctor` about `::1`.
#[must_use]
pub fn ipv6_check() -> Check {
    let name = "IPv6 loopback".to_owned();
    match ipv6_loopback_works() {
        Ok(()) => Check {
            name,
            status: CheckStatus::Ok,
            detail: "programs can listen on [::1]".to_owned(),
            fix: None,
        },
        Err(error) => Check {
            name,
            status: CheckStatus::Problem,
            detail: format!(
                "nothing can listen on [::1] ({error}): Nginx listens there too, so it cannot start"
            ),
            fix: Some(
                "turn IPv6 back on (Windows keeps [::1] working unless IPv6 is disabled in the registry)"
                    .to_owned(),
            ),
        },
    }
}

/// What to do when the databases are slow to start: Microsoft Defender reads every file they
/// open. An administrator can exclude only the data folder; Crisol does not do it (ADR-0021).
#[must_use]
pub fn slow_start_hint(data: &Path) -> String {
    let quoted = crisol_core::shell::powershell_quote(&data.display().to_string());
    format!(
        "if Microsoft Defender is on, an administrator can exclude only the data folder, in \
         PowerShell as administrator: Add-MpPreference -ExclusionPath {quoted} \
         (Remove-MpPreference -ExclusionPath {quoted} undoes it)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::cast_possible_wrap)]
    fn as_exit_code(status: u32) -> i32 {
        status as i32
    }

    #[test]
    fn the_loader_codes_are_explained_and_the_others_are_not() {
        let missing = explain_exit(as_exit_code(DLL_NOT_FOUND)).unwrap();
        assert!(missing.contains("could not find a DLL"), "{missing}");
        assert!(missing.ends_with("(code 0xC0000135)"), "{missing}");
        let old = explain_exit(as_exit_code(ENTRYPOINT_NOT_FOUND)).unwrap();
        assert!(old.contains(VC_RUNTIME_URL), "{old}");
        assert!(explain_exit(as_exit_code(INVALID_IMAGE_FORMAT))
            .unwrap()
            .contains("64-bit"));
        for ordinary in [0, 1, 255, -1] {
            assert_eq!(explain_exit(ordinary), None, "{ordinary}");
        }
    }

    #[test]
    fn the_runtime_dlls_are_looked_for_in_the_folder_given() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!vc_runtime_dlls_in(tmp.path()));
        for dll in VC_RUNTIME_DLLS {
            std::fs::write(tmp.path().join(dll), b"").unwrap();
        }
        assert!(vc_runtime_dlls_in(tmp.path()));
    }

    #[test]
    fn the_checks_of_this_computer_answer() {
        // This computer has the runtime and IPv6; the point is that they read and answer.
        assert_eq!(vc_runtime_check().name, "Visual C++ runtime");
        assert_eq!(ipv6_check().status, CheckStatus::Ok);
    }

    #[test]
    fn the_exclusion_names_only_the_data_folder_and_how_to_undo_it() {
        let hint = slow_start_hint(Path::new(r"C:\Users\o'neil\Crisol\data"));
        assert!(
            hint.contains(r"Add-MpPreference -ExclusionPath 'C:\Users\o''neil\Crisol\data'"),
            "{hint}"
        );
        assert!(hint.contains("Remove-MpPreference"), "{hint}");
    }

    #[test]
    fn an_installation_needs_the_runtime_only_if_one_of_its_programs_imports_it() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!installation_needs_vc_runtime(tmp.path()), "empty");
        // Crisol's own programs link the runtime statically: they do not need it.
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), bin.join("server.exe")).unwrap();
        std::fs::write(tmp.path().join("broken.exe"), "MZ").unwrap();
        assert!(!installation_needs_vc_runtime(tmp.path()));
        // The real ones, where this computer has them (the owner's): PHP and MySQL import it,
        // Nginx does not.
        let real = std::path::Path::new(r"C:\Crisol\bin");
        for (tool, needs) in [
            ("php", true),
            ("mysql", true),
            ("postgresql", true),
            ("nginx", false),
        ] {
            let Ok(versions) = std::fs::read_dir(real.join(tool)) else {
                continue;
            };
            for version in versions.flatten() {
                assert_eq!(
                    installation_needs_vc_runtime(&version.path()),
                    needs,
                    "{}",
                    version.path().display()
                );
            }
        }
    }
}
