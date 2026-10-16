// Copyright 2021 esp-rs
// SPDX-License-Identifier: MIT
// Adapted from esp-radio 1.0.0-beta.0; see README.md and LICENSE-MIT.

use heapless::Deque as Queue;

use esp_hal::{handler, interrupt::Priority, peripherals::IEEE802154};
use esp_phy::{PhyClockGuard, PhyInitGuard};
use esp_sync::NonReentrantMutex;

use super::clocks::{enable_ieee802154, init_clocks as init_radio_clocks};
use super::{
    frame::{
        frame_get_version, frame_is_ack_required, FRAME_SIZE, FRAME_VERSION_1, FRAME_VERSION_2,
    },
    registers::*,
    settings::*,
};
use esp_wifi_sys_esp32c6::include::{
    ieee802154_coex_event_t, ieee802154_coex_event_t_IEEE802154_IDLE,
    ieee802154_coex_event_t_IEEE802154_LOW, ieee802154_coex_event_t_IEEE802154_MIDDLE,
    phy_init_param_set, phy_wifi_enable_set,
};

const PHY_ENABLE_VERSION_PRINT: u8 = 0;

/// IEEE 802.15.4 immediate-ACK wait, 54 symbols.
const ACK_TIMEOUT_US: u32 = crate::kernel::radio::mac::ACK_WAIT_US;

#[repr(C, align(4))]
struct DmaBuffer([u8; FRAME_SIZE]);
static mut RX_BUFFER: DmaBuffer = DmaBuffer([0; FRAME_SIZE]);

struct PendingTx {
    frame: *const u8,
    cca: bool,
}

// Safety: PendingTx holds a raw pointer to a caller-managed buffer that remains
// valid for the lifetime of the pending operation (the resident Radio transmit buffer).
unsafe impl Send for PendingTx {}

struct IeeeState {
    state: Ieee802154State,
    rx_queue: Queue<RawReceived, 4>,
    pending_tx: Option<PendingTx>,
    /// Stores the last received ACK frame (populated during
    /// isr_handle_ack_rx_done and stop_rx_ack). Cleared at the start of
    /// each transmit.
    ack_frame: Option<RawReceived>,
}

static STATE: NonReentrantMutex<IeeeState> = NonReentrantMutex::new(IeeeState {
    state: Ieee802154State::Idle,
    rx_queue: Queue::new(),
    pending_tx: None,
    ack_frame: None,
});

unsafe extern "C" {
    fn bt_bb_v2_init_cmplx(print_version: u8); // from libbtbb.a

    fn esp_coex_ieee802154_ack_pti_set(event: ieee802154_coex_event_t); // from ???

    fn esp_coex_ieee802154_txrx_pti_set(event: ieee802154_coex_event_t); // from ???

    fn esp_coex_ieee802154_coex_break_notify(); // from coex lib
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ieee802154State {
    Idle,
    EnergyDetect,
    Receive,
    Transmit,
    TxAck,
    RxAck,
    TxEnhAck,
}

#[allow(unused)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ieee802154TxRxScene {
    Idle,
    Tx,
    Rx,
    TxAt,
    RxAt,
}

/// A raw payload received on some channel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawReceived {
    /// Payload
    pub data: [u8; FRAME_SIZE],
    /// Receiver channel
    pub channel: u8,
}

pub(crate) fn esp_ieee802154_enable(
    radio: IEEE802154<'_>,
) -> (PhyClockGuard<'_>, PhyInitGuard<'_>) {
    init_radio_clocks();
    let phy_clock_guard = esp_phy::enable_phy_clock();
    enable_ieee802154(true);

    // C6 is a combo-radio part. esp-phy 0.2.0 leaves this selection disabled;
    // vendor initialization and esp-hal PR #5776 select it before calibration.
    // This port owns an 802.15.4-only PHY, so it must not retain default Wi-Fi RX.
    unsafe { phy_init_param_set(1) };
    let phy_init_guard = esp_phy::enable_phy();
    unsafe { phy_wifi_enable_set(0) };

    esp_btbb_enable();
    ieee802154_mac_init(radio);

    (phy_clock_guard, phy_init_guard)
}

fn esp_btbb_enable() {
    unsafe { bt_bb_v2_init_cmplx(PHY_ENABLE_VERSION_PRINT) };
}

