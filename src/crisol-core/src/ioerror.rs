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
        other => msg!("io.other", action = other, path = path, error = error),
    }
}
