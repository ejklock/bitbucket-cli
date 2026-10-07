use anyhow::Result;

use super::{Credential, FileStore, SecretBackend};

/// Keychain key of the single credential written before profiles existed.
const LEGACY_KEY: &str = "credentials";

/// Where a legacy credential was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacySource {
    Keyring,
    File,
}

/// Read access to the pre-profile credential, kept so it can be migrated into
/// a profile and then removed.
///
/// It lived in the platform secret store, or in a plain-text file where no
/// secret store was available.
pub struct LegacyCredentialStore {
    backend: Option<Box<dyn SecretBackend>>,
    file: FileStore,
}

impl LegacyCredentialStore {
    pub fn new(backend: Option<Box<dyn SecretBackend>>, file: FileStore) -> Self {
        Self { backend, file }
    }

    /// The legacy credential and where it was found, preferring the secret
    /// store over the file.
    ///
    /// A secret store that cannot be read counts as holding nothing, so a
    /// locked keyring never blocks commands for users with no legacy entry.
    pub fn find(&self) -> Result<Option<(Credential, LegacySource)>> {
        if let Some(backend) = &self.backend {
            if let Ok(Some(json)) = backend.get(LEGACY_KEY) {
                if let Ok(credential) = serde_json::from_str::<Credential>(&json) {
                    return Ok(Some((credential, LegacySource::Keyring)));
                }
            }
        }

        Ok(self
            .file
            .get_credential()?
            .map(|credential| (credential, LegacySource::File)))
    }

    /// Delete the legacy copy of the credential found at `source`.
    pub fn remove(&self, source: LegacySource) {
        match source {
            LegacySource::Keyring => {
                if let Some(backend) = &self.backend {
                    if let Err(err) = backend.delete(LEGACY_KEY) {
                        eprintln!(
                            "Warning: could not remove legacy keyring credential: {}",
                            err
                        );
                    }
                }
            }
            LegacySource::File => {
                if let Err(err) = self.file.delete_credential() {
                    eprintln!(
                        "Warning: could not remove legacy plaintext credential file: {}",
                        err
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::profile_store::fakes::MemoryBackend;

    fn credential(key: &str) -> Credential {
        Credential::ApiKey {
            username: "u".to_string(),
            api_key: key.to_string(),
        }
    }

    fn store_at(
        dir: &tempfile::TempDir,
        backend: &MemoryBackend,
    ) -> (LegacyCredentialStore, std::path::PathBuf) {
        let path = dir.path().join("credentials.json");
        let store = LegacyCredentialStore::new(
            Some(Box::new(backend.clone())),
            FileStore::at(path.clone()),
        );
        (store, path)
    }

    // Proves: C10 a keyring hit removes only the keyring entry
    #[test]
    fn profile_c10_keyring_credential_removal_leaves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        backend
            .set(
                LEGACY_KEY,
                &serde_json::to_string(&credential("k")).unwrap(),
            )
            .unwrap();
        let (store, path) = store_at(&dir, &backend);
        std::fs::write(&path, serde_json::to_string(&credential("f")).unwrap()).unwrap();

        let (_, source) = store.find().unwrap().unwrap();
        store.remove(source);

        assert!(backend.keys().is_empty());
        assert!(path.exists());
    }

    // Proves: C10 a file hit removes only the file
    #[test]
    fn profile_c10_file_credential_removal_leaves_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        backend.set("profile:other", "x").unwrap();
        let (store, path) = store_at(&dir, &backend);
        std::fs::write(&path, serde_json::to_string(&credential("f")).unwrap()).unwrap();

        let (_, source) = store.find().unwrap().unwrap();
        assert_eq!(source, LegacySource::File);
        store.remove(source);

        assert!(!path.exists());
        assert_eq!(backend.keys(), vec!["profile:other".to_string()]);
    }

    // Proves: C10 nothing found yields nothing to remove
    #[test]
    fn profile_c10_nothing_found_yields_no_source() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        backend.set("profile:other", "x").unwrap();
        let (store, _) = store_at(&dir, &backend);

        assert!(store.find().unwrap().is_none());
        assert_eq!(backend.keys(), vec!["profile:other".to_string()]);
    }
}
