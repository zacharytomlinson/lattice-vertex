use super::*;
use crate::{
    ir::Value,
    kernel::peer::{
        frame::{InvokeStatus, StateReadResult},
        Network,
    },
};
use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

const NET: Network = Network {
    founder: 1,
    channel: 15,
};
struct NativeResult {
    state: StateReadResult,
    status: InvokeStatus,
    calls: AtomicUsize,
}
impl Native for NativeResult {
    fn ready(&self) -> bool {
        true
    }
    fn owns(&self, _: *const (), _: u64) -> bool {
        false
    }
    fn read(&self, _: &TaskContext, _: u64) -> StateReadResult {
        self.state
    }
    fn invoke(&self, _: &TaskContext, _: u64, _: &[Value]) -> InvokeStatus {
        self.calls.fetch_add(1, Relaxed);
        self.status
    }
}
const NATIVE: NativeResult = NativeResult {
    state: StateReadResult::Busy,
    status: InvokeStatus::Invoked,
    calls: AtomicUsize::new(0),
};
fn reset() -> TaskContext {
    crate::port::test_port_reset();
    access(|m| *m = Mailbox::new());
    unsafe { TaskContext::assume() }
}
fn poll<'a>(host: &'a mut Host, node: &mut Forwarder, now: u32) -> Option<Message<'a>> {
    host.poll(
        node,
        &Description::new(&crate::surface::EMPTY).unwrap(),
        &NATIVE,
        &mut Invocations::new(),
        &unsafe { TaskContext::assume() },
        now,
    )
}
fn changed(sequence: u32) -> StateChange {
    StateChange {
        owner: 2,
        attribute: 11,
        sequence,
        value: None,
    }
}
fn state_request(token: u32) -> Packet {
    Packet::ReadState {
        token,
        owner: 2,
        attribute: 11,
    }
}
fn state_reply(read: StateRead) -> Message<'static> {
    Message {
        source: 2,
        destination: Some(1),
        path_cost: TimePerByte::UNKNOWN,
        content: Content::StateValue {
            network: NET,
            read,
            hops: 1,
            result: StateReadResult::Busy,
        },
    }
}

#[test]
fn mailbox_bounds() {
    for count in [
        0,
        1,
        CHANGE_CAPACITY - 1,
        CHANGE_CAPACITY,
        CHANGE_CAPACITY + 1,
        2 * CHANGE_CAPACITY + 1,
    ] {
        let context = reset();
        publish(changed(99));
        connect(&context);
        assert_eq!(receive(&context), None);
        assert_eq!(
            submit(&context, Packet::ChangesLost { dropped: 1 }),
            Err(Failure::InvalidRequest)
        );
        submit(&context, Packet::Status { token: 1 }).unwrap();
        for sequence in 0..count {
            publish(changed(sequence as u32));
        }
        assert_eq!(
            submit(&context, Packet::Status { token: 2 }),
            Err(Failure::Busy)
        );
        poll(&mut Host::new(), &mut Forwarder::new(1, NET), 0);
        assert_eq!(
            receive(&context),
            Some(Packet::Ready {
                token: 1,
                address: 1,
                founder: 1,
                channel: 15
            })
        );
        if count > CHANGE_CAPACITY {
            assert_eq!(
                receive(&context),
                Some(Packet::ChangesLost {
                    dropped: (count - CHANGE_CAPACITY) as u32
                })
            );
        }
        for sequence in count.saturating_sub(CHANGE_CAPACITY)..count {
            assert_eq!(
                receive(&context),
                Some(Packet::Changed {
                    change: changed(sequence as u32)
                })
            );
        }
        assert_eq!(receive(&context), None);
        publish(changed(100));
        disconnect(&context);
        publish(changed(101));
        connect(&context);
        assert_eq!(receive(&context), None);
    }
    let context = reset();
    connect(&context);
    submit(&context, Packet::Status { token: 1 }).unwrap();
    publish(changed(1));
    assert!(matches!(receive(&context), Some(Packet::Changed { .. })));
    assert_eq!(
        submit(&context, Packet::Status { token: 2 }),
        Err(Failure::Busy),
        "event delivery cannot release command admission"
    );
    disconnect(&context);
}

