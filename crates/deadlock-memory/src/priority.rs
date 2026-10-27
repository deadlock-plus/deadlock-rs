//! Scheduling priority of the calling thread, for long scans that run beside the game.

use std::marker::PhantomData;

/// Restores the calling thread's priority when dropped.
///
/// Not `Send`: thread priority belongs to one thread, and restoring it from another would
/// change the wrong thread.
#[derive(Debug)]
#[must_use = "the priority is restored as soon as the guard is dropped"]
pub struct Lowered {
    #[cfg(windows)]
    restore: Option<i32>,
    _thread: PhantomData<*const ()>,
}

/// Drop the calling thread to below-normal priority until the guard is dropped.
///
/// A thread already at or below that level is left alone. Windows only; elsewhere this does
/// nothing.
pub fn lower() -> Lowered {
    Lowered {
        #[cfg(windows)]
        restore: windows::lower(),
        _thread: PhantomData,
    }
}

/// Whether the calling thread runs below normal priority. Always false off Windows.
pub fn is_lowered() -> bool {
    #[cfg(windows)]
    {
        windows::current() < 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

impl Drop for Lowered {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(previous) = self.restore {
            windows::set(previous);
        }
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL;

    pub(super) use crate::process::{set_thread_priority as set, thread_priority as current};

    pub(super) fn lower() -> Option<i32> {
        let previous = current();
        (previous > THREAD_PRIORITY_BELOW_NORMAL && set(THREAD_PRIORITY_BELOW_NORMAL))
            .then_some(previous)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn the_guard_lowers_the_thread_and_restores_it() {
        std::thread::spawn(|| {
            assert!(!is_lowered());
            {
                let _low = lower();
                assert!(is_lowered());
            }
            assert!(!is_lowered());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_thread_already_lowered_further_is_not_raised() {
        std::thread::spawn(|| {
            let lowest = windows_sys::Win32::System::Threading::THREAD_PRIORITY_LOWEST;
            assert!(windows::set(lowest));
            {
                let _low = lower();
                assert_eq!(windows::current(), lowest);
            }
            assert_eq!(windows::current(), lowest);
        })
        .join()
        .unwrap();
    }
}
