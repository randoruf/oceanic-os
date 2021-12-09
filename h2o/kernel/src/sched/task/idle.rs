use alloc::vec::Vec;
use core::hint;

use spin::Lazy;

use super::{ctx::Kstack, *};
use crate::{
    mem::space::{self, Space},
    sched::deque,
};

/// Context dropper - used for dropping kernel stacks of threads.
///
/// The function [`task_exit`] runs on its own thread stack, so we cannot drop
/// its kernel stack immediately, or the current context will crash. That's
/// where context dropper tasks come into play. They collect the kernel stack
/// from its CPU-local queue and drop it.
///
/// [`task_exit`]: crate::sched::task::syscall::task_exit
#[thread_local]
pub(super) static CTX_DROPPER: Lazy<deque::Injector<Kstack>> = Lazy::new(|| deque::Injector::new());

#[thread_local]
pub(super) static IDLE: Lazy<Tid> = Lazy::new(|| {
    let cpu = unsafe { crate::cpu::id() };

    let ti = TaskInfo {
        from: UnsafeCell::new(Some((ROOT.clone(), None))),
        name: format!("IDLE{}", cpu),
        ty: Type::Kernel,
        affinity: crate::cpu::current_mask(),
        prio: prio::IDLE,
        handles: RwLock::new(HandleMap::new()),
        signal: Mutex::new(None),
    };

    let space = Space::clone(unsafe { space::current() }, Type::Kernel);
    let entry = LAddr::new(idle as *mut u8);

    let tid = super::tid::allocate(ti).expect("Tid exhausted");
    let init = Init::new(
        tid.clone(),
        space,
        entry,
        DEFAULT_STACK_SIZE,
        Some(paging::LAddr::from(
            unsafe { archop::msr::read(archop::msr::FS_BASE) } as usize,
        )),
        [cpu as u64, 0],
    )
    .expect("Failed to initialize IDLE");

    crate::sched::SCHED.push(init);
    tid
});

fn idle(cpu: usize) -> ! {
    use crate::sched::{task, SCHED};
    log::debug!("IDLE #{}", cpu);

    let (ctx_dropper, ..) = task::create_fn(
        Some(String::from("CTXD")),
        DEFAULT_STACK_SIZE,
        None,
        LAddr::new(ctx_dropper as *mut u8),
        unsafe { archop::msr::read(archop::msr::FS_BASE) } as *mut u8,
    )
    .expect("Failed to create context dropper");
    SCHED.push(ctx_dropper);

    if cpu == 0 {
        let (me, chan) = Channel::new();

        me.send(crate::sched::ipc::Packet::new(Vec::new(), &[]))
            .expect("Failed to send message");

        let image = unsafe {
            core::slice::from_raw_parts(
                *crate::KARGS.tinit_phys.to_laddr(minfo::ID_OFFSET),
                crate::KARGS.tinit_len,
            )
        };

        let (tinit, ..) = task::from_elf(
            image,
            String::from("TINIT"),
            crate::cpu::all_mask(),
            Some(chan),
        )
        .expect("Failed to initialize TINIT");
        SCHED.push(tinit);
    }

    unsafe { archop::halt_loop(Some(true)) };
}

fn ctx_dropper(_: u64, fs_base: u64) -> ! {
    unsafe { archop::msr::write(archop::msr::FS_BASE, fs_base) };
    log::debug!("Context dropper for cpu #{}", unsafe { crate::cpu::id() });

    let worker = deque::Worker::new_fifo();
    loop {
        match CTX_DROPPER.steal_batch(&worker) {
            deque::Steal::Empty | deque::Steal::Retry => hint::spin_loop(),
            deque::Steal::Success(_) => {
                while let Some(kstack) = worker.pop() {
                    drop(kstack);
                }
            }
        }
        crate::sched::SCHED.with_current(|cur| cur.running_state = RunningState::NeedResched);
        unsafe { archop::resume_intr(None) };
    }
}
