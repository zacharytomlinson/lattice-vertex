//! Two-board IEEE 802.15.4 experiment using the vertex's portable radio interface.
//! It uses the compile-only default RTOS port and never starts the scheduler.
//! Each board originates probes and echoes peer probes over the radio. Serial output only
//! reports results. The periodic probe traffic is an experiment, not graph work.

#![no_std]
#![no_main]

use esp_hal::{delay::Delay, time::Instant};
use esp_println::println;
use lattice_vertex::radio::{
    BROADCAST, Config, DataFrame, MAX_FRAME_SIZE, Transceiver, TransmitStatus,
};

mod esp32c6;
use esp32c6::Esp32C6;

esp_bootloader_esp_idf::esp_app_desc!();

const CHANNEL: u8 = 15;
const TX_POWER_DBM: i8 = 20;
const PAN: u16 = 0x4c56;
const ADDRESS: u16 = if cfg!(feature = "board-b") { 2 } else { 1 };
const PEER: u16 = if cfg!(feature = "board-b") { 1 } else { 2 };
const PERIOD_MS: u64 = if cfg!(feature = "board-b") {
    2300
} else {
    1700
};
const PAYLOAD_LEN: usize = 16;
const PING: u8 = 0;
const REPLY: u8 = 1;

#[derive(Default)]
struct Counts {
    sent: [u32; 2],
    received: [u32; 2],
    replies: [u32; 2],
    tx_failed: u32,
    acked: u32,
    ignored: u32,
}

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

/// Common framing and dispatch, independent of the concrete radio driver.
fn transmit<R: Transceiver>(
    radio: &mut R,
    destination: u16,
    sequence: u8,
    payload: &[u8; PAYLOAD_LEN],
    counts: &mut Counts,
) -> bool
where
    R::Error: core::fmt::Debug,
{
    let mut buffer = [0u8; MAX_FRAME_SIZE];
    let frame = DataFrame {
        sequence,
        pan_id: PAN,
        destination,
        source: ADDRESS,
        request_ack: destination != BROADCAST,
        payload,
    };
    let encoded = frame.encode(&mut buffer).unwrap();
    match radio.transmit(encoded).unwrap() {
        TransmitStatus::Sent => true,
        TransmitStatus::Acknowledged => {
            counts.acked += 1;
            true
        }
        status => {
            counts.tx_failed += 1;
            println!(
                "TX_FAILED node={} dst={:04x} mac_seq={} kind={} mode={} probe={} status={:?}",
                ADDRESS,
                destination,
                sequence,
                payload[4],
                payload[5],
                u16::from_le_bytes([payload[6], payload[7]]),
                status
            );
            false
        }
    }
}

/// Apply only this experiment's payload and peer selection to a decoded frame.
fn decode(bytes: &[u8]) -> Option<(u16, [u8; PAYLOAD_LEN])> {
    let frame = DataFrame::decode(bytes).ok()?;
    if frame.pan_id != PAN
        || frame.source != PEER
        || (frame.destination != ADDRESS && frame.destination != BROADCAST)
        || frame.payload.len() != PAYLOAD_LEN
    {
        return None;
    }
    let mut payload = [0u8; PAYLOAD_LEN];
    payload.copy_from_slice(frame.payload);
    if &payload[..4] != b"LVP\x01" || payload[4] > REPLY || payload[5] > 1 {
        return None;
    }
    Some((frame.source, payload))
}

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    // The upstream radio driver's RX VecDeque needs an allocator. This bounded
    // backing heap belongs only to the experiment, not to lattice-vertex.
    esp_alloc::heap_allocator!(size: 8192);
    let mut radio = Esp32C6::new(peripherals.IEEE802154);
    radio
        .configure(Config {
            channel: CHANNEL,
            tx_power_dbm: TX_POWER_DBM,
            pan_id: PAN,
            address: ADDRESS,
        })
        .unwrap();
    println!(
        "READY node={} peer={} channel={} pan={:04x} power_dbm={} logging_cs={} receive_notification={}",
        ADDRESS,
        PEER,
        CHANNEL,
        PAN,
        TX_POWER_DBM,
        cfg!(feature = "logging-critical-section"),
        cfg!(feature = "receive-notification")
    );

    run(&mut radio)
}

fn run<R: Transceiver>(radio: &mut R) -> !
where
    R::Error: core::fmt::Debug,
{
    let mut received_frame = [0u8; MAX_FRAME_SIZE];
    let mut counts = Counts::default();
    let mut sequence = 0u16;
    let mut mac_sequence = 0u8;
    let mut next_probe = now_us() / 1000 + 3000 + u64::from(ADDRESS) * 300;
    let mut next_report = now_us() / 1000 + 5000;
    let delay = Delay::new();
    loop {
        while let Some(received) = radio.receive(&mut received_frame).unwrap() {
            let bytes = received_frame
                .get(..received.length)
                .expect("adapter returned an invalid frame length");
            if let Some((source, mut payload)) = decode(bytes) {
                let rssi = received.rssi_dbm;
                let mode = usize::from(payload[5]);
                let probe_sequence = u16::from_le_bytes([payload[6], payload[7]]);
                if payload[4] == PING {
                    counts.received[mode] += 1;
                    println!(
                        "RX node={} from={} mode={} seq={} rssi={:?}",
                        ADDRESS, source, mode, probe_sequence, rssi
                    );
                    payload[4] = REPLY;
                    mac_sequence = mac_sequence.wrapping_add(1);
                    transmit(radio, source, mac_sequence, &payload, &mut counts);
                } else {
                    counts.replies[mode] += 1;
                    let sent_at = u64::from_le_bytes(payload[8..16].try_into().unwrap());
                    println!(
                        "REPLY node={} from={} mode={} seq={} rtt_us={} rssi={:?}",
                        ADDRESS,
                        source,
                        mode,
                        probe_sequence,
                        now_us().wrapping_sub(sent_at),
                        rssi
                    );
                }
            } else {
                counts.ignored += 1;
            }
        }

        let now = now_us() / 1000;
        if now >= next_probe {
            let mode = (sequence & 1) as usize;
            let destination = if mode == 0 { PEER } else { BROADCAST };
            let mut payload = [0u8; PAYLOAD_LEN];
            payload[..4].copy_from_slice(b"LVP\x01");
            payload[4] = PING;
            payload[5] = mode as u8;
            payload[6..8].copy_from_slice(&sequence.to_le_bytes());
            payload[8..16].copy_from_slice(&now_us().to_le_bytes());
            counts.sent[mode] += 1;
            mac_sequence = mac_sequence.wrapping_add(1);
            let ok = transmit(radio, destination, mac_sequence, &payload, &mut counts);
            println!(
                "TX node={} mode={} seq={} completed={}",
                ADDRESS, mode, sequence, ok
            );
            sequence = sequence.wrapping_add(1);
            next_probe = now + PERIOD_MS;
        }
        if now >= next_report {
            println!(
                "COUNTS node={} sent={:?} rx={:?} replies={:?} acked={} failed={} ignored={}",
                ADDRESS,
                counts.sent,
                counts.received,
                counts.replies,
                counts.acked,
                counts.tx_failed,
                counts.ignored
            );
            next_report = now + 5000;
        }
        delay.delay_micros(100);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC node={} {}", ADDRESS, info);
    loop {
        core::hint::spin_loop();
    }
}
