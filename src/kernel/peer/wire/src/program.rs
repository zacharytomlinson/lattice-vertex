//! Owner-local installed records. Numeric locations and links, never Rust pointers
//! or an expression tree. The kernel validates bindings before publication.

use crate::{scalar, Type, Value};
pub mod execution;
pub mod transfer;
pub use lattice_ir::{Binary, Unary};

pub const HEADER_BYTES: usize = 48;
pub const SOURCE_BYTES: usize = 12;
pub const NONE: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Encoding,
    Bounds,
    Type,
    Link,
    Binding,
    Capacity,
    Compatibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference {
    pub owner: u64,
    pub definition: u16,
    pub generation: u32,
    pub record: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Stop,
    Local(u16),
    Remote(Reference),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Carried,
    Scratch,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub region: Region,
    pub ty: Type,
    pub offset: u16,
}
impl Location {
    pub fn width(self) -> u16 {
        width(self.ty)
    }
    pub fn end(self) -> Option<u16> {
        self.offset.checked_add(self.width())
    }
}
pub fn width(ty: Type) -> u16 {
    match ty {
        Type::Bool => 1,
        Type::Integer(i) => (i.width() / 8) as u16,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Constant(Value),
    Location(Location),
}
impl Operand {
    pub fn ty(self) -> Type {
        match self {
            Self::Constant(v) => v.ty(),
            Self::Location(l) => l.ty,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Operands<'a> {
    bytes: &'a [u8],
    count: u8,
}
impl<'a> Operands<'a> {
    pub fn len(self) -> usize {
        self.count as usize
    }
    pub fn is_empty(self) -> bool {
        self.count == 0
    }
    pub fn iter(self) -> impl Iterator<Item = Operand> + 'a {
        let mut reader = Reader(self.bytes);
        (0..self.count).map(move |_| reader.operand().expect("validated operand"))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Operation<'a> {
    Entry {
        upstream: Link,
        inputs: Operands<'a>,
        next: Link,
    },
    Read {
        state: u64,
        output: Location,
        next: Link,
    },
    Unary {
        operation: Unary,
        input: Operand,
        output: Location,
        next: Link,
    },
    Binary {
        operation: Binary,
        left: Operand,
        right: Operand,
        output: Location,
        next: Link,
    },
    Test {
        operation: Binary,
        left: Operand,
        right: Operand,
        yes: Link,
        no: Link,
    },
    Branch {
        input: Operand,
        yes: Link,
        no: Link,
    },
    Invoke {
        transformer: u64,
        arguments: Operands<'a>,
        success: Link,
        failure: Link,
    },
}
impl Operation<'_> {
    pub fn successors(self) -> [Link; 2] {
        match self {
            Self::Entry { next, .. }
            | Self::Read { next, .. }
            | Self::Unary { next, .. }
            | Self::Binary { next, .. } => [next, Link::Stop],
            Self::Test { yes, no, .. } | Self::Branch { yes, no, .. } => [yes, no],
            Self::Invoke {
                success, failure, ..
            } => [success, failure],
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    pub offset: u16,
    pub length: u16,
    pub operation: Operation<'a>,
}
#[derive(Clone, Copy, Debug)]
pub struct Source {
    pub offset: u16,
    pub state: u64,
    pub entry: u16,
}

#[derive(Clone, Copy)]
pub struct Definition<'a> {
    bytes: &'a [u8],
    records: u16,
}
impl<'a> Definition<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_BYTES
            || bytes.len() >= NONE as usize
            || bytes[..4] != *b"LPD\x01"
            || bytes[14..16] != [0, 0]
        {
            return Err(Error::Encoding);
        }
        let sources = u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as usize;
        let records = u16::from_le_bytes(bytes[12..14].try_into().unwrap());
        if records as usize != HEADER_BYTES + sources * SOURCE_BYTES
            || records as usize >= bytes.len()
        {
            return Err(Error::Bounds);
        }
        let definition = Self { bytes, records };
        for record in definition.records() {
            record?;
        }
        Ok(definition)
    }
    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }
    pub fn carried_bytes(self) -> u16 {
        u16::from_le_bytes(self.bytes[4..6].try_into().unwrap())
    }
    pub fn scratch_bytes(self) -> u16 {
        u16::from_le_bytes(self.bytes[6..8].try_into().unwrap())
    }
    pub fn head(self) -> Option<u16> {
        let offset = u16::from_le_bytes(self.bytes[10..12].try_into().unwrap());
        (offset != NONE).then_some(offset)
    }
    pub fn surface(self) -> &'a [u8; 32] {
        self.bytes[16..48].try_into().unwrap()
    }
    pub fn sources(self) -> impl Iterator<Item = Source> + 'a {
        self.bytes[HEADER_BYTES..self.records as usize]
            .chunks_exact(SOURCE_BYTES)
            .enumerate()
            .map(|(index, b)| Source {
                offset: (HEADER_BYTES + index * SOURCE_BYTES) as u16,
                state: u64::from_le_bytes(b[..8].try_into().unwrap()),
                entry: u16::from_le_bytes(b[8..10].try_into().unwrap()),
            })
    }
    pub fn records(self) -> impl Iterator<Item = Result<Record<'a>, Error>> + 'a {
        let mut at = self.records as usize;
        core::iter::from_fn(move || {
            if at >= self.bytes.len() {
                return None;
            }
            let result = decode_record(&self.bytes[at..], at as u16);
            at = result
                .as_ref()
                .map_or(self.bytes.len(), |r| at + r.length as usize);
            Some(result)
        })
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
    pub fn check_location(self, location: Location) -> Result<(), Error> {
        let capacity = match location.region {
            Region::Carried => self.carried_bytes(),
            Region::Scratch => self.scratch_bytes(),
        };
        if location.end().is_none_or(|end| end > capacity) {
            return Err(Error::Bounds);
        }
        Ok(())
    }
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let b = self.0.get(..n).ok_or(Error::Encoding)?;
        self.0 = &self.0[n..];
        Ok(b)
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn word(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn long(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn reference(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn ty(&mut self) -> Result<Type, Error> {
        scalar::decode_type(self.byte()?).ok_or(Error::Type)
    }
    fn link(&mut self) -> Result<Link, Error> {
        Ok(match self.byte()? {
            0 => Link::Stop,
            1 => Link::Local(self.word()?),
            2 => {
                let r = Reference {
                    owner: self.reference()?,
                    definition: self.word()?,
                    generation: self.long()?,
                    record: self.word()?,
                };
                if r.owner == 0
                    || r.owner == u64::MAX
                    || r.generation == 0
                    || r.definition == NONE
                    || r.record == NONE
                {
                    return Err(Error::Link);
                }
                Link::Remote(r)
            }
            _ => return Err(Error::Encoding),
        })
    }
    fn operand(&mut self) -> Result<Operand, Error> {
        Ok(match self.byte()? {
            0 => {
                let (value, n) = scalar::decode_value(self.0).ok_or(Error::Encoding)?;
                self.take(n)?;
                Operand::Constant(value)
            }
            region @ (1 | 2) => Operand::Location(Location {
                region: if region == 1 {
                    Region::Carried
                } else {
                    Region::Scratch
                },
                ty: self.ty()?,
                offset: self.word()?,
            }),
            _ => return Err(Error::Encoding),
        })
    }
    fn location(&mut self) -> Result<Location, Error> {
        match self.operand()? {
            Operand::Location(l) => Ok(l),
            _ => Err(Error::Type),
        }
    }
    fn operands(&mut self) -> Result<Operands<'a>, Error> {
        let count = self.byte()?;
        let begin = self.0;
        for _ in 0..count {
            self.operand()?;
        }
        Ok(Operands {
            bytes: &begin[..begin.len() - self.0.len()],
            count,
        })
    }
    fn unary(&mut self) -> Result<Unary, Error> {
        let tag = self.byte()?;
        if tag < 2 {
            return Ok(if tag == 0 {
                Unary::Not
            } else {
                Unary::Complement
            });
        }
        let Type::Integer(ty) = self.ty()? else {
            return Err(Error::Type);
        };
        match tag {
            2 => Ok(Unary::ZeroExtend(ty)),
            3 => Ok(Unary::SignExtend(ty)),
            4 => Ok(Unary::Truncate(ty)),
            5 => Ok(Unary::Reinterpret(ty)),
            _ => Err(Error::Encoding),
        }
    }
    fn binary(&mut self) -> Result<Binary, Error> {
        use Binary::*;
        Ok(match self.byte()? {
            0 => Equal,
            1 => NotEqual,
            2 => Less,
            3 => LessEqual,
            4 => Greater,
            5 => GreaterEqual,
            6 => Add,
            7 => Subtract,
            8 => Multiply,
            9 => Divide,
            10 => Remainder,
            11 => BitAnd,
            12 => BitOr,
            13 => BitXor,
            14 => ShiftLeft,
            15 => ShiftRight,
            16 => ShiftArithmetic,
            _ => return Err(Error::Encoding),
        })
    }
}
fn decode_record(bytes: &[u8], offset: u16) -> Result<Record<'_>, Error> {
    let mut prefix = Reader(bytes);
    let length = prefix.word()?;
    if length < 4 {
        return Err(Error::Encoding);
    }
    let mut r = Reader(bytes.get(2..length as usize).ok_or(Error::Encoding)?);
    let operation = match r.byte()? {
        0 => Operation::Entry {
            upstream: r.link()?,
            inputs: r.operands()?,
            next: r.link()?,
        },
        1 => Operation::Read {
            state: r.reference()?,
            output: r.location()?,
            next: r.link()?,
        },
        2 => Operation::Unary {
            operation: r.unary()?,
            input: r.operand()?,
            output: r.location()?,
            next: r.link()?,
        },
        3 => Operation::Binary {
            operation: r.binary()?,
            left: r.operand()?,
            right: r.operand()?,
            output: r.location()?,
            next: r.link()?,
        },
        4 => Operation::Test {
            operation: r.binary()?,
            left: r.operand()?,
            right: r.operand()?,
            yes: r.link()?,
            no: r.link()?,
        },
        5 => Operation::Branch {
            input: r.operand()?,
            yes: r.link()?,
            no: r.link()?,
        },
        6 => Operation::Invoke {
            transformer: r.reference()?,
            arguments: r.operands()?,
            success: r.link()?,
            failure: r.link()?,
        },
        _ => return Err(Error::Encoding),
    };
    if !r.0.is_empty() {
        return Err(Error::Encoding);
    }
    Ok(Record {
        offset,
        length,
        operation,
    })
}
