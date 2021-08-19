use super::Entry;
use crate::cpu::arch::seg::ndt::{KRL_CODE_X64, KRL_DATA_X64, USR_CODE_X64, USR_DATA_X64};
use crate::cpu::arch::seg::SegSelector;
use crate::sched::task;

pub const DEFAULT_STACK_SIZE: usize = 6 * paging::PAGE_SIZE;

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
      pub fn set_entry(&mut self, entry: Entry, ty: task::Type) {
            let (cs, ss) = match ty {
                  task::Type::User => (USR_CODE_X64, USR_DATA_X64),
                  task::Type::Kernel => (KRL_CODE_X64, KRL_DATA_X64),
            };

            self.rip = entry.entry.val() as u64;
            self.rsp = entry.stack.val() as u64;
            self.rflags = archop::reg::rflags::IF;
            self.cs = SegSelector::into_val(cs) as u64;
            self.ss = SegSelector::into_val(ss) as u64;
            self.rdi = entry.args[0];
            self.rsi = entry.args[1];
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
                        info!("> cr2 (PF addr) = {:#018X}", unsafe {
                              archop::reg::cr2::read()
                        });
                  }
            }
            info!("> Code addr  = {:#018X}", self.rip);
            info!("> RFlags     = {}", Flags::new(self.rflags, Self::RFLAGS));

            info!("> GPRs: ");
            info!("  rax = {:#018X}, rcx = {:#018X}", self.rax, self.rcx);
            info!("  rdx = {:#018X}, rbx = {:#018X}", self.rdx, self.rbx);
            info!("  rbp = {:#018X}, rsp = {:#018X}", self.rbp, self.rsp);
            info!("  rsi = {:#018X}, rdi = {:#018X}", self.rsi, self.rdi);
            info!("  r8  = {:#018X}, r9  = {:#018X}", self.r8, self.r9);
            info!("  r10 = {:#018X}, r11 = {:#018X}", self.r10, self.r11);
            info!("  r12 = {:#018X}, r13 = {:#018X}", self.r12, self.r13);
            info!("  r14 = {:#018X}, r15 = {:#018X}", self.r14, self.r15);

            info!("> Segments:");
            info!("  cs  = {:#018X}, ss  = {:#018X}", self.cs, self.ss);
            info!("  fs_base = {:#018X}", self.fs_base);
            info!("  gs_base = {:#018X}", self.gs_base);
      }
}

/// A temporary module for storing the thread stack.
/// TODO: Must be removed after thread module creation.
pub mod test {
      use super::Frame;
      #[thread_local]
      static mut THREAD_STACK_TOP: *mut u8 = core::ptr::null_mut();

      pub unsafe fn save_regs(frame: *const Frame) -> *mut u8 {
            let thread_frame = THREAD_STACK_TOP.cast::<Frame>().sub(1);
            thread_frame.copy_from(frame, 1);
            thread_frame.cast()
      }

      pub unsafe fn init_stack_top(st: *mut u8) {
            THREAD_STACK_TOP = st;
      }
}