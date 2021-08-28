//! # Address space management for H2O.
//!
//! This module is responsible for managing system memory and address space in a higher
//! level, especially for large objects like APIC.

use crate::sched::task;
use alloc::collections::BTreeMap;
use bitop_ex::BitOpEx;
use canary::Canary;
use collection_ex::RangeSet;
use paging::{LAddr, PAddr};

use alloc::sync::Arc;
use core::alloc::Layout;
use core::ops::Range;
use core::pin::Pin;
use spin::{Lazy, Mutex, MutexGuard};

cfg_if::cfg_if! {
      if #[cfg(target_arch = "x86_64")] {
            mod x86_64;
            type ArchSpace = x86_64::Space;
            pub use x86_64::init_pgc;
      }
}

static INIT: Lazy<Arc<Space>> = Lazy::new(|| Arc::new(Space::new(task::Type::Kernel)));

#[thread_local]
static mut CURRENT: Option<Arc<Space>> = None;

bitflags::bitflags! {
      /// Flags to describe a block of memory.
      pub struct Flags: u32 {
            const USER_ACCESS = 1;
            const READABLE    = 1 << 1;
            const WRITABLE    = 1 << 2;
            const EXECUTABLE  = 1 << 3;
            const ZEROED      = 1 << 4;
      }
}

/// The total available range of address space for the create type.
///
/// We cannot simply pass a [`Range`] to [`Space`]'s constructor because without control
/// arbitrary, even incanonical ranges would be passed and cause unrecoverable errors.
fn ty_to_range_set(ty: task::Type) -> RangeSet<LAddr> {
      let range = match ty {
            task::Type::Kernel => minfo::KERNEL_ALLOCABLE_RANGE,
            task::Type::User => LAddr::from(minfo::USER_BASE)..LAddr::from(minfo::USER_STACK_BASE),
      };

      let mut range_set = RangeSet::new();
      let _ = range_set.insert(range);
      range_set
}

#[derive(Debug)]
pub enum AllocType {
      Layout(Layout),
      Virt(Range<LAddr>),
}

/// The structure that represents an address space.
///
/// The address space is defined from the concept of the virtual addressing in CPU. It's arch-
/// specific responsibility to map the virtual address to the real (physical) address in RAM.
/// This structure is used to allocate & reserve address space ranges for various requests.
///
/// >TODO: Support the requests for reserving address ranges.
#[derive(Debug)]
pub struct Space {
      canary: Canary<Space>,
      ty: task::Type,

      /// The arch-specific part of the address space.
      arch: ArchSpace,

      /// The free ranges in allocation.
      free_range: Mutex<RangeSet<LAddr>>,

      record: Mutex<BTreeMap<LAddr, Layout>>,
      stack_blocks: Mutex<BTreeMap<LAddr, Layout>>,
}

unsafe impl Send for Space {}
unsafe impl Sync for Space {}

impl Space {
      /// Create a new address space.
      pub fn new(ty: task::Type) -> Self {
            Space {
                  canary: Canary::new(),
                  ty,
                  arch: ArchSpace::new(),
                  free_range: Mutex::new(ty_to_range_set(ty)),
                  record: Mutex::new(BTreeMap::new()),
                  stack_blocks: Mutex::new(BTreeMap::new()),
            }
      }

      /// Allocate an address range in the space.
      pub fn alloc(
            &self,
            ty: AllocType,
            phys: Option<PAddr>,
            flags: Flags,
      ) -> Result<Pin<&mut [u8]>, &'static str> {
            self.canary.assert();

            if phys.map_or(false, |phys| phys.contains_bit(paging::PAGE_MASK)) {
                  return Err("Physical address must be aligned");
            }

            // Get the virtual address.
            // `prefix` and `suffix` are the gaps beside the allocated address range.
            let mut range = self.free_range.lock();

