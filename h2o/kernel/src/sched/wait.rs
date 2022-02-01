mod cell;
mod futex;
mod queue;

use alloc::boxed::Box;
use core::time::Duration;

use crossbeam_queue::SegQueue;

pub use self::{cell::WaitCell, futex::*, queue::WaitQueue};
use super::{ipc::Arsc, *};
use crate::cpu::time::Timer;

#[derive(Debug)]
pub struct WaitObject {
    pub(super) wait_queue: SegQueue<Arsc<Timer>>,
}

unsafe impl Send for WaitObject {}
unsafe impl Sync for WaitObject {}

impl WaitObject {
    #[inline]
    pub fn new() -> Self {
        WaitObject {
            wait_queue: SegQueue::new(),
        }
    }

    #[inline]
    pub fn wait<T>(&self, guard: T, timeout: Duration, block_desc: &'static str) -> bool {
        let timer = SCHED.block_current(guard, Some(self), timeout, block_desc);
        timer.map_or(false, |timer| !timer.is_fired())
    }

    pub fn notify(&self, num: usize) -> usize {
        let num = if num == 0 { usize::MAX } else { num };

        let mut cnt = 0;
        while cnt < num {
            match self.wait_queue.pop() {
                Some(timer) if !timer.cancel() => {
                    let blocked = unsafe { Box::from_raw(timer.callback_arg().as_ptr()) };
                    SCHED.unblock(Box::into_inner(blocked));
                    cnt += 1;
                }
                Some(_) => {}
                None => break,
            }
        }
        cnt
    }
}

impl Default for WaitObject {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}
