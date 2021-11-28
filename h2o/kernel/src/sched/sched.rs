pub mod deque;
pub mod epoch;

use alloc::vec::Vec;
use core::{cell::UnsafeCell, mem, time::Duration};

use archop::{PreemptState, PreemptStateGuard};
use canary::Canary;
use deque::{Injector, Steal, Worker};
use spin::Lazy;

use super::task;
use crate::cpu::time::Instant;

const MINIMUM_TIME_GRANULARITY: Duration = Duration::from_millis(30);
const WAKE_TIME_GRANULARITY: Duration = Duration::from_millis(1);

static MIGRATION_QUEUE: Lazy<Vec<Injector<task::Ready>>> = Lazy::new(|| {
    let count = crate::cpu::count();
    core::iter::repeat_with(Injector::new).take(count).collect()
});

#[thread_local]
pub static SCHED: Lazy<Scheduler> = Lazy::new(|| Scheduler {
    canary: Canary::new(),
    cpu: unsafe { crate::cpu::id() },
    current: UnsafeCell::new(None),
    run_queue: Worker::new_fifo(),
});

#[thread_local]
pub static PREEMPT: PreemptState = PreemptState::new();

pub struct Scheduler {
    canary: Canary<Scheduler>,
    cpu: usize,
    run_queue: Worker<task::Ready>,
    current: UnsafeCell<Option<task::Ready>>,
}

impl Scheduler {
    pub fn push(&self, task: task::Init) {
        self.canary.assert();
        log::trace!(
            "Pushing new task {:?}, P{}",
            task.tid().raw(),
            PREEMPT.raw(),
        );

        let affinity = task.tid().info().read().affinity();

        let time_slice = MINIMUM_TIME_GRANULARITY;
        if !affinity.get(self.cpu).map_or(false, |r| *r) {
            let cpu = select_cpu(&affinity).expect("Zero affinity");
            let task = task::Ready::from_init(task, cpu, time_slice);
            MIGRATION_QUEUE[cpu].push(task);

            unsafe { crate::cpu::arch::apic::ipi::task_migrate(cpu) };
        } else {
            let task = task::Ready::from_init(task, self.cpu, time_slice);

            let pree = PREEMPT.lock();
            self.enqueue(task, pree);
        }
    }

    #[inline]
    fn enqueue(&self, task: task::Ready, pree: PreemptStateGuard) {
        // SAFE: We have `pree`, which means preemption is disabled.
        match unsafe { &*self.current.get() } {
            Some(ref cur) if Self::should_preempt(&cur, &task) => {
                log::trace!(
                    "Preempting to task {:?}, P{}",
                    task.tid().raw(),
                    PREEMPT.raw(),
                );
                self.schedule_impl(Instant::now(), pree, Some(task), |mut task| {
                    task.running_state = task::RunningState::NotRunning;
                    self.run_queue.push(task);
                });
            }
            _ => self.run_queue.push(task),
        }
    }

    pub fn with_current<F, R>(&self, func: F) -> Option<R>
    where
        F: FnOnce(&mut task::Ready) -> R,
    {
        self.canary.assert();

        let _pree = PREEMPT.lock();
        // SAFE: We have `_pree`, which means preemption is disabled.
        unsafe { (*self.current.get()).as_mut().map(func) }
    }

    pub fn current(&self) -> *mut Option<task::Ready> {
        self.current.get()
    }

    #[inline]
    pub unsafe fn preempt_current(&self) {
        self.canary.assert();

        let _pree = PREEMPT.lock();
        unsafe {
            // SAFE: We have `_pree`, which means preemption is disabled.
            if let Some(ref mut cur) = *self.current.get() {
                cur.running_state = task::RunningState::NeedResched;
            }
        }
    }

    pub fn block_current<T>(
        &self,
        cur_time: Instant,
        guard: T,
        wo: &super::wait::WaitObject,
        block_desc: &'static str,
    ) -> bool {
        self.canary.assert();

        let pree = PREEMPT.lock();

        // SAFE: We have `pree`, which means preemption is disabled.
        log::trace!(
            "Blocking task {:?}, P{}",
            unsafe { &*self.current.get() }
                .as_ref()
                .unwrap()
                .tid()
                .raw(),
            PREEMPT.raw(),
        );
        self.schedule_impl(cur_time, pree, None, |task| {
            task::Ready::block(task, wo, block_desc);
            drop(guard);
        })
        .is_some()
    }

    pub fn unblock(&self, task: task::Blocked) {
        self.canary.assert();
        log::trace!("Unblocking task {:?}, P{}", task.tid().raw(), PREEMPT.raw());

        let time_slice = MINIMUM_TIME_GRANULARITY;
        let task = task::Ready::unblock(task, time_slice);
        if task.cpu == self.cpu {
            let pree = PREEMPT.lock();
            unsafe { self.enqueue(task, pree) };
        } else {
            let cpu = task.cpu;
            MIGRATION_QUEUE[cpu].push(task);
            unsafe { crate::cpu::arch::apic::ipi::task_migrate(cpu) };
        }
    }

    #[inline]
    fn should_preempt(cur: &task::Ready, task: &task::Ready) -> bool {
        cur.runtime > task.runtime + WAKE_TIME_GRANULARITY
    }

