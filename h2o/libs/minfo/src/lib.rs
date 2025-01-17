#![no_std]

use core::alloc::Layout;

// Physical addresses

// The rust compiler panics on debug mode runtime when using the 0 address. Use
// another free address for passing arguments.
pub const KARGS_BASE: usize = 0x1000;

pub const TRAMPOLINE_RANGE: core::ops::Range<usize> = 0..0x100000;

pub const LAPIC_BASE: usize = 0xFEE0_0000;

pub const INITIAL_ID_SPACE: usize = 0x1_0000_0000;

pub use pmm::{KMEM_PHYS_BASE, PF_SIZE};

// Virtual addresses

pub const USER_BASE: usize = 0x100000;

pub const USER_END: usize = 0x7FFF_0000_0000;

pub const KERNEL_SPACE_START: usize = 0xFFFF_8000_0000_0000;

/// WARN: The range must contains only 1 page sized 512G (a.k.a. the largest
/// size). If the kernel memory space may be exhausted, be sure to make
/// corresponding modifications to `KERNEL_ROOT` in the kernel crate!
pub const KERNEL_ALLOCABLE_RANGE: core::ops::Range<usize> =
    0xFFFF_A000_0000_0000..0xFFFF_A080_0000_0000;

pub const ID_OFFSET: usize = KERNEL_SPACE_START;

// Kernel args

#[derive(Debug, Copy, Clone)]
pub struct KernelArgs {
    pub rsdp: paging::PAddr,
    pub smbios: paging::PAddr,

    pub efi_mmap_paddr: paging::PAddr,
    pub efi_mmap_len: usize,
    pub efi_mmap_unit: usize,

    pub pls_layout: Option<Layout>,

    pub tinit_phys: paging::PAddr,
    pub tinit_len: usize,

    pub bootfs_phys: paging::PAddr,
    pub bootfs_len: usize,
}
