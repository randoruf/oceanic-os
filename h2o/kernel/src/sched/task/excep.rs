use core::{
    mem::{self, MaybeUninit},
    slice,
    time::Duration,
};

use archop::reg::cr2;
use bytes::Buf;
use sv_call::task::excep::{Exception, ExceptionResult, EXRES_CODE_OK};

use super::{ctx::x86_64::Frame, hdl};
use crate::{
    cpu::intr::arch::ExVec,
    sched::{ipc::Packet, PREEMPT, SCHED},
};

pub fn dispatch_exception(frame: &mut Frame, vec: ExVec) -> bool {
    let slot = match SCHED.with_current(|cur| Ok(cur.tid.excep_chan())) {
        Ok(slot) => slot,
        _ => return false,
    };

    let excep_chan = match PREEMPT.scope(|| slot.lock().take()) {
        Some(chan) => chan,
        _ => return false,
    };

    let data: [u8; mem::size_of::<Exception>()] = unsafe {
        mem::transmute(Exception {
            vec: vec as u8,
            errc: unsafe { frame.errc_vec },
            cr2: match vec {
                ExVec::PageFault => cr2::read(),
                _ => 0,
            },
        })
    };

    let mut excep = Packet::new(0, hdl::List::default(), &data);
    if excep_chan.send(&mut excep).is_err() {
        PREEMPT.scope(|| *slot.lock() = Some(excep_chan));
        return false;
    }

    #[allow(const_item_mutation)]
    let ret = match excep_chan.receive(Duration::MAX, &mut usize::MAX, &mut usize::MAX) {
        Ok(mut res) => {
            let mut data = MaybeUninit::<ExceptionResult>::uninit();
            res.buffer_mut().copy_to_slice(unsafe {
                slice::from_raw_parts_mut(
                    data.as_mut_ptr().cast(),
                    mem::size_of::<ExceptionResult>(),
                )
            });

            let res = unsafe { data.assume_init() };
            Some(res.code == EXRES_CODE_OK)
        }
        Err(err) => match err {
            sv_call::Error::EPIPE => None,
            _ => Some(false),
        },
    };

    ret.map_or(false, |ret| {
        PREEMPT.scope(|| *slot.lock() = Some(excep_chan));
        ret
    })
}
