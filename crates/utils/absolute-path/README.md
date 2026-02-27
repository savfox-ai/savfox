# savfox-utils-absolute-path

A path normalization crate that provides `AbsolutePathBuf`, a newtype wrapper around `PathBuf` guaranteeing the contained path is absolute and normalized. Paths are not required to exist on the filesystem or be canonicalized -- only that they are expressed in absolute form. On non-Windows platforms, tilde (`~`) prefixes are expanded to the user's home directory.

`AbsolutePathBuf` supports construction from absolute paths directly (`from_absolute_path`), resolution of relative paths against a base directory (`resolve_path_against_base`), and a thread-local `AbsolutePathBufGuard` mechanism that supplies a base path during Serde deserialization. The guard must be held on the deserializing thread for relative paths to resolve correctly; deserializing a relative path without a guard produces an error.

The type implements `Serialize`, `Deserialize`, `JsonSchema`, and `TS` for use across the workspace in configuration, protocol definitions, and TypeScript schema generation.
