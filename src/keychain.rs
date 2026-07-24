//! The OS credential store: macOS Keychain, Windows Credential Manager, Secret Service on *nix.
//!
//! The best place for an API token is the one the OS already guards. Reads are best-effort — a
//! missing entry, a locked keyring, a headless box with no Secret Service — all fall through
//! quietly to the next source, because "no token here" is a normal answer, not an error.
//!
//! Writes are loud: `--set-token` failing silently would be a lie about where your secret went.

/// The service name entries are filed under. The account is the JIRA login email, so one machine can
/// hold tokens for several accounts (or several sites) without collision.
pub const SERVICE: &str = "jira-mcp";

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod imp {
    use anyhow::{Context, Result};

    fn entry(account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(super::SERVICE, account)
            .with_context(|| format!("opening the credential store for {account}"))
    }

    /// The stored token, or `None` for any reason at all.
    pub fn get(account: &str) -> Option<String> {
        entry(account).ok()?.get_password().ok()
    }

    pub fn set(account: &str, token: &str) -> Result<()> {
        entry(account)?
            .set_password(token)
            .with_context(|| format!("storing the token for {account}"))
    }

    pub fn delete(account: &str) -> Result<()> {
        entry(account)?
            .delete_credential()
            .with_context(|| format!("deleting the token for {account}"))
    }
}

/// Platforms without a credential store we support: the token comes from a file or the environment.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    use anyhow::{Result, bail};

    pub fn get(_account: &str) -> Option<String> {
        None
    }

    pub fn set(_account: &str, _token: &str) -> Result<()> {
        bail!("no OS credential store on this platform; use token_file instead")
    }

    pub fn delete(_account: &str) -> Result<()> {
        bail!("no OS credential store on this platform")
    }
}

pub use imp::{delete, get, set};
