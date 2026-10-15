//! Resident adjacent peers and reciprocal link maintenance.
//! Only confirmed relationships are enumerable. Full storage never evicts a
//! healthy relationship to admit a new candidate; pending/expired entries clear.

use super::frame::{LinkPhase, PeerLink};

const CAPACITY: usize = 8;
const EXCHANGE_MS: u32 = 6_000;
const REFRESH_MS: u32 = 15_000;
const EXPIRE_MS: u32 = 60_000;

#[derive(Clone, Copy)]
struct Neighbor {
    peer: u64,
    retained_at: u32, // reservation or most recent successful confirmation
    offered_at: u32,
    offer_token: u32,
    accepted_token: u32,
    accepted_at: u32,
    round_trip_ms: u16,
    established: bool,
    waiting_accept: bool,
}
impl Neighbor {
    const EMPTY: Self = Self {
        peer: 0,
        retained_at: 0,
        offered_at: 0,
        offer_token: 0,
        accepted_token: 0,
        accepted_at: 0,
        round_trip_ms: 0,
        established: false,
        waiting_accept: false,
    };
}

pub(super) struct Links {
    neighbors: [Neighbor; CAPACITY],
    token: u32,
    revision: u32,
}
const _: () = assert!(core::mem::size_of::<Links>() <= 320);

impl Links {
    pub(super) const fn new() -> Self {
        Self {
            neighbors: [Neighbor::EMPTY; CAPACITY],
            token: 0,
            revision: 0,
        }
    }

    fn reserve(&mut self, peer: u64, now: u32) -> Option<usize> {
        if peer == 0 || peer == u64::MAX {
            return None;
        }
        if let Some(i) = self.neighbors.iter().position(|n| n.peer == peer) {
            return Some(i);
        }
        let i = self.neighbors.iter().position(|n| n.peer == 0)?;
        self.neighbors[i] = Neighbor {
            peer,
            retained_at: now,
            // Give a new candidate its first maintenance opportunity promptly.
            offered_at: now.wrapping_sub(REFRESH_MS),
            ..Neighbor::EMPTY
        };
        Some(i)
    }

    /// Presence is a candidate, not evidence of reciprocal retention.
    pub(super) fn record_presence(&mut self, peer: u64, now: u32) {
        self.reserve(peer, now);
    }

    pub(super) fn contains(&self, peer: u64) -> bool {
        self.neighbors
            .iter()
            .any(|n| n.peer == peer && n.established)
    }

    /// One complete relationship after the caller's peer-address cursor.
    /// Revision changes on membership changes, not on updated timing samples.
    pub(super) fn read(&self, after: u64) -> (u32, Option<PeerLink>) {
        let link = self
            .neighbors
            .iter()
            .filter(|n| n.established && n.peer > after)
            .min_by_key(|n| n.peer)
            .map(|n| PeerLink {
                peer: n.peer,
                round_trip_ms: n.round_trip_ms,
            });
        (self.revision, link)
    }

    pub(super) fn expire(&mut self, now: u32, mut lost: impl FnMut(u64)) {
        for neighbor in &mut self.neighbors {
            if neighbor.peer == 0 {
                continue;
            }
            if now.wrapping_sub(neighbor.retained_at) >= EXPIRE_MS {
                if neighbor.established {
                    lost(neighbor.peer);
                    self.revision = self.revision.wrapping_add(1);
                }
                *neighbor = Neighbor::EMPTY;
            } else {
                if now.wrapping_sub(neighbor.offered_at) >= EXCHANGE_MS {
                    neighbor.offer_token = 0;
                    neighbor.waiting_accept = false;
                }
                if now.wrapping_sub(neighbor.accepted_at) >= EXCHANGE_MS {
                    neighbor.accepted_token = 0;
                }
            }
        }
    }

