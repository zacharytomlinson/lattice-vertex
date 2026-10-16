//! USB CDC diagnostics for the Nordic board fixture, independent of radio work.
//! The low-priority reporter polls USB once per kernel millisecond. This avoids
//! a second executor and keeps console disconnects from blocking radio traffic.

use core::{
    fmt,
    fmt::Write,
    future::{poll_fn, Future},
    pin::pin,
    task::{Context, Poll, Waker},
};
use embassy_futures::join::join;
use embassy_usb::{
    class::cdc_acm::{CdcAcmClass, State},
    Builder, Config,
};
use hal::{
    bind_interrupts,
    interrupt::{self, InterruptExt},
    peripherals, usb,
};
use lattice_vertex::{kernel::tasks, port::nrf52840::hal, sync::TaskContext};

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
});

static mut PERIPHERAL: Option<hal::Peri<'static, peripherals::USBD>> = None;

pub fn initialize(peripheral: hal::Peri<'static, peripherals::USBD>) {
    unsafe {
        PERIPHERAL = Some(peripheral);
    }
}

struct Text<const N: usize> {
    bytes: [u8; N],
    len: usize,
}
impl<const N: usize> Text<N> {
    fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }
}
impl<const N: usize> fmt::Write for Text<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self
            .len
            .checked_add(text.len())
            .filter(|end| *end <= N)
            .ok_or(fmt::Error)?;
        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

pub fn run(context: &TaskContext) -> ! {
    interrupt::USBD.set_priority(interrupt::Priority::P6);
    interrupt::CLOCK_POWER.set_priority(interrupt::Priority::P6);
    let driver = usb::Driver::new(
        unsafe { PERIPHERAL.take().unwrap() },
        Irqs,
        usb::vbus_detect::HardwareVbusDetect::new(Irqs),
    );
    let mut serial = Text::<16>::new();
    write!(
        serial,
        "{:08X}{:08X}",
        hal::pac::FICR.deviceid(1).read(),
        hal::pac::FICR.deviceid(0).read()
    )
    .unwrap();
    // Development VID/PID used by Embassy's USB examples; fixture only.
    let mut config = Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Lattice");
    config.product = Some("nRF52840 kernel test");
    config.serial_number = Some(core::str::from_utf8(&serial.bytes).unwrap());
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    let mut descriptor = [0; 256];
    let mut bos = [0; 64];
    let mut msos = [0; 64];
    let mut control = [0; 64];
    let mut state = State::new();
    let mut builder = Builder::new(
        driver,
        config,
        &mut descriptor,
        &mut bos,
        &mut msos,
        &mut control,
    );
    let class = CdcAcmClass::new(&mut builder, &mut state, 64);
    let (mut sender, receiver) = class.split();
    let mut device = builder.build();
    let reporter = async {
        let mut last = tasks::xTaskGetTickCount();
        let mut reset_since = None;
        loop {
            poll_fn(|_| {
                check_bootloader(
                    receiver.line_coding().data_rate(),
                    receiver.dtr(),
                    &mut reset_since,
                );
                let now = tasks::xTaskGetTickCount();
                if !sender.dtr() {
                    last = now;
                    return Poll::Pending;
                }
                if now.wrapping_sub(last)
                    >= lattice_vertex::milliseconds_to_ticks_checked(1000).unwrap()
                {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
            last = tasks::xTaskGetTickCount();
            let mut text = Text::<1536>::new();
            if crate::report_once(context, &mut text).is_err() {
                text.len = 0;
                writeln!(text, "LOG report exceeds console buffer").unwrap();
            }
            for bytes in text.bytes[..text.len]
                .chunks(64)
                .chain((text.len % 64 == 0).then_some(&[][..]))
            {
                let mut write = pin!(sender.write_packet(bytes));
                let result = poll_fn(|cx| {
                    check_bootloader(
                        receiver.line_coding().data_rate(),
                        receiver.dtr(),
                        &mut reset_since,
                    );
                    write.as_mut().poll(cx)
                })
                .await;
                if result.is_err() {
                    break;
                }
            }
        }
    };
    let mut work = pin!(join(device.run(), reporter));
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        let _ = work.as_mut().poll(&mut cx);
        lattice_vertex::delay_ms(context, 1);
    }
}

// Sample USB control state on every fixture poll, including while an IN transfer
// is pending. A control-change notification must never spin or block device.run.
fn check_bootloader(rate: u32, dtr: bool, started: &mut Option<lattice_vertex::types::TickType_t>) {
    #[cfg(feature = "xiao-bootloader")]
    {
        if rate != 1200 || dtr {
            *started = None;
            return;
        }
        let now = tasks::xTaskGetTickCount();
        let since = *started.get_or_insert(now);
        if now.wrapping_sub(since) >= lattice_vertex::milliseconds_to_ticks_checked(100).unwrap() {
            hal::pac::POWER.gpregret().write(|w| w.set_gpregret(0x4e));
            cortex_m::peripheral::SCB::sys_reset();
        }
    }
    #[cfg(not(feature = "xiao-bootloader"))]
    let _ = (rate, dtr, started);
}
