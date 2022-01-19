mod node;

use core::{
    any::Any,
    marker::{PhantomData, Unsize},
    ops::CoerceUnsized,
    ptr::NonNull,
};

use modular_bitfield::prelude::*;
use solvent::Result;
use spin::{Lazy, Mutex};

pub use self::node::{List, Ptr, Ref, MAX_HANDLE_COUNT};
use crate::sched::{ipc::Channel, PREEMPT};

#[bitfield]
struct Value {
    gen: B14,
    index: B18,
}

#[derive(Debug)]
pub struct Object<T: ?Sized> {
    send: bool,
    sync: bool,
    data: T,
}

impl<U: ?Sized, T: ?Sized + CoerceUnsized<U> + Unsize<U>> CoerceUnsized<Object<U>> for Object<T> {}

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

    pub fn decode(&self, handle: solvent::Handle) -> Result<Ptr> {
        let value = handle.raw() ^ self.mix;
        let value = Value::from_bytes(value.to_ne_bytes());
        let _ = value.gen();
        usize::try_from(value.index())
            .map_err(Into::into)
            .and_then(node::decode)
    }

    #[inline]
    pub fn get<T: Send + 'static>(&self, handle: solvent::Handle) -> Result<&Ref<T>> {
        // SAFETY: The type is `Send`.
        unsafe { self.get_unchecked(handle) }
    }

    /// # Safety
    ///
    /// The caller must ensure that the list belongs to the current task if the
    /// expected type is not [`Send`].
    #[inline]
    pub unsafe fn get_unchecked<T: 'static>(&self, handle: solvent::Handle) -> Result<&Ref<T>> {
        self.decode(handle)
            .and_then(|ptr| unsafe { ptr.as_ref().downcast_ref::<T>() })
    }

    pub fn encode(&self, value: Ptr) -> Result<solvent::Handle> {
        let index =
            node::encode(value).and_then(|index| u32::try_from(index).map_err(Into::into))?;
        let value = Value::new()
            .with_gen(0)
            .with_index_checked(index)
            .map_err(|_| solvent::Error::ERANGE)?;
        Ok(solvent::Handle::new(
            u32::from_ne_bytes(value.into_bytes()) ^ self.mix,
        ))
    }

    /// # Safety
    ///
    /// The caller must ensure that `value` comes from the current task if its
    /// not [`Send`].
    #[inline]
    pub unsafe fn insert_ref(&self, value: Ref<dyn Any>) -> Result<solvent::Handle> {
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
        send: bool,
        sync: bool,
    ) -> Result<solvent::Handle> {
        // SAFETY: The safety condition is guaranteed by the caller.
        let value = unsafe { Ref::new_unchecked(data, send, sync) };
        // SAFETY: The safety condition is guaranteed by the caller.
        unsafe { self.insert_ref(value.coerce_unchecked()) }
    }

    #[inline]
    pub fn insert<T: Send + 'static>(&self, data: T) -> Result<solvent::Handle> {
        let value = Ref::new(data);
        // SAFETY: data is `Send`.
        unsafe { self.insert_ref(value.coerce_unchecked()) }
    }

    /// # Safety
    ///
    /// The caller must ensure that the list belongs to the current task if
    /// `link` is not [`Send`].
    #[inline]
    pub unsafe fn remove_ref(&self, handle: solvent::Handle) -> Result<Ref<dyn Any>> {
        let link = self.decode(handle)?;
        // SAFETY: The safety condition is guaranteed by the caller.
        PREEMPT.scope(|| unsafe { self.list.lock().remove_impl(link) })
    }

    #[inline]
    pub fn remove<T: Send + 'static>(&self, handle: solvent::Handle) -> Result<Ref<dyn Any>> {
        let _ = PhantomData::<T>;
        self.decode(handle)
            // SAFETY: Dereference within the available range.
            .and_then(|value| unsafe { value.as_ref().downcast_ref::<T>() })
            // SAFETY: The type is `Send`.
            .and_then(|_| unsafe { self.remove_ref(handle) })
    }

    pub fn send(&self, handles: &[solvent::Handle], src: &Channel) -> Result<List> {
        if handles.is_empty() {
            return Ok(List::new());
        }
        PREEMPT.scope(|| {
            self.list
                .lock()
                .split(
                    handles.iter().map(|&handle| self.decode(handle)),
                    |value| match value.downcast_ref::<Channel>() {
                        Ok(chan) if chan.peer_eq(src) => Err(solvent::Error::EPERM),
                        Err(_) if !value.is_send() => Err(solvent::Error::EPERM),
                        _ => Ok(()),
                    },
                )
        })
    }

    #[inline]
    pub fn receive(&self, other: &mut List, handles: &mut [solvent::Handle]) {
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

pub fn init() {
    Lazy::force(&node::HR_ARENA);
}

mod syscall {
    use solvent::*;

    use crate::sched::SCHED;

    #[syscall]
    fn obj_clone(_hdl: Handle) -> Result<Handle> {
        todo!()
    }

    #[syscall]
    fn obj_drop(hdl: Handle) -> Result {
        hdl.check_null()?;
        SCHED
            .with_current(|cur| unsafe { cur.tid().handles().remove_ref(hdl) })
            .map(|_| ())
    }
}
