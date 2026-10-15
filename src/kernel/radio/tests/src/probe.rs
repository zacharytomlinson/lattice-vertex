//! Diagnostic workload executed on the kernel's resident radio task.
use super::{
    frame::{Kind, Message},
    statistics::*,
    MAX_FRAME_SIZE,
};
use core::sync::atomic::Ordering;
use lattice_vertex::{
    kernel::{
        radio::testing::{self, Event, Transport},
        tasks,
    },
    port::RadioPeripheral,
    sync::TaskContext,
};

pub fn start(context: TaskContext, radio: RadioPeripheral) -> ! {
    testing::start(context, radio, run, event);
}

fn event(event: Event) {
    match event {
        Event::NotificationWait => {
            NOTIFICATION_WAITS.fetch_add(1, Ordering::Relaxed);
        }
        Event::InterruptWake => {
            INTERRUPT_WAKES.fetch_add(1, Ordering::Relaxed);
        }
        Event::ReceiveEvicted => {
            EVICTED.fetch_add(1, Ordering::Relaxed);
        }
        Event::ReceiveSignal(rssi) => {
            RSSI.store(rssi.map_or(i32::MIN, i32::from), Ordering::Relaxed);
        }
        Event::ReceiveAborted(reason) => {
            RX_ABORT_EVENTS.fetch_add(1, Ordering::Relaxed);
            RX_ABORT_LAST.store(reason, Ordering::Relaxed);
        }
        // Common peer events are not emitted by this replacement workload.
        _ => {}
    }
}

fn run(radio: &mut Transport) -> ! {
    radio.configure(15, 0xffff).unwrap();
    let local = radio.address();
    let mut buffer = [0; MAX_FRAME_SIZE];
    let mut sequence = 0u8;
    let mut token = 0u32;
    let mut replies = ReplyWindow::default();
    let period = lattice_vertex::milliseconds_to_ticks_checked(1700 + local % 601).unwrap();
    let mut last_probe = tasks::xTaskGetTickCount();
    loop {
        let timeout = period.saturating_sub(tasks::xTaskGetTickCount().wrapping_sub(last_probe));
        ACTIVITY.store(Activity::Receiving as u8, Ordering::Relaxed);
        let response = match radio.receive(&mut buffer, timeout) {
            Some(Ok(length)) => {
                RECEIVED.fetch_add(1, Ordering::Relaxed);
                match Message::decode(&buffer[..length], local) {
                    Ok(message) => {
                        replies.observe_peer();
                        match message.kind {
                            Kind::Probe => Some(Message {
                                source: local,
                                destination: Some(message.source),
                                kind: Kind::Reply,
                                token: message.token,
                            }),
                            Kind::Reply => {
                                if replies.reply(message.token) {
                                    REPLIES_RECEIVED.fetch_add(1, Ordering::Relaxed);
                                    PENDING_PROBE.store(false, Ordering::Relaxed);
                                } else {
                                    UNMATCHED_REPLIES.fetch_add(1, Ordering::Relaxed);
                                }
                                None
                            }
                        }
                    }
                    Err(_) => {
                        IGNORED.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            }
            Some(Err(_)) => {
                RECEIVE_ERRORS.fetch_add(1, Ordering::Relaxed);
                None
            }
            None => {
                RECEIVE_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                None
            }
        };
        let outgoing = response.or_else(|| {
            if tasks::xTaskGetTickCount().wrapping_sub(last_probe) >= period {
                last_probe = tasks::xTaskGetTickCount();
                token = token.wrapping_add(1);
                Some(Message {
                    source: local,
                    destination: None,
                    kind: Kind::Probe,
                    token,
                })
            } else {
                None
            }
        });
        if let Some(message) = outgoing {
            if message.kind == Kind::Probe {
                if replies.close() {
                    UNANSWERED.fetch_add(1, Ordering::Relaxed);
                }
                PENDING_PROBE.store(false, Ordering::Relaxed);
            }
            let length = message.encode(sequence, &mut buffer);
            sequence = sequence.wrapping_add(1);
            // TX must finish before its stable driver buffer can be reused. No
            // timeout drops an active DMA operation or retries an invocation.
            ACTIVITY.store(Activity::Transmitting as u8, Ordering::Relaxed);
            match radio.transmit(&buffer[..length]) {
                Ok(()) => {
                    TRANSMITTED.fetch_add(1, Ordering::Relaxed);
                    if message.kind == Kind::Reply {
                        REPLIES_SENT.fetch_add(1, Ordering::Relaxed);
                    } else {
                        PROBES_SENT.fetch_add(1, Ordering::Relaxed);
                        if !replies.sent(message.token) {
                            PRE_PEER_PROBES.fetch_add(1, Ordering::Relaxed);
                        }
                        LAST_PROBE_TOKEN.store(message.token, Ordering::Relaxed);
                        PENDING_PROBE.store(true, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    TRANSMIT_ERRORS.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}
