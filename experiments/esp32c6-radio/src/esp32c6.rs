//! C6-specific adaptation to the kernel's frame I/O interface.
//! Vendor buffer layout, completion interrupts, and the probe's timeout stay here.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use esp_hal::{peripherals::IEEE802154, time::Instant};
use esp_radio::ieee802154::{Config as EspConfig, Ieee802154};
use lattice_vertex::radio::{Config, MAX_FRAME_SIZE, Received, Transceiver, TransmitStatus};

// One physical radio per chip. Interrupt callbacks only publish completion flags.
static TX_STATUS: AtomicU8 = AtomicU8::new(0);
static TX_ABORT: AtomicU8 = AtomicU8::new(0);
static RX_AVAILABLE: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum Error {
    Configuration,
    FrameLength,
    Driver(esp_radio::ieee802154::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Configuration => f.write_str("unsupported C6 radio configuration"),
            Self::FrameLength => f.write_str("invalid IEEE 802.15.4 frame length"),
            Self::Driver(error) => write!(f, "C6 radio driver: {error}"),
        }
    }
}

impl core::error::Error for Error {}

pub struct Esp32C6<'a> {
    driver: Ieee802154<'a>,
    draining: bool,
}

impl<'a> Esp32C6<'a> {
    pub fn new(peripheral: IEEE802154<'a>) -> Self {
        let mut driver = Ieee802154::new(peripheral);
        driver.set_tx_done_callback_fn(|| TX_STATUS.store(1, Ordering::Release));
        driver.set_tx_failed_callback_fn(|| {
            TX_ABORT.store(
                IEEE802154::regs()
                    .tx_status()
                    .read()
                    .tx_abort_status()
                    .bits(),
                Ordering::Relaxed,
            );
            TX_STATUS.store(2, Ordering::Release);
        });
        driver.set_rx_available_callback_fn(|| RX_AVAILABLE.store(true, Ordering::Release));
        Self {
            driver,
            draining: false,
        }
    }
}

impl Transceiver for Esp32C6<'_> {
    type Error = Error;

    fn configure(&mut self, config: Config) -> Result<(), Error> {
        if !(11..=26).contains(&config.channel)
            || !(-15..=20).contains(&config.tx_power_dbm)
            || config.address >= 0xfffe
        {
            return Err(Error::Configuration);
        }
        self.driver.set_config(EspConfig {
            channel: config.channel,
            pan_id: Some(config.pan_id),
            short_addr: Some(config.address),
            auto_ack_tx: true,
            auto_ack_rx: true,
            rx_when_idle: true,
            promiscuous: false,
            txpower: config.tx_power_dbm,
            rx_queue_size: 4,
            ..EspConfig::default()
        });
        self.driver.start_receive();
        Ok(())
    }

    fn transmit(&mut self, frame: &[u8]) -> Result<TransmitStatus, Error> {
        if !(3..=MAX_FRAME_SIZE).contains(&frame.len()) {
            return Err(Error::FrameLength);
        }
        // esp-radio expects two bytes reserved for hardware-generated FCS.
        let mut with_checksum = [0u8; MAX_FRAME_SIZE + 2];
        with_checksum[..frame.len()].copy_from_slice(frame);
        TX_STATUS.store(0, Ordering::Release);
        TX_ABORT.store(0, Ordering::Relaxed);
        self.driver
            .transmit_raw(&with_checksum[..frame.len() + 2], true)
            .map_err(Error::Driver)?;
        let started = Instant::now();
        loop {
            match TX_STATUS.load(Ordering::Acquire) {
                1 => {
                    return Ok(if frame[0] & 0x20 == 0 {
                        TransmitStatus::Sent
                    } else if self.driver.get_ack_frame().is_some() {
                        TransmitStatus::Acknowledged
                    } else {
                        TransmitStatus::NoAcknowledgement
                    });
                }
                2 => {
                    return Ok(match TX_ABORT.load(Ordering::Relaxed) {
                        16 if frame[0] & 0x20 != 0 => TransmitStatus::NoAcknowledgement,
                        24 | 25 => TransmitStatus::ChannelBusy,
                        _ => TransmitStatus::Aborted,
                    });
                }
                _ if started.elapsed().as_millis() >= 250 => {
                    // Same diagnostic stop as the original probe. The vendor
                    // allows 200 ms for ACK reception; never recycle an active
                    // driver buffer after an unexplained missing completion.
                    panic!("radio TX completion timeout");
                }
                _ => core::hint::spin_loop(),
            }
        }
    }

    fn receive(&mut self, frame: &mut [u8; MAX_FRAME_SIZE]) -> Result<Option<Received>, Error> {
        if cfg!(feature = "receive-notification")
            && !self.draining
            && !RX_AVAILABLE.swap(false, Ordering::Acquire)
        {
            return Ok(None);
        }
        let Some(raw) = self.driver.raw_received() else {
            self.draining = false;
            return Ok(None);
        };
        self.draining = true;
        let wire_length = usize::from(raw.data[0]);
        if !(5..=MAX_FRAME_SIZE + 2).contains(&wire_length) {
            return Err(Error::FrameLength);
        }
        let length = wire_length - 2;
        frame[..length].copy_from_slice(&raw.data[1..1 + length]);
        Ok(Some(Received {
            length,
            rssi_dbm: Some(raw.data[wire_length - 1] as i8),
        }))
    }
}
