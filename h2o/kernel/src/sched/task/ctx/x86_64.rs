use super::Entry;
use crate::cpu::arch::seg::ndt::{KRL_CODE_X64, KRL_DATA_X64, USR_CODE_X64, USR_DATA_X64};
use crate::cpu::arch::seg::SegSelector;
use crate::sched::task;

use core::alloc::Layout;
use core::mem::size_of;

pub const DEFAULT_STACK_SIZE: usize = 64 * paging::PAGE_SIZE;
pub const DEFAULT_STACK_LAYOUT: Layout =
      unsafe { Layout::from_size_align_unchecked(DEFAULT_STACK_SIZE, paging::PAGE_SIZE) };

pub const EXTENDED_FRAME_SIZE: usize = 768;

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Frame {
      gs_base: u64,
      fs_base: u64,

      r15: u64,
      r14: u64,
      r13: u64,
      r12: u64,
      r11: u64,
      r10: u64,
      r9: u64,
      r8: u64,
      rsi: u64,
      rdi: u64,
      rbp: u64,
      rbx: u64,
      rdx: u64,
      rcx: u64,
      rax: u64,

      pub errc_vec: u64,

      pub rip: u64,
      pub cs: u64,
      pub rflags: u64,
      pub rsp: u64,
      pub ss: u64,
}

impl Frame {
      pub fn set_entry<'a>(&mut self, entry: Entry<'a>, ty: task::Type) -> Option<&'a [u64]> {
            let (cs, ss) = match ty {
                  task::Type::User => (USR_CODE_X64, USR_DATA_X64),
                  task::Type::Kernel => (KRL_CODE_X64, KRL_DATA_X64),
            };

            self.rip = entry.entry.val() as u64;
            self.rsp = (entry.stack.val() - size_of::<usize>()) as u64;
            self.rflags = archop::reg::rflags::IF;
            self.cs = SegSelector::into_val(cs) as u64;
            self.ss = SegSelector::into_val(ss) as u64;

            if let Some(tls) = entry.tls {
                  self.fs_base = tls.val() as u64;
            }

            {
                  let mut reg_args = [
                        &mut self.rdi,
                        &mut self.rsi,
                        &mut self.rdx,
                        &mut self.rcx,
                        &mut self.r8,
                        &mut self.r9,
                  ];
                  for (reg, &arg) in reg_args.iter_mut().zip(entry.args.iter()) {
                        **reg = arg;
                  }
            }

            (entry.args.len() > 6).then(|| &entry.args[6..])
      }

      pub fn syscall_args(&self) -> solvent::Arguments {
            solvent::Arguments {
                  fn_num: self.rax as usize,
                  args: [
                        self.rdi as usize,
                        self.rsi as usize,
                        self.rdx as usize,
                        self.r8 as usize,
                        self.r9 as usize,
                  ],
            }
      }

      pub fn set_syscall_retval(&mut self, retval: usize) {
            self.rax = retval as u64;
      }

      const RFLAGS: &'static str =
            "CF - PF - AF - ZF SF TF IF DF OF IOPLL IOPLH NT - RF VM AC VIF VIP ID";

      pub const ERRC: &'static str = "EXT IDT TI";
      pub const ERRC_PF: &'static str = "P WR US RSVD ID PK SS - - - - - - - - SGX";

      pub fn dump(&self, errc_format: &'static str) {
            use crate::log::flags::Flags;
            use log::info;

            info!("Frame dump on CPU #{}", unsafe { crate::cpu::id() });

            if self.errc_vec != 0u64.wrapping_sub(1) && errc_format != "" {
                  info!("> Error Code = {}", Flags::new(self.errc_vec, errc_format));
                  if errc_format == Self::ERRC_PF {
                        info!("> cr2 (PF addr) = {:#018x}", unsafe {
                              archop::reg::cr2::read()
                        });
                  }
            }
            info!("> Code addr  = {:#018x}", self.rip);
            info!("> RFlags     = {}", Flags::new(self.rflags, Self::RFLAGS));

            info!("> GPRs: ");
            info!("  rax = {:#018x}, rcx = {:#018x}", self.rax, self.rcx);
            info!("  rdx = {:#018x}, rbx = {:#018x}", self.rdx, self.rbx);
            info!("  rbp = {:#018x}, rsp = {:#018x}", self.rbp, self.rsp);
            info!("  rsi = {:#018x}, rdi = {:#018x}", self.rsi, self.rdi);
            info!("  r8  = {:#018x}, r9  = {:#018x}", self.r8, self.r9);
            info!("  r10 = {:#018x}, r11 = {:#018x}", self.r10, self.r11);
            info!("  r12 = {:#018x}, r13 = {:#018x}", self.r12, self.r13);
            info!("  r14 = {:#018x}, r15 = {:#018x}", self.r14, self.r15);

            info!("> Segments:");
            info!("  cs  = {:#018x}, ss  = {:#018x}", self.cs, self.ss);
            info!("  fs_base = {:#018x}", self.fs_base);
            info!("  gs_base = {:#018x}", self.gs_base);
      }
}

/// # Safety
///
/// This function must be called only by assembly stubs.
#[no_mangle]
unsafe extern "C" fn save_intr(frame: *mut Frame) -> *const Frame {
      let mut sched = crate::sched::SCHED.lock();
      sched.need_reload = false;
      sched.current_mut()
            .map_or(frame, |cur| cur.save_intr(frame))
}

/// # Safety
///
/// This function must be called only by assembly stubs.
#[no_mangle]
unsafe extern "C" fn load_intr(frame: *const Frame) -> *const Frame {
      let sched = crate::sched::SCHED.lock();
      sched.current()
            .map_or(frame, |cur| cur.load_intr(sched.need_reload))
}

#[no_mangle]
unsafe extern "C" fn sync_syscall(frame: *const Frame) -> *const Frame {
      let mut sched = crate::sched::SCHED.lock();
      sched.current_mut()
            .map_or(frame, |cur| cur.sync_syscall(frame))
}
