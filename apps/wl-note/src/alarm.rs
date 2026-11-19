//! The one timer for the earliest due note: a `timerfd` on the real-time
//! clock, armed for an absolute time.
//!
//! Not calloop's monotonic `Timer`: that stops while the machine sleeps,
//! so a note that fell due during a suspend would only show after the
//! time it was asleep had passed again. This one fires on wake if its
//! time went by, and with `TFD_TIMER_CANCEL_ON_SET` it is told when the
//! clock is set, so the next due time is worked out again.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::rc::Rc;

/// What a readable alarm turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The time came.
    Expired,
    /// The clock was set: what was due may have changed.
    ClockSet,
    /// Nothing (spurious).
    Nothing,
}

/// The timer. Clones share it: one goes in the event loop, the other
/// stays to be armed.
#[derive(Clone, Debug)]
pub struct Alarm(Rc<OwnedFd>);

impl AsFd for Alarm {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Alarm {
    /// A disarmed alarm.
    pub fn new() -> io::Result<Self> {
        // SAFETY: timerfd_create returns a new descriptor or -1.
        let fd = unsafe {
            libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: it is open, and ours alone.
        Ok(Self(Rc::new(unsafe { OwnedFd::from_raw_fd(fd) })))
    }

    /// Fire at `at` (Unix seconds), or never with None. A time already
    /// gone fires at once.
    pub fn set(&self, at: Option<i64>) -> io::Result<()> {
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                // An all-zero value disarms.
                tv_sec: at.map_or(0, |t| t.max(1)) as libc::time_t,
                tv_nsec: 0,
            },
        };
        let flags = match at {
            Some(_) => libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
            None => 0,
        };
        // SAFETY: the fd is ours and the spec is valid for the call.
        let r = unsafe {
            libc::timerfd_settime(self.0.as_raw_fd(), flags, &spec, std::ptr::null_mut())
        };
        match r {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Read what woke the loop up.
    pub fn wake(&self) -> Wake {
        let mut count = 0u64;
        // SAFETY: reads at most eight bytes into `count`.
        let n = unsafe {
            libc::read(
                self.0.as_raw_fd(),
                (&mut count as *mut u64).cast(),
                size_of::<u64>(),
            )
        };
        if n == size_of::<u64>() as isize {
            return Wake::Expired;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::ECANCELED) if n < 0 => Wake::ClockSet,
            _ => Wake::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_notes::due::now;

    #[test]
    fn a_time_gone_fires_at_once_and_once() {
        let alarm = Alarm::new().unwrap();
        assert_eq!(alarm.wake(), Wake::Nothing);
        alarm.set(Some(now() - 5)).unwrap();
        assert_eq!(alarm.wake(), Wake::Expired);
        assert_eq!(alarm.wake(), Wake::Nothing);
    }

    #[test]
    fn a_time_to_come_waits_and_disarming_stops_it() {
        let alarm = Alarm::new().unwrap();
        alarm.set(Some(now() + 3600)).unwrap();
        assert_eq!(alarm.wake(), Wake::Nothing);
        alarm.set(Some(now() - 1)).unwrap();
        alarm.set(None).unwrap();
        assert_eq!(alarm.wake(), Wake::Nothing);
    }

    #[test]
    fn it_fires_when_its_time_comes() {
        let alarm = Alarm::new().unwrap();
        // Aligned to the next second: it fires within about one.
        alarm.set(Some(now() + 1)).unwrap();
        assert_eq!(alarm.wake(), Wake::Nothing);
        let mut poll = libc::pollfd {
            fd: alarm.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, 2500) };
        assert_eq!(ready, 1);
        assert_eq!(alarm.wake(), Wake::Expired);
    }
}
