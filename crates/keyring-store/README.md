# savfox-keyring-store

A thin abstraction over the `keyring` crate for platform-native credential storage. This crate defines the `KeyringStore` trait with three operations -- `load`, `save`, and `delete` -- keyed by service name and account, and provides a `DefaultKeyringStore` implementation that delegates directly to the system keyring.

Platform-specific keyring backends are selected at compile time: Apple Keychain on macOS, Windows Credential Manager on Windows, the native sync-persistent backend on Linux, and the Secret Service API on FreeBSD/OpenBSD. Linux retains the same keyutils and Secret Service credential identities and statically builds libdbus; a running Secret Service is still required for persistent storage. The `CredentialStoreError` type wraps keyring errors with a uniform interface.

On Linux, `bash .ci/with-test-secret-service.sh cargo run --locked -p savfox-keyring-store --example native-credential-smoke` checks real credential storage in an isolated D-Bus session and temporary home. The helper requires `dbus-x11`, `gnome-keyring`, and `libsecret-tools`.

A `MockKeyringStore` is provided in the `tests` module for use in unit tests across the workspace. It stores credentials in an in-memory `HashMap` behind an `Arc<Mutex<...>>`, supports error injection, and implements the same `KeyringStore` trait.
