//! Windows implementation of [`FileOwners`]: who owns a file (ADR-0010).
//!
//! Only the owner is read (`OWNER_SECURITY_INFORMATION`), never the whole DACL, and only for
//! files that exist: the shim calls this on every run and has to stay fast. What counts as
//! trusted is not decided here but by [`is_trusted_trustee`], the same rule that judges
//! whether the Crisol home is private (ADR-0008): the current user, `SYSTEM` or the
//! administrators.

use std::io;
use std::path::Path;

use crisol_core::project::{FileOwner, FileOwners};

use crate::private_dir::{current_user_sid, is_trusted_trustee, owner_as_sddl, owner_of};

/// Reads file owners with Windows security descriptors.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsFileOwners;

impl WindowsFileOwners {
    /// A new reader. It holds no state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl FileOwners for WindowsFileOwners {
    fn owner_of(&self, path: &Path) -> io::Result<FileOwner> {
        let sddl = owner_as_sddl(path)?;
        let owner = owner_of(&sddl)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the file has no owner"))?;
        Ok(FileOwner {
            trusted: is_trusted_trustee(owner, &current_user_sid()?),
            name: owner.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn a_file_created_by_this_user_is_trusted() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join(".crisol.toml");
        fs::write(&file, "php = \"8.4\"\n").unwrap();

        let owner = WindowsFileOwners.owner_of(&file).unwrap();
        assert!(owner.trusted, "{owner:?}");
        assert_ne!(owner.name, "");
    }

    #[test]
    fn a_missing_file_is_an_error_not_an_owner() {
        let tmp = TempDir::new().unwrap();
        assert!(WindowsFileOwners
            .owner_of(&tmp.path().join("nope.toml"))
            .is_err());
    }

    #[test]
    fn the_trust_rule_is_the_one_used_for_the_home_folder() {
        let me = "S-1-5-21-1-2-3-1001";
        for trusted in [me, "SY", "BA", "S-1-5-18", "S-1-5-32-544"] {
            assert!(is_trusted_trustee(trusted, me), "{trusted}");
        }
        for stranger in ["S-1-5-21-1-2-3-1002", "BU", "AU", "WD", "S-1-5-32-545", ""] {
            assert!(!is_trusted_trustee(stranger, me), "{stranger}");
        }
        assert_eq!(owner_of("O:BAG:BA"), Some("BA"));
        assert_eq!(
            owner_of("O:S-1-5-21-1-2-3-1001"),
            Some("S-1-5-21-1-2-3-1001")
        );
        assert_eq!(owner_of("D:P"), None);
    }
}
