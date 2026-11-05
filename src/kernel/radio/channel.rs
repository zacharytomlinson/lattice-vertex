//! Admission for logical channels over the shared transport.
//! Codecs identify updates; storage owns acceptance evidence and payload bytes.
//! Policies have no tasks, clocks, or per-message-type execution behavior.

use core::marker::PhantomData;

pub(crate) trait Policies {
    const UNIQUE: bool = false;
    const TEMPORAL: bool = false;
    const COMPLETE: bool = false;
}
pub(crate) struct Unique;
pub(crate) struct Temporal;
pub(crate) struct Complete;
impl Policies for Unique {
    const UNIQUE: bool = true;
}
impl Policies for Temporal {
    const TEMPORAL: bool = true;
}
impl Policies for Complete {
    const COMPLETE: bool = true;
}
impl<A: Policies, B: Policies> Policies for (A, B) {
    const UNIQUE: bool = A::UNIQUE || B::UNIQUE;
    const TEMPORAL: bool = A::TEMPORAL || B::TEMPORAL;
    const COMPLETE: bool = A::COMPLETE || B::COMPLETE;
}
impl<A: Policies, B: Policies, C: Policies> Policies for (A, B, C) {
    const UNIQUE: bool = A::UNIQUE || B::UNIQUE || C::UNIQUE;
    const TEMPORAL: bool = A::TEMPORAL || B::TEMPORAL || C::TEMPORAL;
    const COMPLETE: bool = A::COMPLETE || B::COMPLETE || C::COMPLETE;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Invalid,
    Capacity,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Received {
    Accepted,
    Buffered,
    Duplicate,
    Stale,
}

#[derive(Clone, Copy)]
pub(crate) struct Metadata {
    pub sequence: Option<u32>,
    pub start: u32,
    pub end: u32,
}
impl Metadata {
    pub fn update(sequence: u32) -> Self {
        Self {
            sequence: Some(sequence),
            start: 0,
            end: 1,
        }
    }
    pub fn piece(offset: u32, length: usize) -> Result<Self, Error> {
        let length = u32::try_from(length).map_err(|_| Error::Invalid)?;
        let end = offset.checked_add(length).ok_or(Error::Invalid)?;
        if length == 0 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            sequence: None,
            start: offset,
            end,
        })
    }
}

/// The wire codec supplies an already decoded message and its policy metadata.
/// Channel admission does not decode it a second time.
pub(crate) trait Definition {
    const HAS_SEQUENCE: bool;
    type Message<'a>;
    fn metadata(message: &Self::Message<'_>) -> Result<Metadata, Error>;
}
pub(crate) trait ValueDefinition: Definition {
    type Value: Copy;
    fn value(message: &Self::Message<'_>) -> Self::Value;
}
pub(crate) trait BytesDefinition: Definition {
    fn bytes<'a, 'm>(message: &'m Self::Message<'a>) -> &'m [u8];
}

/// Common decoded shapes supplied by the wire codecs after binding validation.
pub(crate) struct Update<T>(PhantomData<T>);
impl<T: Copy> Definition for Update<T> {
    const HAS_SEQUENCE: bool = true;
    type Message<'a> = (u32, T);
    fn metadata(message: &Self::Message<'_>) -> Result<Metadata, Error> {
        Ok(Metadata::update(message.0))
    }
}
impl<T: Copy> ValueDefinition for Update<T> {
    type Value = T;
    fn value(message: &Self::Message<'_>) -> T {
        message.1
    }
}
pub(crate) struct Piece;
impl Definition for Piece {
    const HAS_SEQUENCE: bool = false;
    type Message<'a> = (u32, &'a [u8]);
    fn metadata(message: &Self::Message<'_>) -> Result<Metadata, Error> {
        Metadata::piece(message.0, message.1.len())
    }
}
impl BytesDefinition for Piece {
    fn bytes<'a, 'm>(message: &'m Self::Message<'a>) -> &'m [u8] {
        message.1
    }
}

