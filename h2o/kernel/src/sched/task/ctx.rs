cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        pub mod x86_64;
        pub use x86_64 as arch;
    }
}

use alloc::boxed::Box;
use core::{
    alloc::Layout,
    fmt::Debug,
    ops::{Deref, DerefMut},
    ptr::{self, NonNull},
};

use paging::{LAddr, PAGE_SIZE};

use crate::{
    cpu::arch::seg::ndt::INTR_CODE,
    mem::space::{self, AllocType, Flags, KernelVirt},
};

pub const KSTACK_SIZE: usize = paging::PAGE_SIZE * 13;

#[derive(Debug)]
pub struct Entry {
    pub entry: LAddr,
    pub stack: LAddr,
    pub tls: Option<LAddr>,
    pub args: [u64; 2],
}

#[repr(align(4096))]
pub struct KstackData([u8; KSTACK_SIZE]);

impl KstackData {
    pub fn top(&self) -> LAddr {
        LAddr::new(self.0.as_ptr_range().end as *mut u8)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn task_frame(&self) -> &arch::Frame {
        let ptr = self.0.as_ptr_range().end.cast::<arch::Frame>();

        unsafe { &*ptr.sub(1) }
    }

    #[cfg(target_arch = "x86_64")]
    pub fn task_frame_mut(&mut self) -> &mut arch::Frame {
        let ptr = self.0.as_mut_ptr_range().end.cast::<arch::Frame>();

        unsafe { &mut *ptr.sub(1) }
    }
}

pub struct Kstack {
    ptr: NonNull<KstackData>,
    virt: KernelVirt,
    kframe_ptr: Box<*mut u8>,
}

unsafe impl Send for Kstack {}

impl Kstack {
    pub fn new(entry: Entry, ty: super::Type) -> Self {
        let (virt, ptr) = {
            let virt = space::KRL
                .allocate_kernel(
                    AllocType::Layout(Layout::new::<KstackData>()),
                    None,
                    Flags::READABLE | Flags::WRITABLE,
                )
                .expect("Failed to allocate kernel stack");
            let ptr = virt.as_ptr();
            let pad = NonNull::slice_from_raw_parts(ptr.as_non_null_ptr(), PAGE_SIZE);
            unsafe {
                virt.modify(pad, Flags::READABLE)
                    .expect("Failed to set padding");
            }
            (virt, ptr)
        };
        let mut kstack = ptr.cast::<KstackData>();
        let kframe_ptr = unsafe {
            let this = kstack.as_mut();
            let frame = this.task_frame_mut();
            frame.set_entry(entry, ty);
            let kframe = (frame as *mut arch::Frame).cast::<arch::Kframe>().sub(1);
            kframe.write(arch::Kframe::new(
                (frame as *mut arch::Frame).cast(),
                INTR_CODE.into_val() as u64,
            ));
            kframe.cast()
        };
        Kstack {
            ptr: kstack,
            virt,
            kframe_ptr: box kframe_ptr,
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub fn kframe_ptr(&self) -> *mut u8 {
        *self.kframe_ptr
    }

    #[cfg(target_arch = "x86_64")]
    pub fn kframe_ptr_mut(&mut self) -> *mut *mut u8 {
        &mut *self.kframe_ptr
    }

    pub fn virt(&self) -> &KernelVirt {
        &self.virt
    }
}

impl Deref for Kstack {
    type Target = KstackData;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl DerefMut for Kstack {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut() }
    }
}

impl Debug for Kstack {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Kstack {{ {:?} }} ", *self.task_frame())
    }
}

#[derive(Debug)]
#[repr(align(16))]
pub struct ExtendedFrame([u8; arch::EXTENDED_FRAME_SIZE]);

impl ExtendedFrame {
    pub fn zeroed() -> Box<Self> {
        box ExtendedFrame([0; arch::EXTENDED_FRAME_SIZE])
    }

    pub unsafe fn save(&mut self) {
        let ptr = self.0.as_mut_ptr();
        archop::fpu::save(ptr);
    }

    pub unsafe fn load(&self) {
        let ptr = self.0.as_ptr();
        archop::fpu::load(ptr);
    }
}

pub unsafe fn switch_ctx(old: Option<*mut *mut u8>, new: *mut u8) {
    arch::switch_kframe(old.unwrap_or(ptr::null_mut()), new);
    arch::switch_finishing();
}
