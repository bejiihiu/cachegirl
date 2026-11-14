//! Crash recovery for instance directories.
//!
//! Each manager instance owns a directory under the shared root and a
//! lock file naming its pid. After an unclean shutdown, stale instance
//! directories can be reclaimed safely on the next startup without ever
//! touching directories owned by live processes.

mod manager;

pub use manager::{RecoveryManager, RecoveryPolicy, RecoveryReport, is_pid_alive, lock_age};
