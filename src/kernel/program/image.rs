//! One stored computation can have several addressed connection sets. A view
//! binds only the selected set; sharing bytes never merges visits or successors.
use lattice_peer_wire::program::{
    Definition as Code, Error, Link, Location, Operation, Record, Reference, Source, HEADER_BYTES,
    NONE, SOURCE_BYTES,
};

#[derive(Clone, Copy)]
pub(super) struct Definition<'a> {
    code: Code<'a>,
    connections: Option<&'a [u8]>,
}
impl<'a> Definition<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        Ok(Self {
            code: Code::parse(bytes)?,
            connections: None,
        })
    }
    pub fn connected(code: Code<'a>, bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < 12 {
            return Err(Error::Encoding);
        }
        let this = Self {
            code,
            connections: Some(bytes),
        };
        if 12 + this.source_bytes() > bytes.len()
            || this.len() < HEADER_BYTES
            || this.len() >= NONE as usize
        {
            return Err(Error::Bounds);
        }
        Ok(this)
    }
    pub fn head(self) -> Option<u16> {
        self.connections.map_or_else(
            || self.code.head(),
            |b| {
                let v = word(b, 6);
                (v != NONE).then_some(v)
            },
        )
    }
    pub fn surface(self) -> &'a [u8; 32] {
        self.code.surface()
    }
    pub fn carried_bytes(self) -> u16 {
        self.code.carried_bytes()
    }
    pub fn scratch_bytes(self) -> u16 {
        self.code.scratch_bytes()
    }
    pub fn check_location(self, l: Location) -> Result<(), Error> {
        self.code.check_location(l)
    }
    fn source_bytes(self) -> usize {
        self.connections.map_or_else(
            || self.code.sources().count() * SOURCE_BYTES,
            |b| word(b, 10) as usize * SOURCE_BYTES,
        )
    }
    pub fn sources(self) -> impl Iterator<Item = Source> + 'a {
        let bytes = self.connections.unwrap_or(self.code.bytes());
        let base = if self.connections.is_some() {
            12
        } else {
            HEADER_BYTES
        };
        let count = self.source_bytes() / SOURCE_BYTES;
        (0..count).map(move |i| {
            let at = base + i * SOURCE_BYTES;
            Source {
                offset: at as u16,
                state: u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()),
                entry: word(bytes, at + 8),
            }
        })
    }
    fn connected_records(self) -> impl Iterator<Item = Result<(Record<'a>, u16), Error>> + 'a {
        let mut at = 12 + self.source_bytes();
        let mut expected = HEADER_BYTES + self.source_bytes();
        let mut records = self.code.records();
        let mut done = false;
        core::iter::from_fn(move || {
            if done {
                return None;
            }
            let Some(original) = records.next() else {
                done = true;
                return self
                    .connections
                    .filter(|b| at != b.len() || expected != word(b, 8) as usize)
                    .map(|_| Err(Error::Encoding));
            };
            let result = (|| {
                let original = original?;
                let Some(bytes) = self.connections else {
                    return Ok((original, original.offset));
                };
                let physical = at;
                let header = bytes.get(at..at + 4).ok_or(Error::Encoding)?;
                at += 4;
                let offset = word(header, 0);
                let length = word(header, 2);
                let mut op = original.operation;
                if let Operation::Temporal { entry, .. } = &mut op {
                    *entry = word(bytes.get(at..at + 2).ok_or(Error::Encoding)?, 0);
                    at += 2;
                }
                let mut replacements = [Link::Stop; 2];
                for (i, link) in links(original.operation).iter().enumerate() {
                    if link.is_some() {
                        let (link, n) = decode_link(bytes.get(at..).ok_or(Error::Encoding)?)?;
                        replacements[i] = link;
                        at += n;
                    }
                }
                replace_links(&mut op, replacements);
                let encoded = 3
                    + payload(self.code, original).len()
                    + links(op)
                        .iter()
                        .flatten()
                        .map(|l| link_len(*l))
                        .sum::<usize>()
                    + if matches!(op, Operation::Temporal { .. }) {
                        2
                    } else {
                        0
                    };
                if offset as usize != expected || length as usize != encoded {
                    return Err(Error::Encoding);
                }
                expected += encoded;
                Ok((
                    Record {
                        offset,
                        length,
                        operation: op,
                    },
                    physical as u16,
                ))
            })();
            if result.is_err() {
                done = true;
            }
            Some(result)
        })
    }
    pub fn records(self) -> impl Iterator<Item = Result<Record<'a>, Error>> + 'a {
        self.connected_records().map(|r| r.map(|(r, _)| r))
    }
    pub fn record(self, offset: u16) -> Result<Record<'a>, Error> {
        self.records()
            .find_map(|r| match r {
                Ok(r) if r.offset == offset => Some(Ok(r)),
                Err(e) => Some(Err(e)),
                _ => None,
            })
            .unwrap_or(Err(Error::Link))
    }
    pub fn storage_record(self, offset: u16) -> Option<u16> {
        self.connected_records()
            .filter_map(Result::ok)
            .find(|(r, _)| r.offset == offset)
            .map(|(_, at)| at)
    }
    pub fn len(self) -> usize {
        self.connections
            .map_or(self.code.bytes().len(), |b| word(b, 8) as usize)
    }
    pub fn copy_range(self, offset: usize, out: &mut [u8]) {
        out.fill(0);
        let mut header = [0; HEADER_BYTES];
        header.copy_from_slice(&self.code.bytes()[..HEADER_BYTES]);
        header[8..10].copy_from_slice(&((self.source_bytes() / SOURCE_BYTES) as u16).to_le_bytes());
        header[10..12].copy_from_slice(&self.head().unwrap_or(NONE).to_le_bytes());
        header[12..14]
            .copy_from_slice(&((HEADER_BYTES + self.source_bytes()) as u16).to_le_bytes());
        copy(out, offset, 0, &header);
        for (i, s) in self.sources().enumerate() {
            let at = HEADER_BYTES + i * SOURCE_BYTES;
            copy(out, offset, at, &s.state.to_le_bytes());
            copy(out, offset, at + 8, &s.entry.to_le_bytes());
            copy(out, offset, at + 10, &[255; 2]);
        }
        for (raw, bound) in self.code.records().zip(self.records()) {
            let (Ok(raw), Ok(bound)) = (raw, bound) else {
                break;
            };
            let mut at = bound.offset as usize;
            copy(out, offset, at, &bound.length.to_le_bytes());
            at += 2;
            copy(
                out,
                offset,
                at,
                &self.code.bytes()[raw.offset as usize + 2..raw.offset as usize + 3],
            );
            at += 1;
            let bs = links(bound.operation);
            let mut buf = [0; 17];
            if let Operation::Entry { .. } = bound.operation {
                let n = encode_link(bs[0].unwrap(), &mut buf);
                copy(out, offset, at, &buf[..n]);
                at += n;
            }
            if let Operation::Temporal { entry, .. } = bound.operation {
                copy(out, offset, at, &entry.to_le_bytes());
                at += 2;
            }
            let p = payload(self.code, raw);
            copy(out, offset, at, p);
            at += p.len();
            for l in bs
                .iter()
                .skip(if matches!(bound.operation, Operation::Entry { .. }) {
                    1
                } else {
                    0
                })
                .flatten()
            {
                let n = encode_link(*l, &mut buf);
                copy(out, offset, at, &buf[..n]);
                at += n;
            }
        }
    }
}
fn copy(out: &mut [u8], offset: usize, at: usize, bytes: &[u8]) {
    let begin = offset.max(at);
    let end = (offset + out.len()).min(at + bytes.len());
    if begin < end {
        out[begin - offset..end - offset].copy_from_slice(&bytes[begin - at..end - at]);
    }
}
fn word(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn links(op: Operation<'_>) -> [Option<Link>; 2] {
    match op {
        Operation::Entry { upstream, next, .. } => [Some(upstream), Some(next)],
        Operation::Read { next, .. }
        | Operation::Unary { next, .. }
        | Operation::Binary { next, .. } => [Some(next), None],
        Operation::Test { yes, no, .. }
        | Operation::Branch { yes, no, .. }
        | Operation::Temporal { yes, no, .. } => [Some(yes), Some(no)],
        Operation::Invoke {
            success, failure, ..
        } => [Some(success), Some(failure)],
    }
}
fn replace_links(op: &mut Operation<'_>, l: [Link; 2]) {
    match op {
        Operation::Entry { upstream, next, .. } => {
            *upstream = l[0];
            *next = l[1];
        }
        Operation::Read { next, .. }
        | Operation::Unary { next, .. }
        | Operation::Binary { next, .. } => *next = l[0],
        Operation::Test { yes, no, .. }
        | Operation::Branch { yes, no, .. }
        | Operation::Temporal { yes, no, .. } => {
            *yes = l[0];
            *no = l[1];
        }
        Operation::Invoke {
            success, failure, ..
        } => {
            *success = l[0];
            *failure = l[1];
        }
    }
}
fn payload<'a>(code: Code<'a>, r: Record<'_>) -> &'a [u8] {
    let edges = links(r.operation);
    let edge_bytes = edges.iter().flatten().map(|l| link_len(*l)).sum::<usize>();
    let (front, back) = match r.operation {
        Operation::Entry { upstream, next, .. } => (link_len(upstream), link_len(next)),
        Operation::Temporal { .. } => (2, edge_bytes),
        _ => (0, edge_bytes),
    };
    &code.bytes()[r.offset as usize + 3 + front..r.offset as usize + r.length as usize - back]
}
/// Exact computation/operand-layout equality. Connection addresses, local jump
/// positions, source subscriptions and head admission belong to the selected pair.
pub(super) fn same_computation(a: Code<'_>, b: Code<'_>) -> bool {
    a.surface() == b.surface()
        && a.carried_bytes() == b.carried_bytes()
        && a.scratch_bytes() == b.scratch_bytes()
        && a.records().count() == b.records().count()
        && a.records().zip(b.records()).all(|(x, y)| match (x, y) {
            (Ok(x), Ok(y)) => {
                a.bytes()[x.offset as usize + 2] == b.bytes()[y.offset as usize + 2]
                    && payload(a, x) == payload(b, y)
            }
            _ => false,
        })
}
pub(super) fn connection_len(d: Code<'_>) -> usize {
    12 + d.sources().count() * SOURCE_BYTES
        + d.records()
            .filter_map(Result::ok)
            .map(|r| {
                4 + if matches!(r.operation, Operation::Temporal { .. }) {
                    2
                } else {
                    0
                } + links(r.operation)
                    .iter()
                    .flatten()
                    .map(|l| link_len(*l))
                    .sum::<usize>()
            })
            .sum::<usize>()
}
pub(super) fn write_connections(d: Code<'_>, body: u16, generation: u32, out: &mut [u8]) -> usize {
    let n = connection_len(d);
    assert!(out.len() >= n);
    out[..2].copy_from_slice(&body.to_le_bytes());
    out[2..6].copy_from_slice(&generation.to_le_bytes());
    out[6..8].copy_from_slice(&d.head().unwrap_or(NONE).to_le_bytes());
    out[8..10].copy_from_slice(&(d.bytes().len() as u16).to_le_bytes());
    out[10..12].copy_from_slice(&(d.sources().count() as u16).to_le_bytes());
    let mut at = 12;
    for s in d.sources() {
        out[at..at + 8].copy_from_slice(&s.state.to_le_bytes());
        out[at + 8..at + 10].copy_from_slice(&s.entry.to_le_bytes());
        out[at + 10..at + 12].fill(255);
        at += 12;
    }
    for r in d.records().filter_map(Result::ok) {
        out[at..at + 2].copy_from_slice(&r.offset.to_le_bytes());
        out[at + 2..at + 4].copy_from_slice(&r.length.to_le_bytes());
        at += 4;
        if let Operation::Temporal { entry, .. } = r.operation {
            out[at..at + 2].copy_from_slice(&entry.to_le_bytes());
            at += 2;
        }
        for l in links(r.operation).iter().flatten() {
            at += encode_link(*l, &mut out[at..]);
        }
    }
    assert_eq!(at, n);
    n
}
fn link_len(l: Link) -> usize {
    match l {
        Link::Stop => 1,
        Link::Local(_) => 3,
        Link::Remote(_) => 17,
    }
}
fn encode_link(l: Link, b: &mut [u8]) -> usize {
    match l {
        Link::Stop => b[0] = 0,
        Link::Local(at) => {
            b[0] = 1;
            b[1..3].copy_from_slice(&at.to_le_bytes());
        }
        Link::Remote(r) => {
            b[0] = 2;
            b[1..9].copy_from_slice(&r.owner.to_le_bytes());
            b[9..11].copy_from_slice(&r.definition.to_le_bytes());
            b[11..15].copy_from_slice(&r.generation.to_le_bytes());
            b[15..17].copy_from_slice(&r.record.to_le_bytes());
        }
    }
    link_len(l)
}
fn decode_link(b: &[u8]) -> Result<(Link, usize), Error> {
    let l = match b.first() {
        Some(0) => Link::Stop,
        Some(1) if b.len() >= 3 => Link::Local(word(b, 1)),
        Some(2) if b.len() >= 17 => {
            let r = Reference {
                owner: u64::from_le_bytes(b[1..9].try_into().unwrap()),
                definition: word(b, 9),
                generation: u32::from_le_bytes(b[11..15].try_into().unwrap()),
                record: word(b, 15),
            };
            if r.owner == 0
                || r.owner == u64::MAX
                || r.definition == NONE
                || r.generation == 0
                || r.record == NONE
            {
                return Err(Error::Link);
            }
            Link::Remote(r)
        }
        _ => return Err(Error::Encoding),
    };
    Ok((l, link_len(l)))
}

pub(super) const INLINE: u8 = 0;
pub(super) const CLOCK: u8 = 1;
pub(super) const CONNECTION: u8 = 2;
pub(super) const BODY: u8 = 3;
pub(super) const FREE: u8 = 4;
pub(super) const STAGING: u8 = 8;
#[derive(Clone, Copy)]
pub(super) struct Extent {
    pub at: usize,
    pub span: usize,
    pub kind: u8,
    pub flags: u8,
}
impl Extent {
    pub fn end(self) -> usize {
        self.at + self.span - (self.flags >> 6) as usize
    }
}
pub(super) fn extents(bytes: &[u8]) -> impl Iterator<Item = Result<Extent, Error>> + '_ {
    let mut at = 4;
    core::iter::from_fn(move || {
        if at >= bytes.len() {
            return None;
        }
        let r = (|| {
            let h = bytes.get(at..at + 4).ok_or(Error::Encoding)?;
            let n = word(h, 0) as usize;
            if n < 4 || at + n > bytes.len() || h[3] > FREE || h[2] & 0x34 != 0 {
                return Err(Error::Encoding);
            }
            if h[3] == FREE {
                if h[2] != 0 {
                    return Err(Error::Encoding);
                }
            } else if n < super::PREFIX + (h[2] >> 6) as usize {
                return Err(Error::Encoding);
            }
            let result = Extent {
                at,
                span: n,
                kind: h[3],
                flags: h[2],
            };
            at += n;
            Ok(result)
        })();
        if r.is_err() {
            at = bytes.len();
        }
        Some(r)
    })
}
pub(super) fn extent(bytes: &[u8], at: u16) -> Result<Extent, Error> {
    extents(bytes)
        .find_map(|r| match r {
            Ok(e) if e.at == at as usize => Some(Ok(e)),
            Err(e) => Some(Err(e)),
            _ => None,
        })
        .unwrap_or(Err(Error::Binding))
}
pub(super) fn code(bytes: &[u8], at: u16, generation: u32) -> Result<Code<'_>, Error> {
    let e = extent(bytes, at)?;
    if !matches!(e.kind, INLINE | BODY)
        || u32::from_le_bytes(bytes[e.at + 4..e.at + 8].try_into().unwrap()) != generation
    {
        return Err(Error::Binding);
    }
    Code::parse(&bytes[e.at + super::PREFIX..e.end()])
}
pub(super) fn definition(bytes: &[u8], e: Extent) -> Result<Definition<'_>, Error> {
    match e.kind {
        INLINE => Definition::parse(&bytes[e.at + super::PREFIX..e.end()]),
        CONNECTION => {
            let b = &bytes[e.at + super::PREFIX..e.end()];
            if b.len() < 12 {
                return Err(Error::Encoding);
            }
            Definition::connected(
                code(
                    bytes,
                    word(b, 0),
                    u32::from_le_bytes(b[2..6].try_into().unwrap()),
                )?,
                b,
            )
        }
        _ => Err(Error::Binding),
    }
}
/// Largest free run, including the uncommitted tail. Live extents never move.
pub(super) fn vacancy(bytes: &[u8], capacity: usize) -> Result<(usize, usize), Error> {
    let mut best = (bytes.len(), capacity.saturating_sub(bytes.len()));
    let mut free = None;
    for e in extents(bytes) {
        let e = e?;
        if e.kind == FREE {
            free.get_or_insert(e.at);
        } else if let Some(at) = free.take() {
            if e.at - at > best.1 {
                best = (at, e.at - at);
            }
        }
    }
    if let Some(at) = free {
        if capacity - at > best.1 {
            best = (at, capacity - at);
        }
    }
    Ok(best)
}
pub(super) fn free_header(out: &mut [u8], span: usize) {
    assert!(span >= 4 && span <= u16::MAX as usize);
    out[..span].fill(0);
    out[..2].copy_from_slice(&(span as u16).to_le_bytes());
    out[3] = FREE;
}

