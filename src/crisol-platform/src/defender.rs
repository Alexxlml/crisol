//! Windows implementation of [`ScanExclusions`]: the exclusions of Microsoft Defender, over the
//! PowerShell `Defender` cmdlets (F11.2, ADR-0022).
//!
//! Only the helper (`crisol-elevate`) uses it, and only to remove: without an administrator
//! Windows does not show the exclusions at all (`Get-MpPreference` answers "N/A: Must be an
//! administrator to view exclusions"), and Crisol never adds one. Which exclusions are removed is
//! decided by `crisol_core::exclusions::remove_inside`; the values that reach a script come from
//! Defender itself and are put in single quotes with their quotes doubled. The tests never touch
//! the real list; the owner tries it in Windows Sandbox (`src/scripts/sandbox/`).

use std::io;

use crisol_core::exclusions::{parse_list_json, Exclusion, ExclusionKind, ScanExclusions};

use crate::nrpt::powershell;

/// The script that lists the exclusions as JSON: `{"Path": [...], "Process": [...]}`.
#[must_use]
pub fn list_script() -> String {
    "$p = Get-MpPreference -ErrorAction Stop; \
     [pscustomobject]@{ Path = @($p.ExclusionPath | Where-Object { $_ }); \
     Process = @($p.ExclusionProcess | Where-Object { $_ }) } | ConvertTo-Json -Compress"
        .to_owned()
}

/// The script that removes one exclusion, or `None` for a value no script should carry.
#[must_use]
pub fn remove_script(exclusion: &Exclusion) -> Option<String> {
    if exclusion.value.chars().any(char::is_control) {
        return None;
    }
    let parameter = match exclusion.kind {
        ExclusionKind::Path => "ExclusionPath",
        ExclusionKind::Process => "ExclusionProcess",
    };
    Some(format!(
        "Remove-MpPreference -{parameter} '{}' -ErrorAction Stop",
        exclusion.value.replace('\'', "''")
    ))
}

/// The exclusions of Microsoft Defender. Reading and writing need an administrator.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefenderExclusions;

impl ScanExclusions for DefenderExclusions {
    fn list(&self) -> io::Result<Vec<Exclusion>> {
        parse_list_json(&powershell(&list_script())?)
    }

    fn remove(&self, exclusion: &Exclusion) -> io::Result<()> {
        let script = remove_script(exclusion).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the exclusion has a control character",
            )
        })?;
        powershell(&script).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_reaches_the_script_only_in_quotes() {
        let script = remove_script(&Exclusion {
            kind: ExclusionKind::Path,
            value: r"C:\Users\O'Brien\Crisol\data'; Remove-Item C:\ -Recurse; '".to_owned(),
        })
        .unwrap();
        assert_eq!(
            script,
            r"Remove-MpPreference -ExclusionPath 'C:\Users\O''Brien\Crisol\data''; Remove-Item C:\ -Recurse; ''' -ErrorAction Stop"
        );
        let process = remove_script(&Exclusion {
            kind: ExclusionKind::Process,
            value: r"C:\Crisol\bin\mysqld.exe".to_owned(),
        })
        .unwrap();
        assert!(process.starts_with("Remove-MpPreference -ExclusionProcess 'C:"));
        assert!(remove_script(&Exclusion {
            kind: ExclusionKind::Path,
            value: "C:\\Crisol\nRemove-Item".to_owned(),
        })
        .is_none());
        assert!(list_script().contains("Get-MpPreference -ErrorAction Stop"));
    }
}
