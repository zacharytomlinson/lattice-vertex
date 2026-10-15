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
    let (mut sender, receiver, changed) = class.split_with_control();
    let mut device = builder.build();
    let control_changes = async {
        loop {
            changed.control_changed().await;
            // Match Seeed/Adafruit's application-to-serial-DFU handoff. This
            // hook is only present in the XIAO diagnostic image.
            #[cfg(feature = "xiao-bootloader")]
            if receiver.line_coding().data_rate() == 1200 && !receiver.dtr() {
                // Let the host finish opening/closing the port. Resetting in
                // the control transfer itself can make that transfer fail.
                let started = tasks::xTaskGetTickCount();
                let reset = poll_fn(|_| {
                    if receiver.line_coding().data_rate() != 1200 || receiver.dtr() {
                        Poll::Ready(false)
                    } else if tasks::xTaskGetTickCount().wrapping_sub(started)
                        >= lattice_vertex::milliseconds_to_ticks_checked(100).unwrap()
                    {
                        Poll::Ready(true)
                    } else {
                        Poll::Pending
                    }
                })
                .await;
                if reset {
                    hal::pac::POWER.gpregret().write(|w| w.set_gpregret(0x4e));
                    cortex_m::peripheral::SCB::sys_reset();
                }
            }
            #[cfg(not(feature = "xiao-bootloader"))]
            let _ = &receiver;
        }
    };
    let reporter = async {
        loop {
            sender.wait_connection().await;
            poll_fn(|_| {
                if sender.dtr() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
            if sender
                .write_packet(b"START kernel radio: nRF52840 Cortex-M4F\r\n")
                .await
                .is_err()
            {
                continue;
            }
            let mut last = tasks::xTaskGetTickCount();
            'connected: loop {
                poll_fn(|_| {
                    if tasks::xTaskGetTickCount().wrapping_sub(last)
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
                for bytes in text.bytes[..text.len].chunks(64) {
                    if !sender.dtr() || sender.write_packet(bytes).await.is_err() {
                        break 'connected;
                    }
                }
                if text.len % 64 == 0 && sender.write_packet(&[]).await.is_err() {
                    break;
                }
            }
        }
    };
    let mut work = pin!(join(device.run(), join(reporter, control_changes)));
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        let _ = work.as_mut().poll(&mut cx);
        lattice_vertex::delay_ms(context, 1);
    }
}
