//! C6 radio operation with fixed receive storage and task-notification wakers.
//! Hardware state/clock code is extracted from esp-radio; see README.md.

mod clocks;
mod mac;
mod registers;
mod settings;

use crate::kernel::radio::{Error, MAX_FRAME_SIZE};
use core::{
    future::poll_fn,
    sync::atomic::{AtomicI16, AtomicU8, Ordering},
    task::Poll,
};
use embassy_sync::waitqueue::AtomicWaker;
use esp_phy::{PhyClockGuard, PhyInitGuard};
use settings::IEEE802154_FRAME_EXT_ADDR_SIZE;

// Helpers consumed by the extracted MAC state machine. The driver receives a
// PSDU here, without the PHY length prefix. No allocating Frame API is retained.
mod frame {
    pub const FRAME_SIZE: usize = 129;
    pub const FRAME_VERSION_1: u8 = 0x10;
    pub const FRAME_VERSION_2: u8 = 0x20;
    pub fn frame_is_ack_required(frame: &[u8]) -> bool {
        frame.first().is_some_and(|b| b & 0x20 != 0)
    }
    pub fn frame_get_version(frame: &[u8]) -> u8 {
        frame.get(1).copied().unwrap_or(0) & 0x30
    }
}

static RX_WAKER: AtomicWaker = AtomicWaker::new();
static TX_WAKER: AtomicWaker = AtomicWaker::new();
static TX_STATUS: AtomicU8 = AtomicU8::new(0);
static ED_WAKER: AtomicWaker = AtomicWaker::new();
static ED_RESULT: AtomicI16 = AtomicI16::new(i16::MIN);
fn energy_done(result: Result<i8, Error>) {
    ED_RESULT.store(
        result.map(i16::from).unwrap_or(i16::MIN + 1),
        Ordering::Release,
    );
    ED_WAKER.wake();
}
fn rx_available() {
    RX_WAKER.wake();
}
fn tx_done() {
    TX_STATUS.store(1, Ordering::Release);
    TX_WAKER.wake();
}
fn tx_failed() {
    TX_STATUS.store(2, Ordering::Release);
    TX_WAKER.wake();
}

#[repr(C, align(4))]
struct TransmitBuffer([u8; 128]);

pub(crate) struct Radio {
    transmit: TransmitBuffer,
    _clock: PhyClockGuard<'static>,
    _phy: PhyInitGuard<'static>,
}

impl Radio {
    pub fn new(peripheral: super::RadioPeripheral) -> Self {
        let (clock, phy) = mac::esp_ieee802154_enable(peripheral);
        Self {
            transmit: TransmitBuffer([0; 128]),
            _clock: clock,
            _phy: phy,
        }
    }

    pub fn configure(&mut self, channel: u8, pan_id: u16) -> Result<(), Error> {
        if !(11..=26).contains(&channel) {
            return Err(Error::InvalidConfiguration);
        }
        mac::prepare_reconfigure();
        mac::set_channel(channel);
        mac::set_tx_power(20);
        mac::set_panid(0, pan_id);
        mac::set_short_address(0, 0xfffe);
        mac::set_extended_address(0, self.address().to_le_bytes());
        mac::set_promiscuous(false);
        mac::set_auto_ack_tx(false);
        mac::set_auto_ack_rx(false);
        mac::set_enhance_ack_tx(false);
        mac::set_rx_when_idle(true);
        Ok(())
    }

    pub fn address(&self) -> u64 {
        let address = esp_hal::efuse::base_mac_address();
        let a = address.as_bytes();
        // Locally administered EUI-64 derived from the factory EUI-48.
        u64::from_be_bytes([a[0] | 2, a[1], a[2], 0xff, 0xfe, a[3], a[4], a[5]])
    }

    pub async fn energy_detect(&mut self, duration_us: u16) -> Result<i8, Error> {
        if duration_us == 0 {
            return Err(Error::InvalidConfiguration);
        }
        struct Cancel;
        impl Drop for Cancel {
            fn drop(&mut self) {
                mac::cancel_energy_detect();
            }
        }
        let _cancel = Cancel;
        ED_RESULT.store(i16::MIN, Ordering::Release);
        // ESP-IDF's ED duration is in 16-us symbols. Max accumulation matches
        // Nordic's EDCNT scan; restore the normal CCA averaging mode on exit.
        mac::start_energy_detect(u32::from(duration_us).div_ceil(16) as u16);
        poll_fn(|cx| {
            ED_WAKER.register(cx.waker());
            match ED_RESULT.load(Ordering::Acquire) {
                i16::MIN => Poll::Pending,
                n if n == i16::MIN + 1 => Poll::Ready(Err(Error::Aborted)),
                n => Poll::Ready(Ok(n as i8)),
            }
        })
        .await
    }

    pub async fn receive(&mut self, out: &mut [u8; MAX_FRAME_SIZE]) -> Result<usize, Error> {
        mac::ieee802154_receive();
        let raw = poll_fn(|cx| {
            RX_WAKER.register(cx.waker());
            mac::ieee802154_poll().map_or(Poll::Pending, Poll::Ready)
        })
        .await;
        let length = raw.data[0] as usize;
        if !(5..=127).contains(&length) {
            return Err(Error::InvalidFrame);
        }
        #[cfg(feature = "test-support")]
        crate::kernel::radio::testing::event(crate::kernel::radio::testing::Event::ReceiveSignal(
            Some(raw.data[length - 1] as i8),
        ));
        let length = length - 2; // Hardware FCS is replaced by the receive trailer.
        out[..length].copy_from_slice(&raw.data[1..1 + length]);
        Ok(length)
    }

    pub async fn transmit(&mut self, frame: &[u8]) -> Result<(), Error> {
        if !(3..=MAX_FRAME_SIZE).contains(&frame.len()) {
            return Err(Error::InvalidFrame);
        }
        self.transmit.0[0] = (frame.len() + 2) as u8;
        self.transmit.0[1..1 + frame.len()].copy_from_slice(frame);
        TX_STATUS.store(0, Ordering::Release);
        mac::ieee802154_transmit(self.transmit.0.as_ptr(), true);
        poll_fn(|cx| {
            TX_WAKER.register(cx.waker());
            match TX_STATUS.load(Ordering::Acquire) {
                0 => Poll::Pending,
                1 => Poll::Ready(Ok(())),
                _ => Poll::Ready(Err(Error::Aborted)),
            }
        })
        .await
    }
}
