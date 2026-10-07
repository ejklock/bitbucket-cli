pub mod api_key;
pub mod credential_store;
pub mod file_store;
pub mod keyring_store;
pub mod oauth;
pub mod profile_store;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::config::Config;
pub use api_key::*;
pub use credential_store::*;
pub use file_store::*;
pub use keyring_store::*;
pub use oauth::*;
pub use profile_store::{
    DEFAULT_PROFILE, Profile, ProfileStore, Selection, classify_selection, requested_profile,
};

/// Credential types for Bitbucket authentication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Credential {
    /// OAuth 2.0 credentials (preferred method)
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_secret: Option<String>,
    },
    /// API Key / HTTP Access Token (for automation/CI)
    ApiKey { username: String, api_key: String },
}

impl Credential {
    /// Get the authorization header value for API requests
    #[inline]
    pub fn auth_header(&self) -> String {
        match self {
            Credential::OAuth { access_token, .. } => {
                let mut result = String::with_capacity(7 + access_token.len());
                result.push_str("Bearer ");
                result.push_str(access_token);
                result
            }
            Credential::ApiKey { username, api_key } => {
                let basic = format!("{}:{}", username, api_key);
                let encoded = base64::engine::general_purpose::STANDARD.encode(basic.as_bytes());
                let mut result = String::with_capacity(6 + encoded.len());
                result.push_str("Basic ");
                result.push_str(&encoded);
                result
            }
        }
    }

    /// Get the credential type name for display
    #[inline]
    pub fn type_name(&self) -> &'static str {
        match self {
            Credential::OAuth { .. } => "OAuth 2.0",
            Credential::ApiKey { .. } => "API Key",
        }
    }

    /// Check if the credential needs refresh
    #[inline]
    pub fn needs_refresh(&self) -> bool {
        match self {
            Credential::OAuth {
                expires_at: Some(expires),
                ..
            } => {
                // Refresh if expiring within 5 minutes (300 seconds)
                *expires < chrono::Utc::now().timestamp() + 300
            }
            _ => false,
        }
    }

    /// Get stored OAuth consumer credentials (client_id, client_secret)
    pub fn oauth_consumer_credentials(&self) -> Option<(&str, &str)> {
        match self {
            Credential::OAuth {
                client_id: Some(id),
                client_secret: Some(secret),
                ..
            } => Some((id, secret)),
            _ => None,
        }
    }

    /// Get username (only available for API key credentials)
    pub fn username(&self) -> Option<&str> {
        match self {
            Credential::ApiKey { username, .. } => Some(username),
            _ => None,
        }
    }
}

/// Authentication manager: reads and writes the credential of the selected
/// profile (`--profile`, then `BITBUCKET_PROFILE`, then the only profile).
pub struct AuthManager {
    store: ProfileStore,
    requested: Option<String>,
    login_target: Option<String>,
    /// Database path while the row-storage warning is still owed.
    row_storage_notice: Mutex<Option<PathBuf>>,
}

impl AuthManager {
    /// Open the profile database, migrating a pre-profile credential into
    /// profile `default` on first use.
    ///
    /// Where no Secret Service is reachable (Linux/BSD only), secrets go into
    /// the profile database instead, with a warning on the first write.
    pub fn new() -> Result<Self> {
        let keyring_usable = !KeyringBackend::is_unavailable();
        let backend = || -> Option<Box<dyn SecretBackend>> {
            keyring_usable.then(|| Box::new(KeyringBackend) as Box<dyn SecretBackend>)
        };
        let requested = requested_profile(
            crate::cli::profile_override().as_deref(),
            std::env::var("BITBUCKET_PROFILE").ok().as_deref(),
        );
        let config = Config::load().ok();
        Self::open_with(
            &profile_store::db_path()?,
            backend(),
            LegacyCredentialStore::new(backend(), FileStore::new()?),
            config.as_ref(),
            requested,
        )
    }