fn ieee802154_mac_init(radio: IEEE802154<'_>) {
    unsafe {
        unsafe extern "C" {
            static mut coex_pti_tab_ptr: u32;
            static coex_pti_tab: u8;
        }

        // Manually set `coex_pti_tab_ptr` pointing to `coex_pti_tab`
        core::ptr::addr_of_mut!(coex_pti_tab_ptr).write_volatile(&coex_pti_tab as *const _ as u32);
    }

    ieee802154_pib_init();

    enable_events(Event::mask());
    disable_events(Event::Timer0Overflow | Event::Timer1Overflow);

    enable_tx_abort_events(
        TxAbortReason::RxAckTimeout
            | TxAbortReason::TxCoexBreak
            | TxAbortReason::TxSecurityError
            | TxAbortReason::CcaFailed
            | TxAbortReason::CcaBusy,
    );
    enable_rx_abort_events(RxAbortReason::TxAckTimeout | RxAbortReason::TxAckCoexBreak);

    // The vendor MAC-reset default is one sample/us. Set it explicitly so a
    // warm restart cannot retain the sampling-rate write from the old bug.
    set_ed_sample_rate(EdSampleRate::OnePerUs);
    set_ed_sample_mode(EdSampleMode::Avg);

    unsafe { esp_coex_ieee802154_ack_pti_set(ieee802154_coex_event_t_IEEE802154_MIDDLE) };
    ieee802154_set_txrx_pti(Ieee802154TxRxScene::Idle);

    // bt_bb_v2_init_cmplx configures the baseband and MAC turnaround together.
    // In the linked esp-wifi-sys-esp32c6 0.2.0 library these include baseband
    // TX-on 29, MAC TX-on 35, RX-on 50, and TX/RX switch 127. The older Rust
    // driver overwrote only baseband TX-on with 50, breaking that pairing.
    // Preserve the initializer's values instead of imposing another delay.

    // memset(s_rx_frame, 0, sizeof(s_rx_frame));
    // s_ieee802154_state = IEEE802154_STATE_IDLE;

    radio.bind_mac_interrupt(zb_mac_handler);
}

fn ieee802154_set_txrx_pti(txrx_scene: Ieee802154TxRxScene) {
    match txrx_scene {
        Ieee802154TxRxScene::Idle => {
            unsafe { esp_coex_ieee802154_txrx_pti_set(ieee802154_coex_event_t_IEEE802154_IDLE) };
        }
        Ieee802154TxRxScene::Tx | Ieee802154TxRxScene::Rx => {
            unsafe { esp_coex_ieee802154_txrx_pti_set(ieee802154_coex_event_t_IEEE802154_LOW) };
        }
        Ieee802154TxRxScene::TxAt | Ieee802154TxRxScene::RxAt => {
            unsafe { esp_coex_ieee802154_txrx_pti_set(ieee802154_coex_event_t_IEEE802154_MIDDLE) };
        }
    }
}

fn tx_init(state: &mut IeeeState, frame: *const u8) {
    stop_current_operation_inner(state);

    ieee802154_pib_update();
    set_transmit_security(false);

    state.ack_frame = None;

    set_tx_addr(frame);

    if frame_is_ack_required(unsafe { core::slice::from_raw_parts(frame.add(1), *frame as usize) })
    {
        // set rx pointer for ack frame
        set_next_rx_buffer();
    }
}

/// Pointer to the current TX frame (stored for ACK handling)
static mut TX_FRAME: *const u8 = core::ptr::null();

pub fn ieee802154_transmit(frame: *const u8, cca: bool) -> i32 {
    STATE.with(|state| {
        // TX deferral: don't abort in-flight frame reception or ACK transmission.
        // Matches the C driver's ieee802154_transmit() which defers to pending_tx.
        if state.state == Ieee802154State::TxAck
            || state.state == Ieee802154State::TxEnhAck
            || (state.state == Ieee802154State::Receive && is_current_rx_frame())
        {
            // Defer: store pending TX and enable all RX abort events so we
            // know when the current operation finishes.
            state.pending_tx = Some(PendingTx { frame, cca });
            enable_rx_abort_events(RxAbortReason::all());
            return;
        }

        state.pending_tx = None;
        transmit_internal(state, frame, cca);
    });

    0 // ESP_OK
}