/// A storage implementation must commit evidence only after owning the data.
pub(crate) trait Storage<D: Definition> {
    const HAS_SEQUENCE: bool = false;
    fn sequence(&self) -> Option<u32> {
        None
    }
    fn contains(&self, metadata: Metadata) -> bool;
    fn put(&mut self, message: &D::Message<'_>, metadata: Metadata) -> Result<(), Error>;
    fn complete(&self) -> bool {
        true
    }
}

pub(crate) struct Channel<D, P, S> {
    pub storage: S,
    marker: PhantomData<(D, P)>,
}
impl<D, P, S: Copy> Copy for Channel<D, P, S> {}
impl<D, P, S: Clone> Clone for Channel<D, P, S> {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            marker: PhantomData,
        }
    }
}
impl<D: Definition, P: Policies, S: Storage<D>> Channel<D, P, S> {
    pub const fn new(storage: S) -> Self {
        assert!(
            !P::TEMPORAL || (D::HAS_SEQUENCE && S::HAS_SEQUENCE),
            "Temporal requires a logical update sequence and retained acceptance"
        );
        Self {
            storage,
            marker: PhantomData,
        }
    }
    /// Inspect without advancing evidence, allowing the owner to check capacity
    /// or lifecycle constraints before accepting a new message.
    pub fn inspect(&self, message: &D::Message<'_>) -> Result<Received, Error> {
        let metadata = D::metadata(message)?;
        if P::UNIQUE && self.storage.contains(metadata) {
            return Ok(Received::Duplicate);
        }
        if P::TEMPORAL {
            let sequence = metadata.sequence.ok_or(Error::Invalid)?;
            if self
                .storage
                .sequence()
                .is_some_and(|old| !newer(sequence, old))
            {
                return Ok(Received::Stale);
            }
        }
        Ok(Received::Accepted)
    }
    pub fn receive(&mut self, message: &D::Message<'_>) -> Result<Received, Error> {
        let result = self.inspect(message)?;
        if result != Received::Accepted {
            return Ok(result);
        }
        self.storage.put(message, D::metadata(message)?)?;
        Ok(if P::COMPLETE && !self.storage.complete() {
            Received::Buffered
        } else {
            Received::Accepted
        })
    }
}

/// Serial-number comparison; one established stream cannot span an ambiguous
/// half-range. Re-establishment resets the channel's acceptance evidence.
pub(crate) fn newer(sequence: u32, previous: u32) -> bool {
    let distance = sequence.wrapping_sub(previous);
    distance != 0 && distance < (1 << 31)
}

#[derive(Clone, Copy)]
pub(crate) struct Latest<T: Copy> {
    pub current: Option<(u32, T)>,
}
impl<T: Copy> Latest<T> {
    pub const fn new() -> Self {
        Self { current: None }
    }
}
impl<D: ValueDefinition> Storage<D> for Latest<D::Value> {
    const HAS_SEQUENCE: bool = true;
    fn sequence(&self) -> Option<u32> {
        self.current.map(|v| v.0)
    }
    fn contains(&self, metadata: Metadata) -> bool {
        self.current.is_some_and(|v| Some(v.0) == metadata.sequence)
    }
    fn put(&mut self, message: &D::Message<'_>, metadata: Metadata) -> Result<(), Error> {
        let sequence = metadata.sequence.ok_or(Error::Invalid)?;
        self.current = Some((sequence, D::value(message)));
        Ok(())
    }
}
impl<D: ValueDefinition, P: Policies> Channel<D, P, Latest<D::Value>> {
    pub fn current(&self) -> Option<(u32, D::Value)> {
        self.storage.current
    }
}