            let (layout, size, prefix, virt, suffix) = match ty {
                  AllocType::Layout(layout) => {
                        // Calculate the real size used.
                        let layout = layout.align_to(paging::PAGE_LAYOUT.align()).unwrap();
                        let size = layout.pad_to_align().size();
                        let (prefix, virt, suffix) = {
                              let res = range.range_iter().find_map(|r| {
                                    let mut start = r.start.val();
                                    while start & (layout.align() - 1) != 0 {
                                          start += 1 << start.trailing_zeros();
                                    }
                                    if start + size <= r.end.val() {
                                          Some((
                                                r.start..LAddr::from(start),
                                                LAddr::from(start)..LAddr::from(start + size),
                                                LAddr::from(start + size)..r.end,
                                          ))
                                    } else {
                                          None
                                    }
                              });
                              res.ok_or("No satisfactory virtual space")?
                        };
                        (layout, size, prefix, virt, suffix)
                  }
                  AllocType::Virt(virt) => {
                        let size = unsafe { virt.end.offset_from(*virt.start) } as usize;
                        let layout = Layout::from_size_align(size, paging::PAGE_SIZE)
                              .map_err(|_| "Address range must be aligned")?;

                        let (prefix, suffix) = {
                              let res = range.range_iter().find_map(|r| {
                                    (r.start <= virt.start && virt.end <= r.end)
                                          .then_some((r.start..virt.start, virt.end..r.end))
                              });

                              res.ok_or("No satisfactory virtual space")?
                        };
                        (layout, size, prefix, virt, suffix)
                  }
            };

            // Get the physical address mapped to.
            let (phys, alloc_ptr) = match phys {
                  Some(phys) => (phys, None),
                  None => {
                        let ptr = unsafe {
                              if flags.contains(Flags::ZEROED) {
                                    alloc::alloc::alloc_zeroed(layout)
                              } else {
                                    alloc::alloc::alloc(layout)
                              }
                        };

                        if ptr.is_null() {
                              return Err("Memory allocation failed");
                        }

                        (LAddr::new(ptr).to_paddr(minfo::ID_OFFSET), Some(ptr))
                  }
            };

            // Map it.
            let ptr = *virt.start;
            self.arch.maps(virt, phys, flags).map_err(|_| {
                  if let Some(alloc_ptr) = alloc_ptr {
                        unsafe { alloc::alloc::dealloc(alloc_ptr, layout) };
                  }
                  "Paging error"
            })?;

            range.remove(prefix.start);
            if !prefix.is_empty() {
                  let _ = range.insert(prefix.clone());
            }
            if !suffix.is_empty() {
                  let _ = range.insert(suffix.clone());
            }
            drop(range);

            let ret = unsafe { Pin::new_unchecked(core::slice::from_raw_parts_mut(ptr, size)) };
            let _ = self
                  .record
                  .lock()
                  .insert(LAddr::new(ptr), layout)
                  .map(|_| panic!("Duplicate allocation"));