fn transmit_internal(state: &mut IeeeState, frame: *const u8, cca: bool) {
    unsafe { TX_FRAME = frame };

    tx_init(state, frame);

    ieee802154_set_txrx_pti(Ieee802154TxRxScene::Tx);

    if cca {
        set_cmd(Command::CcaTxStart);
    } else {
        set_cmd(Command::TxStart);
    }
    state.state = Ieee802154State::Transmit;
}

pub fn ieee802154_receive() -> i32 {
    STATE.with(|state| {
        if state.state == Ieee802154State::Receive || state.state == Ieee802154State::TxAck {
            // already in rx or tx_ack state, don't abort current operation
            return;
        }

        rx_init(state);
        enable_rx();

        state.state = Ieee802154State::Receive;
    });

    0 // ESP-OK
}

pub fn ieee802154_poll() -> Option<RawReceived> {
    STATE.with(|state| state.rx_queue.pop_front())
}

/// Leave the old channel with no queued frames that could be mistaken for
/// replies on the next channel. Called only between resident task operations.
pub fn prepare_reconfigure() {
    STATE.with(|state| {
        stop_current_operation_inner(state);
        state.state = Ieee802154State::Idle;
        state.rx_queue.clear();
    });
}

pub fn waiting_for_ack() -> bool {
    STATE.with(|state| state.state == Ieee802154State::RxAck)
}

pub fn cancel_transmit() {
    STATE.with(|state| {
        state.pending_tx = None;
        stop_current_operation_inner(state);
        state.state = Ieee802154State::Idle;
    });
}

pub fn start_energy_detect(symbols: u16) {
    STATE.with(|state| {
        stop_current_operation_inner(state);
        ieee802154_pib_update();
        ieee802154_set_txrx_pti(Ieee802154TxRxScene::Rx);
        clear_events(Event::EdDone | Event::RxAbort);
        set_ed_sample_mode(EdSampleMode::Max);
        set_ed_duration(symbols);
        enable_rx_abort_events(RxAbortReason::EdAbort | RxAbortReason::EdCoexReject);
        state.state = Ieee802154State::EnergyDetect;
        set_cmd(Command::EdStart);
    });
}

pub fn cancel_energy_detect() {
    STATE.with(|state| {
        if state.state == Ieee802154State::EnergyDetect {
            stop_current_operation_inner(state);
            state.state = Ieee802154State::Idle;
        }
    });
}

/// Get the ACK frame received in response to the last transmission.
/// Returns `None` if no ACK was received (e.g., frame didn't require ACK,
/// or ACK was not received before timeout).
/// The C driver passes this via `esp_ieee802154_transmit_done(frame, ack, ack_info)`.
pub fn get_ack_frame() -> Option<RawReceived> {
    STATE.with(|state| state.ack_frame)
}

fn rx_init(state: &mut IeeeState) {
    stop_current_operation_inner(state);
    ieee802154_pib_update();
}

fn enable_rx() {
    set_next_rx_buffer();
    ieee802154_set_txrx_pti(Ieee802154TxRxScene::Rx);

    set_cmd(Command::RxStart);

    // ieee802154_state = IEEE802154_STATE_RX;
}

fn stop_current_operation_inner(state: &mut IeeeState) {
    event_end_process();
    match state.state {
        Ieee802154State::EnergyDetect => {
            set_cmd(Command::Stop);
            clear_events(Event::EdDone | Event::RxAbort);
            set_ed_sample_mode(EdSampleMode::Avg);
            disable_rx_abort_events(RxAbortReason::EdAbort | RxAbortReason::EdCoexReject);
        }
        Ieee802154State::Idle => {
            set_cmd(Command::Stop);
        }
        Ieee802154State::Receive => {
            stop_rx(state);
        }
        Ieee802154State::TxAck => {
            stop_tx_ack();
        }
        Ieee802154State::Transmit | Ieee802154State::TxEnhAck => {
            stop_tx(state);
        }
        Ieee802154State::RxAck => {
            stop_rx_ack(state);
        }
    }
}

fn stop_rx(state: &mut IeeeState) {
    set_cmd(Command::Stop);

    let evts = events();
    if evts & Event::RxDone != 0 {
        receive_done(state);
    }

    clear_events(Event::RxDone | Event::RxAbort | Event::RxSfdDone);
}

