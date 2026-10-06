//! Portable IEEE 802.15.4 frame I/O and common short-address data framing.
//!
//! Appliance firmware initializes its chip, pins, clocks, and interrupts, then
//! supplies a [`Transceiver`]. Lattice owns the bytes carried by that radio.
//! Hardware adapters normalize length prefixes, checksums, and receive trailers;
//! none of those vendor buffer details belong in appliance or protocol code.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Maximum 2.4 GHz IEEE 802.15.4 MAC frame, excluding its two-byte checksum.
pub const MAX_FRAME_SIZE: usize = 125;
/// Header size for data frames with short addresses and a shared PAN ID.
pub const DATA_HEADER_SIZE: usize = 9;
/// Largest payload in the data-frame format implemented here.
pub const MAX_PAYLOAD_SIZE: usize = MAX_FRAME_SIZE - DATA_HEADER_SIZE;
/// IEEE 802.15.4 broadcast short address (also the broadcast PAN ID).
pub const BROADCAST: u16 = 0xffff;

const DATA_CONTROL: u16 = 0x8841;
const REQUEST_ACK: u16 = 0x0020;

/// Radio settings to be supplied by Lattice's common network startup/join code.
/// The adapter applies them; appliance implementations do not select network
/// membership. That startup/join path is not implemented yet, and the hardware
/// probe supplies fixed test values. Board-specific antenna/pin setup stays in
/// the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// 2.4 GHz IEEE 802.15.4 channel, 11 through 26.
    pub channel: u8,
    /// Requested transmit power in dBm, subject to the chip's supported settings.
    pub tx_power_dbm: i8,
    /// Shared network PAN used for local frame filtering, selected by Lattice.
    /// This is not a per-vertex identity or an authentication credential.
    pub pan_id: u16,
    /// Local short radio address. Lattice owns its assignment/discovery; that
    /// common behavior is not implemented by this low-level radio interface.
    pub address: u16,
}

/// Result of one transmission attempt, after the adapter releases its TX buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransmitStatus {
    /// Transmission finished without requesting an ACK; peer receipt is unknown.
    Sent,
    /// The requested MAC ACK arrived. This does not acknowledge program execution.
    Acknowledged,
    /// The requested MAC ACK did not arrive. The data may still have been received.
    NoAcknowledgement,
    /// Channel assessment prevented transmission.
    ChannelBusy,
    /// The attempt aborted. This alone does not establish non-delivery.
    Aborted,
}

/// Metadata for the frame copied into the caller's receive buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Received {
    /// MAC bytes written, excluding PHY length prefix and checksum/trailer bytes.
    pub length: usize,
    /// Received signal strength when supplied by the chip.
    pub rssi_dbm: Option<i8>,
}

/// The three hardware operations needed by a frame-oriented participant.
///
/// Implementations handle chip initialization outside this trait. `configure`
/// starts reception, applies local PAN/address filtering, and enables the MAC ACK
/// behavior requested by incoming/outgoing frame headers. A chip driver may do
/// this in hardware or software; this interface does not require ESP-specific
/// registers, an allocator, an async executor, or a particular HAL.
///
/// Buffers contain MAC bytes only, with no PHY length prefix, checksum, or vendor
/// trailer. The adapter generates TX checksums, rejects bad RX checksums, and
/// handles ACK control frames. It leaves reception enabled between operations.
///
/// There is no application retry or graph queue here. A send is one attempt with
/// clear-channel assessment. Do not enable a vendor's automatic retransmissions.
/// ISR plumbing/RTOS notification belongs to the hardware port: `receive` can be
/// called when notified, without imposing a periodic graph evaluation loop.
pub trait Transceiver {
    /// Chip/adapter-specific configuration and I/O failures.
    type Error;

    /// Apply settings and start listening. Reject out-of-range settings explicitly;
    /// document any rounding of requested power to the chip's supported steps.
    fn configure(&mut self, config: Config) -> Result<(), Self::Error>;

