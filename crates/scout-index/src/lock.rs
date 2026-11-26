//! Advisory file locks (flock) for writers and the per-repo daemon.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result};

pub struct FileLock {
    _file: File,
}

fn open(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening lock {}", path.display()))
}

/// Blocks until an exclusive lock on `path` is held.
pub fn lock(path: &Path) -> Result<FileLock> {
    let f = open(path)?;
    loop {
        let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        if r == 0 {
            return Ok(FileLock { _file: f });
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err).context("flock");
        }
    }
}

/// Takes an exclusive lock on `path` if nobody else holds it.
pub fn try_lock(path: &Path) -> Result<Option<FileLock>> {
    let f = open(path)?;
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if r == 0 {
        Ok(Some(FileLock { _file: f }))
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(err).context("flock")
        }
    }
}

/// Name of the lock every index writer holds while swapping the manifest.
pub const WRITE_LOCK: &str = "write.lock";
/// Name of the lock the daemon holds for its lifetime.
pub const DAEMON_LOCK: &str = "daemon.lock";