            Ok(ret)
      }

      /// Modify the access flags of an address range without a specific type.
      ///
      /// # Safety
      ///
      /// The caller must ensure that `b` was allocated by this `Space` and no pointers or
      /// references to the block are present (or influenced by the modification).
      pub unsafe fn modify<'b>(
            &self,
            mut b: Pin<&'b mut [u8]>,
            flags: Flags,
      ) -> Result<Pin<&'b mut [u8]>, &'static str> {
            self.canary.assert();

            let virt = {
                  let ptr = b.as_mut_ptr_range();
                  LAddr::new(ptr.start)..LAddr::new(ptr.end)
            };

            self.arch
                  .reprotect(virt, flags)
                  .map_err(|_| "Paging error")?;

            Ok(b)
      }

      /// Deallocate an address range in the space without a specific type.
      ///
      /// # Safety
      ///
      /// The caller must ensure that `b` was allocated by this `Space` and `free_phys`
      /// is only set if the physical address range is allocated within `b`'s allocation.
      pub unsafe fn dealloc(
            &self,
            mut b: Pin<&mut [u8]>,
            free_phys: bool,
      ) -> Result<(), &'static str> {
            self.canary.assert();

            let mut virt = {
                  let ptr = b.as_mut_ptr_range();
                  LAddr::new(ptr.start)..LAddr::new(ptr.end)
            };

            // Get the virtual address range from the given memory block.
            let layout = Layout::for_value(&*b);
            {
                  let mut record = self.record.lock();
                  match record.remove(&virt.start) {
                        Some(l) if layout != l => {
                              record.insert(virt.start, l);
                              return Err("Invalid memory block");
                        }
                        None => return Err("Invalid memory block"),
                        _ => {}
                  }
            }

            // Unmap the virtual address & get the physical address.
            let phys = self.arch.unmaps(virt.clone()).map_err(|_| "Paging error")?;
            if free_phys {
                  if let Some(phys) = phys {
                        let alloc_ptr = phys.to_laddr(minfo::ID_OFFSET);
                        alloc::alloc::dealloc(*alloc_ptr, layout);
                  }
            }

            // Deallocate the virtual address range.
            let mut range = self.free_range.lock();
            let (prefix, suffix) = range.neighbors(virt.clone());
            if let Some(prefix) = prefix {
                  virt.start = prefix.start;
                  range.remove(prefix.start);
            }
            if let Some(suffix) = suffix {
                  virt.end = suffix.end;
                  range.remove(suffix.start);
            }
            range.insert(virt).map_err(|_| "Occupied range")
      }

      /// # Safety
      ///
      /// The caller must ensure that loading the space is safe and not cause any #PF.
      pub unsafe fn load(&self) {
            self.canary.assert();
            self.arch.load()
      }

      fn alloc_stack(
            ty: task::Type,
            arch: &ArchSpace,
            stack_blocks: &mut MutexGuard<BTreeMap<LAddr, Layout>>,
            base: LAddr,
            size: usize,
      ) -> Result<LAddr, &'static str> {
            let layout = {
                  let n = size.div_ceil_bit(paging::PAGE_SHIFT);
                  paging::PAGE_LAYOUT
                        .repeat(n)
                        .expect("Failed to get layout")
                        .0
            };

            if base.val() < minfo::USER_STACK_BASE {
                  return Err("Max allocation size exceeded");
            }

            match ty {
                  task::Type::User => {
                        let (phys, alloc_ptr) = unsafe {
                              let ptr = alloc::alloc::alloc(layout);

                              if ptr.is_null() {
                                    return Err("Memory allocation failed");
                              }

                              (LAddr::new(ptr).to_paddr(minfo::ID_OFFSET), ptr)
                        };
                        let virt = base..LAddr::from(base.val() + size);

                        arch.maps(
                              virt,
                              phys,
                              Flags::READABLE | Flags::WRITABLE | Flags::USER_ACCESS,
                        )
                        .map_err(|_| unsafe {
                              alloc::alloc::dealloc(alloc_ptr, layout);
                              "Paging error"
                        })?;

                        if let Some(_) = stack_blocks.insert(base, layout) {
                              panic!("Duplicate allocation");
                        }

                        Ok(base)
                  }
                  task::Type::Kernel => {
                        let ptr = unsafe { alloc::alloc::alloc(layout) };
                        Ok(LAddr::new(ptr))
                  }
            }
      }

      pub fn init_stack(&self, size: usize) -> Result<LAddr, &'static str> {
            self.canary.assert();
            // if matches!(self.ty, task::Type::Kernel) {
            //       return Err("Stack allocation is not allowed in kernel");
            // }

            let size = size.round_up_bit(paging::PAGE_SHIFT);

            let base = Self::alloc_stack(
                  self.ty,
                  &self.arch,
                  &mut self.stack_blocks.lock(),
                  LAddr::from(minfo::USER_END - size),
                  size,
            )?;

            Ok(LAddr::from(base.val() + size))
      }

      pub fn grow_stack(&self, addr: LAddr) -> Result<(), &'static str> {
            self.canary.assert();
            if matches!(self.ty, task::Type::Kernel) {
                  return Err("Kernel-typed tasks cannot grow its stack");
            }

            let addr = LAddr::from(addr.val().round_down_bit(paging::PAGE_SHIFT));

            let mut stack_blocks = self.stack_blocks.lock();

            let last = stack_blocks
                  .iter()
                  .next()
                  .map_or(LAddr::from(minfo::USER_END), |(&k, _v)| k);

            let size = unsafe { last.offset_from(*addr) } as usize;

            Self::alloc_stack(self.ty, &self.arch, &mut stack_blocks, addr, size)?;

            Ok(())
      }

      pub fn clear_stack(&self) -> Result<(), &'static str> {
            self.canary.assert();

            let mut stack_blocks = self.stack_blocks.lock();
            while let Some((base, layout)) = stack_blocks.pop_first() {
                  match self.ty {
                        task::Type::Kernel => unsafe { alloc::alloc::dealloc(*base, layout) },
                        task::Type::User => {
                              let virt =
                                    base..LAddr::from(base.val() + layout.pad_to_align().size());
                              if let Ok(Some(phys)) = self.arch.unmaps(virt) {
                                    let ptr = phys.to_laddr(minfo::ID_OFFSET);
                                    unsafe { alloc::alloc::dealloc(*ptr, layout) };
                              }
                        }
                  }
            }
            Ok(())
      }

      pub fn duplicate(&self, ty: task::Type) -> Arc<Self> {
            let ty = match self.ty {
                  task::Type::Kernel => ty,
                  task::Type::User => task::Type::User,
            };

            Arc::new(Space {
                  canary: Canary::new(),
                  ty,
                  arch: self.arch.clone(),
                  free_range: Mutex::new(match ty {
                        task::Type::User => ty_to_range_set(ty),
                        task::Type::Kernel => self.free_range.lock().clone(),
                  }),
                  record: Mutex::new(match ty {
                        task::Type::User => BTreeMap::new(),
                        task::Type::Kernel => self.record.lock().clone(),
                  }),
                  stack_blocks: Mutex::new(BTreeMap::new()),
            })
      }
}

