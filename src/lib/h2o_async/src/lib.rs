#![no_std]
#![feature(control_flow_enum)]

pub mod dev;
pub mod disp;
pub mod exe;
pub mod io;
pub mod ipc;
pub mod mem;
pub mod sync;
mod utils;

pub use solvent_core as reexport_std;

extern crate alloc;

#[cfg(feature = "runtime")]
pub use self::exe::{block_on, dispatch, spawn, spawn_blocking};

#[cfg(feature = "runtime")]
pub mod test {
    use core::future::Future;

    use solvent::{
        ipc::Packet,
        prelude::{Handle, PhysOptions},
        random,
    };

    const NUM_PACKET: usize = 2000;

    fn test_tx() -> (impl Future<Output = ()>, impl Future<Output = ()>) {
        let (i1, i2) = solvent::ipc::Channel::new();
        let i1 = crate::ipc::Channel::new(i1);
        let i2 = crate::ipc::Channel::new(i2);

        let recv = async move {
            let mut packet = Packet {
                buffer: alloc::vec![0; 4],
                handles: alloc::vec![Handle::NULL; 4],
                ..Default::default()
            };
            for index in 0..NUM_PACKET {
                // log::debug!("\t\t\tReceive #{index}");
                i2.receive(&mut packet)
                    .await
                    .expect("Failed to receive packet");
                // log::debug!("\t\t\tGot #{index}");
                assert_eq!(packet.buffer[0], index as u8);
            }
            log::debug!("\t\t\tReceive finished");
        };

        let send = async move {
            let mut packet = Packet {
                id: None,
                buffer: alloc::vec![0],
                ..Default::default()
            };
            for index in 0..NUM_PACKET {
                packet.buffer.resize(1, index as u8);
                packet
                    .buffer
                    .extend(core::iter::repeat_with(|| random() as u8).take(199));
                async {
                    // log::debug!("Send #{index}");
                    i1.send(&mut packet).expect("Failed to send packet")
                }
                .await;
            }
            log::debug!("Send finished");
        };

        (send, recv)
    }

    pub async fn test_disp() {
        log::debug!("Has {} cpus available", solvent::task::cpu_num());

        let phys = solvent::mem::Phys::allocate(5, PhysOptions::ZEROED | PhysOptions::RESIZABLE)
            .expect("Failed to allocate memory");
        let stream = unsafe {
            crate::io::Stream::new(solvent_core::io::RawStream {
                phys,
                seeker: 0,
            })
        };
        stream.write(&[1, 2, 3, 4, 5, 6, 7]).await.unwrap();
        stream
            .seek(solvent_core::io::SeekFrom::Current(-4))
            .await
            .unwrap();
        let mut buf = [0; 10];
        let len = stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], [4, 5, 6, 7]);

        let (send, recv) = test_tx();
        let recv = crate::spawn(recv);
        let send = crate::spawn(send);
        recv.await;
        send.await;
    }
}
