//! Native invocation and bounded replay of its result at the appliance owner.
//! A result records the native call's return, not a physical post-state.

use super::frame::{
    decode_arguments, Invocation, InvokeStatus, CALL_ARGUMENT_BYTES, MAX_CALL_ARGUMENTS,
};
use crate::{ir::Value, kernel::appliance::Native, sync::TaskContext};

const RECENT: usize = 8;
#[derive(Clone, Copy)]
struct InvocationRecord {
    caller: u64,
    transformer: u64,
    request: u32,
    arguments: [u8; CALL_ARGUMENT_BYTES],
    length: u8,
    result: InvokeStatus,
}
impl InvocationRecord {
    const EMPTY: Self = Self {
        caller: 0,
        transformer: 0,
        request: 0,
        arguments: [0; CALL_ARGUMENT_BYTES],
        length: 0,
        result: InvokeStatus::Busy,
    };
}

pub(super) struct Invocations {
    records: [InvocationRecord; RECENT],
    next: usize,
}
const _: () = assert!(core::mem::size_of::<Invocations>() <= 768);

impl Invocations {
    pub(super) const fn new() -> Self {
        Self {
            records: [InvocationRecord::EMPTY; RECENT],
            next: 0,
        }
    }

    pub(super) fn invoke(
        &mut self,
        native: &dyn Native,
        context: &TaskContext,
        call: Invocation,
        arguments: &[u8],
    ) -> InvokeStatus {
        if let Some(old) = self
            .records
            .iter()
            .find(|r| r.caller == call.caller && r.request == call.request)
        {
            return if old.transformer == call.transformer
                && &old.arguments[..old.length as usize] == arguments
            {
                old.result
            } else {
                InvokeStatus::ConflictingRequest
            };
        }
        if arguments.len() > CALL_ARGUMENT_BYTES {
            return InvokeStatus::Arguments;
        }
        let mut values = [Value::boolean(false); MAX_CALL_ARGUMENTS];
        let result = match decode_arguments(arguments, &mut values) {
            Some(values) => native.invoke(context, call.transformer, values),
            None => InvokeStatus::Arguments,
        };
        let mut record = InvocationRecord {
            caller: call.caller,
            transformer: call.transformer,
            request: call.request,
            length: arguments.len() as u8,
            result,
            ..InvocationRecord::EMPTY
        };
        record.arguments[..arguments.len()].copy_from_slice(arguments);
        self.records[self.next] = record;
        self.next = (self.next + 1) % RECENT;
        result
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests {
    use crate::kernel::radio::cost::TimePerByte;
    extern crate std;
    use self::std::{boxed::Box, vec::Vec};
    use super::super::{
        forwarding::Forwarder,
        frame::{Content, Message, MAX_HOPS},
        Network,
    };
    use super::*;
    use crate::{
        ir::{Effect, Expr, Integer},
        kernel::{change, Appliance},
        observation::{self, Dependent},
        surface::{
            Arguments, Definition, Operation, Predicate, State, StateHandle, StateTransformer,
            Surface, TransformerHandle,
        },
    };

    #[derive(Default)]
    struct Device {
        observed: Option<bool>,
        requested: Option<bool>,
        other: bool,
        calls: u32,
    }
    struct Power;
    impl State for Power {
        type Appliance = Device;
        type Value = bool;
        fn read(a: &Device) -> Option<bool> {
            a.observed
        }
    }
    struct Other;
    impl State for Other {
        type Appliance = Device;
        type Value = bool;
        fn read(a: &Device) -> Option<bool> {
            Some(a.other)
        }
    }
    static POWER: StateHandle<Device> = StateHandle::new::<Power>();
    static OTHER: StateHandle<Device> = StateHandle::new::<Other>();
    static HIDDEN: StateHandle<Device> = StateHandle::new::<Other>();
    static TRUE: Definition<Device> = Definition {
        body: Expr::Constant(Value::boolean(true)),
        arguments: &[],
        inputs: &[],
    };
    struct SetPower;
    impl StateTransformer for SetPower {
        type Appliance = Device;
        type Arguments = (bool,);
        type Error = ();
        const DEFINITION: Operation<Device> = Operation {
            precondition: Predicate {
                definition: &TRUE,
                inputs: &[],
            },
            effect: Effect {
                inputs: &[],
                outputs: &[POWER.definition()],
                results: &[Expr::Argument(0)],
                arguments: Self::Arguments::TYPES,
            },
        };
        fn invoke(a: &mut Device, (value,): (bool,)) -> Result<(), ()> {
            a.calls += 1;
            a.requested = Some(value); // A request is not an established observation.
            Ok(())
        }
    }
    struct PartialFailure;
    impl StateTransformer for PartialFailure {
        type Appliance = Device;
        type Arguments = (bool,);
        type Error = ();
        const DEFINITION: Operation<Device> = SetPower::DEFINITION;
        fn invoke(a: &mut Device, (value,): (bool,)) -> Result<(), ()> {
            a.calls += 1;
            POWER.update(a, |a| a.observed = Some(value));
            Err(())
        }
    }
    static SET: TransformerHandle<Device, ()> = TransformerHandle::new::<SetPower>();
    static FAIL: TransformerHandle<Device, ()> = TransformerHandle::new::<PartialFailure>();
    static SURFACE: Surface<'static, Device, ()> = Surface {
        states: &[&POWER, &OTHER],
        transformers: &[&SET, &FAIL],
        axioms: &[],
        invariants: &[],
    };
    const NETWORK: Network = Network {
        founder: 9,
        channel: 15,
    };
    const ON: &[u8] = &[1, 0, 1];
    const OFF: &[u8] = &[1, 0, 0];

    struct Fixture {
        device: &'static Appliance<Device, ()>,
        context: TaskContext,
    }
    impl Fixture {
        fn new() -> Self {
            crate::port::test_port_reset();
            change::reset();
            POWER.set_dependents(None);
            let context = unsafe { TaskContext::assume() };
            let device = Box::leak(Box::new(Appliance::new(&SURFACE)));
            device.initialize(&context, Device::default());
            change::bind(device);
            Self { device, context }
        }
        fn calls(&self) -> u32 {
            self.device.with(&self.context, |a| a.calls)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            POWER.set_dependents(None);
            change::reset();
        }
    }
    fn call(request: u32) -> Invocation {
        Invocation {
            caller: 1,
            owner: 3,
            request,
            transformer: core::ptr::from_ref(&SET) as usize as u64,
        }
    }
    fn request(call: Invocation, arguments: &[u8]) -> Message<'_> {
        Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: Some(2),
            content: Content::Invoke {
                copies: 1,
                network: NETWORK,
                call,
                remaining: MAX_HOPS,
                arguments,
            },
        }
    }
    fn drain() -> Vec<change::Change> {
        core::iter::from_fn(change::take).collect()
    }

    #[test]
    fn redundant_encoded_routes_reconverge_or_lose_a_branch_without_repeating_native_work() {
        use super::super::frame::{LinkRead, StateRead, StateReadResult};
        // disjoint branches, reconvergence, one lost branch, late reconvergence
        for (merge, drop_first, late_second) in [
            (false, false, false),
            (true, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            let f = Fixture::new();
            let mut invocations = Invocations::new();
            let mut nodes = [
                Forwarder::new(2, NETWORK),
                Forwarder::new(3, NETWORK),
                Forwarder::new(4, NETWORK),
                Forwarder::new(5, NETWORK),
                Forwarder::new(7, NETWORK),
            ];
            let ids = [2, 3, 4, 5, 7];
            // Seed directions through correlated reads, including an alternate
            // guided approach at the fork. No test-only routing API is needed.
            for (i, next, hops, via, token) in [
                (0, 3, if merge { 3 } else { 2 }, 0, 1),
                (0, 4, if merge { 3 } else { 2 }, 4, 2),
                (
                    1,
                    if merge { 5 } else { 7 },
                    if merge { 2 } else { 1 },
                    0,
                    3,
                ),
                (
                    2,
                    if merge { 5 } else { 7 },
                    if merge { 2 } else { 1 },
                    0,
                    4,
                ),
                (3, 7, 1, 0, 5),
            ] {
                let read = LinkRead {
                    reader: 1,
                    owner: 7,
                    request: token,
                    after: 0,
                    via,
                };
                let q = Message {
                    path_cost: TimePerByte::UNKNOWN,
                    source: if i == 0 { 1 } else { 2 },
                    destination: Some(ids[i]),
                    content: Content::ReadLinks {
                        network: NETWORK,
                        read,
                        remaining: 7,
                    },
                };
                let started = u32::from(token) * 10;
                nodes[i].receive_links(q, started).unwrap();
                let reply = Message {
                    path_cost: TimePerByte::UNKNOWN,
                    source: next,
                    destination: Some(ids[i]),
                    content: Content::LinkEntry {
                        network: NETWORK,
                        read,
                        revision: 0,
                        link: None,
                        hops,
                    },
                };
                nodes[i].receive_links(reply, started + 1).unwrap();
            }
            let call = Invocation {
                owner: 7,
                ..call(91)
            };
            let initial = Message {
                path_cost: TimePerByte::UNKNOWN,
                source: 1,
                destination: Some(2),
                content: Content::Invoke {
                    network: NETWORK,
                    call,
                    remaining: 8,
                    copies: 2,
                    arguments: ON,
                },
            };
            let mut wire = [0; crate::kernel::radio::MAX_FRAME_SIZE];
            let size = initial.encode(0, &mut wire);
            let mut pending = self::std::vec![(1, 2, wire, size)];
            let mut deferred = None;
            let mut cursor = 0;
            let mut responses = 0;
            let mut owner_deliveries = 0;
            loop {
                if cursor == pending.len() {
                    if let Some(delayed) = deferred.take() {
                        pending.push(delayed);
                    } else {
                        break;
                    }
                }
                assert!(pending.len() < 32, "duplicate forwarding did not terminate");
                let (source, destination, wire, size) = pending[cursor];
                cursor += 1;
                if source == 2 && destination == 3 && drop_first {
                    continue;
                }
                if source == 2
                    && destination == 4
                    && late_second
                    && deferred.is_none()
                    && responses == 0
                {
                    deferred = Some((source, destination, wire, size));
                    continue;
                }
                let incoming = Message::decode(&wire[..size], destination, 15).unwrap();
                if destination == 1 {
                    assert!(
                        matches!(incoming.content,Content::InvocationResult {call:c,status:InvokeStatus::Invoked,..} if c==call)
                    );
                    responses += 1;
                    continue;
                }
                let index = ids.iter().position(|id| *id == destination).unwrap();
                let output =
                    nodes[index].receive_control(incoming, 100 + cursor as u32, |c, args| {
                        assert_eq!(destination, 7, "relay executed native code");
                        owner_deliveries += 1;
                        invocations.invoke(f.device, &f.context, c, args)
                    });
                for out in output.into_iter().flatten() {
                    let next = out.destination.expect("known path unexpectedly flooded");
                    let mut bytes = [0; crate::kernel::radio::MAX_FRAME_SIZE];
                    let n = out.encode(cursor as u8, &mut bytes);
                    pending.push((out.source, next, bytes, n));
                }
            }
            assert_eq!(f.calls(), 1);
            assert_eq!(
                owner_deliveries,
                if merge { 1 } else { 2 },
                "merge={merge} drop={drop_first} late={late_second}, transmissions={:?}",
                pending
                    .iter()
                    .map(|(s, d, _, _)| (*s, *d))
                    .collect::<Vec<_>>()
            );
            assert_eq!(responses, if drop_first { 1 } else { 2 });
            assert!(
                !change::pending(),
                "network copies fabricated physical observations"
            );
            f.device
                .with(&f.context, |a| POWER.update(a, |a| a.observed = Some(true)));
            assert_eq!(drain().len(), 1);
            if drop_first {
                nodes[0].expire(10_101);
                let q = Message {
                    path_cost: TimePerByte::UNKNOWN,
                    source: 1,
                    destination: Some(2),
                    content: Content::ReadState {
                        network: NETWORK,
                        read: StateRead {
                            reader: 1,
                            owner: 7,
                            request: 99,
                            attribute: 42,
                        },
                        remaining: 8,
                    },
                };
                let next = nodes[0]
                    .receive_state(q, 10_102, |_| StateReadResult::UnknownAttribute)
                    .unwrap();
                assert_eq!(next.destination, Some(4));
            }
        }
    }

    #[test]
    fn state_reads_report_established_values_without_dispatching_or_consuming_deltas() {
        use super::super::frame::StateReadResult;
        let f = Fixture::new();
        let attribute = core::ptr::from_ref(POWER.definition()) as usize as u64;
        assert_eq!(
            f.device.read(&f.context, attribute),
            StateReadResult::Value {
                sequence: 0,
                value: None
            }
        );
        for unknown in [
            0,
            123,
            core::ptr::from_ref(HIDDEN.definition()) as usize as u64,
            core::ptr::from_ref(&SET) as usize as u64,
        ] {
            assert_eq!(
                f.device.read(&f.context, unknown),
                StateReadResult::UnknownAttribute
            );
        }
        f.device.with(&f.context, |_| {
            assert_eq!(f.device.read(&f.context, attribute), StateReadResult::Busy)
        });
        assert!(!change::pending());
        let mut invocations = Invocations::new();
        assert_eq!(
            invocations.invoke(f.device, &f.context, call(1), ON),
            InvokeStatus::Invoked
        );
        assert_eq!(
            f.device.read(&f.context, attribute),
            StateReadResult::Value {
                sequence: 0,
                value: None
            }
        );
        f.device.with(&f.context, |a| {
            POWER.update(a, |a| a.observed = Some(false))
        });
        for _ in 0..2 {
            assert_eq!(
                f.device.read(&f.context, attribute),
                StateReadResult::Value {
                    sequence: 1,
                    value: Some(Value::boolean(false))
                }
            );
        }
        assert_eq!(
            change::take().unwrap().sequence,
            1,
            "read consumed or registered a delta"
        );
        assert!(!change::pending());
        assert_eq!(f.calls(), 1, "read invoked a transformer");
        // Other attributes can advance the watermark without changing this value.
        f.device
            .with(&f.context, |a| OTHER.update(a, |a| a.other = true));
        assert_eq!(
            f.device.read(&f.context, attribute),
            StateReadResult::Value {
                sequence: 2,
                value: Some(Value::boolean(false))
            }
        );
        f.device
            .with(&f.context, |a| POWER.update(a, |a| a.observed = None));
        let records = drain();
        assert_eq!(
            f.device.read(&f.context, attribute),
            StateReadResult::Value {
                sequence: 3,
                value: None
            }
        );
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn state_reply_crosses_two_relays_and_keeps_its_value_when_a_newer_delta_arrives() {
        use super::super::frame::{StateRead, StateReadResult};
        let f = Fixture::new();
        f.device.with(&f.context, |a| {
            POWER.update(a, |a| a.observed = Some(false))
        });
        let mut relay_a = Forwarder::new(2, NETWORK);
        let mut relay_b = Forwarder::new(3, NETWORK);
        let mut owner = Forwarder::new(4, NETWORK);
        let read = StateRead {
            reader: 1,
            owner: 4,
            request: 7,
            attribute: core::ptr::from_ref(POWER.definition()) as usize as u64,
        };
        let mut message = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: None,
            content: Content::ReadState {
                network: NETWORK,
                read,
                remaining: 8,
            },
        };
        let mut wire = [0; crate::kernel::radio::MAX_FRAME_SIZE];
        for (address, node) in [(2, &mut relay_a), (3, &mut relay_b), (4, &mut owner)] {
            let n = message.encode(0, &mut wire);
            let incoming = Message::decode(&wire[..n], address, 15).unwrap();
            message = node
                .receive_state(incoming, 0, |attribute| {
                    assert_eq!(address, 4, "relay read its own appliance");
                    f.device.read(&f.context, attribute)
                })
                .unwrap();
        }
        let baseline = StateReadResult::Value {
            sequence: 1,
            value: Some(Value::boolean(false)),
        };
        assert!(matches!(message.content,Content::StateValue {result,..} if result==baseline));
        // A delayed reply must retain the value/watermark sampled at the owner.
        f.device
            .with(&f.context, |a| POWER.update(a, |a| a.observed = Some(true)));
        let deltas = drain();
        assert_eq!(
            (deltas[1].sequence, deltas[1].value),
            (2, Some(Value::boolean(true)))
        );
        for (address, node) in [(3, &mut relay_b), (2, &mut relay_a)] {
            let n = message.encode(1, &mut wire);
            let incoming = Message::decode(&wire[..n], address, 15).unwrap();
            message = node
                .receive_state(incoming, 1, |_| panic!("relay read native state"))
                .unwrap();
        }
        let n = message.encode(2, &mut wire);
        let received = Message::decode(&wire[..n], 1, 15).unwrap();
        assert_eq!(received.destination, Some(1));
        assert!(
            matches!(received.content,Content::StateValue {result,hops:3,..} if result==baseline)
        );
        assert_eq!(f.calls(), 0);
        let next = Message {
            path_cost: TimePerByte::UNKNOWN,
            source: 1,
            destination: None,
            content: Content::ReadState {
                network: NETWORK,
                read: StateRead { request: 8, ..read },
                remaining: 8,
            },
        };
        assert_eq!(
            relay_a
                .receive_state(next, 2, |_| panic!())
                .unwrap()
                .destination,
            Some(3)
        );
        assert!(!change::pending());
    }

    #[test]
    fn routed_native_call_replays_result_and_only_reports_established_state() {
        let f = Fixture::new();
        let mut invocations = Invocations::new();
        let mut relay = Forwarder::new(2, NETWORK);
        let mut owner = Forwarder::new(3, NETWORK);
        let original = request(call(7), ON);
        let forward = relay
            .receive_control(original, 0, |_, _| panic!("relay invoked native code"))[0]
            .unwrap();
        assert!(relay.receive_control(original, 1, |_, _| panic!())[0].is_none());
        let mut wire = [0; crate::kernel::radio::MAX_FRAME_SIZE];
        let size = forward.encode(1, &mut wire);
        let incoming = Message::decode(&wire[..size], 3, 15).unwrap();
        let result = owner.receive_control(incoming, 2, |call, args| {
            invocations.invoke(f.device, &f.context, call, args)
        })[0]
            .unwrap();
        assert_eq!(f.calls(), 1);
        assert!(
            !change::pending(),
            "request acceptance invented an observation"
        );
        assert!(matches!(
            result.content,
            Content::InvocationResult {
                status: InvokeStatus::Invoked,
                hops: 1,
                ..
            }
        ));
        assert_eq!(
            owner.receive_control(incoming, 3, |call, args| invocations
                .invoke(f.device, &f.context, call, args))[0],
            Some(result)
        );
        assert_eq!(f.calls(), 1);
        let reply = relay.receive_control(result, 4, |_, _| panic!())[0].unwrap();
        assert_eq!(reply.destination, Some(1));
        assert!(matches!(
            reply.content,
            Content::InvocationResult { hops: 2, .. }
        ));
        assert_eq!(
            relay.receive_control(original, 5, |_, _| panic!())[0],
            Some(reply)
        );
        assert_eq!(f.calls(), 1, "lost result recovery repeated the operation");
        // The next distinct command uses the direction learned from the result.
        assert_eq!(
            relay.receive_control(request(call(8), OFF), 6, |_, _| panic!())[0]
                .unwrap()
                .destination,
            Some(3)
        );

        // A later physical report, independently of the invocation result.
        f.device
            .with(&f.context, |a| POWER.update(a, |a| a.observed = Some(true)));
        let change = change::take().unwrap();
        assert_eq!(change.value, Some(Value::boolean(true)));
        assert_eq!(
            change.attribute,
            core::ptr::from_ref(POWER.definition()) as usize as u64
        );
        let delta = owner.announce_change(change.attribute, change.sequence, change.value, 7);
        let mut sent = None;
        assert!(relay
            .receive_change(delta, 7, |out| sent = Some(out))
            .is_some());
        let forwarded = sent.unwrap();
        let mut tablet = Forwarder::new(1, NETWORK);
        let seen = tablet.receive_change(forwarded, 8, |_| {}).unwrap();
        assert_eq!(
            (seen.owner, seen.attribute, seen.value),
            (3, change.attribute, change.value)
        );
        assert!(relay.receive_change(delta, 9, |_| panic!()).is_none());
        assert!(owner.receive_change(forwarded, 9, |_| panic!()).is_none());
        f.device
            .with(&f.context, |a| POWER.update(a, |a| a.observed = Some(true)));
        assert!(!change::pending(), "identical report manufactured a delta");
    }

    #[test]
    fn native_dispatch_rejects_bad_calls_and_bounds_result_replay() {
        let f = Fixture::new();
        let mut invocations = Invocations::new();
        let integer = [1, 1, 1];
        for (call, args, expected) in [
            (call(1), ON, InvokeStatus::Invoked),
            (call(1), ON, InvokeStatus::Invoked),
            (call(1), OFF, InvokeStatus::ConflictingRequest),
            (call(2), &integer[..], InvokeStatus::Arguments),
            (
                Invocation {
                    transformer: 123,
                    ..call(3)
                },
                ON,
                InvokeStatus::UnknownTransformer,
            ),
            (call(4), &[1, 0, 2][..], InvokeStatus::Arguments),
        ] {
            assert_eq!(
                invocations.invoke(f.device, &f.context, call, args),
                expected
            );
        }
        assert_eq!(f.calls(), 1);
        f.device.with(&f.context, |_| {
            assert_eq!(
                invocations.invoke(f.device, &f.context, call(5), ON),
                InvokeStatus::Busy
            )
        });
        assert_eq!(
            invocations.invoke(f.device, &f.context, call(5), ON),
            InvokeStatus::Busy
        );
        assert_eq!(f.calls(), 1);
        assert_eq!(
            invocations.invoke(
                f.device,
                &f.context,
                Invocation {
                    transformer: core::ptr::from_ref(&FAIL) as usize as u64,
                    ..call(6)
                },
                ON
            ),
            InvokeStatus::ApplianceError
        );
        assert_eq!(
            change::take().unwrap().value,
            Some(Value::boolean(true)),
            "native error discarded an actual change"
        );
        assert!(!change::pending());
        // This is a bounded replay cache, not an exactly-once ledger.
        for id in 10..18 {
            assert_eq!(
                invocations.invoke(f.device, &f.context, call(id), ON),
                InvokeStatus::Invoked
            );
        }
        let calls = f.calls();
        assert_eq!(
            invocations.invoke(f.device, &f.context, call(1), ON),
            InvokeStatus::Invoked
        );
        assert_eq!(f.calls(), calls + 1);
        let mut values = [Value::boolean(false); MAX_CALL_ARGUMENTS];
        assert_eq!(
            decode_arguments(&integer, &mut values).unwrap(),
            &[Value::integer(Integer::U8, 1).unwrap()]
        );
    }

    #[test]
    fn observation_transport_preserves_report_order_instance_scope_and_hard_eviction() {
        let f = Fixture::new();
        let mut foreign = Device::default();
        POWER.update(&mut foreign, |a| a.observed = Some(true));
        f.device.with(&f.context, |a| HIDDEN.changed(a));
        assert!(
            !change::pending(),
            "unbound instance or hidden state escaped"
        );
        static NEXT: Dependent<Device> = Dependent {
            activate: |a, _| {
                OTHER.update(a, |a| a.other = false);
            },
            next: None,
        };
        POWER.set_dependents(Some(&NEXT));
        f.device.with(&f.context, |a| {
            observation::update(a, &[&POWER, &OTHER], |a| {
                a.observed = Some(true);
                a.other = true;
            })
        });
        let changes = drain();
        assert_eq!(
            changes.iter().map(|c| c.value).collect::<Vec<_>>(),
            [
                Some(Value::boolean(true)),
                Some(Value::boolean(true)),
                Some(Value::boolean(false))
            ]
        );
        assert_eq!(
            changes.iter().map(|c| c.sequence).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(changes[1].attribute, changes[2].attribute);
        POWER.set_dependents(None);
        f.device.with(&f.context, |a| {
            POWER.update(a, |a| a.observed = None);
            POWER.changed(a);
        });
        assert_eq!(
            drain().iter().map(|c| c.value).collect::<Vec<_>>(),
            [None, None]
        );
        f.device.with(&f.context, |a| {
            for _ in 0..10 {
                POWER.changed(a);
            }
        });
        let changes = drain();
        assert_eq!(changes.len(), 8);
        assert_eq!(change::evicted(), 2);
        assert_eq!(
            (
                changes.first().unwrap().sequence,
                changes.last().unwrap().sequence
            ),
            (8, 15)
        );
    }
}
