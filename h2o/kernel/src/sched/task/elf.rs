use super::*;
use crate::cpu::CpuMask;
use crate::mem::space::{AllocType, Flags, Space};
use bitop_ex::BitOpEx;
use paging::{LAddr, PAddr};

use alloc::string::String;
use goblin::elf::*;

fn load_prog(
      space: &Space,
      flags: u32,
      virt: LAddr,
      phys: PAddr,
      fsize: usize,
      msize: usize,
) -> Result<()> {
      fn flags_to_pg_attr(flags: u32) -> Flags {
            let mut ret = Flags::USER_ACCESS;
            if (flags & program_header::PF_R) != 0 {
                  ret |= Flags::READABLE;
            }
            if (flags & program_header::PF_W) != 0 {
                  ret |= Flags::WRITABLE;
            }
            if (flags & program_header::PF_X) != 0 {
                  ret |= Flags::EXECUTABLE;
            }
            ret
      }
      log::trace!("Loading LOAD phdr (flags = {:?}, virt = {:?}, phys = {:?}, fsize = {:#x}, msize = {:#x})", flags, virt, phys, fsize, msize);

      let flags = flags_to_pg_attr(flags);
      let (vstart, vend) = (virt.val(), virt.val() + fsize);

      if fsize > 0 {
            let virt = LAddr::from(vstart)..LAddr::from(vend);
            log::trace!("Mapping {:?}", virt);
            unsafe { space.alloc(AllocType::Virt(virt), Some(phys), flags) }
                  .map_err(TaskError::Memory)?;
      }

      if msize > fsize {
            let extra = msize - fsize;

            let virt = LAddr::from(vend)..LAddr::from(vend + extra);
            log::trace!("Allocating {:?}", virt);
            unsafe { space.alloc(AllocType::Virt(virt), None, flags | Flags::ZEROED) }
                  .map_err(TaskError::Memory)?;
      }

      Ok(())
}

fn load_tls(space: &Space, size: usize, align: usize) -> Result<LAddr> {
      log::trace!("Loading TLS phdr (size = {:?}, align = {:?})", size, align);
      let layout = paging::PAGE_LAYOUT
            .repeat(size.div_ceil_bit(paging::PAGE_SHIFT))
            .map_or(Err(TaskError::InvalidFormat), |(layout, _)| Ok(layout))?
            .align_to(align)
            .map_err(|_| TaskError::InvalidFormat)?;
      let size = layout.size();

      let tls = unsafe {
            let (alloc_layout, off) = layout
                  .extend(core::alloc::Layout::new::<*mut u8>())
                  .map_err(|_| TaskError::Memory("Invalid TLS layout"))?;
            assert_eq!(off, size);

            log::trace!("Allocating TLS {:?}", alloc_layout);
            let mut memory = space
                  .alloc(
                        AllocType::Layout(alloc_layout),
                        None,
                        Flags::READABLE | Flags::WRITABLE | Flags::EXECUTABLE | Flags::USER_ACCESS,
                  )
                  .map_err(TaskError::Memory)?;
            memory.as_mut_ptr()
      };

      unsafe {
            let self_ptr = tls.add(size).cast::<usize>();
            // TLS's self-pointer is written its address there.
            self_ptr.write(self_ptr as usize);

            Ok(LAddr::new(self_ptr.cast()))
      }
}

fn load_elf(space: &Space, file: &Elf, image: &[u8]) -> Result<(LAddr, Option<LAddr>, usize)> {
      log::trace!(
            "Loading ELF file from image {:?}, space = {:?}",
            image.as_ptr(),
            space as *const _
      );
      let entry = LAddr::new(file.entry as *mut u8);
      let mut stack_size = DEFAULT_STACK_SIZE;
      let mut tls = None;

      for phdr in file.program_headers.iter() {
            match phdr.p_type {
                  program_header::PT_GNU_STACK if phdr.p_memsz != 0 => {
                        log::trace!("Found stack size {:?}", phdr.p_memsz);
                        stack_size = phdr.p_memsz as usize
                  }

                  program_header::PT_LOAD => {
                        load_prog(
                              space,
                              phdr.p_flags,
                              LAddr::from(phdr.p_vaddr as usize),
                              LAddr::new(unsafe { image.as_ptr().add(phdr.p_offset as usize) }
                                    as *mut u8)
                              .to_paddr(minfo::ID_OFFSET),
                              (phdr.p_filesz as usize).round_up_bit(paging::PAGE_SHIFT),
                              (phdr.p_memsz as usize).round_up_bit(paging::PAGE_SHIFT),
                        )?
                  }

                  program_header::PT_TLS => {
                        tls = Some(load_tls(
                              space,
                              phdr.p_memsz as usize,
                              phdr.p_align as usize,
                        )?)
                  }

                  _ => return Err(TaskError::NotSupported),
            }
      }
      Ok((entry, tls, stack_size))
}

pub fn from_elf<'a, 'b>(
      image: &'b [u8],
      name: String,
      affinity: CpuMask,
      args: &'a [u64],
) -> Result<(Init, Option<&'a [u64]>)> {
      let file = Elf::parse(image)
            .map_err(|_| TaskError::InvalidFormat)
            .and_then(|file| {
                  if file.is_64 {
                        Ok(file)
                  } else {
                        Err(TaskError::InvalidFormat)
                  }
            })?;

      super::create(
            name,
            Type::User,
            affinity,
            prio::DEFAULT,
            |space| load_elf(space, &file, image),
            args,
      )
}