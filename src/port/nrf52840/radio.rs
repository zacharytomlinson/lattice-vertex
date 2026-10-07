//! Nordic RADIO integration. The HAL's futures wake the resident kernel task.

use super::{hal, RadioPeripheral, KERNEL_INTERRUPT_PRIORITY};
use crate::kernel::radio::{Error, MAX_FRAME_SIZE};
use core::{
    future::poll_fn,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};
use embassy_sync::waitqueue::AtomicWaker;
use hal::{
    interrupt::InterruptExt,
    radio::ieee802154::{Packet, Radio as Driver},
};

static ED_ACTIVE: AtomicBool = AtomicBool::new(false);
static ED_WAKER: AtomicWaker = AtomicWaker::new();

// The HAL exposes packet I/O but no ED operation. Share its interrupt binding;
// the resident task never polls a HAL packet future and an ED future together.
struct EnergyInterrupt;
impl hal::interrupt::typelevel::Handler<hal::interrupt::typelevel::RADIO> for EnergyInterrupt {
    unsafe fn on_interrupt() {
        if ED_ACTIVE.load(Ordering::Acquire) {
            hal::pac::RADIO.intenclr().write(|w| w.set_disabled(true));
            ED_WAKER.wake();
        }
    }
}

hal::bind_interrupts!(struct RadioInterrupt {
    RADIO => EnergyInterrupt, hal::radio::InterruptHandler<hal::peripherals::RADIO>;
});

pub(crate) struct Radio {
    driver: Driver<'static>,
    packet: Packet,
}

impl Radio {
    pub fn new(peripheral: RadioPeripheral) -> Self {
        // Set the IRQ priority before the HAL enables it. Its waker is allowed
        // to use the kernel's FromISR notification API at this priority.
        hal::interrupt::RADIO.set_priority(KERNEL_INTERRUPT_PRIORITY);
        let driver = Driver::new(peripheral, RadioInterrupt);
        Self {
            driver,
            packet: Packet::new(),
        }
    }

    pub fn configure(&mut self, channel: u8, _pan_id: u16) -> Result<(), Error> {
        if !(11..=26).contains(&channel) {
            return Err(Error::InvalidConfiguration);
        }
        // The Nordic HAL receives raw frames; PAN/address filtering belongs to
        // the common MAC consumer, including the hardware-test codec.
        self.driver.set_channel(channel);
        self.driver.set_transmission_power(8);
        Ok(())
    }

    pub fn address(&self) -> u64 {
        let low = hal::pac::FICR.deviceid(0).read() as u64;
        let high = hal::pac::FICR.deviceid(1).read() as u64;
        // Locally administered extended address from the chip's factory ID.
        ((high << 32) | low) & !(1 << 56) | (1 << 57)
    }

    pub async fn energy_detect(&mut self, duration_us: u16) -> Result<i8, Error> {
        use hal::pac::radio::vals::State;
        if duration_us == 0 {
            return Err(Error::InvalidConfiguration);
        }
        let r = hal::pac::RADIO;
        struct Cancel;
        impl Drop for Cancel {
            fn drop(&mut self) {
                let r = hal::pac::RADIO;
                r.intenclr().write(|w| w.0 = u32::MAX);
                r.shorts().write(|_| {});
                if r.state().read().state() != State::Disabled {
                    r.tasks_edstop().write_value(1);
                    r.tasks_disable().write_value(1);
                    // Like the HAL's packet cancellation, finish the hardware
                    // stop before its owner can begin another operation.
                    while r.state().read().state() != State::Disabled {}
                }
                r.events_edend().write_value(0);
                r.events_disabled().write_value(0);
                ED_ACTIVE.store(false, Ordering::Release);
            }
        }
        let _cancel = Cancel;
        ED_ACTIVE.store(true, Ordering::Release);
        self.driver.clear_all_interrupts();
        r.shorts().write(|_| {});
        if r.state().read().state() != State::Disabled {
            r.events_disabled().write_value(0);
            r.tasks_disable().write_value(1);
            poll_fn(|cx| {
                ED_WAKER.register(cx.waker());
                r.intenset().write(|w| w.set_disabled(true));
                if r.state().read().state() == State::Disabled {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
        }
        r.events_disabled().write_value(0);
        r.events_edend().write_value(0);
        // EDCNT+1 windows of 128 us; hardware retains the maximum measurement.
        r.edcnt()
            .write(|w| w.set_edcnt(u32::from(duration_us).div_ceil(128) - 1));
        r.shorts().write(|w| {
            w.set_ready_edstart(true);
            w.set_edend_disable(true);
        });
        r.tasks_rxen().write_value(1);
        poll_fn(|cx| {
            ED_WAKER.register(cx.waker());
            r.intenset().write(|w| w.set_disabled(true));
            if r.events_disabled().read() != 0 {
                if r.events_edend().read() == 0 {
                    return Poll::Ready(Err(Error::Aborted));
                }
                // nRF52840 Product Specification: ED_RSSIOFFS=-92, ED_RSSISCALE=4.
                let dbm = -92i16 + 4 * r.edsample().read().edlvl() as i16;
                Poll::Ready(Ok(dbm.clamp(i8::MIN as i16, i8::MAX as i16) as i8))
            } else {
                Poll::Pending
            }
        })
        .await
    }

    pub async fn receive(&mut self, out: &mut [u8; MAX_FRAME_SIZE]) -> Result<usize, Error> {
        self.driver
            .receive(&mut self.packet)
            .await
            .map_err(|_| Error::InvalidFrame)?;
        let length = self.packet.len() as usize;
        if length > MAX_FRAME_SIZE {
            return Err(Error::InvalidFrame);
        }
        out[..length].copy_from_slice(&self.packet);
        Ok(length)
    }

    pub async fn transmit(&mut self, frame: &[u8]) -> Result<(), Error> {
        if frame.len() > MAX_FRAME_SIZE {
            return Err(Error::InvalidFrame);
        }
        self.packet.copy_from_slice(frame);
        self.driver
            .try_send(&mut self.packet)
            .await
            .map_err(|e| match e {
                hal::radio::Error::ChannelInUse => Error::ChannelBusy,
                _ => Error::Aborted,
            })
    }
}
