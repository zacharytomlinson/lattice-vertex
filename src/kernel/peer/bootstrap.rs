//! Bounded channel surveys and local formation before ordinary participation.
//! All search storage is fixed and local to the resident radio task's stack.

use super::{frame::Content, Network};
#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
use crate::kernel::radio::cost::TimePerByte;

const CHANNELS: usize = 16;
const SURVEYS: u8 = 2;
const PRIME: u32 = 17;
const ROUND_SLOTS: u32 = 4 * PRIME;
const FORMATION_SLOTS: u32 = 3 * ROUND_SLOTS;
const SLOT_MS: u64 = 40;

/// Three prime-stride sweeps followed by a stay on this node's preferred channel.
/// A different home gives a different nonzero stride modulo 17; equal homes
/// give a common stay channel. Advance the start in each round. The virtual
/// seventeenth channel maps to home, never to a fixed physical rendezvous channel.
/// This is the small jump/stay construction described in the hardware notes;
/// overlap opportunities do not guarantee successful packets under interference.
fn rendezvous_channel(home: u8, start: u32, slot: u32) -> u8 {
    let phase = slot % ROUND_SLOTS;
    if phase >= 3 * PRIME {
        return home;
    }
    let index = (start + slot / ROUND_SLOTS + phase * u32::from(home - 10)) % PRIME;
    if index == CHANNELS as u32 {
        home
    } else {
        11 + index as u8
    }
}

struct Survey {
    peak: [i8; CHANNELS],
    samples: [u8; CHANNELS],
}

impl Survey {
    const fn new() -> Self {
        Self {
            peak: [i8::MIN; CHANNELS],
            samples: [0; CHANNELS],
        }
    }

    fn record(&mut self, channel: u8, result: Option<i8>) {
        if let Some(dbm) = result {
            let i = usize::from(channel - 11);
            self.peak[i] = self.peak[i].max(dbm);
            self.samples[i] = self.samples[i].saturating_add(1);
        }
    }

    fn quietest(&self, start: usize) -> Option<u8> {
        // Missing/failed measurements are never interpreted as quiet. Rotating
        // the search breaks equal-energy ties without always crowding channel 11.
        (0..CHANNELS)
            .map(|n| (n + start) % CHANNELS)
            .filter(|&i| self.samples[i] >= SURVEYS)
            .min_by_key(|&i| self.peak[i])
            .map(|i| i as u8 + 11)
    }
}

fn retain_proposal(current: &mut Option<Network>, proposed: Network) {
    if current.is_none_or(|old| (proposed.founder, proposed.channel) < (old.founder, old.channel)) {
        *current = Some(proposed);
    }
}

/// Established residents take precedence over uncommitted proposals, regardless
/// of address order. Proposal ties use the same order on every participating MCU.
fn consider(content: Content<'_>, proposal: &mut Option<Network>) -> Option<Network> {
    match content {
        Content::Present(network) | Content::Arrive { network, .. } => Some(network),
        Content::Propose(network) => {
            retain_proposal(proposal, network);
            None
        }
        Content::Discover
        | Content::Stream(_)
        | Content::Program { .. }
        | Content::ProgramResult { .. }
        | Content::ReadSurface { .. }
        | Content::SurfacePart { .. }
        | Content::FindOwners { .. }
        | Content::OwnerFound { .. }
        | Content::Invoke { .. }
        | Content::InvocationResult { .. }
        | Content::StateChanged { .. }
        | Content::ReadState { .. }
        | Content::StateValue { .. }
        | Content::LinkExchange { .. }
        | Content::ReadLinks { .. }
        | Content::LinkEntry { .. }
        | Content::UpdateAbsence { .. }
        | Content::Update { .. }
        | Content::Updated { .. } => None,
    }
}

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub(super) use runtime::join_or_form;

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
mod runtime {
    use super::super::{discovery_response, receive, send, Message, PAN_ID};
    use super::*;
    use crate::{
        kernel::{
            radio::{task::Transport, MAX_FRAME_SIZE},
            tasks,
        },
        milliseconds_to_ticks_checked as ticks,
    };

