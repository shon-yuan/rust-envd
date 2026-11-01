//! Resource-limit guard for compose's isolated child-process HTTP test.
use nix::libc;

pub struct FileSizeLimit(libc::rlimit);
impl FileSizeLimit {
    pub fn set(soft: libc::rlim_t) -> Self {
        let mut original = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: original is writable storage for getrlimit. The caller runs in
        // a dedicated test child; the hard limit remains unchanged and Drop
        // restores the soft limit before LLVM flushes its exit-time profile.
        unsafe {
            assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut original), 0);
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limited = libc::rlimit {
                rlim_cur: soft,
                rlim_max: original.rlim_max,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limited), 0);
        }
        Self(original)
    }
}
impl Drop for FileSizeLimit {
    fn drop(&mut self) {
        // SAFETY: self.0 came from getrlimit and its hard limit was preserved.
        // Restore during unwinding too, without causing a second panic.
        let result = unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &self.0) };
        if result != 0 {
            eprintln!(
                "failed to restore compose test file size limit: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}