impl Drop for Space {
      fn drop(&mut self) {
            unsafe { self.load() };
            let _ = self.clear_stack();

            let mut record = self.record.lock();
            while let Some((base, layout)) = record.pop_first() {
                  let virt = base..LAddr::from(base.val() + layout.pad_to_align().size());
                  if let Ok(Some(phys)) = self.arch.unmaps(virt) {
                        let ptr = phys.to_laddr(minfo::ID_OFFSET);
                        unsafe { alloc::alloc::dealloc(*ptr, layout) };
                  }
            }

            unsafe { current().load() };
      }
}

/// Initialize the kernel memory space.
///
/// # Safety
///
/// The function must be called only once from the bootstrap CPU.
pub unsafe fn init_bsp_early() {
      INIT.load();
}

/// Load the kernel space for enery CPU.
///
/// # Safety
///
/// The function must be called only once from each application CPU.
pub unsafe fn init() {
      let space = INIT.clone();
      unsafe { space.load() };
      CURRENT = Some(space);
}

/// Get the reference of the per-CPU current space.
pub fn current() -> &'static Arc<Space> {
      unsafe { CURRENT.as_ref().expect("No current space available") }
}

/// Set the current memory space of the current CPU.
///
/// # Safety
///
/// The function must be called only from the epilogue of context switching.
pub unsafe fn set_current(space: Arc<Space>) {
      space.load();
      CURRENT = Some(space);
}

/// # Safety
///
/// The caller must ensure that [`set_current`] won't be called during the execution of `f`.
pub unsafe fn with<S, F, R>(space: S, f: F) -> R
where
      S: AsRef<Space>,
      F: FnOnce(&Space) -> R,
{
      space.as_ref().load();
      let ret = f(space.as_ref());
      current().load();

      ret
}
