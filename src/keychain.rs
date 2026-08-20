//! The OS credential store: macOS Keychain, Windows Credential Manager, Secret Service on *nix.
//!
//! The best place for an API token is the one the OS already guards. Reads are best-effort — a
//! missing entry, a locked keyring, a headless box with no Secret Service — all fall through
//! quietly to the next source, because "no token here" is a normal answer, not an error.
//!
//! Writes are loud: `--set-token` failing silently would be a lie about where your secret went.

/// The service name entries are filed under. The account is the JIRA login email, so one machine can
/// hold tokens for several accounts (or several sites) without collision.
pub const SERVICE: &str = "ujira";
/// The pre-rename service name. Reads fall back to it; writes go to [`SERVICE`].
pub const LEGACY_SERVICE: &str = "jira-mcp";

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod imp {
    use anyhow::{Context, Result};

    fn entry(service: &str, account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(service, account)
            .with_context(|| format!("opening the credential store for {account}"))
    }

    /// The stored token, or `None` for any reason at all.
    pub fn get(account: &str) -> Option<String> {
        [crate::keychain::SERVICE, crate::keychain::LEGACY_SERVICE]
            .iter()
            .find_map(
                |service| match entry(service, account).ok()?.get_password() {
                    Ok(t) => {
                        tracing::debug!(service, account, "keychain hit");
                        Some(t)
                    }
                    Err(e) => {
                        tracing::debug!(service, account, error = %e, "keychain miss");
                        None
                    }
                },
            )
    }

    pub fn set(account: &str, token: &str) -> Result<()> {
        entry(crate::keychain::SERVICE, account)?
            .set_password(token)
            .with_context(|| format!("storing the token for {account}"))
    }

    /// Removes the pre-rename entry too, so a delete can't leave a token behind under the old name.
    /// Succeeds if either entry was deleted; errors only when neither existed.
    pub fn delete(account: &str) -> Result<()> {
        let deleted = [crate::keychain::SERVICE, crate::keychain::LEGACY_SERVICE]
            .iter()
            .filter(|service| entry(service, account).is_ok_and(|e| e.delete_credential().is_ok()))
            .count();
        anyhow::ensure!(deleted > 0, "no stored token for {account}");
        Ok(())
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
