//! crisol-core: platform-independent logic of Crisol.

pub mod config;
pub mod paths;

pub use config::{Config, ConfigError, ConfigWarning, LoadedConfig};
pub use paths::{CrisolPaths, PathsError};

/// Crate version, exposed for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert_ne!(VERSION, "");
    }
}
