use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::{Credential, LegacyCredentialStore, SecretBackend};
use crate::config::Config;

/// Profile created from a pre-profile credential, and the one `auth login`
/// writes when no profile was named.
pub const DEFAULT_PROFILE: &str = "default";

const DB_FILE: &str = "bitbucket.db";
const DB_ENV: &str = "BITBUCKET_DB";

/// Public metadata of a stored profile; never carries the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub name: String,
    pub method: String,
    pub username: Option<String>,
    pub default_workspace: Option<String>,
}

/// Choose the profile name asked for: the `--profile` flag, then the
/// `BITBUCKET_PROFILE` value; an empty value counts as unset.
pub fn requested_profile(flag: Option<&str>, env: Option<&str>) -> Option<String> {
    flag.filter(|v| !v.is_empty())
        .or(env.filter(|v| !v.is_empty()))
        .map(String::from)
}

/// Outcome of applying the selection rule to stored profile names.
#[derive(Debug, PartialEq, Eq)]
pub enum Selection {
    /// Nothing stored and nothing requested.
    Empty,
    One(String),
    /// Several stored profiles and none requested.
    Ambiguous(Vec<String>),
}

/// Apply the selection rule: a requested name must exist; without one, the
/// only stored profile is chosen.
pub fn classify_selection(names: &[String], requested: Option<&str>) -> Result<Selection> {
    if let Some(requested) = requested {
        anyhow::ensure!(
            names.iter().any(|n| n == requested),
            "Profile '{}' not found. Run 'bitbucket auth list' to see stored profiles.",
            requested
        );
        return Ok(Selection::One(requested.to_string()));
    }

    Ok(match names {
        [] => Selection::Empty,
        [only] => Selection::One(only.clone()),
        _ => Selection::Ambiguous(names.to_vec()),
    })
}

/// Location of the profile database: `BITBUCKET_DB` when set, otherwise
/// `bitbucket.db` in the config directory.
pub fn db_path() -> Result<PathBuf> {
    db_path_from(std::env::var(DB_ENV).ok().as_deref())
}

fn db_path_from(env: Option<&str>) -> Result<PathBuf> {
    match env.filter(|v| !v.is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(Config::config_dir()?.join(DB_FILE)),
    }
}

fn secret_key(name: &str) -> String {
    format!("profile:{}", name)
}

/// Named credentials: metadata in SQLite, secrets in a [`SecretBackend`]
/// (or in the row itself when no backend can hold them).
pub struct ProfileStore {
    conn: Connection,
    backend: Option<Box<dyn SecretBackend>>,
}