    /// Open the database at `db` over the given secret backend (`None` keeps
    /// secrets in the row), migrating a pre-profile credential on first use.
    fn open_with(
        db: &Path,
        backend: Option<Box<dyn SecretBackend>>,
        legacy: LegacyCredentialStore,
        config: Option<&Config>,
        requested: Option<String>,
    ) -> Result<Self> {
        let row_storage = backend.is_none();
        let store = ProfileStore::open(db, backend)?;
        let manager = Self {
            store,
            requested,
            login_target: None,
            row_storage_notice: Mutex::new(row_storage.then(|| db.to_path_buf())),
        };

        if manager.store.is_empty()? {
            let migrated = manager.store.migrate_legacy(
                &legacy,
                config.and_then(|c| c.username()),
                config.and_then(|c| c.default_workspace()),
            )?;
            if migrated {
                manager.announce_row_storage();
            }
        }
        Ok(manager)
    }

    /// Manager over an already opened store; `requested` is the profile name
    /// chosen by flag or environment, if any.
    pub fn with_store(store: ProfileStore, requested: Option<String>) -> Self {
        Self {
            store,
            requested,
            login_target: None,
            row_storage_notice: Mutex::new(None),
        }
    }

    /// The one-time warning for a write into the profile database, if it is
    /// still owed.
    fn row_storage_warning(&self) -> Option<String> {
        let path = self.row_storage_notice.lock().ok()?.take()?;
        Some(format!(
            "Warning: no Secret Service available; storing the credential in the profile database ({}, mode 0600)",
            path.display()
        ))
    }

    fn announce_row_storage(&self) {
        if let Some(warning) = self.row_storage_warning() {
            eprintln!("{}", warning);
        }
    }