    pub fn exit_current(&self, retval: usize) -> ! {
        self.canary.assert();
        let pree = PREEMPT.lock();

        // SAFE: We have `pree`, which means preemption is disabled.
        log::trace!(
            "Exiting task {:?}, P{}",
            unsafe { &*self.current.get() }
                .as_ref()
                .unwrap()
                .tid()
                .raw(),
            PREEMPT.raw(),
        );
        self.schedule_impl(Instant::now(), pree, None, |task| {
            task::Ready::exit(task, retval)
        });
        unreachable!("Dead task");
    }

    pub fn tick(&self, mut cur_time: Instant) {
        // log::trace!("Scheduler tick");

        let pree = PREEMPT.lock();
        let pree = match self.check_signal(cur_time, pree) {
            Some(pree) => pree,
            None => {
                cur_time = Instant::now();
                PREEMPT.lock()
            }
        };

        if unsafe { self.update(cur_time) } {
            self.schedule(cur_time, pree);
        }
    }

    fn check_signal<'a>(
        &'a self,
        cur_time: Instant,
        pree: PreemptStateGuard<'a>,
    ) -> Option<PreemptStateGuard<'a>> {
        // SAFE: We have `pree`, which means preemption is disabled.
        let cur = match unsafe { &*self.current.get() } {
            Some(ref cur) => cur,
            None => return Some(pree),
        };
        log::trace!("Checking task {:?}'s pending signal", cur.tid().raw());
        let mut ti = cur.tid().info().write();

        if ti.ty() == task::Type::Kernel {
            return Some(pree);
        }

        match ti.take_signal() {
            Some(task::sig::Signal::Kill) => {
                drop(ti);
                log::trace!("Killing task {:?}, P{}", cur.tid().raw(), PREEMPT.raw());
                self.schedule_impl(cur_time, pree, None, |task| {
                    task::Ready::exit(task, (-solvent::EKILLED) as usize)
                });
                unreachable!("Dead task");
            }
            Some(task::sig::Signal::Suspend(wo)) => {
                drop(ti);

                log::trace!("Suspending task {:?}, P{}", cur.tid().raw(), PREEMPT.raw());
                self.schedule_impl(cur_time, pree, None, |task| {
                    task::Ready::block(task, &wo, "task_ctl_suspend");
                });

                None
            }
            None => Some(pree),
        }
    }

    unsafe fn update(&self, cur_time: Instant) -> bool {
        self.canary.assert();

        let sole = self.run_queue.is_empty();
        let cur = match *self.current.get() {
            Some(ref mut task) => task,
            None => return !sole,
        };
        // log::trace!("Updating task {:?}'s timer slice", cur.tid().raw());

        match &cur.running_state {
            task::RunningState::Running(start_time) => {
                debug_assert!(cur_time > *start_time);
                let runtime_delta = cur_time - *start_time;
                cur.runtime += runtime_delta;
                if cur.time_slice() < runtime_delta && !sole {
                    cur.running_state = task::RunningState::NeedResched;
                    true
                } else {
                    false
                }
            }
            task::RunningState::NotRunning => panic!("Not running"),
            task::RunningState::NeedResched => true,
        }
    }

    fn schedule(&self, cur_time: Instant, pree: PreemptStateGuard) -> bool {
        self.canary.assert();
        #[cfg(debug_assertions)]
        // SAFE: We have `pree`, which means preemption is disabled.
        if let Some(ref cur) = unsafe { &*self.current.get() } {
            log::trace!("Scheduling task {:?}, P{}", cur.tid().raw(), PREEMPT.raw());
        }

        self.schedule_impl(cur_time, pree, None, |mut task| {
            debug_assert!(matches!(
                task.running_state,
                task::RunningState::NeedResched
            ));
            task.running_state = task::RunningState::NotRunning;
            self.run_queue.push(task);
        })
        .is_some()
    }

    fn schedule_impl<F, R>(
        &self,
        cur_time: Instant,
        pree: PreemptStateGuard,
        next: Option<task::Ready>,
        func: F,
    ) -> Option<R>
    where
        F: FnOnce(task::Ready) -> R,
    {
        self.canary.assert();

        let mut next = match next {
            Some(next) => next,
            None => match self.run_queue.pop() {
                Some(task) => task,
                None => return None,
            },
        };

        next.running_state = task::RunningState::Running(cur_time);
        next.cpu = self.cpu;
        let new = next.kframe();

        // SAFE: We have `pree`, which means preemption is disabled.
        let cur_slot = unsafe { &mut *self.current.get() };
        let (old, ret) = match cur_slot.replace(next) {
            Some(mut prev) => {
                let kframe_mut = prev.kframe_mut();
                let ret = func(prev);

                Some((kframe_mut, ret))
            }
            None => None,
        }
        .unzip();

        // We will enable preemption in `switch_ctx`.
        mem::forget(pree);
        unsafe { task::ctx::switch_ctx(old, new) };
        ret
    }
}

fn select_cpu(affinity: &crate::cpu::CpuMask) -> Option<usize> {
    affinity.iter_ones().next()
}

/// # Safety
///
/// This function must be called only in task-migrate IPI handlers.
pub unsafe fn task_migrate_handler() {
    loop {
        match MIGRATION_QUEUE[SCHED.cpu].steal_batch(&SCHED.run_queue) {
            Steal::Empty | Steal::Success(_) => break,
            Steal::Retry => {}
        }
    }
}