impl ProfileStore {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: &Path, backend: Option<Box<dyn SecretBackend>>) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.exists()) {
            std::fs::create_dir_all(parent)
                .context("Failed to create profile database directory")?;
            restrict_dir(parent)?;
        }

        let conn = Connection::open(path).context("Failed to open profile database")?;
        restrict_file(path)?;
        conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA busy_timeout=5000;")
            .context("Failed to configure profile database")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS profiles (
                name              TEXT PRIMARY KEY,
                method            TEXT NOT NULL,
                username          TEXT,
                default_workspace TEXT,
                secret            TEXT,
                created_at        TEXT NOT NULL
            );",
        )
        .context("Failed to initialize profile database")?;

        Ok(Self { conn, backend })
    }

    /// Whether no profile is stored.
    pub fn is_empty(&self) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM profiles", [], |r| r.get(0))
            .context("Failed to count profiles")?;
        Ok(count == 0)
    }

    /// Every stored profile, ordered by name.
    pub fn list(&self) -> Result<Vec<Profile>> {
        let mut statement = self
            .conn
            .prepare("SELECT name, method, username, default_workspace FROM profiles ORDER BY name")
            .context("Failed to list profiles")?;
        let profiles = statement
            .query_map([], profile_from_row)
            .context("Failed to list profiles")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to read profiles")?;
        Ok(profiles)
    }

    /// Metadata of one profile.
    pub fn find(&self, name: &str) -> Result<Option<Profile>> {
        self.conn
            .query_row(
                "SELECT name, method, username, default_workspace FROM profiles WHERE name = ?1",
                [name],
                profile_from_row,
            )
            .optional()
            .context("Failed to read profile")
    }

    /// Store `credential` as profile `name`, keeping the profile's existing
    /// default workspace and, for non-API-key credentials, its username.
    pub fn store(&self, name: &str, credential: &Credential) -> Result<()> {
        self.upsert(name, credential, credential.username(), None)
    }

    fn upsert(
        &self,
        name: &str,
        credential: &Credential,
        username: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(!name.is_empty(), "Profile name must not be empty");
        let json = serde_json::to_string(credential).context("Failed to serialize credential")?;

        let column = match &self.backend {
            Some(backend) => {
                backend
                    .set(&secret_key(name), &json)
                    .context("Failed to store the credential in the keyring; nothing was saved")?;
                None
            }
            None => Some(json),
        };

        self.conn
            .execute(
                "INSERT INTO profiles (name, method, username, default_workspace, secret, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(name) DO UPDATE SET
                    method = excluded.method,
                    username = COALESCE(excluded.username, profiles.username),
                    default_workspace = COALESCE(excluded.default_workspace, profiles.default_workspace),
                    secret = excluded.secret",
                params![
                    name,
                    credential.type_name(),
                    username,
                    workspace,
                    column,
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .context("Failed to store profile")?;
        Ok(())
    }

    /// The credential of profile `name`, or `None` when the profile is unknown.
    pub fn get_credential(&self, name: &str) -> Result<Option<Credential>> {
        let row: Option<Option<String>> = self
            .conn
            .query_row("SELECT secret FROM profiles WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()
            .context("Failed to read profile")?;
        let Some(column) = row else {
            return Ok(None);
        };

        let json = match (column, &self.backend) {
            (Some(json), _) => json,
            (None, Some(backend)) => backend.get(&secret_key(name))?.with_context(|| {
                format!(
                    "Credential for profile '{}' is missing from the keyring",
                    name
                )
            })?,
            (None, None) => anyhow::bail!(
                "Credential for profile '{}' is missing: no keyring is available",
                name
            ),
        };
        let credential =
            serde_json::from_str(&json).context("Failed to parse stored credential")?;
        Ok(Some(credential))
    }

    /// Set the default workspace of an existing profile.
    pub fn set_default_workspace(&self, name: &str, workspace: &str) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE profiles SET default_workspace = ?2 WHERE name = ?1",
                params![name, workspace],
            )
            .context("Failed to update profile")?;
        anyhow::ensure!(changed > 0, "Profile '{}' not found", name);
        Ok(())
    }

    /// Remove profile `name` and its secret; the other profiles are untouched.
    pub fn delete(&self, name: &str) -> Result<()> {
        anyhow::ensure!(self.find(name)?.is_some(), "Profile '{}' not found", name);

        if let Some(backend) = &self.backend {
            if let Err(err) = backend.delete(&secret_key(name)) {
                eprintln!("Warning: could not remove keyring entry: {}", err);
            }
        }
        self.conn
            .execute("DELETE FROM profiles WHERE name = ?1", [name])
            .context("Failed to delete profile")?;
        Ok(())
    }

    /// Apply the selection rule to the stored profiles.
    ///
    /// A requested name must exist; without one, the only stored profile is
    /// chosen. `None` means nothing is stored and nothing was requested.
    pub fn select(&self, requested: Option<&str>) -> Result<Option<String>> {
        let names: Vec<String> = self.list()?.into_iter().map(|p| p.name).collect();

        match classify_selection(&names, requested)? {
            Selection::Empty => Ok(None),
            Selection::One(name) => Ok(Some(name)),
            Selection::Ambiguous(names) => anyhow::bail!(
                "Multiple profiles stored ({}). Choose one with --profile or BITBUCKET_PROFILE.",
                names.join(", ")
            ),
        }
    }

    /// Turn the legacy single credential into profile `default` when no
    /// profile exists yet. Returns whether a migration happened.
    pub fn migrate_legacy(
        &self,
        legacy: &LegacyCredentialStore,
        username: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<bool> {
        if !self.is_empty()? {
            return Ok(false);
        }
        let Some((credential, source)) = legacy.find()? else {
            return Ok(false);
        };

        let username = credential.username().or(username);
        self.upsert(DEFAULT_PROFILE, &credential, username, workspace)?;
        legacy.remove(source);
        Ok(true)
    }
}

fn profile_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        name: row.get(0)?,
        method: row.get(1)?,
        username: row.get(2)?,
        default_workspace: row.get(3)?,
    })
}

