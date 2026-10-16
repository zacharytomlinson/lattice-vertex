//! Raw reception and immediate 2006 ACK turnaround. Hardware TIFS schedules
//! ACK TX; no task scheduling, CCA, allocator, or appliance callback intervenes.
//! See nRF52840 Product Specification, RADIO interframe spacing and 802.15.4:
//! https://docs.nordicsemi.com/r/bundle/ps_nrf52840/page/radio.html
use super::hal::pac::{radio::vals::State, RADIO};
use crate::kernel::radio::{mac, Error, MAX_FRAME_SIZE};
use core::{
    future::poll_fn,
    sync::atomic::{compiler_fence, AtomicU8, Ordering},
    task::Poll,
};
use embassy_sync::waitqueue::AtomicWaker;

const IDLE: u8 = 0;
const RECEIVING: u8 = 1;
const ACK_RAMP: u8 = 2;
const ACK_TX: u8 = 3;
const VALID: u8 = 4;
const INVALID: u8 = 5;
static PHASE: AtomicU8 = AtomicU8::new(IDLE);
static WAKER: AtomicWaker = AtomicWaker::new();
// Written only while RADIO IRQ is masked and disabled. Read only by that IRQ.
static mut PAN: u16 = 0;
static mut LOCAL: u64 = 0;
#[repr(C, align(4))]
struct Ack([u8; 4]);
static mut ACK: Ack = Ack([5, 2, 0, 0]);

#[repr(C, align(4))]
pub(super) struct Receiver([u8; 128]);
impl Receiver {
    pub(super) const fn new() -> Self {
        Self([0; 128])
    }
    pub(super) async fn receive(
        &mut self,
        pan: u16,
        local: u64,
        out: &mut [u8; MAX_FRAME_SIZE],
    ) -> Result<usize, Error> {
        let cancel = Cancel;
        let mut started = false;
        let result = poll_fn(|cx| {
            WAKER.register(cx.waker());
            if !started {
                cortex_m::interrupt::free(|_| unsafe {
                    stop();
                    PAN = pan;
                    LOCAL = local;
                    RADIO.events_end().write_value(0);
                    RADIO.events_phyend().write_value(0);
                    RADIO.events_txready().write_value(0);
                    RADIO.events_framestart().write_value(0);
                    RADIO.packetptr().write_value(self.0.as_mut_ptr() as u32);
                    RADIO.tifs().write(|w| w.set_tifs(192));
                    // Arrange turnaround before the last bit arrives. At END,
                    // the IRQ either supplies a valid ACK or cancels TX ramp-up.
                    // TXREADY_START is armed only after CRC and address checks.
                    RADIO.shorts().write(|w| {
                        w.set_rxready_start(true);
                        w.set_end_disable(true);
                        w.set_disabled_txen(true);
                    });
                    compiler_fence(Ordering::SeqCst);
                    PHASE.store(RECEIVING, Ordering::Release);
                    RADIO.intenset().write(|w| w.set_end(true));
                    RADIO.tasks_rxen().write_value(1);
                });
                started = true;
            }
            match PHASE.load(Ordering::Acquire) {
                VALID => Poll::Ready(Ok(())),
                INVALID => Poll::Ready(Err(Error::InvalidFrame)),
                _ => Poll::Pending,
            }
        })
        .await;
        result?;
        compiler_fence(Ordering::SeqCst);
        let size = self.0[0] as usize;
        if !(2..=127).contains(&size) {
            return Err(Error::InvalidFrame);
        }
        let length = size - 2;
        out[..length].copy_from_slice(&self.0[1..1 + length]);
        drop(cancel);
        Ok(length)
    }
}

pub(super) fn active_frame() -> bool {
    match PHASE.load(Ordering::Acquire) {
        ACK_RAMP | ACK_TX => true,
        RECEIVING => RADIO.events_framestart().read() != 0,
        _ => false,
    }
}

/// Stop before an operation's DMA storage may be reused, including on timeout.
pub(super) fn stop() {
    RADIO.intenclr().write(|w| w.0 = u32::MAX);
    RADIO.shorts().write(|_| {});
    if RADIO.state().read().state() != State::Disabled {
        RADIO.tasks_disable().write_value(1);
        while RADIO.state().read().state() != State::Disabled {}
    }
    compiler_fence(Ordering::SeqCst);
    PHASE.store(IDLE, Ordering::Release);
}
struct Cancel;
impl Drop for Cancel {
    fn drop(&mut self) {
        cortex_m::interrupt::free(|_| stop());
    }
}

unsafe fn complete(valid: bool) {
    stop();
    PHASE.store(if valid { VALID } else { INVALID }, Ordering::Release);
    WAKER.wake();
}

/// Return true when this receiver, rather than the HAL TX/ED future, owns IRQs.
pub(super) unsafe fn interrupt() -> bool {
    match PHASE.load(Ordering::Acquire) {
        RECEIVING if RADIO.events_end().read() != 0 => {
            RADIO.intenclr().write(|w| w.0 = u32::MAX);
            RADIO.events_end().write_value(0);
            RADIO.events_phyend().write_value(0);
            compiler_fence(Ordering::SeqCst);
            let valid = RADIO.crcstatus().read().crcstatus()
                == super::hal::pac::radio::vals::Crcstatus::CrcOk;
            let data = RADIO.packetptr().read() as *const u8;
            let size = data.read() as usize;
            let sequence = if valid && (2..=127).contains(&size) {
                mac::ack_for(
                    core::slice::from_raw_parts(data.add(1), size - 2),
                    PAN,
                    LOCAL,
                )
            } else {
                None
            };
            if let Some(sequence) = sequence {
                ACK.0[3] = sequence;
                RADIO
                    .packetptr()
                    .write_value(core::ptr::addr_of_mut!(ACK) as u32);
                compiler_fence(Ordering::SeqCst);
                PHASE.store(ACK_RAMP, Ordering::Release);
                RADIO.shorts().modify(|w| w.set_txready_start(true));
                RADIO.intenset().write(|w| w.set_txready(true));
                // If an unusually long masked interval missed READY, finish
                // the exchange; the sender may already have timed out.
                if RADIO.state().read().state() == State::TxIdle {
                    RADIO.tasks_start().write_value(1);
                }
            } else {
                complete(valid);
            }
            true
        }
        ACK_RAMP if RADIO.events_txready().read() != 0 => {
            RADIO.events_txready().write_value(0);
            RADIO.intenclr().write(|w| w.0 = u32::MAX);
            // TX must finish on PHYEND, not END (which precedes its final bits).
            RADIO.shorts().write(|w| w.set_phyend_disable(true));
            PHASE.store(ACK_TX, Ordering::Release);
            RADIO.intenset().write(|w| w.set_phyend(true));
            true
        }
        ACK_TX if RADIO.events_phyend().read() != 0 => {
            RADIO.events_phyend().write_value(0);
            complete(true);
            true
        }
        IDLE => false,
        _ => true,
    }
}
