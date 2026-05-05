//! OS-keyring–backed credential storage for Savfox secrets.
//!
//! The crate exposes a small [`KeyringStore`] trait whose canonical
//! implementation, [`DefaultKeyringStore`], delegates to the [`keyring`]
//! crate. Each call ultimately reaches a synchronous platform API
//! (DPAPI on Windows, Secret Service on Linux, the macOS Keychain),
//! which the trait makes pluggable so higher layers can substitute a
//! [`tests::MockKeyringStore`] in test code.
//!
//! # Security
//!
//! Secrets stored through this module live in the user's OS keyring under
//! the service name supplied by the caller. The OS — not Savfox — owns
//! access control: encryption at rest, ACL, and per-user isolation are
//! delegated to the platform's standard credential vault.
//!
//! # Example
//!
//! ```
//! use savfox_keyring_store::{KeyringStore, tests::MockKeyringStore};
//!
//! let store = MockKeyringStore::default();
//! store.save("savfox-gateway", "token", "abc123").unwrap();
//! let value = store.load("savfox-gateway", "token").unwrap();
//! assert_eq!(value.as_deref(), Some("abc123"));
//! ```

use std::error::Error;
use std::fmt;
use std::fmt::Debug;

use keyring::{Entry, Error as KeyringError};
use tracing::trace;

/// Errors that can be returned by the credential-store helpers.
///
/// Currently this is a single-variant wrapper around [`KeyringError`]; the
/// enum exists so we can grow it (e.g. `Locked`, `Unavailable`) without
/// breaking downstream `match` exhaustiveness expectations.
#[derive(Debug)]
pub enum CredentialStoreError {
    /// An error reported by the underlying [`keyring`] crate.
    Other(KeyringError),
}

impl CredentialStoreError {
    #[must_use]
    pub fn new(error: KeyringError) -> Self {
        Self::Other(error)
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Other(error) => error.to_string(),
        }
    }

    #[must_use]
    pub fn into_error(self) -> KeyringError {
        match self {
            Self::Other(error) => error,
        }
    }
}

impl fmt::Display for CredentialStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Other(error) => write!(f, "{error}"),
        }
    }
}

impl Error for CredentialStoreError {}

/// Shared credential store abstraction for keyring-backed implementations.
pub trait KeyringStore: Debug + Send + Sync {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, CredentialStoreError>;
    fn save(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError>;
    fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError>;
}

#[derive(Debug)]
pub struct DefaultKeyringStore;

impl KeyringStore for DefaultKeyringStore {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, CredentialStoreError> {
        trace!("keyring.load start, service={service}, account={account}");
        let entry = Entry::new(service, account).map_err(CredentialStoreError::new)?;
        match entry.get_password() {
            Ok(password) => {
                trace!("keyring.load success, service={service}, account={account}");
                Ok(Some(password))
            }
            Err(keyring::Error::NoEntry) => {
                trace!("keyring.load no entry, service={service}, account={account}");
                Ok(None)
            }
            Err(error) => {
                trace!("keyring.load error, service={service}, account={account}, error={error}");
                Err(CredentialStoreError::new(error))
            }
        }
    }

    fn save(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError> {
        trace!(
            "keyring.save start, service={service}, account={account}, value_len={}",
            value.len()
        );
        let entry = Entry::new(service, account).map_err(CredentialStoreError::new)?;
        match entry.set_password(value) {
            Ok(()) => {
                trace!("keyring.save success, service={service}, account={account}");
                Ok(())
            }
            Err(error) => {
                trace!("keyring.save error, service={service}, account={account}, error={error}");
                Err(CredentialStoreError::new(error))
            }
        }
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError> {
        trace!("keyring.delete start, service={service}, account={account}");
        let entry = Entry::new(service, account).map_err(CredentialStoreError::new)?;
        match entry.delete_credential() {
            Ok(()) => {
                trace!("keyring.delete success, service={service}, account={account}");
                Ok(true)
            }
            Err(keyring::Error::NoEntry) => {
                trace!("keyring.delete no entry, service={service}, account={account}");
                Ok(false)
            }
            Err(error) => {
                trace!("keyring.delete error, service={service}, account={account}, error={error}");
                Err(CredentialStoreError::new(error))
            }
        }
    }
}

pub mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use keyring::Error as KeyringError;
    use keyring::credential::CredentialApi as _;
    use keyring::mock::MockCredential;

    use super::{CredentialStoreError, KeyringStore};

    #[derive(Default, Clone, Debug)]
    pub struct MockKeyringStore {
        credentials: Arc<Mutex<HashMap<String, Arc<MockCredential>>>>,
    }

    impl MockKeyringStore {
        pub fn credential(&self, account: &str) -> Arc<MockCredential> {
            let mut guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard
                .entry(account.to_owned())
                .or_insert_with(|| Arc::new(MockCredential::default()))
                .clone()
        }

        pub fn saved_value(&self, account: &str) -> Option<String> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            }?;
            credential.get_password().ok()
        }