#[cfg(unix)]
fn restrict_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .context("Failed to restrict profile database directory")
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .context("Failed to restrict profile database file")?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
pub(crate) mod fakes {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use anyhow::Result;

    use crate::auth::SecretBackend;

    /// In-memory secret backend; clones share the same entries.
    #[derive(Clone, Default)]
    pub struct MemoryBackend {
        entries: Arc<Mutex<HashMap<String, String>>>,
        broken: bool,
        write_fails: bool,
    }

    impl MemoryBackend {
        pub fn broken() -> Self {
            Self {
                broken: true,
                ..Self::default()
            }
        }

        /// Same entries, but every `set` fails.
        pub fn failing_writes(&self) -> Self {
            Self {
                write_fails: true,
                ..self.clone()
            }
        }

        pub fn keys(&self) -> Vec<String> {
            let mut keys: Vec<String> = self.entries.lock().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        }
    }

    impl SecretBackend for MemoryBackend {
        fn get(&self, key: &str) -> Result<Option<String>> {
            anyhow::ensure!(!self.broken, "keyring unavailable");
            Ok(self.entries.lock().unwrap().get(key).cloned())
        }

        fn set(&self, key: &str, value: &str) -> Result<()> {
            anyhow::ensure!(!self.broken && !self.write_fails, "keyring unavailable");
            self.entries
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, key: &str) -> Result<()> {
            anyhow::ensure!(!self.broken, "keyring unavailable");
            self.entries.lock().unwrap().remove(key);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::MemoryBackend;
    use super::*;
    use crate::auth::FileStore;

    fn api_key(user: &str, key: &str) -> Credential {
        Credential::ApiKey {
            username: user.to_string(),
            api_key: key.to_string(),
        }
    }

    fn oauth(token: &str) -> Credential {
        Credential::OAuth {
            access_token: token.to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: None,
            client_id: Some("id".to_string()),
            client_secret: Some("secret".to_string()),
        }
    }

    fn token_of(credential: Option<Credential>) -> String {
        match credential {
            Some(Credential::OAuth { access_token, .. }) => access_token,
            Some(Credential::ApiKey { api_key, .. }) => api_key,
            None => "<none>".to_string(),
        }
    }

    fn open_in(dir: &tempfile::TempDir, backend: &MemoryBackend) -> ProfileStore {
        ProfileStore::open(
            &dir.path().join("bitbucket.db"),
            Some(Box::new(backend.clone())),
        )
        .unwrap()
    }

    fn raw_secret_column(dir: &tempfile::TempDir, name: &str) -> Option<String> {
        let conn = Connection::open(dir.path().join("bitbucket.db")).unwrap();
        conn.query_row("SELECT secret FROM profiles WHERE name = ?1", [name], |r| {
            r.get(0)
        })
        .unwrap()
    }

    // Proves: C1 a then b leave both intact
    #[test]
    fn profile_c1_two_profiles_keep_their_own_credential_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_in(&dir, &MemoryBackend::default());
        store.store("a", &api_key("alice", "key-a")).unwrap();
        store.store("b", &oauth("token-b")).unwrap();

        assert_eq!(token_of(store.get_credential("a").unwrap()), "key-a");
        assert_eq!(token_of(store.get_credential("b").unwrap()), "token-b");
        let a = store.find("a").unwrap().unwrap();
        assert_eq!(a.method, "API Key");
        assert_eq!(a.username.as_deref(), Some("alice"));
        let b = store.find("b").unwrap().unwrap();
        assert_eq!(b.method, "OAuth 2.0");
        assert_eq!(b.username, None);
    }

