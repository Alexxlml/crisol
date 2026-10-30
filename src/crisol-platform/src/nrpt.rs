//! Windows implementation of [`NrptPolicy`] over the PowerShell `DnsClient` cmdlets (F7.2,
//! ADR-0014), and the way to run a write as an administrator.
//!
//! - [`PowerShellNrpt`] reads with `Get-DnsClientNrptRule` and writes with
//!   `Add-DnsClientNrptRule` / `Remove-DnsClientNrptRule`. Writing needs an administrator, so
//!   only the helper (`crisol-elevate`) uses it for that.
//! - [`ElevatedNrpt`] is what the rest of Crisol uses: it reads directly, and for each change
//!   starts the helper through the UAC prompt with the fixed arguments of an
//!   [`ElevatedOp`](crisol_core::dns::ElevatedOp). The helper is found only at its fixed place
//!   and checked first ([`HelperLocation`]). Nothing else ever runs elevated.
//!
//! Every value that reaches a script was validated first (a special-use suffix, an IP), so none
//! of them can carry quotes or commands; the scripts are built by pure functions and tested as
//! text. The tests never touch the real table; the owner tries the real thing by hand
//! (`examples/dns_manual.rs`).

use std::io;
use std::net::IpAddr;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::helper_location::HelperLocation;
use crate::system_paths::{powershell_exe_in, system_directory};
use crisol_core::dns::{parse_nrpt_json, ElevatedOp, NrptPolicy, NrptRule, RULE_COMMENT};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The script that lists the rules as JSON.
#[must_use]
pub fn list_script() -> String {
    // `NameServers` holds .NET `IPAddress` objects; ask for their text so the JSON stays small.
    "@(Get-DnsClientNrptRule | Select-Object Namespace, \
      @{n='NameServers';e={@($_.NameServers | ForEach-Object { $_.ToString() })}}, Comment) \
      | ConvertTo-Json -Depth 3"
        .to_owned()
}

/// The script that adds the rule. `namespace` and `server` must have been validated.
#[must_use]
pub fn add_script(namespace: &str, server: IpAddr) -> String {
    format!(
        "Add-DnsClientNrptRule -Namespace '{namespace}' -NameServers '{server}' -Comment '{RULE_COMMENT}'"
    )
}

/// The script that removes our rule for `namespace` and nothing else.
#[must_use]
pub fn remove_script(namespace: &str) -> String {
    format!(
        "Get-DnsClientNrptRule | Where-Object {{ ($_.Namespace -contains '{namespace}') -and \
         ($_.Comment -eq '{RULE_COMMENT}') }} | Remove-DnsClientNrptRule -Force"
    )
}

