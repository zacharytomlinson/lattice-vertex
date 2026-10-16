//! Nordic RADIO: HAL initialization/CCA TX, port-owned RX and immediate ACKs.

use super::{hal, RadioPeripheral, KERNEL_INTERRUPT_PRIORITY};
use crate::kernel::radio::{mac, receive::ReceiveBuffer, Error, MAX_FRAME_SIZE};
mod receive;
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

static ACK_WAIT: AtomicBool = AtomicBool::new(false);
static ED_ACTIVE: AtomicBool = AtomicBool::new(false);
static ED_WAKER: AtomicWaker = AtomicWaker::new();

// One IRQ owner dispatches to the active RX/ACK, ED, or HAL TX operation.
struct Interrupt;
impl hal::interrupt::typelevel::Handler<hal::interrupt::typelevel::RADIO> for Interrupt {
    unsafe fn on_interrupt() {
        if receive::interrupt() {
            return;
        }
        if ED_ACTIVE.load(Ordering::Acquire) {
            hal::pac::RADIO.intenclr().write(|w| w.set_disabled(true));
            ED_WAKER.wake();
        } else {
            <hal::radio::InterruptHandler<hal::peripherals::RADIO> as hal::interrupt::typelevel::Handler<hal::interrupt::typelevel::RADIO>>::on_interrupt();
        }
    }
}

hal::bind_interrupts!(struct RadioInterrupt {
    RADIO => Interrupt;
});

// The combined handler delegates to the HAL only while HAL TX owns RADIO.
unsafe impl
    hal::interrupt::typelevel::Binding<
        hal::interrupt::typelevel::RADIO,
        hal::radio::InterruptHandler<hal::peripherals::RADIO>,
    > for RadioInterrupt
{
}

pub(crate) struct Radio {
    driver: Driver<'static>,
    packet: Packet,
    receive: receive::Receiver,
    pan: u16,
}

impl Radio {
    pub fn new(peripheral: RadioPeripheral) -> Self {
        // Set the IRQ priority before the HAL enables it. Its waker is allowed
        // to use the kernel's FromISR notification API at this priority.
        hal::interrupt::RADIO.set_priority(KERNEL_INTERRUPT_PRIORITY);
        let driver = Driver::new(peripheral, RadioInterrupt);
        // embassy-nrf 0.11 selects CarrierMode with a whole-register write,
        // clearing the reset correlation threshold/count. Count zero disables
        // the correlator, so CCA can miss a frame whose SFD has already passed.
        // Restore the nRF52840 CCACTRL reset fields (0x052D0000).
        hal::pac::RADIO.ccactrl().modify(|w| {
            w.set_ccacorrthres(45);
            w.set_ccacorrcnt(5);
        });
        #[cfg(feature = "test-support")]
        assert_eq!(hal::pac::RADIO.ccactrl().read().0, 0x052d0001);
        Self {
            driver,
            packet: Packet::new(),
            receive: receive::Receiver::new(),
            pan: 0,
        }
    }

    pub fn configure(&mut self, channel: u8, pan_id: u16) -> Result<(), Error> {
        if !(11..=26).contains(&channel) {
            return Err(Error::InvalidConfiguration);
        }
        // The Nordic HAL receives raw frames; PAN/address filtering belongs to
        // the common MAC consumer, including the hardware-test codec.
        self.pan = pan_id;
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
                // Raw EDSAMPLE has 1 dB steps above -92 dBm. Multiplication by
                // four encodes the IEEE ED level, not dBm. See Nordic's
                // nrf_802154_rssi_ed_sample_to_dbm_convert (without its optional
                // temperature correction).
                let dbm = -92i16 + r.edsample().read().edlvl() as i16;
                Poll::Ready(Ok(dbm.clamp(i8::MIN as i16, i8::MAX as i16) as i8))
            } else {
                Poll::Pending
            }
        })
        .await
    }

    pub(crate) fn receiving_frame() -> bool {
        receive::active_frame()
    }
    pub(crate) fn waiting_for_ack() -> bool {
        ACK_WAIT.load(Ordering::Acquire)
    }

    pub async fn receive(&mut self, out: &mut [u8; MAX_FRAME_SIZE]) -> Result<usize, Error> {
        self.receive.receive(self.pan, self.address(), out).await
    }

    pub async fn transmit(
        &mut self,
        frame: &[u8],
        received: &mut ReceiveBuffer,
    ) -> Result<(), Error> {
        if frame.len() > MAX_FRAME_SIZE {
            return Err(Error::InvalidFrame);
        }
        struct Cancel;
        impl Drop for Cancel {
            fn drop(&mut self) {
                ACK_WAIT.store(false, Ordering::Release);
                cortex_m::interrupt::free(|_| receive::stop());
            }
        }
        let _cancel = Cancel;
        self.packet.copy_from_slice(frame);
        self.driver
            .try_send(&mut self.packet)
            .await
            .map_err(|e| match e {
                hal::radio::Error::ChannelInUse => Error::ChannelBusy,
                _ => Error::Aborted,
            })?;
        if !mac::requests_ack(frame) {
            return Ok(());
        }
        ACK_WAIT.store(true, Ordering::Release);
        let mut incoming = [0; MAX_FRAME_SIZE];
        loop {
            // The resident task bounds the ACK wait independently of frame TX.
            // Other frames received here still enter its ordinary receive ring.
            if let Ok(length) = self.receive(&mut incoming).await {
                if mac::matches_ack(&incoming[..length], frame[2]) {
                    return Ok(());
                }
                if !mac::is_ack(&incoming[..length]) {
                    let evicted = received.push(&incoming[..length]);
                    #[cfg(feature = "test-support")]
                    if evicted {
                        crate::kernel::radio::testing::event(
                            crate::kernel::radio::testing::Event::ReceiveEvicted,
                        );
                    }
                    let _ = evicted;
                }
            }
        }
    }
}
