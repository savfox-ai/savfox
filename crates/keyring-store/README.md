# savfox-keyring-store

A thin abstraction over the `keyring` crate for platform-native credential storage. This crate defines the `KeyringStore` trait with three operations -- `load`, `save`, and `delete` -- keyed by service name and account, and provides a `DefaultKeyringStore` implementation that delegates directly to the system keyring.

The keyring v4 ecosystem (`keyring-core` and explicit native stores) selects Apple Keychain on macOS, Windows Credential
Manager on Windows, and durable Secret Service on Linux/BSD. The former Linux
keyutils cache is no longer used; durable service/account identities remain in
Secret Service and are checked against the previous v3 backend. A running,
unlocked Secret Service is required; storage failures never fall back to memory.

`.ci/verify-native-credentials.py` builds an example with both actual old/new backends and checks
writes, updates, reads after process restart, and deletion across both libraries in separate processes with the same executable identity (preserving macOS Keychain ACLs).
On Linux invoke it through `bash .ci/with-test-secret-service.sh python3
.ci/verify-native-credentials.py` to isolate D-Bus and the test keyring. The helper
requires `dbus-x11`, `gnome-keyring`, and `libsecret-tools`. CI runs the same real
credential lifecycle on Linux, Windows and macOS using disposable owned entries.

A `MockKeyringStore` is provided in the `tests` module for use in unit tests across the workspace. It stores credentials in an in-memory `HashMap` behind an `Arc<Mutex<...>>`, supports error injection, and implements the same `KeyringStore` trait.