#[test]
fn session_correlation() {
    for seed in [100, 200, u16::MAX as u32] {
        let context = reset();
        let mut host = Host::new();
        let mut node = Forwarder::new(1, NET);
        submit(&context, state_request(seed)).unwrap();
        let Content::ReadState { read: old, .. } = poll(&mut host, &mut node, 0).unwrap().content
        else {
            panic!()
        };
        assert_eq!(old.request, seed as u16);
        disconnect(&context);
        submit(&context, state_request(seed)).unwrap();
        let Content::ReadState { read: new, .. } = poll(&mut host, &mut node, 1).unwrap().content
        else {
            panic!()
        };
        assert_eq!(new.request, old.request.wrapping_add(1));
        host.complete(&mut node, state_reply(old), 2);
        assert_eq!(receive(&context), None);
        host.complete(&mut node, state_reply(new), 3);
        assert_eq!(
            receive(&context),
            Some(Packet::State {
                token: seed,
                owner: 2,
                attribute: 11,
                result: StateReadResult::Busy
            })
        );
        host.complete(&mut node, state_reply(new), 4);
        assert_eq!(receive(&context), None);
        disconnect(&context);
    }
}

#[test]
fn request_termination() {
    for request in [
        state_request(42),
        Packet::Invoke {
            token: 42,
            owner: 2,
            transformer: 11,
            arguments: Arguments::new(&[]).unwrap(),
        },
    ] {
        for start in [0, u32::MAX - 20] {
            for (elapsed, expected) in [
                (READ_TIMEOUT_MS - 1, None),
                (READ_TIMEOUT_MS, Some(Failure::Timeout)),
                (READ_TIMEOUT_MS + 1, Some(Failure::Timeout)),
            ] {
                let context = reset();
                let mut host = Host::new();
                let mut node = Forwarder::new(1, NET);
                submit(&context, request).unwrap();
                assert!(poll(&mut host, &mut node, start).is_some());
                let now = start.wrapping_add(elapsed);
                assert_eq!(
                    host.wait_ms(now),
                    Some(READ_TIMEOUT_MS.saturating_sub(elapsed))
                );
                assert!(
                    poll(&mut host, &mut node, now).is_none(),
                    "timeout cannot retry a request"
                );
                assert_eq!(host.pending.is_some(), expected.is_none());
                assert_eq!(node.work_pending(), expected.is_none());
                assert_eq!(
                    receive(&context),
                    expected.map(|failure| Packet::Error { token: 42, failure })
                );
            }
        }
        for disconnected in [false, true] {
            let context = reset();
            let mut host = Host::new();
            let mut node = Forwarder::new(1, NET);
            submit(&context, request).unwrap();
            assert!(poll(&mut host, &mut node, 0).is_some());
            if disconnected {
                disconnect(&context);
                poll(&mut host, &mut node, 1);
            } else {
                host.transmit_failed(&mut node);
            }
            assert!(host.pending.is_none() && !node.work_pending());
            assert_eq!(
                receive(&context),
                if disconnected {
                    None
                } else {
                    Some(Packet::Error {
                        token: 42,
                        failure: Failure::Transmit,
                    })
                }
            );
        }
    }
}

#[test]
fn state_results() {
    let description = Description::new(&crate::surface::EMPTY).unwrap();
    for owner in [1, 2] {
        let context = reset();
        let mut host = Host::new();
        let mut node = Forwarder::new(1, NET);
        for result in [
            StateReadResult::Busy,
            StateReadResult::UnknownAttribute,
            StateReadResult::Value {
                sequence: 8,
                value: None,
            },
            StateReadResult::Value {
                sequence: 9,
                value: Some(Value::boolean(false)),
            },
        ] {
            let native = NativeResult {
                state: result,
                ..NATIVE
            };
            submit(
                &context,
                Packet::ReadState {
                    token: 1,
                    owner,
                    attribute: 11,
                },
            )
            .unwrap();
            let out = host.poll(
                &mut node,
                &description,
                &native,
                &mut Invocations::new(),
                &context,
                0,
            );
            assert_eq!(out.is_some(), owner != 1);
            if let Some(out) = out {
                let Content::ReadState { read, .. } = out.content else {
                    panic!()
                };
                host.complete(
                    &mut node,
                    Message {
                        content: Content::StateValue {
                            network: NET,
                            read,
                            result,
                            hops: 1,
                        },
                        ..state_reply(read)
                    },
                    1,
                );
            }
            assert_eq!(
                receive(&context),
                Some(Packet::State {
                    token: 1,
                    owner,
                    attribute: 11,
                    result
                })
            );
            assert_eq!(native.calls.load(Relaxed), 0);
        }
    }
}

