//! The one place the server draws randomness for secrets and ids (#41).
//!
//! A failed OS random source is never papered over: a zeroed buffer would make Pairing codes,
//! API token secrets and ids predictable. Callers turn [`Unavailable`] into an internal error and
//! store nothing. Audit ids are the one exception and use their own clock fallback, because a
//! failed audit write must never fail the call (`audit.rs`, #13).

/// The OS random source failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the system random source failed")]
pub struct Unavailable;

/// Fills `bytes` from the OS random source.
pub fn fill(bytes: &mut [u8]) -> Result<(), Unavailable> {
    #[cfg(test)]
    if failing::active() {
        return Err(Unavailable);
    }
    getrandom::fill(bytes).map_err(|_| Unavailable)
}

/// Test-only switch that makes [`fill`] fail on the current thread.
#[cfg(test)]
pub(crate) mod failing {
    use std::cell::Cell;

    thread_local! {
        static FAIL: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn active() -> bool {
        FAIL.with(Cell::get)
    }

    /// Makes [`super::fill`] fail on this thread until the guard drops.
    pub(crate) fn enable() -> Guard {
        FAIL.with(|fail| fail.set(true));
        Guard
    }

    pub(crate) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            FAIL.with(|fail| fail.set(false));
        }
    }
}
