extern crate std;
use super::*;
use crate::{ir::Integer, port};
use core::sync::atomic::{AtomicU16, AtomicU32, Ordering};
struct Device {
    value: AtomicU32,
    calls: AtomicU32,
    dependency: AtomicU16,
}
impl Native for Device {
    fn ready(&self) -> bool {
        true
    }
    fn owns(&self, _: *const (), _: u64) -> bool {
        false
    }
    fn read(&self, _: &TaskContext, attribute: u64) -> StateReadResult {
        if attribute != 1 {
            return StateReadResult::UnknownAttribute;
        }
        StateReadResult::Value {
            sequence: self.calls.load(Ordering::Relaxed),
            value: Some(value(self.value.load(Ordering::Relaxed))),
        }
    }
    fn invoke(&self, _: &TaskContext, transformer: u64, args: &[Value]) -> InvokeStatus {
        assert!((2..=10).contains(&transformer));
        assert_eq!(args.len(), 1);
        self.value
            .store(args[0].as_integer().unwrap() as u32, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
        InvokeStatus::Invoked
    }
    fn state_binding(&self, r: u64) -> Option<(Type, &AtomicU16)> {
        (r == 1).then_some((Type::Integer(Integer::U8), &self.dependency))
    }
    fn transformer_signature(&self, r: u64) -> Option<&[Type]> {
        (2..=10)
            .contains(&r)
            .then_some(&[Type::Integer(Integer::U8)])
    }
}
fn value(n: u32) -> Value {
    Value::integer(Integer::U8, n as i128).unwrap()
}
fn device() -> (&'static Device, TaskContext) {
    port::test_port_reset();
    execution::reset();
    (
        std::boxed::Box::leak(std::boxed::Box::new(Device {
            value: AtomicU32::new(0),
            calls: AtomicU32::new(0),
            dependency: AtomicU16::new(u16::MAX),
        })),
        unsafe { TaskContext::assume() },
    )
}
fn open(c: &mut Control, n: &'static Device, t: &TaskContext, nonce: u64, now: u32) -> State {
    let mut out = [0; DATA_BYTES];
    let r = c.receive(
        3,
        Request::Open {
            epoch: 7,
            nonce,
            transformer: 2 + nonce - 1,
            state: 1,
        },
        7,
        n,
        t,
        now,
        &mut out,
    );
    assert_eq!(r.status, Status::Ok);
    State::decode(r).unwrap()
}
#[test]
fn target_order_and_dispatch() {
    for samples in [[1, 2, 3], [3, 1, 2], [3, 3, 3]] {
        let (n, context) = device();
        let mut c = Control::new();
        let ticket = open(&mut c, n, &context, 1, 0).ticket;
        let mut out = [0; DATA_BYTES];
        for sequence in samples {
            assert_eq!(
                c.receive(
                    3,
                    Request::Target {
                        ticket,
                        sequence,
                        value: value(sequence)
                    },
                    7,
                    n,
                    &context,
                    1,
                    &mut out
                )
                .status,
                Status::Accepted
            );
        }
        c.poll(n, &context, 2); // native task ready, but not executing yet
        c.receive(
            3,
            Request::Target {
                ticket,
                sequence: 4,
                value: value(4),
            },
            7,
            n,
            &context,
            3,
            &mut out,
        );
        execution::run_ready(&context);
        c.poll(n, &context, 4);
        assert_eq!(
            (
                n.calls.load(Ordering::Relaxed),
                n.value.load(Ordering::Relaxed)
            ),
            (1, 4)
        );
        for (caller, request, expected) in [
            (
                3,
                Request::Target {
                    ticket,
                    sequence: 2,
                    value: value(2),
                },
                Status::Accepted,
            ),
            (
                3,
                Request::Target {
                    ticket,
                    sequence: 4,
                    value: value(5),
                },
                Status::Conflict,
            ),
            (
                3,
                Request::Target {
                    ticket,
                    sequence: 5,
                    value: Value::boolean(true),
                },
                Status::Invalid,
            ),
            (
                4,
                Request::Target {
                    ticket,
                    sequence: 5,
                    value: value(5),
                },
                Status::Stale,
            ),
            (
                3,
                Request::Finish {
                    ticket,
                    sequence: 3,
                },
                Status::Conflict,
            ),
            (
                3,
                Request::Finish {
                    ticket,
                    sequence: 4,
                },
                Status::Ok,
            ),
        ] {
            assert_eq!(
                c.receive(caller, request, 7, n, &context, 5, &mut out)
                    .status,
                expected
            );
        }
        c.poll(n, &context, 6);
        execution::run_ready(&context);
        assert_eq!(n.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            c.receive(
                3,
                Request::Close { ticket, nonce: 1 },
                7,
                n,
                &context,
                7,
                &mut out
            )
            .status,
            Status::Ok
        );
        assert_eq!(
            c.receive(
                3,
                Request::Target {
                    ticket,
                    sequence: 6,
                    value: value(6)
                },
                7,
                n,
                &context,
                8,
                &mut out
            )
            .status,
            Status::Stale
        );
    }
}
#[test]
fn capacity_and_lifetime() {
    let (n, context) = device();
    let mut c = Control::new();
    let mut out = [0; DATA_BYTES];
    let first = open(&mut c, n, &context, 1, u32::MAX - 100).ticket;
    assert_eq!(open(&mut c, n, &context, 1, u32::MAX - 100).ticket, first);
    for nonce in 2..=COUNT as u64 {
        open(&mut c, n, &context, nonce, u32::MAX - 100);
    }
    assert_eq!(
        c.receive(
            3,
            Request::Open {
                epoch: 7,
                nonce: 9,
                transformer: 10,
                state: 1
            },
            7,
            n,
            &context,
            0,
            &mut out
        )
        .status,
        Status::Busy
    );
    c.poll(n, &context, 0);
    assert_eq!(
        c.slots.iter().filter(|s| s.session.is_some()).count(),
        COUNT
    );
    c.poll(n, &context, IDLE_MS);
    let replacement = open(&mut c, n, &context, 9, IDLE_MS).ticket;
    assert_ne!(replacement.generation, first.generation);
    assert_eq!(
        c.receive(
            3,
            Request::Finish {
                ticket: first,
                sequence: 0
            },
            7,
            n,
            &context,
            IDLE_MS,
            &mut out
        )
        .status,
        Status::Stale
    );
    for finish in [false, true] {
        let (n, context) = device();
        let mut c = Control::new();
        let ticket = open(&mut c, n, &context, 1, 0).ticket;
        c.receive(
            3,
            Request::Target {
                ticket,
                sequence: 1,
                value: value(9),
            },
            7,
            n,
            &context,
            0,
            &mut out,
        );
        c.poll(n, &context, 0);
        if finish {
            assert_eq!(
                c.receive(
                    3,
                    Request::Finish {
                        ticket,
                        sequence: 1
                    },
                    7,
                    n,
                    &context,
                    1,
                    &mut out
                )
                .status,
                Status::Busy
            );
        }
        c.poll(n, &context, IDLE_MS + 2);
        execution::run_ready(&context);
        c.poll(n, &context, IDLE_MS + 3);
        assert_eq!(n.calls.load(Ordering::Relaxed), finish as u32);
        if finish {
            let r = c.receive(
                3,
                Request::Finish {
                    ticket,
                    sequence: 1,
                },
                7,
                n,
                &context,
                IDLE_MS + 4,
                &mut out,
            );
            assert!(State::decode(r).unwrap().progress.finished);
        }
    }
    // Ordinary already-admitted calls cannot be rewritten by the control primitive.
    execution::reset();
    let t = execution::submit_values(n, &context, 2, &[value(7)]).unwrap();
    assert!(!execution::replace_target(t, value(8)));
    assert!(!execution::cancel_target(t));
    execution::run_ready(&context);
    assert_eq!(n.value.load(Ordering::Relaxed), 7);
}
#[cfg(feature = "host-link")]
#[test]
fn feedback_acceptance() {
    let (n, context) = device();
    super::super::host::disconnect(&context);
    super::super::host::connect(&context);
    let generation = super::super::host::session().unwrap();
    let mut sender = Control::new();
    assert!(sender.watch(2, 5, 7, generation, 0));
    let mut receiver = Control::new();
    let t = open(&mut receiver, n, &context, 5, 0).ticket;
    let mut bytes = [0; REQUEST_BYTES];
    let mut out = [0; DATA_BYTES];
    let (i, _, len) = receiver.outgoing(7, 0, &mut bytes).unwrap();
    let message = Request::decode(&bytes[..len]).unwrap();
    receiver.sent(i, 12, 0);
    let r = sender.receive(2, message, 0, n, &context, 0, &mut out);
    assert_eq!(r.status, Status::Ok);
    assert!(
        matches!(super::super::host::receive(&context),Some(super::super::host::Packet::ControlProgress{feedback}) if feedback.ticket==t)
    );
    receiver.retry(i, 2000);
    let (i, _, len) = receiver.outgoing(7, 2200, &mut bytes).unwrap();
    receiver.sent(i, 13, 2200);
    let r = sender.receive(
        2,
        Request::decode(&bytes[..len]).unwrap(),
        0,
        n,
        &context,
        2200,
        &mut out,
    );
    receiver.reply(i, r, 2200);
    assert!(super::super::host::receive(&context).is_none());
    assert!(receiver.outgoing(7, 2201, &mut bytes).is_none());
    super::super::host::disconnect(&context);
    assert_eq!(
        sender
            .receive(2, message, 0, n, &context, 2300, &mut out)
            .status,
        Status::Stale
    );
    // Reclaim an outstanding route on every listener retirement path.
    for retirement in 0..3 {
        use super::super::host;
        host::connect(&context);
        let mut c = Control::new();
        assert!(c.watch(2, 5, 7, host::session().unwrap(), 0));
        c.offer(
            Target {
                owner: 2,
                nonce: 5,
                ticket: t,
                sequence: 1,
                value: value(1),
            },
            0,
        );
        let (i, _, _) = c.outgoing(7, 0, &mut bytes).unwrap();
        c.sent(i, 20, 0);
        if retirement == 2 {
            c.listeners[0].as_mut().unwrap().received = 1;
            c.poll(n, &context, IDLE_MS);
        } else {
            host::disconnect(&context);
            if retirement == 0 {
                c.poll(n, &context, 1);
            } else {
                host::connect(&context);
                assert!(c.watch(2, 6, 7, host::session().unwrap(), 1));
            }
        }
        assert_eq!(c.canceled(), Some((20, 2)));
        assert!(c.canceled().is_none());
        host::disconnect(&context);
    }
    execution::reset();
}
#[cfg(feature = "host-link")]
#[test]
fn latest_intent_delivery() {
    use super::super::host;
    for delivered in [false, true] {
        let (n, context) = device();
        host::disconnect(&context);
        host::connect(&context);
        let mut source = Control::new();
        let mut receiver = Control::new();
        let ticket = open(&mut receiver, n, &context, 1, 0).ticket;
        assert!(source.watch(2, 1, 7, host::session().unwrap(), 0));
        // An ordinary host request may be outstanding while scalar input arrives.
        host::submit(&context, host::Packet::Status { token: 7 }).unwrap();
        for sequence in 1..=3 {
            host::submit(
                &context,
                host::Packet::ControlTarget {
                    target: Target {
                        owner: 2,
                        nonce: 1,
                        ticket,
                        sequence,
                        value: value(sequence),
                    },
                },
            )
            .unwrap();
        }
        let target = host::take_target().unwrap();
        assert_eq!(target.sequence, 3);
        assert!(host::take_target().is_none());
        assert!(source.offer(target, 0).is_none());
        let mut bytes = [0; REQUEST_BYTES];
        let mut out = [0; DATA_BYTES];
        let (i, owner, len) = source.outgoing(0, 0, &mut bytes).unwrap();
        assert_eq!(owner, 2);
        source.sent(i, 10, 0);
        if delivered {
            receiver.receive(
                3,
                Request::decode(&bytes[..len]).unwrap(),
                7,
                n,
                &context,
                0,
                &mut out,
            );
        }
        // Either the old request or its reply is lost. New intent takes its place
        // immediately, rather than waiting for the old delivery timeout.
        assert_eq!(
            source.offer(
                Target {
                    sequence: 4,
                    value: value(4),
                    ..target
                },
                1
            ),
            Some((10, 2))
        );
        let (i, _, len) = source.outgoing(0, 1, &mut bytes).unwrap();
        source.sent(i, 11, 1);
        let reply = receiver.receive(
            3,
            Request::decode(&bytes[..len]).unwrap(),
            7,
            n,
            &context,
            1,
            &mut out,
        );
        source.reply(i, reply, 1);
        assert!(source.outgoing(0, 2, &mut bytes).is_none());
        receiver.poll(n, &context, 2);
        execution::run_ready(&context);
        receiver.poll(n, &context, 3);
        assert_eq!(
            (
                n.calls.load(Ordering::Relaxed),
                n.value.load(Ordering::Relaxed)
            ),
            (1, 4)
        );
        let (i, _, len) = receiver.outgoing(7, 3, &mut bytes).unwrap();
        receiver.sent(i, 12, 3);
        let reply = source.receive(
            2,
            Request::decode(&bytes[..len]).unwrap(),
            0,
            n,
            &context,
            3,
            &mut out,
        );
        receiver.reply(i, reply, 3);
        assert!(
            matches!(host::receive(&context),Some(host::Packet::ControlProgress{feedback}) if feedback.progress.received==4&&feedback.progress.completed==4)
        );
        host::disconnect(&context);
        source.poll(n, &context, 4);
        assert!(source.outgoing(0, 4, &mut bytes).is_none());
        execution::reset();
    }
}
