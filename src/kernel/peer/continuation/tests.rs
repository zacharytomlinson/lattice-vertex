use super::*;
use crate::kernel::peer::{
    forwarding::Forwarder,
    frame::{Content, Message},
    Network,
};
use lattice_peer_wire::{Integer, MAX_CALL_ARGUMENTS};

fn initialize() {
    crate::port::test_port_reset();
    reset();
    start(1, 100);
}

#[test]
fn admission_and_capacity() {
    crate::port::test_port_reset();
    reset();
    assert_eq!(submit(2, 7, &[]), Err(SubmitError::NotReady));
    initialize();
    for (owner, target) in [(0, 7), (u64::MAX, 7), (1, 7), (2, 0), (2, u64::MAX)] {
        assert_eq!(submit(owner, target, &[]), Err(SubmitError::Target));
    }
    let wide = Value::integer(Integer::I64, -1).unwrap();
    for (values, expected) in [
        (
            &[Value::boolean(true); MAX_CALL_ARGUMENTS + 1][..],
            Err(SubmitError::Arguments),
        ),
        (&[wide; 7][..], Err(SubmitError::Arguments)),
        (&[wide; 6][..], Ok(())),
    ] {
        assert_eq!(submit(2, 7, values), expected);
    }
    for count in [0, 1, CAPACITY - 1, CAPACITY, CAPACITY + 1, 2 * CAPACITY + 1] {
        initialize();
        for i in 0..count {
            submit(2, 7, &[Value::integer(Integer::U32, i as i128).unwrap()]).unwrap();
        }
        assert_eq!(evicted(), count.saturating_sub(CAPACITY) as u32);
        for i in count.saturating_sub(CAPACITY)..count {
            let frame = take().unwrap();
            assert_eq!(
                frame.call(1),
                Invocation {
                    caller: 1,
                    owner: 2,
                    transformer: 7,
                    request: 100 + i as u32
                }
            );
            let mut decoded = [Value::boolean(false); MAX_CALL_ARGUMENTS];
            assert_eq!(
                lattice_peer_wire::scalar::decode_arguments(frame.arguments(), &mut decoded)
                    .unwrap()[0]
                    .as_integer(),
                Some(i as i128)
            );
        }
        assert!(take().is_none() && !pending());
    }
}

#[test]
fn frame_handoff() {
    for values in [
        &[][..],
        &[Value::boolean(true); MAX_CALL_ARGUMENTS][..],
        &[Value::integer(Integer::I64, i64::MIN as i128).unwrap(); 6][..],
    ] {
        initialize();
        submit(2, 7, values).unwrap();
        let packet = take().unwrap();
        let mut origin = Forwarder::new(
            1,
            Network {
                founder: 1,
                channel: 15,
            },
        );
        let message = origin
            .originate_call(packet.call(1), packet.arguments(), 0)
            .unwrap();
        let mut frame = [0; crate::kernel::radio::MAX_FRAME_SIZE];
        let size = message.encode(0, &mut frame);
        assert!(size <= frame.len());
        let received = Message::decode(&frame[..size], 2, 15).unwrap();
        let Content::Invoke {
            call, arguments, ..
        } = received.content
        else {
            panic!()
        };
        assert_eq!(call, packet.call(1));
        let mut decoded = [Value::boolean(false); MAX_CALL_ARGUMENTS];
        assert_eq!(
            lattice_peer_wire::scalar::decode_arguments(arguments, &mut decoded),
            Some(values)
        );
        assert!(!pending()); // The evaluated packet is consumed exactly once.
    }
}

#[test]
fn identity_exhaustion() {
    initialize();
    access(|out| out.next_request = u32::MAX);
    submit(2, 7, &[]).unwrap();
    assert_eq!(take().unwrap().request, u32::MAX);
    assert_eq!(submit(2, 7, &[]), Err(SubmitError::Exhausted));
    assert!(!pending());
}

#[cfg(feature = "host-link")]
#[test]
fn origin_namespaces() {
    initialize();
    let first = host_request(400).unwrap();
    submit(2, 7, &[]).unwrap();
    let fragment = take().unwrap().request;
    assert_eq!(fragment, first + 1);
    assert_eq!(host_request(400), Some(first));
    assert_eq!(
        host_request(fragment),
        Some(fragment + 1),
        "host token is not its radio ID"
    );
    let retained = host_request(u32::MAX).unwrap();
    access(|out| out.next_request = 0);
    assert_eq!(host_request(u32::MAX), Some(retained));
    assert_eq!(host_request(0), None);
    // Mapping eviction cannot recycle the prior wire identity.
    initialize();
    let old = host_request(0).unwrap();
    for token in 1..=8 {
        host_request(token).unwrap();
    }
    assert!(host_request(0).unwrap() > old);
}