    // Proves: C1 re-login of a replaces only a
    #[test]
    fn profile_c1_relogin_replaces_only_that_profile() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_in(&dir, &MemoryBackend::default());
        store.store("a", &api_key("alice", "old")).unwrap();
        store.store("b", &api_key("bob", "key-b")).unwrap();
        store.set_default_workspace("a", "ws-a").unwrap();

        store.store("a", &api_key("alice", "new")).unwrap();

        assert_eq!(token_of(store.get_credential("a").unwrap()), "new");
        assert_eq!(token_of(store.get_credential("b").unwrap()), "key-b");
        assert_eq!(
            store
                .find("a")
                .unwrap()
                .unwrap()
                .default_workspace
                .as_deref(),
            Some("ws-a")
        );
    }

    // Proves: C1 secret absent from the row when the keyring works
    #[test]
    fn profile_c1_secret_stays_out_of_the_row_when_keyring_is_available() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        let store = open_in(&dir, &backend);
        store.store("a", &api_key("alice", "key-a")).unwrap();

        assert_eq!(raw_secret_column(&dir, "a"), None);
        assert_eq!(backend.keys(), vec!["profile:a".to_string()]);
    }

    // Proves: C8 a failing keyring on a new profile errors and creates no row
    #[test]
    fn profile_c8_failing_keyring_rejects_a_new_profile() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_in(&dir, &MemoryBackend::broken());

        let error = store.store("a", &api_key("alice", "key-a")).unwrap_err();
        let message = format!("{:#}", error);

        assert!(message.contains("keyring"), "{message}");
        assert_eq!(store.find("a").unwrap(), None);
        assert!(store.is_empty().unwrap());
    }

    // Proves: C8 a failing keyring on re-login leaves the old credential intact
    #[test]
    fn profile_c8_failing_keyring_keeps_the_previous_credential_on_relogin() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        open_in(&dir, &backend)
            .store("a", &api_key("alice", "old"))
            .unwrap();
        let failing = ProfileStore::open(
            &dir.path().join("bitbucket.db"),
            Some(Box::new(backend.failing_writes())),
        )
        .unwrap();

        let error = failing.store("a", &api_key("alice", "new")).unwrap_err();
        let message = format!("{:#}", error);

        assert!(message.contains("keyring"), "{message}");
        assert_eq!(raw_secret_column(&dir, "a"), None);
        let reopened = open_in(&dir, &backend);
        assert_eq!(token_of(reopened.get_credential("a").unwrap()), "old");
    }

    // Proves: C8 no backend keeps the secret in the row
    #[test]
    fn profile_c8_without_a_backend_the_secret_is_stored_in_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(&dir.path().join("bitbucket.db"), None).unwrap();
        store.store("a", &api_key("alice", "key-a")).unwrap();

        assert!(raw_secret_column(&dir, "a").unwrap().contains("key-a"));
        assert_eq!(token_of(store.get_credential("a").unwrap()), "key-a");
    }

    // Proves: C8 a working keyring leaves the secret column NULL
    #[test]
    fn profile_c8_working_keyring_leaves_the_secret_column_null() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_in(&dir, &MemoryBackend::default());
        store.store("a", &api_key("alice", "key-a")).unwrap();

        assert_eq!(raw_secret_column(&dir, "a"), None);
    }

    // Proves: C1 no keyring at all behaves like an unavailable one
    #[test]
    fn profile_c1_secret_falls_back_to_the_row_without_a_backend() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(&dir.path().join("bitbucket.db"), None).unwrap();
        store.store("a", &api_key("alice", "key-a")).unwrap();

        assert_eq!(token_of(store.get_credential("a").unwrap()), "key-a");
    }

    fn store_with(names: &[&str], dir: &tempfile::TempDir) -> ProfileStore {
        let store = open_in(dir, &MemoryBackend::default());
        for name in names {
            store.store(name, &api_key(name, "k")).unwrap();
        }
        store
    }

    // Proves: C2 flag beats env, env alone, empty env is unset
    #[test]
    fn profile_c2_flag_beats_env_and_empty_env_is_unset() {
        assert_eq!(
            requested_profile(Some("a"), Some("b")).as_deref(),
            Some("a")
        );
        assert_eq!(requested_profile(None, Some("b")).as_deref(), Some("b"));
        assert_eq!(requested_profile(None, Some("")), None);
        assert_eq!(requested_profile(None, None), None);
    }

    // Proves: C2 single profile is chosen without a request
    #[test]
    fn profile_c2_single_profile_is_selected() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&["only"], &dir);
        assert_eq!(store.select(None).unwrap().as_deref(), Some("only"));
    }

    // Proves: C2 two profiles without a request are ambiguous
    #[test]
    fn profile_c2_two_profiles_without_request_error_with_names_and_hints() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&["work", "home"], &dir);
        let message = store.select(None).unwrap_err().to_string();
        assert!(
            message.contains("home") && message.contains("work"),
            "{message}"
        );
        assert!(message.contains("--profile"), "{message}");
        assert!(message.contains("BITBUCKET_PROFILE"), "{message}");
    }

    // Proves: C2 no profiles is the unauthenticated path
    #[test]
    fn profile_c2_no_profiles_selects_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&[], &dir);
        assert_eq!(store.select(None).unwrap(), None);
    }

    // Proves: C2 unknown requested name errors naming it
    #[test]
    fn profile_c2_unknown_requested_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(&["a"], &dir);
        let message = store.select(Some("ghost")).unwrap_err().to_string();
        assert!(message.contains("ghost"), "{message}");
        assert_eq!(store.select(Some("a")).unwrap().as_deref(), Some("a"));
    }

    // Proves: C3 logout removes only the selected profile and its secret
    #[test]
    fn profile_c3_delete_removes_row_and_keychain_entry_of_one_profile() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        let store = open_in(&dir, &backend);
        store.store("a", &api_key("alice", "key-a")).unwrap();
        store.store("b", &api_key("bob", "key-b")).unwrap();

        store.delete("a").unwrap();

        assert_eq!(store.find("a").unwrap(), None);
        assert_eq!(backend.keys(), vec!["profile:b".to_string()]);
        assert_eq!(store.select(None).unwrap().as_deref(), Some("b"));
        assert_eq!(token_of(store.get_credential("b").unwrap()), "key-b");
    }

    // Proves: C3 unknown profile errors and removes nothing
    #[test]
    fn profile_c3_delete_unknown_profile_errors_and_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        let store = open_in(&dir, &backend);
        store.store("a", &api_key("alice", "key-a")).unwrap();

        let message = store.delete("ghost").unwrap_err().to_string();

        assert!(message.contains("ghost"), "{message}");
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(backend.keys(), vec!["profile:a".to_string()]);
    }

    fn legacy_with(
        backend: &MemoryBackend,
        dir: &tempfile::TempDir,
        keyring: bool,
    ) -> LegacyCredentialStore {
        let backend: Option<Box<dyn SecretBackend>> =
            keyring.then(|| Box::new(backend.clone()) as Box<dyn SecretBackend>);
        LegacyCredentialStore::new(backend, FileStore::at(dir.path().join("credentials.json")))
    }

    // Proves: C4 legacy keychain credential becomes profile default
    #[test]
    fn profile_c4_legacy_keychain_credential_becomes_default_profile() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        backend
            .set(
                "credentials",
                &serde_json::to_string(&oauth("old")).unwrap(),
            )
            .unwrap();
        let store = open_in(&dir, &backend);
        let legacy = legacy_with(&backend, &dir, true);

        assert!(
            store
                .migrate_legacy(&legacy, Some("alice"), Some("ws"))
                .unwrap()
        );

        let profile = store.find("default").unwrap().unwrap();
        assert_eq!(profile.username.as_deref(), Some("alice"));
        assert_eq!(profile.default_workspace.as_deref(), Some("ws"));
        assert_eq!(token_of(store.get_credential("default").unwrap()), "old");
        assert_eq!(backend.keys(), vec!["profile:default".to_string()]);
    }

    // Proves: C4 second run is a no-op
    #[test]
    fn profile_c4_second_migration_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        backend
            .set(
                "credentials",
                &serde_json::to_string(&oauth("old")).unwrap(),
            )
            .unwrap();
        let store = open_in(&dir, &backend);
        let legacy = legacy_with(&backend, &dir, true);
        store.migrate_legacy(&legacy, None, None).unwrap();

        assert!(!store.migrate_legacy(&legacy, None, None).unwrap());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    // Proves: C4 existing profiles leave the legacy entry alone
    #[test]
    fn profile_c4_existing_profiles_block_migration() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        backend
            .set(
                "credentials",
                &serde_json::to_string(&oauth("old")).unwrap(),
            )
            .unwrap();
        let store = open_in(&dir, &backend);
        store.store("work", &api_key("w", "k")).unwrap();
        let legacy = legacy_with(&backend, &dir, true);

        assert!(!store.migrate_legacy(&legacy, None, None).unwrap());

        assert_eq!(store.find("default").unwrap(), None);
        assert!(backend.keys().contains(&"credentials".to_string()));
    }

    // Proves: C4 no legacy credential is not an error
    #[test]
    fn profile_c4_no_legacy_credential_leaves_table_empty() {
        let dir = tempfile::tempdir().unwrap();
        let backend = MemoryBackend::default();
        let store = open_in(&dir, &backend);
        let legacy = legacy_with(&backend, &dir, true);

        assert!(!store.migrate_legacy(&legacy, None, None).unwrap());
        assert!(store.is_empty().unwrap());
    }

    // Proves: C4 legacy file without keyring migrates the same way
    #[test]
    fn profile_c4_legacy_file_without_keyring_is_migrated_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("credentials.json");
        std::fs::write(
            &file_path,
            serde_json::to_string(&api_key("alice", "file-key")).unwrap(),
        )
        .unwrap();
        let store = ProfileStore::open(&dir.path().join("bitbucket.db"), None).unwrap();
        let legacy = LegacyCredentialStore::new(None, FileStore::at(file_path.clone()));

        assert!(store.migrate_legacy(&legacy, None, Some("ws")).unwrap());

        let profile = store.find("default").unwrap().unwrap();
        assert_eq!(profile.username.as_deref(), Some("alice"));
        assert_eq!(profile.default_workspace.as_deref(), Some("ws"));
        assert_eq!(
            token_of(store.get_credential("default").unwrap()),
            "file-key"
        );
        assert!(!file_path.exists());
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    // Proves: C15 zero, one and many names without a request
    #[test]
    fn profile_c15_classifier_without_request() {
        assert_eq!(classify_selection(&[], None).unwrap(), Selection::Empty);
        assert_eq!(
            classify_selection(&names(&["solo"]), None).unwrap(),
            Selection::One("solo".to_string())
        );
        assert_eq!(
            classify_selection(&names(&["a", "b"]), None).unwrap(),
            Selection::Ambiguous(names(&["a", "b"]))
        );
    }

    // Proves: C15 a requested name wins, an unknown one errors naming it
    #[test]
    fn profile_c15_classifier_with_request() {
        assert_eq!(
            classify_selection(&names(&["a", "b"]), Some("b")).unwrap(),
            Selection::One("b".to_string())
        );
        let message = classify_selection(&names(&["a"]), Some("ghost"))
            .unwrap_err()
            .to_string();
        assert!(message.contains("ghost"), "{message}");
    }

    // Proves: C6 concurrent writers on one database both succeed
    #[test]
    fn profile_c6_concurrent_writers_both_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitbucket.db");
        let backend = MemoryBackend::default();
        let handles: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|name| {
                let path = path.clone();
                let backend = backend.clone();
                std::thread::spawn(move || {
                    for round in 0..20 {
                        let store =
                            ProfileStore::open(&path, Some(Box::new(backend.clone()))).unwrap();
                        store
                            .store(name, &api_key(name, &format!("k{round}")))
                            .unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let store = ProfileStore::open(&path, Some(Box::new(backend))).unwrap();
        let names: Vec<String> = store.list().unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn db_path_prefers_the_env_override() {
        assert_eq!(
            db_path_from(Some("/tmp/x.db")).unwrap(),
            PathBuf::from("/tmp/x.db")
        );
        assert!(db_path_from(Some("")).unwrap().ends_with("bitbucket.db"));
    }
}
