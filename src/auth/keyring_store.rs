use anyhow::{Context, Result};
use keyring::Entry;

const SERVICE_NAME: &str = "bitbucket-cli";
const PROBE_KEY: &str = "availability-probe";

/// Whether the keyring cannot be used at all: only a Secret Service target
/// (neither macOS nor Windows) whose probe read hit a platform failure, which
/// is how a missing D-Bus session or service surfaces. A locked collection,
/// a missing entry or any other error leaves the keyring in place so later
/// operations fail loudly.
pub fn keyring_unavailable(
    secret_service_target: bool,
    probe: &std::result::Result<String, keyring::Error>,
) -> bool {
    secret_service_target && matches!(probe, Err(keyring::Error::PlatformFailure(_)))
}

/// A place that holds named secrets outside the profile database.
///
/// The production implementation is the platform secret store; tests inject an
/// in-memory fake so they never touch the real Keychain.
pub trait SecretBackend: Send + Sync {
    /// Read the secret stored under `key`, or `None` when there is none.
    fn get(&self, key: &str) -> Result<Option<String>>;

    /// Store `value` under `key`, replacing any previous value.
    fn set(&self, key: &str, value: &str) -> Result<()>;

    /// Remove the secret under `key`; a missing entry is not an error.
    fn delete(&self, key: &str) -> Result<()>;
}

/// Secure secret storage using the platform secret store
/// (macOS Keychain, Windows Credential Manager, freedesktop Secret Service).
pub struct KeyringBackend;

impl KeyringBackend {
    /// Probe once whether this machine has a usable keyring; never touches the
    /// platform store on macOS or Windows.
    pub fn is_unavailable() -> bool {
        let secret_service_target = cfg!(not(any(target_os = "macos", target_os = "windows")));
        if !secret_service_target {
            return false;
        }
        let probe = Entry::new(SERVICE_NAME, PROBE_KEY).and_then(|entry| entry.get_password());
        keyring_unavailable(secret_service_target, &probe)
    }

    fn entry(key: &str) -> Result<Entry> {
        Entry::new(SERVICE_NAME, key).context("Failed to create keyring entry")
    }
}

impl SecretBackend for KeyringBackend {
    fn get(&self, key: &str) -> Result<Option<String>> {
        match Self::entry(key)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(
                "Failed to get credential from keyring: {}",
                e
            )),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        Self::entry(key)?
            .set_password(value)
            .context("Failed to store credential in keyring")
    }

    fn delete(&self, key: &str) -> Result<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!(
                "Failed to delete credential from keyring: {}",
                e
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Probe = std::result::Result<String, keyring::Error>;

    fn platform_failure() -> keyring::Error {
        keyring::Error::PlatformFailure("no secret service".into())
    }

    // Proves: C13 a platform failure on a Secret Service target is unavailable
    #[test]
    fn profile_c13_platform_failure_on_secret_service_target_is_unavailable() {
        assert!(keyring_unavailable(true, &Err(platform_failure())));
    }

    // Proves: C13 every other probe outcome keeps the keyring
    #[test]
    fn profile_c13_other_outcomes_on_secret_service_target_stay_available() {
        let outcomes: Vec<(&str, Probe)> = vec![
            ("no entry", Err(keyring::Error::NoEntry)),
            ("found", Ok("value".to_string())),
            (
                "locked",
                Err(keyring::Error::NoStorageAccess("locked".into())),
            ),
            (
                "invalid",
                Err(keyring::Error::Invalid("a".into(), "b".into())),
            ),
            ("too long", Err(keyring::Error::TooLong("a".into(), 1))),
            ("bad encoding", Err(keyring::Error::BadEncoding(vec![0xff]))),
            ("ambiguous", Err(keyring::Error::Ambiguous(Vec::new()))),
        ];
        for (label, probe) in outcomes {
            assert!(
                !keyring_unavailable(true, &probe),
                "{label} must stay available"
            );
        }
    }

    // Proves: C13 macOS and Windows never fall back
    #[test]
    fn profile_c13_platform_failure_on_other_targets_stays_available() {
        assert!(!keyring_unavailable(false, &Err(platform_failure())));
    }
}
