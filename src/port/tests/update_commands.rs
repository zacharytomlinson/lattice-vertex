// SPDX-License-Identifier: MIT
//! Numeric framing for local update fixtures, not the Lattice OTA protocol.
//! A5 5A, body length, sequence/opcode/payload, CRC16-CCITT (little endian).
//! The checksum covers length and body. Text from serial echo/probing is ignored;
//! an incomplete frame expires after 250 fixture ticks (1 kHz on both ports).

pub const MAX_BODY: usize = 59; // complete command fits one 64-byte USB packet
const IDLE_TICKS: u32 = 250;

pub struct Decoder {
    state: u8,
    length: usize,
    received: usize,
    last_tick: u32,
    bytes: [u8; MAX_BODY + 2],
}

impl Decoder {
    pub const fn new() -> Self {
        Self {
            state: 0,
            length: 0,
            received: 0,
            last_tick: 0,
            bytes: [0; MAX_BODY + 2],
        }
    }

    pub fn push(&mut self, byte: u8, tick: u32) -> Option<&[u8]> {
        if tick.wrapping_sub(self.last_tick) > IDLE_TICKS {
            self.state = 0;
        }
        self.last_tick = tick;
        match self.state {
            0 => {
                if byte == 0xa5 {
                    self.state = 1;
                }
            }
            1 => {
                self.state = match byte {
                    0x5a => 2,
                    0xa5 => 1,
                    _ => 0,
                };
            }
            2 => {
                if (5..=MAX_BODY as u8).contains(&byte) {
                    self.length = byte as usize;
                    self.received = 0;
                    self.state = 3;
                } else {
                    self.state = if byte == 0xa5 { 1 } else { 0 };
                }
            }
            _ => {
                self.bytes[self.received] = byte;
                self.received += 1;
                if self.received == self.length + 2 {
                    self.state = 0;
                    let expected = u16::from_le_bytes(
                        self.bytes[self.length..self.length + 2].try_into().unwrap(),
                    );
                    let actual = checksum(
                        core::iter::once(self.length as u8)
                            .chain(self.bytes[..self.length].iter().copied()),
                    );
                    if expected == actual {
                        return Some(&self.bytes[..self.length]);
                    }
                }
            }
        }
        None
    }
}

fn checksum(bytes: impl Iterator<Item = u8>) -> u16 {
    bytes.fold(0xffff, |mut crc, byte| {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x8000 != 0 { 0x1021 } else { 0 };
        }
        crc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_stream_recovery() {
        assert_eq!(checksum(b"123456789".iter().copied()), 0x29b1);
        let body = [0x78, 0x56, 0x34, 0x12, 0];
        let crc = checksum(core::iter::once(5).chain(body));
        let frame = [
            0xa5,
            0x5a,
            5,
            body[0],
            body[1],
            body[2],
            body[3],
            body[4],
            crc as u8,
            (crc >> 8) as u8,
        ];
        // Fresh enumeration echo, malformed lengths, corrupt frames, and partial
        // commands all recover without executing a command made from that input.
        let text = b"UPDATE image=B slot=1 boot=Valid ready=1\r\nAT\r\n";
        let mut corrupted = frame;
        corrupted[5] ^= 1;
        for (prefix, next_tick) in [
            (&text[..], 0),
            (&[0xa5, 0x5a, 0][..], 0),
            (&[0xa5, 0x5a, 60][..], 0),
            (&corrupted[..], 0),
            (&frame[..6], 251),
        ] {
            let mut decoder = Decoder::new();
            for &byte in prefix {
                assert!(decoder.push(byte, 0).is_none());
            }
            let mut commands = 0;
            for byte in frame {
                if let Some(decoded) = decoder.push(byte, next_tick) {
                    assert_eq!(decoded, body);
                    commands += 1;
                }
            }
            assert_eq!(commands, 1);
        }
        // Fragmented and adjacent frames retain their independent identities.
        let mut decoder = Decoder::new();
        let mut commands = 0;
        for (i, byte) in frame.into_iter().chain(frame).enumerate() {
            if let Some(decoded) = decoder.push(byte, i as u32) {
                assert_eq!(decoded, body);
                commands += 1;
            }
        }
        assert_eq!(commands, 2);
        // A marker in payload is data, not a nested frame boundary.
        let payload = [1, 2, 3, 4, 3, 0xa5, 0x5a, 0];
        let crc = checksum(core::iter::once(payload.len() as u8).chain(payload));
        let mut decoder = Decoder::new();
        let mut commands = 0;
        for byte in [0xa5, 0x5a, payload.len() as u8]
            .into_iter()
            .chain(payload)
            .chain(crc.to_le_bytes())
        {
            if let Some(decoded) = decoder.push(byte, u32::MAX) {
                assert_eq!(decoded, payload);
                commands += 1;
            }
        }
        assert_eq!(commands, 1);
    }
}
