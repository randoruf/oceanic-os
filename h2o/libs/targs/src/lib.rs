#![no_std]

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(usize)]
pub enum HandleIndex {
    MemRes = 0,
    PioRes = 1,
    GsiRes = 2,
    Vdso = 3,
    Bootfs = 4,
    RootVirt = 5,

    Len,
}

#[derive(Debug, Copy, Clone, Default)]
pub struct Targs {
    pub rsdp: usize,
    pub smbios: usize,
}

unsafe impl plain::Plain for Targs {}
