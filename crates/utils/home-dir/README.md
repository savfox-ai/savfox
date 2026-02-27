# savfox-utils-home-dir

Provides `find_savfox_home()`, which resolves the path to the Savfox configuration directory. The directory location is determined by the `SAVFOX_HOME` environment variable; if unset, it defaults to `~/.savfox`.

When `SAVFOX_HOME` is set, the value must point to an existing directory. The path is canonicalized before being returned, and the function returns an error if the path does not exist, is not a directory, or cannot be canonicalized. When the environment variable is absent, the function returns the default path without verifying its existence on disk, allowing callers to create it on first use.
