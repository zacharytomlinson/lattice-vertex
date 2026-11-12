//! The immediate owner-local command boundary. Called only for CRC-valid MAC
//! frames; unrecognized, relayed and setup traffic stays with the peer task.
use super::{
    frame::{Content, Message},
    invocation, Network,
};
use crate::kernel::{appliance::Native, state};
#[derive(Clone, Copy)]
struct Receiver {
    native: &'static dyn Native,
    network: Network,
    local: u64,
    epoch: u64,
}
static mut RECEIVER: Option<Receiver> = None;
pub(super) fn start(native: &'static dyn Native, network: Network, local: u64) {
    state::atomic(|| unsafe {
        RECEIVER = Some(Receiver {
            native,
            network,
            local,
            epoch: 0,
        });
    });
}
pub(super) fn set_epoch(epoch: u64) {
    state::atomic(|| unsafe {
        if let Some(r) = (&mut *core::ptr::addr_of_mut!(RECEIVER)).as_mut() {
            r.epoch = epoch;
        }
    });
}
pub(crate) fn receive(bytes: &[u8]) -> bool {
    state::atomic(|| {
        let Some(receiver) = (unsafe { RECEIVER }) else {
            return false;
        };
        let Ok(message) = Message::decode(bytes, receiver.local, receiver.network.channel) else {
            return false;
        };
        if message.destination != Some(receiver.local) || !receiver.native.ready() {
            return false;
        }
        match message.content {
            Content::Invoke {
                network,
                call,
                arguments,
                ..
            } if network == receiver.network && call.owner == receiver.local => {
                invocation::access(|calls| {
                    // A conflicting request or full receipt ledger must still get a
                    // task-context rejection. Never consume a frame without retaining
                    // its exact completion and response route.
                    let status = calls.submit(receiver.native, call, arguments);
                    if status != calls.result(call) {
                        return false;
                    }
                    calls.return_via(call, message.source);
                    true
                })
            }
            Content::Stream(packet) => super::control::access(|control| {
                control.receive_immediate(
                    message.source,
                    packet,
                    receiver.epoch,
                    receiver.native,
                    ((crate::kernel::tasks::xTaskGetTickCountFromISR() as u64 * 1000)
                        / crate::configTICK_RATE_HZ as u64) as u32,
                )
            }),
            _ => false,
        }
    })
}

#[cfg(all(test, feature = "port-test"))]
mod tests {
    use super::*;
    use crate::{
        ir::Value,
        kernel::{
            peer::frame::{Invocation, InvokeStatus, StateReadResult},
            radio::{self, cost::TimePerByte},
        },
    };
    use core::sync::atomic::{AtomicUsize, Ordering};
    struct Device(AtomicUsize);
    impl Native for Device {
        fn ready(&self) -> bool {
            true
        }
        fn owns(&self, _: *const (), _: u64) -> bool {
            false
        }
        fn read(&self, _: u64) -> StateReadResult {
            StateReadResult::Busy
        }
        fn invoke(&self, _: u64, _: &[Value]) -> InvokeStatus {
            assert_ne!(crate::port::xPortIsInsideInterrupt(), crate::types::pdFALSE);
            self.0.fetch_add(1, Ordering::Relaxed);
            InvokeStatus::Invoked
        }
    }
    static DEVICE: Device = Device(AtomicUsize::new(0));
    #[test]
    fn address_order_and_replay() {
        crate::port::test_port_reset();
        invocation::reset();
        DEVICE.0.store(0, Ordering::Relaxed);
        let net = Network {
            founder: 1,
            channel: 15,
        };
        start(&DEVICE, net, 2);
        let call = Invocation {
            caller: 1,
            owner: 2,
            request: 3,
            transformer: 4,
        };
        for (owner, founder, clear, expected) in [
            (9, 1, true, false),
            (2, 9, true, false),
            (2, 1, false, false),
            (2, 1, true, true),
            (2, 1, true, true),
        ] {
            let message = Message {
                source: 1,
                destination: Some(2),
                path_cost: TimePerByte::ZERO,
                content: Content::Invoke {
                    network: Network { founder, ..net },
                    call: Invocation { owner, ..call },
                    remaining: 1,
                    copies: 1,
                    arguments: &[0],
                },
            };
            let mut bytes = [0; radio::MAX_FRAME_SIZE];
            let length = message.encode(1, &mut bytes);
            let _order = radio::ReceiveOrder::enter(clear);
            crate::port::test_port_set_inside_interrupt(true);
            assert_eq!(radio::receive_immediate(&bytes[..length]), expected);
            crate::port::test_port_set_inside_interrupt(false);
        }
        assert_eq!(DEVICE.0.load(Ordering::Relaxed), 1);
        let result = invocation::access(|calls| calls.take_completion()).unwrap();
        assert_eq!((result.call, result.previous), (call, [1, 0]));
        state::atomic(|| unsafe {
            RECEIVER = None;
        });
    }
}
