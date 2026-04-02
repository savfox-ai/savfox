#[cfg(unix)]
#[allow(unreachable_pub)]
mod posix;

#[cfg(unix)]
pub use posix::ExecResult;
#[cfg(unix)]
pub use posix::main_execve_wrapper;
#[cfg(unix)]
pub use posix::main_mcp_server;