/// One bit per stored byte. The same map supplies Unique and Complete evidence;
/// no message history or second payload buffer is allocated.
#[derive(Clone, Copy)]
pub(crate) struct Coverage<const N: usize> {
    bytes: [u8; N],
}
impl<const N: usize> Coverage<N> {
    pub const fn new() -> Self {
        Self { bytes: [0; N] }
    }
    pub fn clear(&mut self) {
        self.bytes.fill(0);
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.iter().all(|&byte| byte == 0)
    }
    fn has(&self, index: usize) -> bool {
        index < N * 8 && self.bytes[index / 8] & (1 << (index % 8)) != 0
    }
    pub fn contains(&self, start: usize, end: usize) -> bool {
        start <= end && end <= N * 8 && (start..end).all(|i| self.has(i))
    }
    pub fn mark(&mut self, start: usize, end: usize) -> Result<(), Error> {
        if start > end || end > N * 8 {
            return Err(Error::Capacity);
        }
        for i in start..end {
            self.bytes[i / 8] |= 1 << (i % 8);
        }
        Ok(())
    }
    pub fn prefix(&self, length: usize) -> usize {
        (0..length.min(N * 8))
            .find(|&i| !self.has(i))
            .unwrap_or(length.min(N * 8))
    }
    /// Drop a committed prefix when a bounded assembly window advances.
    pub fn discard_prefix(&mut self, count: usize) {
        let whole = count / 8;
        let shift = count % 8;
        for i in 0..N {
            let at = i.saturating_add(whole);
            let low = self.bytes.get(at).copied().unwrap_or(0);
            let high = self.bytes.get(at.saturating_add(1)).copied().unwrap_or(0);
            self.bytes[i] = (low >> shift) | if shift == 0 { 0 } else { high << (8 - shift) };
        }
    }
}