/// Retire one connection set without relocating any surviving address.
pub(super) fn remove(bytes: &mut [u8], at: u16) -> Result<usize, Error> {
    let e = extent(bytes, at)?;
    let body = match e.kind {
        INLINE => at,
        CONNECTION => word(bytes, e.at + super::PREFIX),
        _ => return Err(Error::Binding),
    };
    if e.kind == INLINE {
        bytes[e.at + 3] = BODY;
        bytes[e.at + 2] &= 0xc0;
    } else {
        free_header(&mut bytes[e.at..e.at + e.span], e.span);
    }
    let shared = extents(bytes)
        .filter_map(Result::ok)
        .any(|x| x.kind == CONNECTION && word(bytes, x.at + super::PREFIX) == body);
    let owner = extent(bytes, body)?;
    if !shared && owner.kind == BODY {
        free_header(&mut bytes[owner.at..owner.at + owner.span], owner.span);
    }
    let mut used = 4;
    let mut free = None;
    let mut cursor = 4;
    while cursor < bytes.len() {
        let e = extent(bytes, cursor as u16)?;
        if e.kind == FREE {
            free.get_or_insert(e.at);
        } else {
            if let Some(begin) = free.take() {
                free_header(&mut bytes[begin..cursor], cursor - begin);
            }
            used = cursor + e.span;
        }
        cursor += e.span;
    }
    Ok(used)
}
