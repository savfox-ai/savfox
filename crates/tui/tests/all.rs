// Single integration test binary that aggregates all test modules.
// The submodules live in `tests/suite/`.
#[cfg(feature = "vt100-tests")]
mod test_backend;

#[allow(unused_imports)]
use savfox_cli as _; // Keep dev-dep for cargo-shear; tests spawn the savfox binary.

mod suite;
