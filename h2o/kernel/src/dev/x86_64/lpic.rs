use crate::cpu::intr::{Interrupt, IntrChip};
use archop::io::{Io, Port};

use alloc::sync::Arc;

const MASTER_PORT: u16 = 0x20;
const SLAVE_PORT: u16 = 0xA0;

unsafe fn read_cmd(chip: &Port<u8>) -> u8 {
      chip.read()
}

unsafe fn write_cmd(chip: &mut Port<u8>, value: u8) {
      chip.write(value)
}

unsafe fn read_data(chip: &Port<u8>) -> u8 {
      chip.read_offset(1)
}

unsafe fn write_data(chip: &mut Port<u8>, value: u8) {
      chip.write_offset(1, value)
}

struct LegacyPic {
      master: Port<u8>,
      slave: Port<u8>,
      masked_irq: u16,
}

impl LegacyPic {
      pub fn new() -> Self {
            LegacyPic {
                  // SAFE: These ports are valid and present.
                  master: unsafe { Port::new(MASTER_PORT) },
                  slave: unsafe { Port::new(SLAVE_PORT) },
                  masked_irq: 0,
            }
      }

      /// Shut down the chips due to another alternate interrupt method (I/O APIC).
      ///
      /// # Safety
      ///
      /// The caller must ensure that its called only once.
      pub unsafe fn init_masked(&mut self) {
            write_data(&mut self.master, 0xFF);
            write_data(&mut self.slave, 0xFF);
      }
}

impl IntrChip for LegacyPic {
      unsafe fn mask(&mut self, intr: Arc<Interrupt>) {
            let irq = intr.hw_irq();
            self.masked_irq |= 1 << irq;
            if irq >= 8 {
                  write_data(&mut self.slave, (self.masked_irq >> 8) as u8);
            } else {
                  write_data(&mut self.master, (self.masked_irq & 0xFF) as u8);
            }
      }

      unsafe fn unmask(&mut self, intr: Arc<Interrupt>) {
            let irq = intr.hw_irq();
            self.masked_irq &= !(1 << irq);
            if irq >= 8 {
                  write_data(&mut self.slave, (self.masked_irq >> 8) as u8);
            } else {
                  write_data(&mut self.master, (self.masked_irq & 0xFF) as u8);
            }
      }

      unsafe fn ack(&mut self, _intr: Arc<Interrupt>) {}

      unsafe fn eoi(&mut self, intr: Arc<Interrupt>) {
            let irq = intr.hw_irq();
            if irq >= 8 {
                  write_cmd(&mut self.slave, 0x20);
            } else {
                  write_cmd(&mut self.master, 0x20);
            }
      }
}