    /// Aim every read and write at the profile `auth login` creates or
    /// replaces: the requested one, else `default`.
    pub fn for_login(mut self) -> Self {
        let target = self
            .requested
            .clone()
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string());
        self.login_target = Some(target);
        self
    }

    /// The profile name chosen by flag or environment, if any.
    pub fn requested(&self) -> Option<&str> {
        self.requested.as_deref()
    }

    /// Name of the selected profile, or `None` when no profile is stored.
    pub fn selected_profile(&self) -> Result<Option<String>> {
        match &self.login_target {
            Some(target) => Ok(Some(target.clone())),
            None => self.store.select(self.requested.as_deref()),
        }
    }

    /// Metadata of the selected profile, or `None` when nothing is selected.
    pub fn selected_metadata(&self) -> Result<Option<Profile>> {
        match self.selected_profile()? {
            Some(name) => self.store.find(&name),
            None => Ok(None),
        }
    }

    /// Metadata of every stored profile.
    pub fn profiles(&self) -> Result<Vec<Profile>> {
        self.store.list()
    }

    /// Get the credentials of the selected profile
    pub fn get_credentials(&self) -> Result<Option<Credential>> {
        match self.selected_profile()? {
            Some(name) => self.store.get_credential(&name),
            None => Ok(None),
        }
    }

    /// Store credentials into the selected profile (`default` when none exists yet)
    pub fn store_credentials(&self, credential: &Credential) -> Result<()> {
        let name = self
            .selected_profile()?
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string());
        self.announce_row_storage();
        self.store.store(&name, credential)
    }

    /// Record the default workspace of the selected profile
    pub fn set_default_workspace(&self, workspace: &str) -> Result<()> {
        let name = self
            .selected_profile()?
            .context("Not authenticated. Run 'bitbucket auth login' first.")?;
        self.store.set_default_workspace(&name, workspace)
    }

    /// Remove the selected profile and its secret, leaving the others alone.
    /// Returns the removed name and whether any profile remains.
    pub fn clear_credentials(&self) -> Result<(String, bool)> {
        let name = self
            .selected_profile()?
            .context("Not authenticated. Run 'bitbucket auth login' first.")?;
        self.store.delete(&name)?;
        Ok((name, !self.store.is_empty()?))
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.get_credentials().map(|c| c.is_some()).unwrap_or(false)
    }
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new().expect("Failed to create auth manager")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth_credential(expires_at: Option<i64>) -> Credential {
        Credential::OAuth {
            access_token: "test-access-token".to_string(),
            refresh_token: None,
            expires_at,
            client_id: None,
            client_secret: None,
        }
    }

    #[test]
    fn oauth_auth_header_is_bearer_token() {
        let credential = oauth_credential(None);
        assert_eq!(credential.auth_header(), "Bearer test-access-token");
    }

    #[test]
    fn api_key_auth_header_is_basic_base64() {
        let credential = Credential::ApiKey {
            username: "alice".to_string(),
            api_key: "s3cret-key".to_string(),
        };
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("alice:s3cret-key")
        );
        assert_eq!(credential.auth_header(), expected);
    }

    #[test]
    fn api_key_never_needs_refresh() {
        let credential = Credential::ApiKey {
            username: "alice".to_string(),
            api_key: "s3cret-key".to_string(),
        };
        assert!(!credential.needs_refresh());
    }

    #[test]
    fn oauth_without_expiry_never_needs_refresh() {
        let credential = oauth_credential(None);
        assert!(!credential.needs_refresh());
    }

    #[test]
    fn oauth_expiring_beyond_five_minutes_does_not_need_refresh() {
        // 10 minutes from now: well past the 5-minute refresh window
        let expires_at = chrono::Utc::now().timestamp() + 600;
        let credential = oauth_credential(Some(expires_at));
        assert!(!credential.needs_refresh());
    }

    #[test]
    fn oauth_expiring_within_five_minutes_needs_refresh() {
        // 1 minute from now: inside the 5-minute refresh window
        let expires_at = chrono::Utc::now().timestamp() + 60;
        let credential = oauth_credential(Some(expires_at));
        assert!(credential.needs_refresh());
    }

    #[test]
    fn oauth_already_expired_needs_refresh() {
        let expires_at = chrono::Utc::now().timestamp() - 600;
        let credential = oauth_credential(Some(expires_at));
        assert!(credential.needs_refresh());
    }

    fn manager(dir: &tempfile::TempDir, profiles: &[&str], requested: Option<&str>) -> AuthManager {
        let store = ProfileStore::open(
            &dir.path().join("bitbucket.db"),
            Some(Box::new(profile_store::fakes::MemoryBackend::default())),
        )
        .unwrap();
        for name in profiles {
            store
                .store(
                    name,
                    &Credential::ApiKey {
                        username: name.to_string(),
                        api_key: format!("key-{name}"),
                    },
                )
                .unwrap();
        }
        AuthManager::with_store(store, requested.map(String::from))
    }

    fn api_key_of(credential: Option<Credential>) -> String {
        match credential {
            Some(Credential::ApiKey { api_key, .. }) => api_key,
            Some(Credential::OAuth { access_token, .. }) => access_token,
            None => "<none>".to_string(),
        }
    }

    // Proves: C5 a refresh writes back to the requested profile only
    #[test]
    fn profile_c5_store_credentials_writes_to_the_requested_profile() {
        let dir = tempfile::tempdir().unwrap();
        let auth = manager(&dir, &["a", "b"], Some("a"));

        auth.store_credentials(&oauth_credential(None)).unwrap();

        assert_eq!(
            api_key_of(auth.get_credentials().unwrap()),
            "test-access-token"
        );
        assert_eq!(api_key_of(auth.store.get_credential("b").unwrap()), "key-b");
    }

    // Proves: C5 a refresh with a single profile writes to that profile, not default
    #[test]
    fn profile_c5_store_credentials_targets_the_only_profile() {
        let dir = tempfile::tempdir().unwrap();
        let auth = manager(&dir, &["work"], None);

        auth.store_credentials(&oauth_credential(None)).unwrap();

        let names: Vec<String> = auth
            .profiles()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["work".to_string()]);
        assert_eq!(
            api_key_of(auth.get_credentials().unwrap()),
            "test-access-token"
        );
    }

    // Proves: C5 login without a name creates default beside existing profiles
    #[test]
    fn profile_c5_login_without_a_name_targets_default() {
        let dir = tempfile::tempdir().unwrap();
        let auth = manager(&dir, &["work"], None).for_login();

        auth.store_credentials(&oauth_credential(None)).unwrap();

        let names: Vec<String> = auth
            .profiles()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["default".to_string(), "work".to_string()]);
        assert_eq!(
            api_key_of(auth.store.get_credential("work").unwrap()),
            "key-work"
        );
    }

    // Proves: C2 no profiles keeps the unauthenticated path
    #[test]
    fn profile_c2_no_profiles_yields_no_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let auth = manager(&dir, &[], None);
        assert!(auth.get_credentials().unwrap().is_none());
        assert!(!auth.is_authenticated());
    }

    // Proves: C3 logout of the selected profile keeps the others
    #[test]
    fn profile_c3_clear_credentials_removes_only_the_selected_profile() {
        let dir = tempfile::tempdir().unwrap();
        let auth = manager(&dir, &["a", "b"], Some("a"));

        let (removed, others_remain) = auth.clear_credentials().unwrap();

        assert_eq!(removed, "a");
        assert!(others_remain);
        let remaining = AuthManager::with_store(
            ProfileStore::open(&dir.path().join("bitbucket.db"), None).unwrap(),
            None,
        );
        assert_eq!(remaining.selected_profile().unwrap().as_deref(), Some("b"));
    }

    // Proves: C3 logout of an unknown profile errors
    #[test]
    fn profile_c3_clear_credentials_of_unknown_profile_errors() {
        let dir = tempfile::tempdir().unwrap();
        let auth = manager(&dir, &["a"], Some("ghost"));
        let message = auth.clear_credentials().unwrap_err().to_string();
        assert!(message.contains("ghost"), "{message}");
        assert_eq!(auth.profiles().unwrap().len(), 1);
    }

    fn row_secret(db: &Path) -> Option<String> {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row(
                "SELECT secret FROM profiles WHERE name = 'default'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// An AuthManager over a temp database. The legacy file store points at a
    /// `credentials.json` that exists only when the test writes it.
    fn open_with_fakes(
        dir: &tempfile::TempDir,
        keyring: Option<&profile_store::fakes::MemoryBackend>,
    ) -> AuthManager {
        let backend = || -> Option<Box<dyn SecretBackend>> {
            keyring.map(|k| Box::new(k.clone()) as Box<dyn SecretBackend>)
        };
        AuthManager::open_with(
            &dir.path().join("bitbucket.db"),
            backend(),
            LegacyCredentialStore::new(
                backend(),
                FileStore::at(dir.path().join("credentials.json")),
            ),
            None,
            None,
        )
        .unwrap()
    }

    // Proves: C14 without a keyring login stores the secret in the row and reads it back
    #[test]
    fn profile_c14_unavailable_keyring_stores_the_secret_in_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let auth = open_with_fakes(&dir, None).for_login();

        auth.store_credentials(&oauth_credential(None)).unwrap();

        let stored = row_secret(&dir.path().join("bitbucket.db")).unwrap_or_default();
        assert!(stored.contains("test-access-token"), "row holds {stored:?}");
        assert_eq!(
            api_key_of(auth.get_credentials().unwrap()),
            "test-access-token"
        );
    }

    // Proves: C14 without a keyring migration reads only the legacy file
    #[test]
    fn profile_c14_unavailable_keyring_migrates_the_legacy_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials.json");
        FileStore::at(file.clone())
            .store_credential(&oauth_credential(None))
            .unwrap();

        let auth = open_with_fakes(&dir, None);

        assert_eq!(
            api_key_of(auth.get_credentials().unwrap()),
            "test-access-token"
        );
        assert!(!file.exists());
        assert!(row_secret(&dir.path().join("bitbucket.db")).is_some());
    }

    // Proves: C14 the row-storage warning is owed once and names the path, never the secret
    #[test]
    fn profile_c14_row_storage_warning_is_issued_once_without_secret() {
        let dir = tempfile::tempdir().unwrap();
        let auth = open_with_fakes(&dir, None);
        let db = dir.path().join("bitbucket.db");

        let first = auth.row_storage_warning().unwrap_or_default();
        assert!(first.starts_with("Warning: no Secret Service available"));
        assert!(first.contains(&db.display().to_string()), "{first}");
        assert!(first.contains("mode 0600"), "{first}");
        assert!(!first.contains("test-access-token"));
        assert_eq!(auth.row_storage_warning(), None);
    }

    // Proves: C14 an available keyring keeps the row empty and owes no warning
    #[test]
    fn profile_c14_available_keyring_keeps_the_secret_out_of_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let keyring = profile_store::fakes::MemoryBackend::default();
        let auth = open_with_fakes(&dir, Some(&keyring)).for_login();

        auth.store_credentials(&oauth_credential(None)).unwrap();

        assert_eq!(row_secret(&dir.path().join("bitbucket.db")), None);
        assert_eq!(keyring.keys(), vec!["profile:default".to_string()]);
        assert_eq!(auth.row_storage_warning(), None);
    }
}