fn stop_tx_ack() {
    set_cmd(Command::Stop);

    // Frame was already copied to queue in isr_handle_rx_done.
    // Don't call receive_done again (that caused the "Receive queue full" bug).

    clear_events(Event::AckTxDone | Event::RxAbort | Event::TxSfdDone);
}

fn stop_tx(state: &mut IeeeState) {
    set_cmd(Command::Stop);

    let evts = events();

    if state.state == Ieee802154State::TxEnhAck {
        // Frame was already copied in isr_handle_rx_done, no need to call receive_done
        clear_events(Event::AckTxDone as u16);
    } else if (evts & Event::TxDone != 0)
        && (!frame_is_ack_required(unsafe {
            core::slice::from_raw_parts(TX_FRAME.add(1), *TX_FRAME as usize)
        }) || !rx_auto_ack())
    {
        // tx is done, no ack needed
        super::tx_done();
    } else {
        super::tx_failed();
    }

    clear_events(Event::TxDone | Event::TxAbort | Event::TxSfdDone);
}

fn stop_rx_ack(state: &mut IeeeState) {
    set_cmd(Command::Stop);

    let evts = events();

    timer0_stop();
    disable_events(Event::Timer0Overflow as u16);

    if evts & Event::AckRxDone != 0 {
        // Capture the received ACK frame
        state.ack_frame = Some(RawReceived {
            data: unsafe { RX_BUFFER.0 },
            channel: freq_to_channel(freq()),
        });
        super::tx_done();
    } else {
        super::tx_failed();
    }

    clear_events(Event::AckRxDone | Event::RxSfdDone | Event::TxAbort);
}

fn receive_done(state: &mut IeeeState) {
    if state.rx_queue.is_full() {
        state.rx_queue.pop_front(); // Evict oldest transport frame; retain newest.
        #[cfg(feature = "test-support")]
        crate::kernel::radio::testing::event(crate::kernel::radio::testing::Event::ReceiveEvicted);
    }
    let item = RawReceived {
        data: unsafe { RX_BUFFER.0 },
        channel: freq_to_channel(freq()),
    };
    let _ = state.rx_queue.push_back(item);
}

fn set_next_rx_buffer() {
    // The aligned wrapper is repr(C) and contains only the DMA byte array.
    set_rx_addr(core::ptr::addr_of_mut!(RX_BUFFER).cast());
}

pub fn set_promiscuous(enable: bool) {
    ieee802154_pib_set_promiscuous(enable);
}

pub fn set_auto_ack_tx(enable: bool) {
    ieee802154_pib_set_auto_ack_tx(enable);
}

pub fn set_auto_ack_rx(enable: bool) {
    ieee802154_pib_set_auto_ack_rx(enable);
}

pub fn set_enhance_ack_tx(enable: bool) {
    ieee802154_pib_set_enhance_ack_tx(enable);
}

pub fn set_coordinator(enable: bool) {
    ieee802154_pib_set_coordinator(enable);
}

pub fn set_rx_when_idle(enable: bool) {
    ieee802154_pib_set_rx_when_idle(enable);
}

pub fn set_tx_power(power: i8) {
    ieee802154_pib_set_tx_power(power);
}

pub fn set_channel(channel: u8) {
    ieee802154_pib_set_channel(channel);
}

#[allow(unused)]
pub fn set_pending_mode(mode: PendingMode) {
    ieee802154_pib_set_pending_mode(mode);
}

#[allow(unused)]
pub fn set_multipan_enable(mask: u8) {
    set_multipan_enable_mask(mask);
}

pub fn set_short_address(index: u8, address: u16) {
    ieee802154_pib_set_short_address(index, address);
}

pub fn set_extended_address(index: u8, address: [u8; IEEE802154_FRAME_EXT_ADDR_SIZE]) {
    ieee802154_pib_set_extended_address(index, address);
}

pub fn set_cca_theshold(cca_threshold: i8) {
    ieee802154_pib_set_cca_theshold(cca_threshold);
}

pub fn set_cca_mode(mode: CcaMode) {
    ieee802154_pib_set_cca_mode(mode);
}

pub fn set_panid(index: u8, id: u16) {
    ieee802154_pib_set_panid(index, id);
}

/// Stop timers and clear security at the start of event processing,
/// matching C driver 5.5.2's event_end_process.
#[inline(always)]
fn event_end_process() {
    set_transmit_security(false);
    timer0_stop();
    // timer1 is not currently used; would be stopped here if we add timed TX/RX
}

