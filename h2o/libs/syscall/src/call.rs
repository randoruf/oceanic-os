#[cfg(feature = "call")]
pub mod raw;
pub mod reg;

#[allow(unused_imports)]
use crate::{Arguments, SerdeReg};
use solvent_gen::syscall_stub;

syscall_stub!(0 => pub(crate) fn get_time(ptr: *mut u128));
#[cfg(debug_assertions)]
syscall_stub!(1 => pub(crate) fn log(args: *const ::log::Record));

syscall_stub!(2 => pub(crate) fn task_exit(retval: usize));
syscall_stub!(3 => 
      pub(crate) fn task_fn(name: *mut u8, stack_size: usize, func: *mut u8, arg: *mut u8) 
            -> usize);
syscall_stub!(5 => pub(crate) fn task_join(hdl: usize) -> usize);

syscall_stub!(8 => 
      pub(crate) fn alloc_pages(virt: *mut u8, phys: usize, size: usize, align: usize, flags: u32) 
            -> *mut u8);
syscall_stub!(9 => pub(crate) unsafe fn dealloc_pages(ptr: *mut u8, size: usize));