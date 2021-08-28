use super::*;

use spin::Lazy;

#[thread_local]
pub(super) static IDLE: Lazy<Tid> = Lazy::new(|| {
      let cpu = unsafe { crate::cpu::id() };

      let ti = TaskInfo::new(
            *ROOT,
            format!("IDLE{}", cpu),
            Type::Kernel,
            crate::cpu::current_mask(),
            prio::IDLE,
      );

      let space = unsafe { crate::mem::space::current().duplicate(Type::Kernel) };
      let entry = LAddr::new(idle as *mut u8);

      let (init, _) = Init::new(
            ti,
            space,
            entry,
            DEFAULT_STACK_SIZE,
            Some(paging::LAddr::from(
                  unsafe { archop::msr::read(archop::msr::FS_BASE) } as usize,
            )),
            &[cpu as u64],
      )
      .expect("Failed to initialize IDLE");
      let tid = init.tid;

      let mut sched = crate::sched::SCHED.lock();
      sched.push(init);

      tid
});

fn idle(cpu: usize) -> ! {
      log::debug!("IDLE #{}", cpu);

      if cpu == 0 {
            push_tinit();
      }

      loop {
            unsafe { archop::pause() }
      }
}

fn push_tinit() {
      use crate::sched::{task, SCHED};

      let image = unsafe {
            core::slice::from_raw_parts(
                  *crate::KARGS.tinit_phys.to_laddr(minfo::ID_OFFSET),
                  crate::KARGS.tinit_len,
            )
      };

      let (tinit, _) = task::from_elf(image, String::from("TINIT"), crate::cpu::all_mask(), &[])
            .expect("Failed to initialize TINIT");
      SCHED.lock().push(tinit);
}