fn next_operation_inner(state: &mut IeeeState) {
    // Set state to Idle before dispatching the next operation.
    // This prevents stop_current_operation_inner (called from tx_init)
    // from seeing a stale TxAck state and calling stop_tx_ack, which
    // would duplicate-queue the already-delivered received frame.
    state.state = Ieee802154State::Idle;

    if let Some(pending) = state.pending_tx.take() {
        // Restore RX abort events to normal (matching C driver's next_operation)
        disable_rx_abort_events(RxAbortReason::all());
        enable_rx_abort_events(RxAbortReason::TxAckTimeout | RxAbortReason::TxAckCoexBreak);
        // Clear any stale RX abort events created during deferral
        clear_events(Event::RxAbort as u16);
        transmit_internal(state, pending.frame, pending.cca);
    } else if ieee802154_pib_get_rx_when_idle() {
        enable_rx();
        state.state = Ieee802154State::Receive;
    }
    // else state stays Idle
}

fn next_operation() {
    STATE.with(next_operation_inner);
}

#[handler(priority = Priority::Priority1)]
fn zb_mac_handler() {
    let events = events();
    let rx_abort_reason = get_rx_abort_reason();
    let tx_abort_reason = get_tx_abort_reason();

    clear_events(events);

    let mut needs_next_operation = false;

    // ED shares the MAC interrupt, but is a receive-only operation with its own
    // completion. Do not turn its abort into a packet-receive restart.
    let scanned = STATE.with(|state| {
        if state.state != Ieee802154State::EnergyDetect {
            return false;
        }
        if events & (Event::EdDone | Event::RxAbort) != 0 {
            let result = if events & Event::RxAbort != 0 {
                Err(crate::kernel::radio::Error::Aborted)
            } else {
                Ok(ed_rssi())
            };
            stop_current_operation_inner(state);
            state.state = Ieee802154State::Idle;
            super::energy_done(result);
        }
        true
    });
    if scanned {
        return;
    }

    // First phase RX abort processing (handles RX-state aborts)
    if events & Event::RxAbort != 0 {
        #[cfg(feature = "test-support")]
        crate::kernel::radio::testing::event(crate::kernel::radio::testing::Event::ReceiveAborted(
            rx_abort_reason,
        ));
        isr_handle_rx_phase_rx_abort(rx_abort_reason, &mut needs_next_operation);
    }

    if events & Event::TxDone != 0 {
        isr_handle_tx_done(&mut needs_next_operation);
    }

    if events & Event::RxDone != 0 {
        isr_handle_rx_done(&mut needs_next_operation);
    }

    if events & Event::AckTxDone != 0 {
        isr_handle_ack_tx_done(&mut needs_next_operation);
    }

    if events & Event::AckRxDone != 0 {
        isr_handle_ack_rx_done(&mut needs_next_operation);
    }

    // Second phase RX abort processing (handles TX-ACK-state aborts)
    if events & Event::RxAbort != 0 {
        isr_handle_tx_ack_phase_rx_abort(rx_abort_reason, &mut needs_next_operation);
    }

    if events & Event::TxAbort != 0 {
        isr_handle_tx_abort(tx_abort_reason, &mut needs_next_operation);
    }

    if events & Event::Timer0Overflow != 0 {
        isr_handle_timer0_done(&mut needs_next_operation);
    }

    if needs_next_operation {
        next_operation();
    }
}

/// Handle TX done in ISR - matches C driver's isr_handle_tx_done
fn isr_handle_tx_done(needs_next_op: &mut bool) {
    event_end_process();

    STATE.with(|state| {
        if state.state == Ieee802154State::Transmit {
            let tx_frame = unsafe { TX_FRAME };
            let frame_data =
                unsafe { core::slice::from_raw_parts(tx_frame.add(1), *tx_frame as usize) };

            if frame_is_ack_required(frame_data) && rx_auto_ack() {
                // Hardware changes to RX immediately; bound the ACK wait.
                state.state = Ieee802154State::RxAck;
                receive_ack_timeout_timer_start(ACK_TIMEOUT_US);
                *needs_next_op = false;
            } else {
                // TX complete, no ACK needed
                super::tx_done();
                *needs_next_op = true;
            }
        }
    });
}

