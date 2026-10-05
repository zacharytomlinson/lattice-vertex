use lattice_vertex::radio::{DataFrame, FrameError, BROADCAST, MAX_FRAME_SIZE, MAX_PAYLOAD_SIZE};

#[test]
fn short_address_frames_match_wire_vectors_without_vendor_prefix_or_checksum() {
    let vectors: &[(&[u8], u16, bool)] = &[
        (
            &[0x61, 0x88, 0x2a, 0x56, 0x4c, 2, 0, 1, 0, 0xab, 0xcd],
            2,
            true,
        ),
        (
            &[0x41, 0x88, 0x2a, 0x56, 0x4c, 0xff, 0xff, 1, 0, 0xab, 0xcd],
            BROADCAST,
            false,
        ),
        (
            &[0x41, 0x88, 0x2a, 0x56, 0x4c, 2, 0, 1, 0, 0xab, 0xcd],
            2,
            false,
        ),
    ];
    for &(wire, destination, request_ack) in vectors {
        let frame = DataFrame {
            sequence: 42,
            pan_id: 0x4c56,
            destination,
            source: 1,
            request_ack,
            payload: &[0xab, 0xcd],
        };
        let mut output = [0x55; MAX_FRAME_SIZE];
        assert_eq!(frame.encode(&mut output).unwrap(), wire);
        assert_eq!(output[wire.len()], 0x55);
        let decoded = DataFrame::decode(wire).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(decoded.payload.as_ptr(), wire[9..].as_ptr());
    }
}

#[test]
fn malformed_or_unsupported_frames_never_become_data_messages() {
    let mut buffer = [0x55; MAX_FRAME_SIZE];
    let maximum = [0xab; MAX_PAYLOAD_SIZE];
    let base = DataFrame {
        sequence: 255,
        pan_id: 0x4c56,
        destination: 2,
        source: 1,
        request_ack: true,
        payload: &maximum,
    };
    let wire = base.encode(&mut buffer).unwrap();
    assert_eq!(wire.len(), 125);
    assert_eq!(DataFrame::decode(wire).unwrap().payload.len(), 116);
    let empty = DataFrame {
        payload: &[],
        ..base
    };
    assert_eq!(empty.encode(&mut buffer).unwrap().len(), 9);

    let too_long = [0; MAX_PAYLOAD_SIZE + 1];
    for (frame, error) in [
        (
            DataFrame {
                payload: &too_long,
                ..base
            },
            FrameError::TooLong,
        ),
        (
            DataFrame {
                destination: BROADCAST,
                ..base
            },
            FrameError::BroadcastAcknowledgement,
        ),
        (
            DataFrame {
                source: BROADCAST,
                ..base
            },
            FrameError::ReservedAddress,
        ),
        (
            DataFrame {
                source: 0xfffe,
                ..base
            },
            FrameError::ReservedAddress,
        ),
        (
            DataFrame {
                destination: 0xfffe,
                ..base
            },
            FrameError::ReservedAddress,
        ),
    ] {
        buffer.fill(0x55);
        assert_eq!(frame.encode(&mut buffer), Err(error));
        assert!(buffer.iter().all(|&byte| byte == 0x55));
    }

    let valid = [0x61, 0x88, 0, 0x56, 0x4c, 2, 0, 1, 0];
    for length in 0..9 {
        assert_eq!(
            DataFrame::decode(&valid[..length]),
            Err(FrameError::Truncated)
        );
    }
    assert_eq!(DataFrame::decode(&[0; 126]), Err(FrameError::TooLong));
    // Security, missing PAN compression, extended address, version 2015, and ACK
    // frames require other parsers; none may be mistaken for this data format.
    for control in [0x8869u16, 0x8821, 0x8c61, 0xa861, 0x8862] {
        let mut malformed = valid;
        malformed[..2].copy_from_slice(&control.to_le_bytes());
        assert_eq!(DataFrame::decode(&malformed), Err(FrameError::Unsupported));
    }
    for (offset, address, error) in [
        (5, BROADCAST, FrameError::BroadcastAcknowledgement),
        (5, 0xfffe, FrameError::ReservedAddress),
        (7, BROADCAST, FrameError::ReservedAddress),
    ] {
        let mut malformed = valid;
        malformed[offset..offset + 2].copy_from_slice(&address.to_le_bytes());
        assert_eq!(DataFrame::decode(&malformed), Err(error));
    }
}