        pub fn set_error(&self, account: &str, error: KeyringError) {
            let credential = self.credential(account);
            credential.set_error(error);
        }

        pub fn contains(&self, account: &str) -> bool {
            let guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard.contains_key(account)
        }
    }

    impl KeyringStore for MockKeyringStore {
        fn load(
            &self,
            _service: &str,
            account: &str,
        ) -> Result<Option<String>, CredentialStoreError> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            };

            let Some(credential) = credential else {
                return Ok(None);
            };

            match credential.get_password() {
                Ok(password) => Ok(Some(password)),
                Err(KeyringError::NoEntry) => Ok(None),
                Err(error) => Err(CredentialStoreError::new(error)),
            }
        }

        fn save(
            &self,
            _service: &str,
            account: &str,
            value: &str,
        ) -> Result<(), CredentialStoreError> {
            let credential = self.credential(account);
            credential
                .set_password(value)
                .map_err(CredentialStoreError::new)
        }

        fn delete(&self, _service: &str, account: &str) -> Result<bool, CredentialStoreError> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            };

            let Some(credential) = credential else {
                return Ok(false);
            };

            let removed = match credential.delete_credential() {
                Ok(()) => Ok(true),
                Err(KeyringError::NoEntry) => Ok(false),
                Err(error) => Err(CredentialStoreError::new(error)),
            }?;

            let mut guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard.remove(account);
            Ok(removed)
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::tests::MockKeyringStore;
    use super::{CredentialStoreError, KeyringStore};
    use keyring::Error as KeyringError;

    const SERVICE: &str = "savfox-test";

    #[test]
    fn save_then_load_round_trip() {
        let store = MockKeyringStore::default();
        store.save(SERVICE, "alice", "secret-1").unwrap();
        assert_eq!(
            store.load(SERVICE, "alice").unwrap().as_deref(),
            Some("secret-1")
        );
    }

    #[test]
    fn load_returns_none_for_missing_account() {
        let store = MockKeyringStore::default();
        assert!(store.load(SERVICE, "ghost").unwrap().is_none());
    }

    #[test]
    fn save_overwrites_existing_value() {
        let store = MockKeyringStore::default();
        store.save(SERVICE, "alice", "v1").unwrap();
        store.save(SERVICE, "alice", "v2").unwrap();
        assert_eq!(store.load(SERVICE, "alice").unwrap().as_deref(), Some("v2"));
    }

    #[test]
    fn delete_removes_existing_entry_and_reports_true() {
        let store = MockKeyringStore::default();
        store.save(SERVICE, "alice", "secret").unwrap();
        assert!(store.delete(SERVICE, "alice").unwrap());
        assert!(store.load(SERVICE, "alice").unwrap().is_none());
        assert!(!store.contains("alice"));
    }

    #[test]
    fn delete_missing_entry_returns_false() {
        let store = MockKeyringStore::default();
        assert!(!store.delete(SERVICE, "nope").unwrap());
    }

    #[test]
    fn save_propagates_backend_error() {
        let store = MockKeyringStore::default();
        store.set_error("alice", KeyringError::Invalid("svc".into(), "acct".into()));
        let err = store
            .save(SERVICE, "alice", "value")
            .expect_err("backend error should bubble up");
        assert!(matches!(err, CredentialStoreError::Other(_)));
        // Display surface preserved.
        assert!(!err.message().is_empty());
    }

    #[test]
    fn into_error_returns_inner_keyring_error() {
        let backend = KeyringError::Invalid("svc".into(), "acct".into());
        let wrapped = CredentialStoreError::new(backend);
        // Display surface should be non-empty before unwrapping.
        let inner = wrapped.into_error();
        assert!(matches!(inner, KeyringError::Invalid(_, _)));
    }

    #[test]
    fn message_round_trips_inner_display() {
        let backend = KeyringError::NoEntry;
        let inner_str = backend.to_string();
        let wrapped = CredentialStoreError::new(backend);
        assert_eq!(wrapped.message(), inner_str);
    }
}