/// Handle RX done in ISR - matches C driver's isr_handle_rx_done
fn isr_handle_rx_done(needs_next_op: &mut bool) {
    event_end_process();
    unsafe {
        STATE.with(|state| {
            let frm = if RX_BUFFER.0[0] >= FRAME_SIZE as u8 {
                &RX_BUFFER.0[1..][..FRAME_SIZE - 1]
            } else {
                &RX_BUFFER.0[1..][..RX_BUFFER.0[0] as usize]
            };

            // Always copy RX_BUFFER.0 immediately — we only have one buffer,
            // and the hardware may overwrite it during auto-ACK or later RX.
            // The C driver can defer because it has multiple RX buffers and
            // advances the index in isr_handle_rx_done via next_rx_buffer().
            receive_done(state);

            if will_auto_send_ack(frm) {
                // auto tx ack for frame version 0b00 and 0b01
                // Frame data already copied above. Defer rx_available()
                // notification until ACK completes (isr_handle_ack_tx_done).
                state.state = Ieee802154State::TxAck;
                *needs_next_op = false;
            } else if should_send_enhanced_ack(frm) {
                // Enhanced ACK for frame version 0b10 - TODO: full enh-ack support
                // Frame data already copied above.
                state.state = Ieee802154State::TxEnhAck;
                *needs_next_op = false;
            } else {
                // No ACK needed, notify immediately (data already copied above)
                super::rx_available();
                *needs_next_op = true;
            }
        });
    }
}

/// Handle ACK TX done in ISR - matches C driver's isr_handle_ack_tx_done
fn isr_handle_ack_tx_done(needs_next_op: &mut bool) {
    // Frame was already copied to queue in isr_handle_rx_done (we must copy
    // immediately because we only have one RX buffer). Now notify upper layer.
    super::rx_available();
    *needs_next_op = true;
}

/// Handle ACK RX done in ISR - matches C driver's isr_handle_ack_rx_done
fn isr_handle_ack_rx_done(needs_next_op: &mut bool) {
    timer0_stop();
    disable_events(Event::Timer0Overflow as u16);
    // Capture the received ACK frame before RX buffer is reused
    STATE.with(|state| {
        state.ack_frame = Some(RawReceived {
            data: unsafe { RX_BUFFER.0 },
            channel: freq_to_channel(freq()),
        });
    });
    // ACK received for our transmitted frame
    super::tx_done();
    *needs_next_op = true;
}

/// First phase RX abort - handles aborts while in RX state
/// Matches C driver's isr_handle_rx_phase_rx_abort
fn isr_handle_rx_phase_rx_abort(rx_abort_reason: u32, needs_next_op: &mut bool) {
    event_end_process();
    match rx_abort_reason {
        // Stop reasons - do nothing
        r if r == RxAbortReason::RxStop as u32
            || r == RxAbortReason::TxAckStop as u32
            || r == RxAbortReason::EdStop as u32 => {}

        // RX errors while receiving - just need next operation
        r if r == RxAbortReason::SfdTimeout as u32
            || r == RxAbortReason::CrcError as u32
            || r == RxAbortReason::InvalidLen as u32
            || r == RxAbortReason::FilterFail as u32
            || r == RxAbortReason::NoRss as u32
            || r == RxAbortReason::UnexpectedAck as u32
            || r == RxAbortReason::RxRestart as u32
            || r == RxAbortReason::CoexBreak as u32 =>
        {
            *needs_next_op = true;
        }
        // TX ACK timeout/coex break/enhack error - handled in phase 2
        r if r == RxAbortReason::TxAckTimeout as u32
            || r == RxAbortReason::TxAckCoexBreak as u32
            || r == RxAbortReason::EnhackSecurityError as u32 => {}

        _ => {
            *needs_next_op = true;
        }
    }
}