    // Timing decorrelation only; this PRNG is never used for keys or credentials.
    fn jitter(seed: &mut u32, limit: u32) -> u32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        *seed % limit
    }

    fn pause(ms: u32) {
        let context = unsafe { crate::sync::TaskContext::assume() };
        crate::delay_ms(&context, ms);
    }

    /// Sweep all 16 channels, measuring before transmitting on each one. A
    /// request plus a bounded receive dwell discovers even a quiet resident.
    fn sweep(
        radio: &mut Transport,
        local: u64,
        sequence: &mut u8,
        buffer: &mut [u8; MAX_FRAME_SIZE],
        survey: &mut Survey,
        proposal: &mut Option<Network>,
        seed: &mut u32,
    ) -> Option<Network> {
        let first = jitter(seed, CHANNELS as u32) as usize;
        for offset in 0..CHANNELS {
            let channel = 11 + ((first + offset) % CHANNELS) as u8;
            radio.configure(channel, PAN_ID).unwrap();
            let sample = radio.energy_detect(4096).ok();
            survey.record(channel, sample);
            #[cfg(feature = "test-support")]
            crate::kernel::radio::testing::event(
                crate::kernel::radio::testing::Event::ChannelSample {
                    channel,
                    dbm: sample,
                },
            );
            send(
                radio,
                Message {
                    path_cost: TimePerByte::UNKNOWN,
                    source: local,
                    destination: None,
                    content: Content::Discover,
                },
                sequence,
                buffer,
            );
            let start = tasks::xTaskGetTickCount();
            let dwell = ticks(u64::from(60 + jitter(seed, 31))).unwrap();
            loop {
                let remaining =
                    dwell.saturating_sub(tasks::xTaskGetTickCount().wrapping_sub(start));
                if remaining == 0 {
                    break;
                }
                if let Some(message) = receive(radio, local, channel, buffer, remaining) {
                    if let Some(network) = consider(message.content, proposal) {
                        #[cfg(feature = "test-support")]
                        crate::kernel::radio::testing::event(
                            crate::kernel::radio::testing::Event::DiscoveryResponseReceived(
                                message.source,
                            ),
                        );
                        return Some(network);
                    }
                }
            }
        }
        None
    }

    fn selected(radio: &mut Transport, network: Network, joined: bool) -> Network {
        radio.configure(network.channel, PAN_ID).unwrap();
        #[cfg(feature = "test-support")]
        crate::kernel::radio::testing::event(
            crate::kernel::radio::testing::Event::NetworkSelected {
                founder: network.founder,
                channel: network.channel,
                joined,
            },
        );
        let _ = joined;
        network
    }

    pub(in super::super) fn join_or_form(
        radio: &mut Transport,
        local: u64,
        sequence: &mut u8,
        buffer: &mut [u8; MAX_FRAME_SIZE],
    ) -> Network {
        #[cfg(feature = "test-support")]
        crate::kernel::radio::testing::event(
            crate::kernel::radio::testing::Event::DiscoveryStarted {
                address: local,
                channel: 0,
                pan_id: PAN_ID,
            },
        );
        let mut seed =
            (local as u32 ^ (local >> 32) as u32 ^ tasks::xTaskGetTickCount() as u32) | 1;
        pause(jitter(&mut seed, 501));
        let mut survey = Survey::new();
        let mut proposal = None;
        let candidate = loop {
            for _ in 0..SURVEYS {
                if let Some(network) = sweep(
                    radio,
                    local,
                    sequence,
                    buffer,
                    &mut survey,
                    &mut proposal,
                    &mut seed,
                ) {
                    return selected(radio, network, true);
                }
            }
            if let Some(channel) = survey.quietest(jitter(&mut seed, CHANNELS as u32) as usize) {
                retain_proposal(
                    &mut proposal,
                    Network {
                        founder: local,
                        channel,
                    },
                );
            }
            if let Some(network) = proposal {
                break network;
            }
            // No valid ED measurements or peer proposals: retry the survey,
            // rather than silently falling back to an unmeasured fixed channel.
            pause(250 + jitter(&mut seed, 251));
        };

        let start = tasks::xTaskGetTickCount();
        let slot_ticks = ticks(SLOT_MS).unwrap();
        let sequence_start = jitter(&mut seed, PRIME);
        let mut competing = Some(candidate);
        let mut previous_slot = u32::MAX;
        let mut channel = 0;
        let mut next_send = 0;
        let mut sent_in_slot = 0;
        loop {
            let elapsed = tasks::xTaskGetTickCount().wrapping_sub(start);
            let slot = (elapsed / slot_ticks) as u32;
            if slot >= FORMATION_SLOTS {
                break;
            }
            if slot != previous_slot {
                previous_slot = slot;
                let next_channel = rendezvous_channel(candidate.channel, sequence_start, slot);
                if next_channel != channel {
                    radio.configure(next_channel, PAN_ID).unwrap();
                    channel = next_channel;
                }
                sent_in_slot = 0;
                next_send = slot_ticks * slot + ticks(u64::from(2 + jitter(&mut seed, 6))).unwrap();
            }
            if elapsed >= next_send && sent_in_slot < 2 {
                send(
                    radio,
                    Message {
                        path_cost: TimePerByte::UNKNOWN,
                        source: local,
                        destination: None,
                        content: Content::Propose(competing.unwrap()),
                    },
                    sequence,
                    buffer,
                );
                sent_in_slot += 1;
                next_send =
                    slot_ticks * slot + ticks(u64::from(22 + jitter(&mut seed, 6))).unwrap();
            }
            let elapsed = tasks::xTaskGetTickCount().wrapping_sub(start);
            let slot_end = slot_ticks * (slot + 1);
            let deadline = if sent_in_slot < 2 {
                next_send.min(slot_end)
            } else {
                slot_end
            };
            if let Some(message) = receive(
                radio,
                local,
                channel,
                buffer,
                deadline.saturating_sub(elapsed),
            ) {
                if let Some(network) = consider(message.content, &mut competing) {
                    return selected(radio, network, true);
                }
                let preferred = competing.unwrap();
                // A directed proposal confirms what that peer selected after
                // hearing us. Settle only if it agrees with our preferred record.
                if message.destination == Some(local)
                    && message.content == Content::Propose(preferred)
                {
                    #[cfg(feature = "test-support")]
                    crate::kernel::radio::testing::event(
                        crate::kernel::radio::testing::Event::ProposalAgreed { channel },
                    );
                    return selected(radio, preferred, false);
                }
                if let Some(reply) = discovery_response(local, &message, preferred, false) {
                    send(radio, reply, sequence, buffer);
                }
            }
        }
        selected(radio, competing.unwrap(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendezvous_crosses_phases_without_requiring_any_fixed_channel() {
        // Finite schedule arithmetic, not an RTOS/RF simulation. Exercise every
        // pair of preferred channels, every peer start index and whole-slot
        // phase. Ignore one occupied channel other than either selected home.
        // Fractional-slot timing, packet loss and oscillator drift remain RF
        // concerns; shared-channel opportunities alone do not prove delivery.
        for a in 11..=26 {
            let left: [u8; FORMATION_SLOTS as usize] =
                core::array::from_fn(|i| rendezvous_channel(a, 0, i as u32));
            for b in 11..=26 {
                for start in 0..PRIME {
                    let right: [u8; (FORMATION_SLOTS + ROUND_SLOTS) as usize] =
                        core::array::from_fn(|i| rendezvous_channel(b, start, i as u32));
                    for phase in 0..ROUND_SLOTS as usize {
                        let mut overlap = 0u16;
                        for i in 0..FORMATION_SLOTS as usize {
                            if left[i] == right[i + phase] {
                                overlap |= 1 << (left[i] - 11);
                            }
                            if overlap.count_ones() > 1 {
                                break;
                            }
                        }
                        assert_ne!(overlap, 0, "homes {a}/{b}, start {start}, phase {phase}");
                        for blocked in 11..=26 {
                            if blocked != a && blocked != b {
                                assert_ne!(overlap & !(1 << (blocked - 11)), 0,
                                    "blocked {blocked}, homes {a}/{b}, start {start}, phase {phase}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn surveys_require_measurements_and_penalize_intermittent_interference() {
        let mut survey = Survey::new();
        assert_eq!(survey.quietest(0), None);
        for (channel, readings) in [(11, [-90, -40]), (15, [-80, -80]), (23, [-85, -85])] {
            for reading in readings {
                survey.record(channel, Some(reading));
            }
        }
        survey.record(26, Some(-100));
        survey.record(26, None);
        assert_eq!(
            survey.quietest(0),
            Some(23),
            "a missing second sample must not beat a measured channel"
        );
        survey.record(26, Some(-100));
        assert_eq!(survey.quietest(0), Some(26));
        let mut equal = Survey::new();
        for channel in 11..=26 {
            for _ in 0..SURVEYS {
                equal.record(channel, Some(-92));
            }
        }
        for start in 0..CHANNELS {
            assert_eq!(equal.quietest(start), Some(11 + start as u8));
        }
    }

    #[test]
    fn established_network_wins_and_proposals_converge_independently_of_order() {
        let a = Network {
            founder: 1,
            channel: 23,
        };
        let b = Network {
            founder: 2,
            channel: 15,
        };
        for proposals in [[a, b], [b, a]] {
            let mut retained = None;
            for n in proposals {
                assert_eq!(consider(Content::Propose(n), &mut retained), None);
            }
            assert_eq!(retained, Some(a));
            assert_eq!(
                consider(Content::Present(b), &mut retained),
                Some(b),
                "resident must beat a lower-address candidate"
            );
            assert_eq!(consider(Content::Discover, &mut retained), None);
        }
    }
}
