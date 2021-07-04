pub mod timer;
pub mod ipi;

use crate::mem::space;
use archop::msr;

use alloc::sync::Arc;
use core::pin::Pin;

const LAPIC_LAYOUT: core::alloc::Layout = paging::PAGE_LAYOUT;

pub enum LapicType<'a> {
      X1(Pin<&'a mut [space::MemBlock]>),
      X2,
}

pub struct Lapic<'a> {
      ty: LapicType<'a>,
      id: u32,
}

impl<'a> Lapic<'a> {
      fn reg_32_to_1_off(reg: msr::Msr) -> usize {
            (reg as u32 as usize - 0x800) << 4
      }

      fn reg_64_to_1_off(reg: msr::Msr) -> [usize; 2] {
            let r0 = Self::reg_32_to_1_off(reg);
            [r0, r0 + 0x10]
      }

      unsafe fn read_reg_32(ty: &mut LapicType, reg: msr::Msr) -> u32 {
            match ty {
                  LapicType::X1(memory) => {
                        let base = memory.as_ptr().cast::<u8>();
                        let ptr = base.add(Self::reg_32_to_1_off(reg)).cast::<u32>();
                        ptr.read_volatile()
                  }
                  LapicType::X2 => msr::read(reg) as u32,
            }
      }

      unsafe fn write_reg_32(ty: &mut LapicType, reg: msr::Msr, val: u32) {
            match ty {
                  LapicType::X1(memory) => {
                        let base = memory.as_mut_ptr().cast::<u8>();
                        let ptr = base.add(Self::reg_32_to_1_off(reg)).cast::<u32>();
                        ptr.write_volatile(val)
                  }
                  LapicType::X2 => msr::write(reg, val as u64),
            }
      }

      unsafe fn read_reg_64(ty: &mut LapicType, reg: msr::Msr) -> u64 {
            match ty {
                  LapicType::X1(memory) => {
                        let base = memory.as_ptr().cast::<u8>();

                        let ptr_array = Self::reg_64_to_1_off(reg);
                        let mut ptr_iter = ptr_array.iter().map(|&off| base.add(off).cast::<u32>());
                        let low = ptr_iter.next().unwrap().read_volatile() as u64;
                        let high = ptr_iter.next().unwrap().read_volatile() as u64;
                        low | (high << 32)
                  }
                  LapicType::X2 => msr::read(reg),
            }
      }

      unsafe fn write_reg_64(ty: &mut LapicType, reg: msr::Msr, val: u64) {
            match ty {
                  LapicType::X1(memory) => {
                        let base = memory.as_mut_ptr().cast::<u8>();
                        let (low, high) = ((val & 0xFFFFFFFF) as u32, ((val >> 32) as u32));

                        let ptr_array = Self::reg_64_to_1_off(reg);
                        let mut ptr_iter = ptr_array
                              .iter()
                              .map(|&off| base.add(off).cast::<u32>())
                              .rev(); // !!: The order of writing must be from high to low.
                        ptr_iter.next().unwrap().write_volatile(high);
                        ptr_iter.next().unwrap().write_volatile(low);
                  }
                  LapicType::X2 => msr::write(reg, val),
            }
      }

      pub fn new(ty: acpi::table::madt::LapicType, space: &'a Arc<space::Space>) -> Self {
            let mut ty = match ty {
                  acpi::table::madt::LapicType::X2 => {
                        // SAFE: Enabling Local X2 APIC if possible.
                        unsafe {
                              let val = msr::read(msr::APIC_BASE);
                              msr::write(msr::APIC_BASE, val | (1 << 10));
                        }
                        LapicType::X2
                  }
                  acpi::table::madt::LapicType::X1(paddr) => {
                        // SAFE: The physical address is valid and aligned.
                        let memory = unsafe {
                              space.alloc_manual(
                                    LAPIC_LAYOUT,
                                    Some(paddr),
                                    false,
                                    space::Flags::READABLE | space::Flags::WRITABLE,
                              )
                        }
                        .expect("Failed to allocate space");
                        LapicType::X1(memory)
                  }
            };

            let mut id = unsafe { Self::read_reg_32(&mut ty, msr::X2APICID) };
            if let LapicType::X2 = &ty {
                  id >>= 24;
            }

            Lapic { ty, id }
      }

      /// # Safety
      ///
      /// WARNING: This function modifies the architecture's basic registers. Be sure to make
      /// preparations.
      pub unsafe fn enable(&mut self) {
            Self::write_reg_32(
                  &mut self.ty,
                  msr::X2APIC_SIVR,
                  (1 << 8) | (super::intr::def::ApicVec::Spurious as u32),
            );
      }

      pub fn id(&self) -> u32 {
            self.id
      }

      /// # Safety
      ///
      /// WARNING: This function modifies the architecture's basic registers. Be sure to make
      /// preparations.
      pub unsafe fn eoi(&mut self) {
            Self::write_reg_32(&mut self.ty, msr::X2APIC_EOI, 0)
      }

      /// # Safety
      ///
      /// WARNING: This function modifies the architecture's basic registers. Be sure to make
      /// preparations.
      pub unsafe fn activate_timer(self, mode: timer::TimerMode, div: u8, init_value: u64) -> Self {
            let (ret, _, _) = timer::Timer::new(mode, div, self).activate(init_value);
            ret
      }

      /// # Safety
      ///
      /// The caller must ensure that this function is only called by [`error_handler`].
      pub(self) unsafe fn handle_error(&mut self) {
            let esr = Self::read_reg_32(&mut self.ty, msr::X2APIC_ESR);
            self.eoi();

            const MAX_ERROR: usize = 8;
            const ERROR_MSG: [&str; MAX_ERROR] = [
                  "Send CS error",            /* APIC Error Bit 0 */
                  "Receive CS error",         /* APIC Error Bit 1 */
                  "Send accept error",        /* APIC Error Bit 2 */
                  "Receive accept error",     /* APIC Error Bit 3 */
                  "Redirectable IPI",         /* APIC Error Bit 4 */
                  "Send illegal vector",      /* APIC Error Bit 5 */
                  "Received illegal vector",  /* APIC Error Bit 6 */
                  "Illegal register address", /* APIC Error Bit 7 */
            ];

            log::error!("Local APIC ERROR:");

            let mut it = esr;
            for error_msg in ERROR_MSG.iter() {
                  if (it & 1) != 0 {
                        log::error!("> {}", error_msg);
                  }
                  it >>= 1;
            }
      }
}

/// # Safety
///
/// The caller must ensure that this function is only called by the spurious handler.
pub unsafe fn spurious_handler() {
      asm!("nop");
}

/// # Safety
///
/// The caller must ensure that this function is only called by the error handler.
pub unsafe fn error_handler() {
      // SAFE: Inside the timer interrupt handler.
      let kernel_gs = unsafe { crate::cpu::arch::KernelGs::access_in_intr() };
      let lapic = &mut kernel_gs.lapic;

      lapic.handle_error();
}
