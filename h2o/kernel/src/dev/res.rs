use alloc::sync::{Arc, Weak};
use core::{any::Any, ops::Range};

use collection_ex::RangeMap;
use spin::Mutex;
use sv_call::Feature;

use crate::sched::{task::hdl::DefaultFeature, PREEMPT};

pub struct Resource<T: Ord + Copy> {
    magic: u64,
    range: Range<T>,
    map: Mutex<RangeMap<T, ()>>,
    parent: Option<Weak<Resource<T>>>,
}

impl<T: Ord + Copy> Resource<T> {
    #[inline]
    fn new(magic: u64, range: Range<T>, parent: Weak<Resource<T>>) -> Arc<Self> {
        Arc::new(Resource {
            magic,
            range: range.clone(),
            map: Mutex::new(RangeMap::new(range)),
            parent: Some(parent),
        })
    }

    pub fn new_root(magic: u64, range: Range<T>) -> Arc<Self> {
        Arc::new(Resource {
            magic,
            range: range.clone(),
            map: Mutex::new(RangeMap::new(range)),
            parent: None,
        })
    }

    #[inline]
    pub fn range(&self) -> Range<T> {
        self.range.clone()
    }

    #[must_use]
    pub fn allocate(self: &Arc<Self>, range: Range<T>) -> Option<Arc<Self>> {
        if self.parent.as_ref().map_or(true, |p| p.strong_count() >= 1) {
            PREEMPT.scope(|| {
                let mut map = self.map.lock();
                map.try_insert_with(
                    range.clone(),
                    || Ok::<_, ()>(((), Self::new(self.magic, range, Arc::downgrade(self)))),
                    (),
                )
                .ok()
            })
        } else {
            None
        }
    }

    #[inline]
    #[must_use]
    pub fn magic_eq(&self, other: &Self) -> bool {
        self.magic == other.magic
    }
}

impl<T: Ord + Copy> Drop for Resource<T> {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
            let _ = PREEMPT.scope(|| parent.map.lock().remove(self.range.start));
        }
    }
}

unsafe impl<T: Ord + Copy + Send + Sync + Any> DefaultFeature for Resource<T> {
    fn default_features() -> Feature {
        Feature::SEND | Feature::SYNC | Feature::READ | Feature::WRITE
    }
}

mod syscall {
    use core::{any::Any, ops::Add};

    use sv_call::*;

    use crate::{dev::Resource, sched::SCHED};

    fn res_alloc_typed<T: Ord + Copy + Send + Sync + Any + Add<Output = T>>(
        hdl: Handle,
        base: T,
        size: T,
    ) -> Result<Handle> {
        SCHED.with_current(|cur| {
            let res = cur.space().handles().get::<Resource<T>>(hdl)?;
            if !res.features().contains(Feature::SYNC) {
                return Err(EPERM);
            }
            let sub = res.allocate(base..(base + size)).ok_or(ENOMEM)?;
            drop(res);
            cur.space().handles().insert_raw(sub, None)
        })
    }

    #[syscall]
    fn res_alloc(hdl: Handle, ty: u32, base: usize, size: usize) -> Result<Handle> {
        match ty {
            res::RES_MEM => res_alloc_typed(hdl, base, size),
            res::RES_PIO => res_alloc_typed(hdl, u16::try_from(base)?, u16::try_from(size)?),
            res::RES_GSI => res_alloc_typed(hdl, u32::try_from(base)?, u32::try_from(size)?),
            _ => Err(ETYPE),
        }
    }
}