pub(crate) fn powershell(script: &str) -> io::Result<String> {
    // The absolute path of the system's PowerShell: a bare name is looked for in other places.
    let output = Command::new(powershell_exe_in(&system_directory()?))
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "PowerShell failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn read_rules() -> io::Result<Vec<NrptRule>> {
    parse_nrpt_json(&powershell(&list_script())?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The real table, written directly. Writes need an administrator.
#[derive(Debug, Default, Clone, Copy)]
pub struct PowerShellNrpt;

impl NrptPolicy for PowerShellNrpt {
    fn rules(&self) -> io::Result<Vec<NrptRule>> {
        read_rules()
    }

    fn add(&self, rule: &NrptRule) -> io::Result<()> {
        powershell(&add_script(&rule.namespace, rule.name_server)).map(|_| ())
    }

    fn remove(&self, namespace: &str) -> io::Result<bool> {
        let existed = read_rules()?
            .iter()
            .any(|r| r.is_ours() && r.namespace.eq_ignore_ascii_case(namespace));
        if existed {
            powershell(&remove_script(namespace))?;
        }
        Ok(existed)
    }
}

/// Reads directly and changes the table through the elevated helper, one UAC prompt per change.
#[derive(Debug, Clone)]
pub struct ElevatedNrpt {
    helper: HelperLocation,
}

impl ElevatedNrpt {
    /// With the helper at its fixed place (`%ProgramFiles%\Crisol\crisol-elevate.exe`).
    pub fn new() -> io::Result<Self> {
        Ok(Self::with_location(HelperLocation::installed()?))
    }

    /// With the helper at another place, which is still checked before every prompt. For
    /// tests.
    #[must_use]
    pub(crate) fn with_location(helper: HelperLocation) -> Self {
        Self { helper }
    }
}

fn suffix_of(namespace: &str) -> String {
    namespace.trim_start_matches('.').to_owned()
}

impl NrptPolicy for ElevatedNrpt {
    fn rules(&self) -> io::Result<Vec<NrptRule>> {
        read_rules()
    }

    fn add(&self, rule: &NrptRule) -> io::Result<()> {
        let op = ElevatedOp::NrptAdd {
            suffix: suffix_of(&rule.namespace),
            server: rule.name_server,
        };
        self.helper.run(&op)
    }

    fn remove(&self, namespace: &str) -> io::Result<bool> {
        let existed = read_rules()?
            .iter()
            .any(|r| r.is_ours() && r.namespace.eq_ignore_ascii_case(namespace));
        if existed {
            self.helper.run(&ElevatedOp::NrptRemove {
                suffix: suffix_of(namespace),
            })?;
        }
        Ok(existed)
    }
}

pub(crate) fn run_elevated_op(helper: &Path, op: &ElevatedOp) -> io::Result<()> {
    match run_elevated(helper, &op.to_args())? {
        0 => Ok(()),
        code => Err(io::Error::other(format!(
            "the elevated helper failed with exit code {code}"
        ))),
    }
}

/// Runs `exe` with `args` through the UAC prompt and waits for it. Returns its exit code. If
/// the user answers no, that is an error of kind `PermissionDenied`. The arguments are joined
/// with spaces, so they must not contain any (the helper's never do). Not public: the only
/// caller is [`HelperLocation::run`], which has checked where `exe` is and who can write it.
pub(crate) fn run_elevated(exe: &Path, args: &[String]) -> io::Result<u32> {
    if args.iter().any(|a| a.is_empty() || a.contains([' ', '"'])) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "elevated arguments must be plain words",
        ));
    }
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };
    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(&args.join(" "));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0),
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: 0, // SW_HIDE
        ..Default::default()
    };
    // SAFETY: `info` is fully initialised, and the three strings it points at are NUL-terminated
    // and outlive the call.
    unsafe { ShellExecuteExW(std::ptr::addr_of_mut!(info)) }.map_err(|e| {
        // ERROR_CANCELLED (1223): the user declined the prompt.
        if e.code().0 & 0xFFFF == 1223 {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the elevation prompt was declined",
            )
        } else {
            io::Error::from(e)
        }
    })?;
    let process = info.hProcess;
    // SAFETY: `process` is the handle `SEE_MASK_NOCLOSEPROCESS` left to us; it is closed below.
    let waited = unsafe { WaitForSingleObject(process, INFINITE) };
    let mut code = 1_u32;
    let result = if waited == WAIT_OBJECT_0 {
        // SAFETY: valid process handle, valid out pointer.
        unsafe { GetExitCodeProcess(process, std::ptr::addr_of_mut!(code)) }
            .map_err(io::Error::from)
    } else {
        Err(io::Error::other("waiting for the elevated helper failed"))
    };
    // SAFETY: closing the handle we own, once.
    let _ = unsafe { CloseHandle(process) };
    result.map(|()| code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scripts_carry_only_validated_words_and_our_mark() {
        let add = add_script(".test", "127.77.0.53".parse().unwrap());
        assert_eq!(
            add,
            "Add-DnsClientNrptRule -Namespace '.test' -NameServers '127.77.0.53' -Comment 'Crisol'"
        );
        let remove = remove_script(".test");
        assert!(remove.contains("-contains '.test'"));
        assert!(remove.contains("-eq 'Crisol'"), "only ours: {remove}");
        assert!(remove.ends_with("Remove-DnsClientNrptRule -Force"));
        assert!(!remove.contains("  "), "{remove}");
        assert!(list_script().starts_with("@(Get-DnsClientNrptRule"));
    }

    #[test]
    fn elevated_arguments_must_be_plain_words() {
        let exe = Path::new("C:/nope/crisol-elevate.exe");
        for bad in ["a b", "a\"b", ""] {
            let error = run_elevated(exe, &[bad.to_owned()]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
    }

    /// Reads the real table (read only, no administrator needed). Run by hand.
    #[test]
    #[ignore = "reads the real NRPT through PowerShell"]
    fn the_real_table_can_be_read() {
        let rules = PowerShellNrpt.rules().unwrap();
        println!("{rules:#?}");
    }
}
