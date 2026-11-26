//! Background CPU and I/O priority for indexing work.

/// Lowers this process to nice 10 and the idle I/O class on Linux, or the
/// Darwin background band on macOS. Best effort.
pub fn background() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        const IOPRIO_CLASS_IDLE: libc::c_long = 3;
        const IOPRIO_WHO_PROCESS: libc::c_long = 1;
        let prio = IOPRIO_CLASS_IDLE << 13;
        libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0 as libc::c_long, prio);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG as libc::c_int);
    }
}