#[test]
fn call_results() {
    let description = Description::new(&crate::surface::EMPTY).unwrap();
    for owner in [1, 2] {
        let context = reset();
        let mut host = Host::new();
        let mut node = Forwarder::new(1, NET);
        for (i, status) in [
            InvokeStatus::Invoked,
            InvokeStatus::Busy,
            InvokeStatus::Arguments,
            InvokeStatus::UnknownTransformer,
            InvokeStatus::ApplianceError,
        ]
        .into_iter()
        .enumerate()
        {
            let native = NativeResult { status, ..NATIVE };
            let token = i as u32 + 2;
            submit(
                &context,
                Packet::Invoke {
                    token,
                    owner,
                    transformer: 11,
                    arguments: Arguments::new(&[]).unwrap(),
                },
            )
            .unwrap();
            let mut calls = Invocations::new();
            let out = host.poll(&mut node, &description, &native, &mut calls, &context, 0);
            assert_eq!(out.is_some(), owner != 1);
            if let Some(out) = out {
                let Content::Invoke { call, .. } = out.content else {
                    panic!()
                };
                host.complete(
                    &mut node,
                    Message {
                        source: 2,
                        destination: Some(1),
                        path_cost: TimePerByte::UNKNOWN,
                        content: Content::InvocationResult {
                            network: NET,
                            call,
                            status,
                            hops: 1,
                        },
                    },
                    1,
                );
            }
            assert!(host
                .poll(
                    &mut node,
                    &description,
                    &native,
                    &mut calls,
                    &context,
                    READ_TIMEOUT_MS
                )
                .is_none());
            assert_eq!(
                receive(&context),
                Some(Packet::Invoked {
                    token,
                    owner,
                    transformer: 11,
                    status
                })
            );
            assert_eq!(native.calls.load(Relaxed), usize::from(owner == 1));
        }
    }
}

#[test]
fn enumeration_bounds() {
    let description = Description::new(&crate::surface::EMPTY).unwrap();
    let context = reset();
    let mut host = Host::new();
    let mut node = Forwarder::new(1, NET);
    for offset in [
        description.len() - 1,
        description.len(),
        description.len() + 1,
    ] {
        submit(
            &context,
            Packet::ReadSurface {
                token: 3,
                owner: 1,
                offset,
            },
        )
        .unwrap();
        assert!(poll(&mut host, &mut node, 0).is_none());
        match receive(&context).unwrap() {
            Packet::Surface { total, data, .. } => {
                assert_eq!(total, description.len());
                assert_eq!(data.as_slice().len(), usize::from(total - offset));
            }
            Packet::Error {
                failure: Failure::InvalidRequest,
                ..
            } => assert!(offset > description.len()),
            other => panic!("unexpected reply: {other:?}"),
        }
    }
    submit(
        &context,
        Packet::ReadLinks {
            token: 4,
            owner: 1,
            after: 0,
            via: 0,
        },
    )
    .unwrap();
    assert!(poll(&mut host, &mut node, 0).is_none());
    assert!(matches!(
        receive(&context),
        Some(Packet::Link {
            token: 4,
            owner: 1,
            peer: 0,
            ..
        })
    ));
}

#[test]
fn address_admission() {
    let context = reset();
    let mut host = Host::new();
    let mut node = Forwarder::new(1, NET);
    for invalid in [0, u64::MAX] {
        for request in [
            Packet::ReadState {
                token: 1,
                owner: invalid,
                attribute: 11,
            },
            Packet::ReadState {
                token: 1,
                owner: 2,
                attribute: invalid,
            },
            Packet::Invoke {
                token: 1,
                owner: 2,
                transformer: invalid,
                arguments: Arguments::new(&[]).unwrap(),
            },
        ] {
            submit(&context, request).unwrap();
            assert!(poll(&mut host, &mut node, 0).is_none());
            assert_eq!(
                receive(&context),
                Some(Packet::Error {
                    token: 1,
                    failure: Failure::InvalidRequest
                })
            );
        }
    }
    for (via, after) in [(1, 0), (2, 0), (u64::MAX, 0), (0, u64::MAX)] {
        submit(
            &context,
            Packet::ReadLinks {
                token: 1,
                owner: 2,
                via,
                after,
            },
        )
        .unwrap();
        assert!(poll(&mut host, &mut node, 0).is_none());
        assert_eq!(
            receive(&context),
            Some(Packet::Error {
                token: 1,
                failure: Failure::InvalidRequest
            })
        );
    }
}
