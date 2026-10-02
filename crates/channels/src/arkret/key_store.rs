//! Arkret's platform-protected secret storage.
//!
//! Windows uses the SDK's CurrentUser DPAPI vault rather than the bounded
//! Credential Manager. Other platforms use the native keyring backend.

#[cfg(not(target_os = "windows"))]
pub(super) use savfox_keyring_store::DefaultKeyringStore as ArkretKeyringStore;

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub(super) struct ArkretKeyringStore;

#[cfg(target_os = "windows")]
fn credential_error(error: arkret::KeyStoreError) -> savfox_keyring_store::CredentialStoreError {
    savfox_keyring_store::CredentialStoreError::new(keyring::Error::PlatformFailure(Box::new(
        error,
    )))
}

#[cfg(target_os = "windows")]
impl savfox_keyring_store::KeyringStore for ArkretKeyringStore {
    fn load(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<String>, savfox_keyring_store::CredentialStoreError> {
        use arkret::KeyStore as _;
        let store = arkret::WindowsProtectedKeyStore::new(service).map_err(credential_error)?;
        match store.load(account) {
            Ok(bytes) => std::str::from_utf8(bytes.as_slice())
                .map(|value| Some(value.to_owned()))
                .map_err(|error| {
                    credential_error(arkret::KeyStoreError::backend(error.to_string()))
                }),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(credential_error(error)),
        }
    }

    fn save(
        &self,
        service: &str,
        account: &str,
        value: &str,
    ) -> Result<(), savfox_keyring_store::CredentialStoreError> {
        use arkret::KeyStore as _;
        arkret::WindowsProtectedKeyStore::new(service)
            .map_err(credential_error)?
            .store(account, value.as_bytes())
            .map_err(credential_error)
    }

    fn delete(
        &self,
        service: &str,
        account: &str,
    ) -> Result<bool, savfox_keyring_store::CredentialStoreError> {
        use arkret::KeyStore as _;
        let store = arkret::WindowsProtectedKeyStore::new(service).map_err(credential_error)?;
        match store.load(account) {
            Ok(_) => {
                store.delete(account).map_err(credential_error)?;
                Ok(true)
            }
            Err(error) if error.is_not_found() => Ok(false),
            Err(error) => Err(credential_error(error)),
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use savfox_keyring_store::KeyringStore as _;

    use super::ArkretKeyringStore;

    #[test]
    fn runtime_key_generation_reuses_the_persisted_seed_until_retirement() {
        use crate::arkret::{
            delete_ed25519_key_ref_from_keyring, get_or_generate_ed25519_key_ref_in_keyring,
            load_ed25519_seed_hex,
        };

        let service = "savfox-arkret-vault-test";
        let account = format!("runtime-agent_pairing_request-{}", uuid::Uuid::now_v7());
        let first = get_or_generate_ed25519_key_ref_in_keyring(service, &account).unwrap();
        let mut first_seed = load_ed25519_seed_hex(&first).unwrap();
        let second = get_or_generate_ed25519_key_ref_in_keyring(service, &account).unwrap();
        let mut second_seed = load_ed25519_seed_hex(&second).unwrap();
        assert_eq!(first, second);
        assert_eq!(first_seed, second_seed);
        zeroize::Zeroize::zeroize(&mut first_seed);
        zeroize::Zeroize::zeroize(&mut second_seed);
        assert!(delete_ed25519_key_ref_from_keyring(&first).unwrap());
        assert!(!delete_ed25519_key_ref_from_keyring(&first).unwrap());
        assert!(load_ed25519_seed_hex(&first).is_err());
    }

    #[test]
    fn protected_secrets_survive_reopen_and_preserve_service_isolation() {
        let account = format!("runtime-agent_pairing_request-{}", uuid::Uuid::now_v7());
        let store = ArkretKeyringStore;
        let service = "savfox-arkret-vault-test";
        let other_service = "savfox-arkret-vault-isolation-test";
        assert_eq!(store.load(service, &account).unwrap(), None);
        store.save(service, &account, "seed").unwrap();
        assert_eq!(
            ArkretKeyringStore
                .load(service, &account)
                .unwrap()
                .as_deref(),
            Some("seed")
        );
        assert_eq!(store.load(other_service, &account).unwrap(), None);
        store.save(other_service, &account, "wrapping-key").unwrap();
        store.save(service, &account, "replacement").unwrap();
        assert_eq!(
            store.load(service, &account).unwrap().as_deref(),
            Some("replacement")
        );
        assert!(store.delete(service, &account).unwrap());
        assert!(!store.delete(service, &account).unwrap());
        assert_eq!(
            store.load(other_service, &account).unwrap().as_deref(),
            Some("wrapping-key")
        );
        assert!(store.delete(other_service, &account).unwrap());
    }
}
