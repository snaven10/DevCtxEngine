//! Ctrl-C as a request, for the length of one operation that must clean up
//! after itself (PLAN-009 TASK-018: a compaction removes its temporaries and
//! releases its lock instead of dying halfway).
//!
//! While an [`Interrupts`] lives, SIGINT sets a flag instead of ending the
//! process; dropping it puts back whatever was there before. Elsewhere than
//! Unix the flag is never set and Ctrl-C keeps its default effect.

use std::sync::atomic::{AtomicBool, Ordering};

static FLAG: AtomicBool = AtomicBool::new(false);

/// SIGINT caught as a flag, until dropped.
pub struct Interrupts {
    #[cfg(unix)]
    previous: libc::sighandler_t,
}

#[cfg(unix)]
extern "C" fn on_sigint(_: libc::c_int) {
    FLAG.store(true, Ordering::SeqCst);
}

impl Interrupts {
    /// Start catching SIGINT.
    pub fn catch() -> Self {
        FLAG.store(false, Ordering::SeqCst);
        #[cfg(unix)]
        {
            let handler = on_sigint as extern "C" fn(libc::c_int);
            // SAFETY: installing a handler that only stores to an atomic,
            // which is async-signal-safe.
            let previous = unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
            Self { previous }
        }
        #[cfg(not(unix))]
        Self {}
    }

    /// Set once Ctrl-C was pressed.
    pub fn flag(&self) -> &'static AtomicBool {
        &FLAG
    }
}

impl Drop for Interrupts {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: restoring the handler `catch` replaced.
        unsafe {
            libc::signal(libc::SIGINT, self.previous);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_sigint_sets_the_flag_and_the_old_handler_comes_back() {
        let guard = Interrupts::catch();
        assert!(!guard.flag().load(Ordering::SeqCst));
        // SAFETY: signalling this process, whose handler was just installed.
        unsafe { libc::raise(libc::SIGINT) };
        assert!(guard.flag().load(Ordering::SeqCst));
        drop(guard);
        // SAFETY: reading the current handler back, then restoring it.
        let now = unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
        unsafe { libc::signal(libc::SIGINT, now) };
        assert_ne!(
            now,
            on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t
        );
    }
}