/// Second phase RX abort - handles aborts while in TX_ACK state
/// Matches C driver's isr_handle_tx_ack_phase_rx_abort
fn isr_handle_tx_ack_phase_rx_abort(rx_abort_reason: u32, needs_next_op: &mut bool) {
    event_end_process();

    match rx_abort_reason {
        // Most abort reasons during TX_ACK phase - do nothing
        r if r == RxAbortReason::TxAckStop as u32
            || r == RxAbortReason::RxStop as u32
            || r == RxAbortReason::EdStop as u32
            || r == RxAbortReason::SfdTimeout as u32
            || r == RxAbortReason::CrcError as u32
            || r == RxAbortReason::InvalidLen as u32
            || r == RxAbortReason::FilterFail as u32
            || r == RxAbortReason::NoRss as u32
            || r == RxAbortReason::UnexpectedAck as u32
            || r == RxAbortReason::RxRestart as u32
            || r == RxAbortReason::CoexBreak as u32
            || r == RxAbortReason::EdAbort as u32
            || r == RxAbortReason::EdCoexReject as u32 => {}

        // TX ACK timeout or coex break while sending ACK - notify upper layer
        r if r == RxAbortReason::TxAckTimeout as u32
            || r == RxAbortReason::TxAckCoexBreak as u32 =>
        {
            // Frame was already copied in isr_handle_rx_done, just notify
            super::rx_available();
            *needs_next_op = true;
        }
        // Enhanced ACK security error - notify upper layer
        r if r == RxAbortReason::EnhackSecurityError as u32 => {
            // Frame was already copied in isr_handle_rx_done, just notify
            super::rx_available();
            *needs_next_op = true;
        }
        _ => {}
    }
}

/// Handle Timer0 overflow (ACK receive timeout) - matches C driver's isr_handle_timer0_done
/// which calls ieee802154_rx_ack_timeout_callback
fn isr_handle_timer0_done(needs_next_op: &mut bool) {
    // Timer0 fired while waiting for ACK - TX failed with no ACK
    timer0_stop();
    disable_events(Event::Timer0Overflow as u16);
    super::tx_no_ack();
    *needs_next_op = true;
}

/// Handle TX abort - matches C driver's isr_handle_tx_abort
fn isr_handle_tx_abort(tx_abort_reason: u32, needs_next_op: &mut bool) {
    event_end_process();

    match tx_abort_reason {
        // RX ACK stop or TX stop - do nothing (handled by stop_current_operation)
        r if r == TxAbortReason::RxAckStop as u32 || r == TxAbortReason::TxStop as u32 => {
            // do nothing
        }
        // RX ACK errors while waiting for ACK - invalid ACK, need next_op
        r if r == TxAbortReason::RxAckSfdTimeout as u32
            || r == TxAbortReason::RxAckCrcError as u32
            || r == TxAbortReason::RxAckInvalidLen as u32
            || r == TxAbortReason::RxAckFilterFail as u32
            || r == TxAbortReason::RxAckNoRss as u32
            || r == TxAbortReason::RxAckCoexBreak as u32
            || r == TxAbortReason::RxAckTypeNotAck as u32
            || r == TxAbortReason::RxAckRestart as u32 =>
        {
            super::tx_no_ack();
            *needs_next_op = true;
        }
        // RX ACK timeout - no ACK received
        r if r == TxAbortReason::RxAckTimeout as u32 => {
            timer0_stop();
            disable_events(Event::Timer0Overflow as u16);
            super::tx_no_ack();
            *needs_next_op = true;
        }
        // TX coex break - notify coex manager
        r if r == TxAbortReason::TxCoexBreak as u32 => {
            unsafe { esp_coex_ieee802154_coex_break_notify() };
            super::tx_failed();
            *needs_next_op = true;
        }
        // TX security error
        r if r == TxAbortReason::TxSecurityError as u32 => {
            super::tx_failed();
            *needs_next_op = true;
        }
        // CCA failed
        r if r == TxAbortReason::CcaFailed as u32 => {
            super::tx_busy();
            *needs_next_op = true;
        }
        // CCA busy
        r if r == TxAbortReason::CcaBusy as u32 => {
            super::tx_busy();
            *needs_next_op = true;
        }
        _ => {}
    }
}

fn freq_to_channel(freq: u8) -> u8 {
    (freq - 3) / 5 + 11
}

fn will_auto_send_ack(frame: &[u8]) -> bool {
    frame_is_ack_required(frame) && frame_get_version(frame) <= FRAME_VERSION_1 && tx_auto_ack()
}

fn should_send_enhanced_ack(frame: &[u8]) -> bool {
    frame_is_ack_required(frame) && frame_get_version(frame) <= FRAME_VERSION_2 && tx_enhance_ack()
}

/// Start the ACK receive timeout timer.
/// Duration is in microseconds.
fn receive_ack_timeout_timer_start(duration: u32) {
    enable_events(Event::Timer0Overflow as u16);
    timer0_set_threshold(duration);
    timer0_start();
}