/// A whole small object or a bounded window over a larger object. Positions below
/// base have already been committed by the owner and cannot be delivered again.
pub(crate) struct Bytes<'a, const N: usize> {
    pub data: &'a mut [u8],
    pub coverage: &'a mut Coverage<N>,
    pub base: u32,
    pub total: u32,
}
impl<D: BytesDefinition, const N: usize> Storage<D> for Bytes<'_, N> {
    fn contains(&self, metadata: Metadata) -> bool {
        if metadata.end > self.total {
            return false;
        }
        if metadata.end <= self.base {
            return true;
        }
        let start = metadata.start.saturating_sub(self.base) as usize;
        let end = (metadata.end - self.base) as usize;
        self.coverage.contains(start, end)
    }
    fn put(&mut self, message: &D::Message<'_>, metadata: Metadata) -> Result<(), Error> {
        if metadata.end > self.total || metadata.start >= metadata.end {
            return Err(Error::Invalid);
        }
        if metadata.end <= self.base {
            return Ok(());
        }
        let end = metadata.end.saturating_sub(self.base) as usize;
        let start = metadata.start.saturating_sub(self.base) as usize;
        if end > self.data.len() || end > N * 8 {
            return Err(Error::Capacity);
        }
        let bytes = D::bytes(message);
        if bytes.len() != (metadata.end - metadata.start) as usize {
            return Err(Error::Invalid);
        }
        let skip = self.base.saturating_sub(metadata.start) as usize;
        self.data[start..end].copy_from_slice(&bytes[skip..]);
        self.coverage.mark(start, end)?;
        Ok(())
    }
    fn complete(&self) -> bool {
        self.base
            .saturating_add(self.coverage.prefix(self.data.len()) as u32)
            == self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_boundaries() {
        for (previous, next, expected) in [
            (None, 0, Received::Accepted),
            (Some(10), 11, Received::Accepted),
            (Some(10), 10, Received::Duplicate),
            (Some(11), 10, Received::Stale),
            (Some(u32::MAX), 0, Received::Accepted),
            (Some(0), u32::MAX, Received::Stale),
            (Some(0), 1 << 31, Received::Stale),
        ] {
            let mut channel = Channel::<Update<u8>, (Unique, Temporal), _>::new(Latest {
                current: previous.map(|n| (n, 9)),
            });
            let before = channel.current();
            assert_eq!(channel.inspect(&(next, 7)), Ok(expected));
            assert_eq!(channel.current(), before);
            assert_eq!(channel.receive(&(next, 7)), Ok(expected));
            assert_eq!(
                channel.current(),
                if expected == Received::Accepted {
                    Some((next, 7))
                } else {
                    before
                }
            );
        }
    }

    #[test]
    fn indexed_coverage() {
        let payload = *b"0123456789abc";
        let spans = [(0, 3), (3, 8), (8, 13)];
        for a in 0..3 {
            for b in 0..3 {
                if a == b {
                    continue;
                }
                let c = 3 - a - b;
                let mut data = [255; 13];
                let mut coverage = Coverage::<2>::new();
                let mut channel = Channel::<Piece, (Unique, Complete), _>::new(Bytes {
                    data: &mut data,
                    coverage: &mut coverage,
                    base: 0,
                    total: 13,
                });
                for (i, index) in [a, b, c].into_iter().enumerate() {
                    let (start, end) = spans[index];
                    let piece = (start as u32, &payload[start..end]);
                    assert_eq!(
                        channel.receive(&piece),
                        Ok(if i == 2 {
                            Received::Accepted
                        } else {
                            Received::Buffered
                        })
                    );
                    assert_eq!(channel.receive(&piece), Ok(Received::Duplicate));
                }
                assert_eq!(channel.storage.data, &payload);
                assert_eq!(channel.storage.coverage.prefix(13), 13);
                for piece in [(0, &[][..]), (13, &payload[..1]), (u32::MAX, &payload[..1])] {
                    assert_eq!(channel.receive(&piece), Err(Error::Invalid));
                    assert_eq!(channel.storage.coverage.prefix(13), 13);
                }
            }
        }
    }

    #[test]
    fn bounded_window() {
        let mut data = [0; 16];
        let mut coverage = Coverage::<2>::new();
        let mut channel = Channel::<Piece, (Unique, Complete), _>::new(Bytes {
            data: &mut data,
            coverage: &mut coverage,
            base: 100,
            total: 132,
        });
        assert_eq!(channel.receive(&(116, &[1][..])), Err(Error::Capacity));
        assert_eq!(channel.storage.coverage.prefix(16), 0);
        assert_eq!(channel.receive(&(108, &[2; 8][..])), Ok(Received::Buffered));
        assert_eq!(channel.storage.coverage.prefix(16), 0);
        assert_eq!(channel.receive(&(100, &[1; 8][..])), Ok(Received::Buffered));
        assert_eq!(channel.storage.coverage.prefix(16), 16);
        assert_eq!(&channel.storage.data[..8], &[1; 8]);
        assert_eq!(&channel.storage.data[8..], &[2; 8]);
        channel.storage.base += 16;
        channel.storage.coverage.discard_prefix(16);
        assert_eq!(
            channel.receive(&(100, &[1; 8][..])),
            Ok(Received::Duplicate)
        );
        assert_eq!(channel.storage.coverage.prefix(16), 0);
        assert_eq!(
            channel.receive(&(116, &[3; 16][..])),
            Ok(Received::Accepted)
        );
        assert_eq!(channel.storage.data, &[3; 16]);
    }

    #[test]
    fn coverage_shift_bounds() {
        for count in [0, 1, 7, 8, 9, 15, 16, 24, usize::MAX] {
            let mut coverage = Coverage::<3>::new();
            for (start, end) in [(0, 3), (7, 10), (14, 17), (23, 24)] {
                coverage.mark(start, end).unwrap();
            }
            let before = coverage;
            coverage.discard_prefix(count);
            for bit in 0usize..24 {
                assert_eq!(coverage.has(bit), before.has(bit.saturating_add(count)));
            }
        }
    }
}