    /// Called on the existing discovery interval. At most one link exchange is
    /// initiated per interval; no graph work or additional task is scheduled.
    pub(super) fn maintain(&mut self, now: u32) -> Option<(u64, u32)> {
        let i = self
            .neighbors
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                n.peer != 0
                    && !n.waiting_accept
                    && n.accepted_token == 0
                    && (!n.established || now.wrapping_sub(n.retained_at) >= REFRESH_MS)
            })
            .max_by_key(|(_, n)| now.wrapping_sub(n.offered_at))
            .map(|(i, _)| i)?;
        self.token = self.token.wrapping_add(1);
        if self.token == 0 {
            self.token = 1;
        }
        let n = &mut self.neighbors[i];
        n.offered_at = now;
        n.offer_token = self.token;
        n.waiting_accept = true;
        Some((n.peer, n.offer_token))
    }

    /// The caller has validated the network and one-hop addressed envelope.
    /// Offer -> Accept -> Confirm terminates without echoing confirmations.
    pub(super) fn receive(
        &mut self,
        peer: u64,
        phase: LinkPhase,
        token: u32,
        now: u32,
    ) -> Option<LinkPhase> {
        if token == 0 {
            return None;
        }
        if phase == LinkPhase::Offer {
            let Some(i) = self.reserve(peer, now) else {
                return Some(LinkPhase::Reject);
            };
            let n = &mut self.neighbors[i];
            if n.accepted_token != token {
                n.accepted_token = token;
                n.accepted_at = now;
            }
            return Some(LinkPhase::Accept);
        }
        let n = self.neighbors.iter_mut().find(|n| n.peer == peer)?;
        match phase {
            LinkPhase::Accept
                if n.offer_token == token && now.wrapping_sub(n.offered_at) < EXCHANGE_MS =>
            {
                if n.waiting_accept {
                    if !n.established {
                        self.revision = self.revision.wrapping_add(1);
                    }
                    n.established = true;
                    n.retained_at = now;
                    n.waiting_accept = false;
                    n.round_trip_ms =
                        now.wrapping_sub(n.offered_at).clamp(1, u16::MAX as u32) as u16;
                }
                // A duplicate Accept can recover a lost Confirm, without renewing
                // the relationship indefinitely from a stale response.
                Some(LinkPhase::Confirm)
            }
            LinkPhase::Confirm
                if n.accepted_token == token && now.wrapping_sub(n.accepted_at) < EXCHANGE_MS =>
            {
                if !n.established {
                    self.revision = self.revision.wrapping_add(1);
                }
                n.established = true;
                n.retained_at = now;
                n.round_trip_ms = now.wrapping_sub(n.accepted_at).clamp(1, u16::MAX as u32) as u16;
                n.accepted_token = 0;
                None
            }
            LinkPhase::Reject
                if n.offer_token == token
                    && n.waiting_accept
                    && now.wrapping_sub(n.offered_at) < EXCHANGE_MS =>
            {
                if n.established {
                    self.revision = self.revision.wrapping_add(1);
                }
                *n = Neighbor::EMPTY;
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(a: &mut Links, b: &mut Links, now: u32) {
        a.record_presence(2, now);
        let (peer, token) = a.maintain(now).unwrap();
        assert_eq!(peer, 2);
        assert_eq!(
            b.receive(1, LinkPhase::Offer, token, now + 1),
            Some(LinkPhase::Accept)
        );
        assert_eq!(
            a.receive(2, LinkPhase::Accept, token, now + 2),
            Some(LinkPhase::Confirm)
        );
        assert_eq!(b.receive(1, LinkPhase::Confirm, token, now + 3), None);
    }

    #[test]
    fn only_correlated_exchanges_establish_links_and_lost_confirmation_can_recover() {
        let mut a = Links::new();
        let mut b = Links::new();
        a.record_presence(2, 0);
        assert_eq!(a.read(0), (0, None));
        assert_eq!(a.receive(2, LinkPhase::Accept, 91, 0), None);
        let (_, token) = a.maintain(0).unwrap();
        assert_eq!(a.receive(3, LinkPhase::Accept, token, 1), None);
        assert_eq!(a.receive(2, LinkPhase::Accept, token + 1, 1), None);
        assert_eq!(
            b.receive(1, LinkPhase::Offer, token, 1),
            Some(LinkPhase::Accept)
        );
        assert_eq!(b.read(0), (0, None));
        assert_eq!(b.receive(1, LinkPhase::Confirm, token + 1, 2), None);
        assert_eq!(
            a.receive(2, LinkPhase::Accept, token, 2),
            Some(LinkPhase::Confirm)
        );
        // Lose this Confirm. Only the initiator currently lists the link.
        assert!(a.contains(2));
        assert!(!b.contains(1));
        assert_eq!(
            a.receive(2, LinkPhase::Reject, token, 3),
            None,
            "late rejection erased an established link"
        );
        assert_eq!(
            a.receive(2, LinkPhase::Accept, token, 4),
            Some(LinkPhase::Confirm)
        );
        assert_eq!(b.receive(1, LinkPhase::Confirm, token, 5), None);
        assert!(b.contains(1));
        assert_eq!(
            a.read(0),
            (
                1,
                Some(PeerLink {
                    peer: 2,
                    round_trip_ms: 2
                })
            )
        );
        assert_eq!(
            b.read(0),
            (
                1,
                Some(PeerLink {
                    peer: 1,
                    round_trip_ms: 4
                })
            )
        );
        assert_eq!(b.receive(1, LinkPhase::Confirm, token, 6), None);
        a.expire(EXCHANGE_MS, |_| panic!());
        assert_eq!(
            a.receive(2, LinkPhase::Accept, token, EXCHANGE_MS + 1),
            None
        );
        assert_eq!(a.read(2), (1, None));
        let mut left = Links::new();
        let mut right = Links::new();
        left.record_presence(2, 0);
        let (_, cookie) = left.maintain(0).unwrap();
        right.receive(1, LinkPhase::Offer, cookie, 1).unwrap();
        left.receive(2, LinkPhase::Accept, cookie, 2).unwrap(); // Drop Confirm.
        right.expire(EXCHANGE_MS + 1, |_| panic!());
        let (_, repair) = right.maintain(EXCHANGE_MS + 1).unwrap();
        assert_eq!(
            left.receive(2, LinkPhase::Offer, repair, EXCHANGE_MS + 2),
            Some(LinkPhase::Accept)
        );
        assert_eq!(
            right.receive(1, LinkPhase::Accept, repair, EXCHANGE_MS + 3),
            Some(LinkPhase::Confirm)
        );
        left.receive(2, LinkPhase::Confirm, repair, EXCHANGE_MS + 4);
        assert!(left.contains(2) && right.contains(1));
        // Broadcast presence does not keep an otherwise dead link alive.
        a.record_presence(2, EXPIRE_MS);
        let mut lost = 0;
        a.expire(EXPIRE_MS + 2, |peer| {
            assert_eq!(peer, 2);
            lost += 1;
        });
        assert_eq!((lost, a.read(0)), (1, (2, None)));
        a.record_presence(2, EXPIRE_MS + 3);
        assert_ne!(a.maintain(EXPIRE_MS + 3).unwrap().1, token);
    }

    #[test]
    fn simultaneous_open_and_refresh_terminate_with_bounded_expiry() {
        let mut a = Links::new();
        let mut b = Links::new();
        a.record_presence(2, 0);
        b.record_presence(1, 0);
        let (_, at) = a.maintain(0).unwrap();
        let (_, bt) = b.maintain(0).unwrap();
        assert_eq!(
            b.receive(1, LinkPhase::Offer, at, 1),
            Some(LinkPhase::Accept)
        );
        assert_eq!(
            a.receive(2, LinkPhase::Offer, bt, 1),
            Some(LinkPhase::Accept)
        );
        assert_eq!(
            a.receive(2, LinkPhase::Accept, at, 2),
            Some(LinkPhase::Confirm)
        );
        assert_eq!(
            b.receive(1, LinkPhase::Accept, bt, 2),
            Some(LinkPhase::Confirm)
        );
        assert_eq!(a.receive(2, LinkPhase::Confirm, bt, 3), None);
        assert_eq!(b.receive(1, LinkPhase::Confirm, at, 3), None);
        assert_eq!((a.read(0).0, b.read(0).0), (1, 1));
        assert!(a.maintain(REFRESH_MS).is_none());
        a.expire(REFRESH_MS + 3, |_| panic!());
        let (_, fresh) = a.maintain(REFRESH_MS + 3).unwrap();
        assert_ne!(fresh, at);
        assert_eq!(
            b.receive(1, LinkPhase::Offer, fresh, REFRESH_MS + 4),
            Some(LinkPhase::Accept)
        );
        assert_eq!(
            a.receive(2, LinkPhase::Accept, fresh, REFRESH_MS + 5),
            Some(LinkPhase::Confirm)
        );
        assert_eq!(
            b.receive(1, LinkPhase::Confirm, fresh, REFRESH_MS + 6),
            None
        );
        assert_eq!(
            (a.read(0).0, b.read(0).0),
            (1, 1),
            "refresh changed membership revision"
        );
        assert!(a.maintain(REFRESH_MS + 7).is_none());
        a.expire(REFRESH_MS + EXPIRE_MS + 5, |peer| assert_eq!(peer, 2));
        b.expire(REFRESH_MS + EXPIRE_MS + 6, |peer| assert_eq!(peer, 1));
        assert!(!a.contains(2));
        assert!(!b.contains(1));
        // Wrapping clocks preserve an exchange and its eventual expiry.
        let mut a = Links::new();
        let mut b = Links::new();
        let start = u32::MAX - 10;
        connected(&mut a, &mut b, start);
        a.expire(10, |_| panic!());
        assert!(a.contains(2));
        a.expire(start.wrapping_add(EXPIRE_MS + 2), |peer| {
            assert_eq!(peer, 2)
        });
        assert!(!a.contains(2));
    }

    #[test]
    fn full_capacity_preserves_live_links_and_unconfirmed_candidates_expire() {
        let mut links = Links::new();
        for peer in (1..=CAPACITY as u64).rev() {
            assert_eq!(
                links.receive(peer, LinkPhase::Offer, peer as u32, 0),
                Some(LinkPhase::Accept)
            );
            assert_eq!(
                links.receive(peer, LinkPhase::Confirm, peer as u32, 1),
                None
            );
        }
        assert_eq!(
            links.receive(99, LinkPhase::Offer, 99, 2),
            Some(LinkPhase::Reject)
        );
        links.record_presence(99, 2);
        for cursor in 0..CAPACITY as u64 {
            assert_eq!(
                links.read(cursor),
                (
                    CAPACITY as u32,
                    Some(PeerLink {
                        peer: cursor + 1,
                        round_trip_ms: 1
                    })
                )
            );
        }
        assert_eq!(links.read(u64::MAX), (CAPACITY as u32, None));
        let mut candidate = Links::new();
        candidate.record_presence(99, 0);
        let (_, token) = candidate.maintain(0).unwrap();
        assert_eq!(candidate.receive(99, LinkPhase::Reject, token, 1), None);
        assert!(candidate.maintain(2).is_none());
        for peer in 1..=CAPACITY as u64 {
            candidate.record_presence(peer, 0);
        }
        assert_eq!(candidate.read(0), (0, None));
        assert!(candidate.maintain(0).is_some());
        candidate.expire(EXPIRE_MS, |_| panic!("candidate was advertised as active"));
        assert!(candidate.maintain(EXPIRE_MS).is_none());
        assert_eq!(
            candidate.receive(99, LinkPhase::Offer, 99, EXPIRE_MS + 1),
            Some(LinkPhase::Accept)
        );
    }
}
