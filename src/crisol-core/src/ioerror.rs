//! The sentence of a failed file operation, shared by the errors that carry one (`install`,
//! `manage`): what was being done, the path and what the system said.

use std::io;
use std::path::Path;

use crate::i18n::Message;
use crate::msg;

/// "cannot create `path`: error", with the verb in the language of the dictionary. `action` is
/// the English phrase the code names the operation with; one that is not in the list is said as
/// it is.
pub(crate) fn io_message(action: &str, path: &Path, source: &io::Error) -> Message {
    let (path, error) = (path.display(), source);
    match action {
        "cannot create" => msg!("io.create", path = path, error = error),
        "cannot create a staging folder in" => {
            msg!("io.create_staging", path = path, error = error)
        }
        "cannot move into place" => msg!("io.move_into_place", path = path, error = error),
        "cannot move out of the way" => msg!("io.move_away", path = path, error = error),
        "cannot open" => msg!("io.open", path = path, error = error),
        "cannot place" => msg!("io.place", path = path, error = error),
        "cannot read" => msg!("io.read", path = path, error = error),
        "cannot create a temporary file in" => msg!("io.create_temp", path = path, error = error),
        "cannot list the installed bundles in" => {
            msg!("io.list_bundles", path = path, error = error)
        }
        "cannot remove" => msg!("io.remove", path = path, error = error),
        "cannot write" => msg!("io.write", path = path, error = error),
        "cannot delete" => msg!("io.delete", path = path, error = error),
        "cannot clear" => msg!("io.clear", path = path, error = error),
        other => msg!("io.other", action = other, path = path, error = error),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// Every phrase the code names a file operation with (`"cannot …"` as the first argument of
    /// `io(…)` or `io_error(…)`, or as an `action`) has its own key: one that fell through to
    /// `io.other` would leave its English verb inside a translated sentence.
    #[test]
    fn every_action_the_code_names_has_its_key() {
        let mut found = 0;
        let mut sources = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(path) = sources.pop() {
            if path.is_dir() {
                sources.extend(fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") || path.ends_with("ioerror.rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            for start in ["io(\"", "io_error(\"", "action: \""] {
                for (at, _) in text.match_indices(start) {
                    let rest = &text[at + start.len()..];
                    let action = &rest[..rest.find('"').unwrap()];
                    if !action.starts_with("cannot ") {
                        continue;
                    }
                    found += 1;
                    let said =
                        super::io_message(action, Path::new("x"), &std::io::Error::other("e"));
                    assert_ne!(
                        said.key(),
                        "io.other",
                        "`{action}` ({}) has no key in `io_message`",
                        path.display()
                    );
                }
            }
        }
        assert!(found > 10, "the search found only {found} actions");
    }
}
