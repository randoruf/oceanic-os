mod node;

use alloc::sync::Weak;
use core::{
    any::Any,
    marker::{PhantomData, Unsize},
    ops::CoerceUnsized,
    ptr::NonNull,
};

use archop::Azy;
use modular_bitfield::prelude::*;
use spin::Mutex;
use sv_call::{Feature, Result};

pub use self::node::{List, Ptr, Ref, MAX_HANDLE_COUNT};
use crate::sched::{ipc::Channel, Event, PREEMPT};

#[bitfield]
struct Value {
    gen: B14,
    index: B18,
}

#[derive(Debug)]
pub struct Object<T: ?Sized> {
    event: Weak<dyn Event>,
    data: T,
}

impl<U: ?Sized, T: ?Sized + CoerceUnsized<U> + Unsize<U>> CoerceUnsized<Object<U>> for Object<T> {}

pub unsafe trait DefaultFeature: Any {
    fn default_features() -> Feature;
}

#[derive(Debug)]
pub struct HandleMap {
    list: Mutex<node::List>,
    mix: u32,
}

impl HandleMap {
    #[inline]
    pub fn new() -> Self {
        HandleMap {
            list: Mutex::new(List::new()),
            mix: archop::rand::get() as u32,
        }
    }

    pub fn decode(&self, handle: sv_call::Handle) -> Result<Ptr> {
        let value = handle.raw() ^ self.mix;
        let value = Value::from_bytes(value.to_ne_bytes());
        let _ = value.gen();
        usize::try_from(value.index())
            .map_err(Into::into)
            .and_then(node::decode)
    }

    #[inline]
    pub fn get<T: Send + Any>(&self, handle: sv_call::Handle) -> Result<&Ref<T>> {
        self.decode(handle)
            .and_then(|ptr| {
                unsafe { ptr.as_ref().is_send() }
                    .then(|| ptr)
                    .ok_or(sv_call::Error::EPERM)
            })
            .and_then(|ptr| unsafe { ptr.as_ref().downcast_ref::<T>() })
    }

    #[inline]
    pub fn clone_ref(&self, handle: sv_call::Handle) -> Result<sv_call::Handle> {
        let old_ptr = self.decode(handle)?;
        let new = unsafe { old_ptr.as_ref() }.try_clone()?;
        unsafe { self.insert_ref(new) }
    }

    pub fn encode(&self, value: Ptr) -> Result<sv_call::Handle> {
        let index =
            node::encode(value).and_then(|index| u32::try_from(index).map_err(Into::into))?;
        let value = Value::new()
            .with_gen(0)
            .with_index_checked(index)
            .map_err(|_| sv_call::Error::ERANGE)?;
        Ok(sv_call::Handle::new(
            u32::from_ne_bytes(value.into_bytes()) ^ self.mix,
        ))
    }

    /// # Safety
    ///
    /// The caller must ensure that `value` comes from the current task if its
    /// not [`Send`].
    #[inline]
    pub unsafe fn insert_ref(&self, value: Ref) -> Result<sv_call::Handle> {
        // SAFETY: The safety condition is guaranteed by the caller.
        let link = PREEMPT.scope(|| unsafe { self.list.lock().insert_impl(value) })?;
        self.encode(link)
    }

    /// # Safety
    ///
    /// The caller must ensure that `T` is [`Send`] if `send` and [`Sync`] if
    /// `sync`.
    pub unsafe fn insert_unchecked<T: 'static>(
        &self,
        data: T,
        feat: Feature,
        event: Option<Weak<dyn Event>>,
    ) -> Result<sv_call::Handle> {
        // SAFETY: The safety condition is guaranteed by the caller.
        let value = unsafe { Ref::try_new_unchecked(data, feat, event) }?;
        // SAFETY: The safety condition is guaranteed by the caller.
        unsafe { self.insert_ref(value.coerce_unchecked()) }
    }

    #[inline]
    pub fn insert<T: DefaultFeature + Any>(
        &self,
        data: T,
        event: Option<Weak<dyn Event>>,
    ) -> Result<sv_call::Handle> {
        unsafe { self.insert_unchecked(data, T::default_features(), event) }
    }

    /// # Safety
    ///
    /// The caller must ensure that the list belongs to the current task if
    /// `link` is not [`Send`].
    #[inline]
    pub unsafe fn remove_ref(&self, handle: sv_call::Handle) -> Result<Ref> {
        let link = self.decode(handle)?;
        // SAFETY: The safety condition is guaranteed by the caller.
        PREEMPT.scope(|| unsafe { self.list.lock().remove_impl(link) })
    }

    #[inline]
    pub fn remove<T: Send + Any>(&self, handle: sv_call::Handle) -> Result<Ref> {
        let _ = PhantomData::<T>;
        self.decode(handle)
            // SAFETY: Dereference within the available range.
            .and_then(|value| unsafe { value.as_ref().downcast_ref::<T>() })
            // SAFETY: The type is `Send`.
            .and_then(|_| unsafe { self.remove_ref(handle) })
    }

    pub fn send(&self, handles: &[sv_call::Handle], src: &Channel) -> Result<List> {
        if handles.is_empty() {
            return Ok(List::new());
        }
        PREEMPT.scope(|| {
            self.list
                .lock()
                .split(
                    handles.iter().map(|&handle| self.decode(handle)),
                    |value| match value.downcast_ref::<Channel>() {
                        Ok(chan) if chan.peer_eq(src) => Err(sv_call::Error::EPERM),
                        Err(_) if !value.is_send() => Err(sv_call::Error::EPERM),
                        _ => Ok(()),
                    },
                )
        })
    }

    #[inline]
    pub fn receive(&self, other: &mut List, handles: &mut [sv_call::Handle]) {
        PREEMPT.scope(|| {
            let mut list = self.list.lock();
            for (hdl, obj) in handles.iter_mut().zip(list.merge(other)) {
                *hdl = self.encode(NonNull::from(obj)).unwrap();
            }
        })
    }
}

impl Default for HandleMap {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
pub(super) fn init() {
    Azy::force(&node::HR_ARENA);
}

mod syscall {
    use sv_call::*;

    use crate::{
        sched::SCHED,
        syscall::{InOut, UserPtr},
    };

    #[syscall]
    fn obj_clone(hdl: Handle) -> Result<Handle> {
        hdl.check_null()?;
        SCHED.with_current(|cur| cur.space().handles().clone_ref(hdl))
    }

    #[syscall]
    fn obj_feat(hdl_ptr: UserPtr<InOut, Handle>, feat: Feature) -> Result {
        let old = unsafe { hdl_ptr.r#in().read() }?;
        old.check_null()?;
        let mut obj = SCHED.with_current(|cur| unsafe { cur.space().handles().remove_ref(old) })?;
        let ret = obj.set_features(feat);
        let new = SCHED.with_current(|cur| unsafe { cur.space().handles().insert_ref(obj) })?;
        unsafe { hdl_ptr.out().write(new) }?;
        ret
    }

    #[syscall]
    fn obj_drop(hdl: Handle) -> Result {
        hdl.check_null()?;
        SCHED
            .with_current(|cur| unsafe { cur.space().handles().remove_ref(hdl) })
            .map(|_| {})
    }
}
