//! Exercise the real OS credential store with an owned, disposable entry.
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use savfox_keyring_store::{DefaultKeyringStore, KeyringStore};

fn main() -> Result<(), Box<dyn Error>> {
    let service = match std::env::var("SAVFOX_KEYRING_SMOKE_SERVICE") {
        Ok(service) => service,
        Err(_) => format!(
            "savfox-ci-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ),
    };
    assert!(service.starts_with("savfox-ci-"));
    let account = "native-round-trip";
    let store = DefaultKeyringStore;
    match std::env::args().nth(1).as_deref() {
        Some("seed") => {
            assert!(store.load(&service, account)?.is_none());
            store.save(&service, account, "native-persistence-fixture")?;
            return Ok(());
        }
        Some("updated") => {
            assert_eq!(
                store.load(&service, account)?.as_deref(),
                Some("updated-native-fixture")
            );
            return Ok(());
        }
        Some("absent") => {
            assert!(store.load(&service, account)?.is_none());
            return Ok(());
        }
        _ => {}
    }

    if std::env::args().nth(1).as_deref() == Some("cleanup") {
        store.delete(&service, account)?;
        assert!(store.load(&service, account)?.is_none());
        return Ok(());
    }
    eprintln!("native credential stage: load");
    if std::env::var_os("SAVFOX_KEYRING_SMOKE_SEEDED").is_some() {
        assert_eq!(
            store.load(&service, account)?.as_deref(),
            Some("native-persistence-fixture")
        );
    } else {
        assert!(store.load(&service, account)?.is_none());
    }
    eprintln!("native credential stage: write");
    store.save(&service, account, "native-credential-λ")?;
    assert_eq!(
        store.load(&service, account)?.as_deref(),
        Some("native-credential-λ")
    );
    assert!(store.load(&service, "another-account")?.is_none());
    store.save(&service, account, "updated-native-fixture")?;
    assert_eq!(
        store.load(&service, account)?.as_deref(),
        Some("updated-native-fixture")
    );
    if std::env::var_os("SAVFOX_KEYRING_SMOKE_RETAIN").is_none() {
        assert!(store.delete(&service, account)?);
        assert!(store.load(&service, account)?.is_none());
        assert!(!store.delete(&service, account)?);
    }
    println!("native credential round trip passed");
    Ok(())
}