    /// Transmit once and return completion, including the requested ACK outcome.
    ///
    /// This is a synchronous boundary: use a task context where waiting for the
    /// radio is permitted, with interrupts enabled. The adapter may use existing
    /// RTOS facilities to wait. It must not return while DMA/radio still owns any
    /// caller buffer, even on error, and must not retain a pointer into `frame`.
    /// The frame is at most [`MAX_FRAME_SIZE`] bytes.
    fn transmit(&mut self, frame: &[u8]) -> Result<TransmitStatus, Self::Error>;

    /// Copy one received frame, or return `None` immediately if none is available.
    ///
    /// `length` must not exceed the supplied buffer. Never truncate a frame and
    /// report success. Release any vendor-owned receive buffer before returning.
    fn receive(
        &mut self,
        frame: &mut [u8; MAX_FRAME_SIZE],
    ) -> Result<Option<Received>, Self::Error>;
}

/// Why a frame cannot use the short-address data format implemented here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The MAC frame or payload exceeds the IEEE 802.15.4 size limit.
    TooLong,
    /// The input ends before the complete data header.
    Truncated,
    /// A different frame type, addressing mode, version, or security format.
    Unsupported,
    /// Broadcast data must not request a MAC ACK from every recipient.
    BroadcastAcknowledgement,
    /// A reserved short address was used as a source or unicast destination.
    ReservedAddress,
}

/// Borrowed payload and standard MAC header fields; contains no graph identity.
///
/// This first framing implementation uses unsecured 2003-version data frames,
/// short addresses, and PAN compression, matching the hardware probe. It is not
/// the Lattice message format or a complete IEEE 802.15.4 stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataFrame<'a> {
    /// MAC sequence byte, independent of program/change identifiers.
    pub sequence: u8,
    /// PAN shared by source and destination.
    pub pan_id: u16,
    /// Destination short address, including [`BROADCAST`].
    pub destination: u16,
    /// Originating short address.
    pub source: u16,
    /// Whether the destination should return a MAC ACK.
    pub request_ack: bool,
    /// Message bytes, borrowed from the caller or receive buffer.
    pub payload: &'a [u8],
}

impl<'a> DataFrame<'a> {
    /// Encode into caller-owned storage, preserving the buffer on invalid input.
    pub fn encode<'b>(&self, output: &'b mut [u8; MAX_FRAME_SIZE]) -> Result<&'b [u8], FrameError> {
        self.validate()?;
        let control = DATA_CONTROL | if self.request_ack { REQUEST_ACK } else { 0 };
        output[..2].copy_from_slice(&control.to_le_bytes());
        output[2] = self.sequence;
        output[3..5].copy_from_slice(&self.pan_id.to_le_bytes());
        output[5..7].copy_from_slice(&self.destination.to_le_bytes());
        output[7..9].copy_from_slice(&self.source.to_le_bytes());
        let length = DATA_HEADER_SIZE + self.payload.len();
        output[DATA_HEADER_SIZE..length].copy_from_slice(self.payload);
        Ok(&output[..length])
    }

    /// Decode the supported MAC format while borrowing its payload in place.
    pub fn decode(input: &'a [u8]) -> Result<Self, FrameError> {
        if input.len() > MAX_FRAME_SIZE {
            return Err(FrameError::TooLong);
        }
        if input.len() < DATA_HEADER_SIZE {
            return Err(FrameError::Truncated);
        }
        let control = u16::from_le_bytes([input[0], input[1]]);
        if control & !REQUEST_ACK != DATA_CONTROL {
            return Err(FrameError::Unsupported);
        }
        let frame = Self {
            sequence: input[2],
            pan_id: u16::from_le_bytes([input[3], input[4]]),
            destination: u16::from_le_bytes([input[5], input[6]]),
            source: u16::from_le_bytes([input[7], input[8]]),
            request_ack: control & REQUEST_ACK != 0,
            payload: &input[DATA_HEADER_SIZE..],
        };
        frame.validate()?;
        Ok(frame)
    }

    fn validate(&self) -> Result<(), FrameError> {
        if self.payload.len() > MAX_PAYLOAD_SIZE {
            return Err(FrameError::TooLong);
        }
        if self.source >= 0xfffe || self.destination == 0xfffe {
            return Err(FrameError::ReservedAddress);
        }
        if self.destination == BROADCAST && self.request_ack {
            return Err(FrameError::BroadcastAcknowledgement);
        }
        Ok(())
    }
}
