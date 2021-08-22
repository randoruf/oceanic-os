use super::task;
use super::task::ctx;
use crate::cpu::time::Instant;
use alloc::vec::Vec;
use canary::Canary;

use alloc::collections::LinkedList;
use core::time::Duration;
use spin::{Lazy, Mutex};

const MINIMUM_TIME_GRANULARITY: Duration = Duration::from_millis(30);

pub static MIGRATION_QUEUE: Lazy<Vec<Mutex<LinkedList<task::Init>>>> = Lazy::new(|| {
      let cnt = crate::cpu::count();
      (0..cnt).map(|_| Mutex::new(LinkedList::new())).collect()
});

#[thread_local]
pub static SCHED: Lazy<Mutex<Scheduler>> = Lazy::new(|| {
      Mutex::new(Scheduler {
            canary: Canary::new(),
            cpu: unsafe { crate::cpu::id() },
            running: None,
            list: LinkedList::new(),
      })
});

pub struct Scheduler {
      canary: Canary<Scheduler>,
      cpu: usize,
      running: Option<task::Ready>,
      list: LinkedList<task::Ready>,
}

impl Scheduler {
      /// Push a task into the scheduler's run queue.
      ///
      /// # Safety
      ///
      /// The caller must ensure that the affinity of the task contains the scheduler's CPU.
      pub unsafe fn push(&mut self, task: task::Init) {
            self.canary.assert();

            let affinity = {
                  let ti_map = task::tid::TI_MAP.lock();
                  let ti = ti_map.get(&task.tid()).expect("Invalid init");
                  ti.affinity()
            };

            if !affinity.get(self.cpu).map_or(false, |r| *r) {
                  let cpu = affinity.iter_ones().next().expect("Zero affinity");
                  MIGRATION_QUEUE[cpu].lock().push_back(task);

                  unsafe { crate::cpu::arch::apic::ipi::task_migrate(cpu) };
            } else {
                  let time_slice = MINIMUM_TIME_GRANULARITY;
                  let task = task::Ready::from_init(task, self.cpu, time_slice);
                  self.list.push_back(task);
            }
      }

      pub fn current(&self) -> Option<&task::Ready> {
            self.canary.assert();

            self.running.as_ref()
      }

      pub fn current_mut(&mut self) -> Option<&mut task::Ready> {
            self.canary.assert();

            self.running.as_mut()
      }

      fn update(&mut self, cur_time: Instant) -> bool {
            self.canary.assert();

            let sole = self.list.is_empty();
            let cur = match self.current_mut() {
                  Some(task) => task,
                  None => return !sole,
            };

            match cur.running_state {
                  task::RunningState::Running(start_time) => {
                        if cur.time_slice() < cur_time - start_time && !sole {
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

      pub fn tick(&mut self, cur_time: Instant) {
            let need_resched = self.update(cur_time);

            if need_resched {
                  self.schedule(cur_time);
            }
      }

      fn schedule(&mut self, cur_time: Instant) {
            self.canary.assert();

            let cur_cpu = self.cpu;
            if self.list.is_empty() {
                  return;
            }

            let mut next = self.list.pop_front().unwrap();
            next.running_state = task::RunningState::Running(cur_time);
            next.cpu = cur_cpu;

            if let Some(mut prev) = self.running.replace(next) {
                  prev.running_state = task::RunningState::NotRunning;
                  self.list.push_back(prev);
            }
      }

      /// Restore the context of the current task.
      ///
      /// # Safety
      ///
      /// This function should only be called at the end of interrupt / syscall handlers.
      pub unsafe fn restore_current(
            &mut self,
            frame: *const ctx::arch::Frame,
      ) -> *const ctx::arch::Frame {
            self.current().map_or(frame, |cur| {
                  unsafe { cur.space().load() };
                  unsafe { cur.get_arch_context() }
            })
      }
}

/// # Safety
///
/// This function must be called only in task-migrate IPI handlers.
pub unsafe fn task_migrate_handler() {
      let mut sched = SCHED.lock();
      let mut mq = MIGRATION_QUEUE[sched.cpu].lock();
      while let Some(task) = mq.pop_front() {
            unsafe { sched.push(task) };
      }
}
