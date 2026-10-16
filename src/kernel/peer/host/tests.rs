use super::*;
use crate::kernel::peer::{frame::Content, Network};
use crate::{
    ir::Value,
    kernel::peer::frame::{InvokeStatus, StateReadResult},
};
struct Empty;
impl Native for Empty {
    fn ready(&self) -> bool {
        true
    }
    fn owns(&self, _: *const (), _: u64) -> bool {
        false
    }
    fn read(&self, _: &TaskContext, _: u64) -> StateReadResult {
        StateReadResult::UnknownAttribute
    }
    fn invoke(&self, _: &TaskContext, _: u64, _: &[Value]) -> InvokeStatus {
        InvokeStatus::UnknownTransformer
    }
}
fn poll(host: &mut Host, forwarder: &mut Forwarder, now: u32) -> Option<Message<'static>> {
    let context = unsafe { TaskContext::assume() };
    let description = Description::new(&crate::surface::EMPTY).unwrap();
    host.poll(
        forwarder,
        &description,
        &Empty,
        &mut Invocations::new(),
        &context,
        now,
    )
    .map(|m| delivered(m, 2))
}
fn delivered(message: Message<'_>, recipient: u64) -> Message<'static> {
    // Link messages borrow no payload; reconstruct after exercising the real MAC codec.
    let mut bytes = [0; crate::kernel::radio::MAX_FRAME_SIZE];
    let n = message.encode(1, &mut bytes);
    let decoded = Message::decode(&bytes[..n], recipient, 15).unwrap();
    let content = match decoded.content {
        Content::ReadLinks {
            network,
            read,
            remaining,
        } => Content::ReadLinks {
            network,
            read,
            remaining,
        },
        Content::LinkEntry {
            network,
            read,
            revision,
            link,
            hops,
        } => Content::LinkEntry {
            network,
            read,
            revision,
            link,
            hops,
        },
        _ => panic!("unexpected message"),
    };
    Message {
        source: decoded.source,
        destination: decoded.destination,
        content,
    }
}
#[test]
fn local_host_reads_use_real_routes_and_disconnect_cannot_reuse_a_completion() {
    crate::port::test_port_reset();
    access(|m| *m = Mailbox::new());
    let context = unsafe { TaskContext::assume() };
    let network = Network {
        founder: 1,
        channel: 15,
    };
    let mut local = Forwarder::new(1, network);
    let mut remote = Forwarder::new(2, network);
    local.record_presence(2, 0);
    let offer = local.maintain_links(1).unwrap();
    let accept = remote.receive_link(offer, 2).unwrap();
    let confirm = local.receive_link(accept, 3).unwrap();
    remote.receive_link(confirm, 4);
    let mut host = Host::new();
    submit(&context, Packet::Status { token: 1 }).unwrap();
    assert_eq!(
        submit(&context, Packet::Status { token: 2 }),
        Err(Failure::Busy)
    );
    assert!(poll(&mut host, &mut local, 5).is_none());
    assert_eq!(
        receive(&context),
        Some(Packet::Ready {
            token: 1,
            address: 1,
            founder: 1,
            channel: 15
        })
    );
    submit(
        &context,
        Packet::ReadLinks {
            token: 2,
            owner: 1,
            after: 0,
            via: 0,
        },
    )
    .unwrap();
    assert!(poll(&mut host, &mut local, 6).is_none());
    assert!(matches!(
        receive(&context),
        Some(Packet::Link {
            owner: 1,
            peer: 2,
            ..
        })
    ));
    let request = Packet::ReadLinks {
        token: 77,
        owner: 2,
        after: 0,
        via: 0,
    };
    submit(&context, request).unwrap();
    let outgoing = poll(&mut host, &mut local, 7).unwrap();
    assert_eq!(outgoing.destination, Some(2));
    let reply = remote.receive_links(delivered(outgoing, 2), 8).unwrap();
    let mut wrong = reply;
    if let Content::LinkEntry { ref mut read, .. } = wrong.content {
        read.request = read.request.wrapping_add(1);
    }
    host.complete(&mut local, delivered(wrong, 1), 9);
    assert_eq!(receive(&context), None);
    // Closing USB cancels only this host's operation, without forgetting peers.
    disconnect(&context);
    submit(&context, request).unwrap();
    let fresh = poll(&mut host, &mut local, 10).unwrap();
    assert_ne!(
        fresh.content, outgoing.content,
        "air token must not reset on USB reconnect"
    );
    host.complete(&mut local, delivered(reply, 1), 11);
    assert_eq!(receive(&context), None);
    let reply = remote.receive_links(delivered(fresh, 2), 12).unwrap();
    host.complete(&mut local, delivered(reply, 1), 13);
    assert!(matches!(
        receive(&context),
        Some(Packet::Link {
            token: 77,
            owner: 2,
            peer: 1,
            ..
        })
    ));
    host.complete(&mut local, delivered(reply, 1), 14);
    assert_eq!(receive(&context), None);
    submit(&context, request).unwrap();
    assert!(poll(&mut host, &mut local, 15).is_some());
    assert!(poll(&mut host, &mut local, 15 + READ_TIMEOUT_MS).is_none());
    assert_eq!(
        receive(&context),
        Some(Packet::Error {
            token: 77,
            failure: Failure::Timeout
        })
    );
    // A reference advertised by the adjacent peer remains traversable
    // through that peer, with the completion returning to this local host.
    let mut third = Forwarder::new(3, network);
    remote.record_presence(3, 8_020);
    let offer = remote.maintain_links(8_021).unwrap();
    let accept = third.receive_link(offer, 8_022).unwrap();
    let confirm = remote.receive_link(accept, 8_023).unwrap();
    third.receive_link(confirm, 8_024);
    submit(
        &context,
        Packet::ReadLinks {
            token: 78,
            owner: 3,
            after: 0,
            via: 2,
        },
    )
    .unwrap();
    let first = poll(&mut host, &mut local, 8_025).unwrap();
    assert_eq!(first.destination, Some(2));
    let onward = remote.receive_links(delivered(first, 2), 8_026).unwrap();
    assert_eq!(onward.destination, Some(3));
    let reply = third.receive_links(delivered(onward, 3), 8_027).unwrap();
    let reply = remote.receive_links(delivered(reply, 2), 8_028).unwrap();
    host.complete(&mut local, delivered(reply, 1), 8_029);
    assert!(matches!(
        receive(&context),
        Some(Packet::Link {
            token: 78,
            owner: 3,
            via: 2,
            peer: 2,
            ..
        })
    ));
    for via in [1, 3, u64::MAX] {
        submit(
            &context,
            Packet::ReadLinks {
                token: 79,
                owner: 3,
                after: 0,
                via,
            },
        )
        .unwrap();
        assert!(poll(&mut host, &mut local, 8_030).is_none());
        assert_eq!(
            receive(&context),
            Some(Packet::Error {
                token: 79,
                failure: Failure::InvalidRequest
            })
        );
    }
    disconnect(&context);
}
#[test]
fn surface_state_and_calls_preserve_native_results_and_never_retry_mutations() {
    use crate::ir::Integer;
    use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};
    struct Device {
        level: AtomicU8,
        calls: AtomicU32,
    }
    impl Native for Device {
        fn ready(&self) -> bool {
            true
        }
        fn owns(&self, _: *const (), _: u64) -> bool {
            false
        }
        fn read(&self, _: &TaskContext, attribute: u64) -> StateReadResult {
            if attribute != 11 {
                return StateReadResult::UnknownAttribute;
            }
            StateReadResult::Value {
                sequence: self.calls.load(Ordering::Relaxed),
                value: Some(
                    Value::integer(Integer::U8, self.level.load(Ordering::Relaxed) as i128)
                        .unwrap(),
                ),
            }
        }
        fn invoke(&self, _: &TaskContext, transformer: u64, values: &[Value]) -> InvokeStatus {
            if transformer != 22 {
                return InvokeStatus::UnknownTransformer;
            }
            if values.len() != 1 || values[0].ty() != crate::ir::Type::Integer(Integer::U8) {
                return InvokeStatus::Arguments;
            }
            let value = values[0].as_integer().unwrap();
            if value > 200 {
                return InvokeStatus::ApplianceError;
            }
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.level.store(value as u8, Ordering::Relaxed);
            InvokeStatus::Invoked
        }
    }
    fn bytes(message: Message<'_>) -> std::vec::Vec<u8> {
        let mut bytes = [0; crate::kernel::radio::MAX_FRAME_SIZE];
        let n = message.encode(1, &mut bytes);
        bytes[..n].to_vec()
    }
    crate::port::test_port_reset();
    access(|m| *m = Mailbox::new());
    let context = unsafe { TaskContext::assume() };
    let description = Description::new(&crate::surface::EMPTY).unwrap();
    let device = Device {
        level: AtomicU8::new(0),
        calls: AtomicU32::new(0),
    };
    let mut invocations = Invocations::new();
    let mut host = Host::new();
    let network = Network {
        founder: 1,
        channel: 15,
    };
    let mut local = Forwarder::new(1, network);
    let mut remote = Forwarder::new(2, network);
    let mut range = [0; SURFACE_BYTES];
    let requests = [
        Packet::ReadSurface {
            token: 1,
            owner: 2,
            offset: 0,
        },
        Packet::ReadState {
            token: 2,
            owner: 2,
            attribute: 11,
        },
        Packet::ReadState {
            token: 3,
            owner: 2,
            attribute: 99,
        },
        Packet::Invoke {
            token: 4,
            owner: 2,
            transformer: 22,
            arguments: Arguments::new(&[Value::integer(Integer::U8, 75).unwrap()]).unwrap(),
        },
        Packet::ReadState {
            token: 5,
            owner: 2,
            attribute: 11,
        },
        Packet::Invoke {
            token: 6,
            owner: 2,
            transformer: 22,
            arguments: Arguments::new(&[Value::boolean(true)]).unwrap(),
        },
        Packet::Invoke {
            token: 7,
            owner: 2,
            transformer: 22,
            arguments: Arguments::new(&[Value::integer(Integer::U8, 201).unwrap()]).unwrap(),
        },
        Packet::Invoke {
            token: 8,
            owner: 2,
            transformer: 99,
            arguments: Arguments::new(&[]).unwrap(),
        },
    ];
    for (i, request) in requests.into_iter().enumerate() {
        let now = i as u32 * 10;
        submit(&context, request).unwrap();
        let sent = bytes(
            host.poll(
                &mut local,
                &description,
                &device,
                &mut invocations,
                &context,
                now,
            )
            .unwrap(),
        );
        let incoming = Message::decode(&sent, 2, 15).unwrap();
        let response = match incoming.content {
            Content::ReadSurface { .. } => remote
                .receive_surface(incoming, &description, &mut range, now + 1)
                .unwrap(),
            Content::ReadState { .. } => remote
                .receive_state(incoming, now + 1, |a| device.read(&context, a))
                .unwrap(),
            Content::Invoke {
                call, arguments, ..
            } => {
                let out = remote.receive_control(incoming, now + 1, |c, a| {
                    invocations.invoke(&device, &context, c, a)
                })[0]
                    .unwrap();
                // Identical radio copies replay the native result, including failures.
                let before = device.calls.load(Ordering::Relaxed);
                invocations.invoke(&device, &context, call, arguments);
                assert_eq!(device.calls.load(Ordering::Relaxed), before);
                out
            }
            _ => unreachable!(),
        };
        let encoded = bytes(response);
        host.complete(
            &mut local,
            Message::decode(&encoded, 1, 15).unwrap(),
            now + 2,
        );
        let reply = receive(&context).unwrap();
        match (i, reply) {
            (
                0,
                Packet::Surface {
                    total: 13, data, ..
                },
            ) => assert_eq!(&data.as_slice()[..4], b"LSF\x01"),
            (
                1,
                Packet::State {
                    result:
                        StateReadResult::Value {
                            sequence: 0,
                            value: Some(v),
                        },
                    ..
                },
            ) => assert_eq!(v.as_integer(), Some(0)),
            (
                2,
                Packet::State {
                    result: StateReadResult::UnknownAttribute,
                    ..
                },
            ) => {}
            (
                3,
                Packet::Invoked {
                    status: InvokeStatus::Invoked,
                    ..
                },
            ) => {}
            (
                4,
                Packet::State {
                    result:
                        StateReadResult::Value {
                            sequence: 1,
                            value: Some(v),
                        },
                    ..
                },
            ) => assert_eq!(v.as_integer(), Some(75)),
            (
                5,
                Packet::Invoked {
                    status: InvokeStatus::Arguments,
                    ..
                },
            ) => {}
            (
                6,
                Packet::Invoked {
                    status: InvokeStatus::ApplianceError,
                    ..
                },
            ) => {}
            (
                7,
                Packet::Invoked {
                    status: InvokeStatus::UnknownTransformer,
                    ..
                },
            ) => {}
            _ => panic!("unexpected reply {i}: {reply:?}"),
        }
    }
    assert_eq!(device.calls.load(Ordering::Relaxed), 1);
    // A lost result leaves the call uncertain, without invoking it again.
    submit(
        &context,
        Packet::Invoke {
            token: 99,
            owner: 2,
            transformer: 22,
            arguments: Arguments::new(&[Value::integer(Integer::U8, 100).unwrap()]).unwrap(),
        },
    )
    .unwrap();
    let sent = bytes(
        host.poll(
            &mut local,
            &description,
            &device,
            &mut invocations,
            &context,
            100,
        )
        .unwrap(),
    );
    remote.receive_control(Message::decode(&sent, 2, 15).unwrap(), 101, |c, a| {
        invocations.invoke(&device, &context, c, a)
    });
    assert!(host
        .poll(
            &mut local,
            &description,
            &device,
            &mut invocations,
            &context,
            100 + READ_TIMEOUT_MS
        )
        .is_none());
    assert_eq!(
        receive(&context),
        Some(Packet::Error {
            token: 99,
            failure: Failure::Timeout
        })
    );
    assert_eq!(device.calls.load(Ordering::Relaxed), 2);
    // Local reads and calls use the same native binding, with no radio request.
    for request in [
        Packet::ReadSurface {
            token: 100,
            owner: 1,
            offset: 0,
        },
        Packet::ReadState {
            token: 101,
            owner: 1,
            attribute: 11,
        },
        Packet::Invoke {
            token: 102,
            owner: 1,
            transformer: 22,
            arguments: Arguments::new(&[Value::integer(Integer::U8, 50).unwrap()]).unwrap(),
        },
    ] {
        submit(&context, request).unwrap();
        assert!(host
            .poll(
                &mut local,
                &description,
                &device,
                &mut invocations,
                &context,
                9_000
            )
            .is_none());
        assert!(receive(&context).is_some());
    }
    assert_eq!(device.calls.load(Ordering::Relaxed), 3);
    disconnect(&context);
}

#[test]
fn rebooted_reader_does_not_deliberately_restart_at_token_one() {
    crate::port::test_port_reset();
    access(|m| *m = Mailbox::new());
    let context = unsafe { TaskContext::assume() };
    let network = Network {
        founder: 1,
        channel: 15,
    };
    let mut remote = Forwarder::new(2, network);
    for (now, token) in [(0, 100u32), (1, 200u32)] {
        disconnect(&context);
        let mut host = Host::new();
        let mut local = Forwarder::new(1, network);
        submit(
            &context,
            Packet::ReadLinks {
                token,
                owner: 2,
                after: 0,
                via: 0,
            },
        )
        .unwrap();
        let request = poll(&mut host, &mut local, now).unwrap();
        let reply = remote
            .receive_links(delivered(request, 2), now)
            .expect("new boot was suppressed as an old read");
        host.complete(&mut local, delivered(reply, 1), now);
        assert!(matches!(receive(&context), Some(Packet::Link { .. })));
    }
    disconnect(&context);
}
