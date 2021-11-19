use core::mem::size_of;

use archop::{msr, reg};
use paging::LAddr;

use super::seg::ndt::{KRL_CODE_X64, USR_CODE_X86};
use crate::sched::task::ctx::arch::Frame;

extern "C" {
    fn rout_syscall();
}

/// # Safety
///
/// This function should only be called once per CPU.
pub unsafe fn init() -> Option<LAddr> {
    let stack = crate::mem::alloc_system_stack()?
        .as_ptr()
        .sub(size_of::<usize>());

    let star = (USR_CODE_X86.into_val() as u64) << 48 | (KRL_CODE_X64.into_val() as u64) << 32;
    msr::write(msr::STAR, star);
    msr::write(msr::LSTAR, rout_syscall as u64);
    msr::write(msr::FMASK, reg::rflags::IF | reg::rflags::TF);

    let efer = msr::read(msr::EFER);
    msr::write(msr::EFER, efer | 1);

    Some(LAddr::new(stack))
}

#[no_mangle]
unsafe extern "C" fn hdl_syscall(frame: *const Frame) {
    let arg = (*frame).syscall_args();

    let res = crate::syscall::handler(&arg);
    crate::sched::SCHED.tick(crate::cpu::time::Instant::now());

    if !matches!(res, Err(solvent::Error(0))) {
        let val = solvent::Error::encode(res);
        crate::sched::SCHED.with_current(|cur| cur.save_syscall_retval(val));
    }
}
