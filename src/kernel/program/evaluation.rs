//! Declared-width operations over a visit's compiler-assigned byte locations.
//! Graph edges select control flow; no IR expression tree is evaluated here.

use crate::ir::{Binary, Integer, Type, Unary, Value};
use lattice_peer_wire::{
    program::{Error, Location, Operand, Region},
    scalar,
};

pub(super) struct Values<'a> {
    pub carried: &'a mut [u8],
    pub scratch: &'a mut [u8],
}
impl Values<'_> {
    pub fn read(&self, operand: Operand) -> Result<Value, Error> {
        let Operand::Location(location) = operand else {
            let Operand::Constant(value) = operand else {
                unreachable!()
            };
            return Ok(value);
        };
        let bytes = match location.region {
            Region::Carried => &*self.carried,
            Region::Scratch => &*self.scratch,
        };
        let mut encoded = [0; 9];
        let n = location.width() as usize;
        encoded[0] = scalar::type_tag(location.ty);
        encoded[1..1 + n].copy_from_slice(
            bytes
                .get(location.offset as usize..location.offset as usize + n)
                .ok_or(Error::Bounds)?,
        );
        scalar::decode_value(&encoded[..1 + n])
            .map(|(v, _)| v)
            .ok_or(Error::Type)
    }
    pub fn write(&mut self, location: Location, value: Value) -> Result<(), Error> {
        if value.ty() != location.ty {
            return Err(Error::Type);
        }
        let mut encoded = [0; 9];
        let n = scalar::encode_value(value, &mut encoded) - 1;
        let bytes = match location.region {
            Region::Carried => &mut *self.carried,
            Region::Scratch => &mut *self.scratch,
        };
        bytes
            .get_mut(location.offset as usize..location.offset as usize + n)
            .ok_or(Error::Bounds)?
            .copy_from_slice(&encoded[1..1 + n]);
        Ok(())
    }
}

fn integer(ty: Integer, bits: u64) -> Value {
    let width = ty.width();
    let bits = bits & (u64::MAX >> (64 - width));
    let value = if ty.signed() && bits & (1u64 << (width - 1)) != 0 {
        bits as i128 - (1i128 << width)
    } else {
        bits as i128
    };
    Value::integer(ty, value).unwrap()
}
pub(super) fn unary(op: Unary, value: Value) -> Result<Value, Error> {
    let ty = op.result_type(value.ty()).map_err(|_| Error::Type)?;
    if op == Unary::Not {
        return Ok(Value::boolean(!value.as_bool().unwrap()));
    }
    let Type::Integer(ty) = ty else {
        return Err(Error::Type);
    };
    let bits = value.as_integer().unwrap() as u64;
    Ok(integer(
        ty,
        if op == Unary::Complement { !bits } else { bits },
    ))
}
pub(super) fn binary(op: Binary, left: Value, right: Value) -> Result<Value, Error> {
    op.result_type(left.ty(), right.ty())
        .map_err(|_| Error::Type)?;
    use Binary::*;
    if matches!(op, Equal | NotEqual) {
        return Ok(Value::boolean(if op == Equal {
            left == right
        } else {
            left != right
        }));
    }
    let Type::Integer(ty) = left.ty() else {
        return Err(Error::Type);
    };
    let a = left.as_integer().unwrap();
    let b = right.as_integer().unwrap();
    if matches!(op, Less | LessEqual | Greater | GreaterEqual) {
        return Ok(Value::boolean(match op {
            Less => a < b,
            LessEqual => a <= b,
            Greater => a > b,
            _ => a >= b,
        }));
    }
    let bits = match op {
        Add => (a as u64).wrapping_add(b as u64),
        Subtract => (a as u64).wrapping_sub(b as u64),
        Multiply => (a as u64).wrapping_mul(b as u64),
        Divide => {
            if b == 0 {
                u64::MAX
            } else {
                (a / b) as u64
            }
        }
        Remainder => {
            if b == 0 {
                a as u64
            } else {
                (a % b) as u64
            }
        }
        BitAnd => a as u64 & b as u64,
        BitOr => a as u64 | b as u64,
        BitXor => a as u64 ^ b as u64,
        ShiftLeft => {
            if b >= ty.width() as i128 {
                0
            } else {
                (a as u64) << b as u32
            }
        }
        ShiftRight => {
            if b >= ty.width() as i128 {
                0
            } else {
                (a as u64) >> b as u32
            }
        }
        ShiftArithmetic => {
            if b >= ty.width() as i128 {
                if a < 0 {
                    u64::MAX
                } else {
                    0
                }
            } else {
                (a >> b as u32) as u64
            }
        }
        _ => return Err(Error::Type),
    };
    Ok(integer(ty, bits))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scalar_boundaries() {
        use Binary::*;
        use Integer::*;
        for ty in [U8, U16, U32, U64, I8, I16, I32, I64] {
            let bits = ty.width();
            let minimum = if ty.signed() {
                -(1i128 << (bits - 1))
            } else {
                0
            };
            let maximum = if ty.signed() {
                (1i128 << (bits - 1)) - 1
            } else {
                (1i128 << bits) - 1
            };
            let all_ones = if ty.signed() { -1 } else { maximum };
            let value = |n| Value::integer(ty, n).unwrap();
            for (op, a, b, expected) in [
                (Add, maximum, 1, minimum),
                (Subtract, minimum, 1, maximum),
                (Divide, 1, 0, all_ones),
                (Remainder, maximum, 0, maximum),
                (Multiply, maximum, 0, 0),
            ] {
                assert_eq!(
                    binary(op, value(a), value(b)).unwrap(),
                    value(expected),
                    "{ty:?} {op:?}"
                );
            }
            if ty.signed() {
                assert_eq!(
                    binary(Divide, value(minimum), value(-1)).unwrap(),
                    value(minimum)
                );
                assert_eq!(
                    binary(Remainder, value(minimum), value(-1)).unwrap(),
                    value(0)
                );
                assert_eq!(
                    binary(Less, value(minimum), value(0)).unwrap(),
                    Value::boolean(true)
                );
            }
            for count in [bits, bits + 1, u32::MAX] {
                let n = Value::integer(U32, count as i128).unwrap();
                assert_eq!(binary(ShiftLeft, value(maximum), n).unwrap(), value(0));
                assert_eq!(
                    binary(
                        if ty.signed() {
                            ShiftArithmetic
                        } else {
                            ShiftRight
                        },
                        value(all_ones),
                        n
                    )
                    .unwrap(),
                    value(if ty.signed() { -1 } else { 0 })
                );
            }
            assert_eq!(unary(Unary::Complement, value(0)).unwrap(), value(all_ones));
            assert_eq!(
                binary(Add, value(0), Value::boolean(false)),
                Err(Error::Type)
            );
        }
        for (op, from, input, expected) in [
            (
                Unary::SignExtend(I64),
                I8,
                -128,
                Value::integer(I64, -128).unwrap(),
            ),
            (
                Unary::ZeroExtend(U64),
                U8,
                255,
                Value::integer(U64, 255).unwrap(),
            ),
            (
                Unary::Truncate(U8),
                U64,
                257,
                Value::integer(U8, 1).unwrap(),
            ),
            (
                Unary::Reinterpret(I8),
                U8,
                255,
                Value::integer(I8, -1).unwrap(),
            ),
        ] {
            assert_eq!(
                unary(op, Value::integer(from, input).unwrap()).unwrap(),
                expected
            );
        }
    }
    #[test]
    fn location_bounds() {
        for n in [0, 1, 2, 7, 8] {
            let mut carried = [0; 8];
            let mut scratch = [0; 8];
            let mut v = Values {
                carried: &mut carried[..n],
                scratch: &mut scratch,
            };
            for region in [Region::Carried, Region::Scratch] {
                let l = Location {
                    region,
                    ty: Type::Integer(Integer::U64),
                    offset: 0,
                };
                let ok = region == Region::Scratch || n == 8;
                let value = Value::integer(Integer::U64, u64::MAX as i128).unwrap();
                assert_eq!(v.write(l, value).is_ok(), ok);
                if ok {
                    assert_eq!(v.read(Operand::Location(l)), Ok(value));
                }
            }
        }
    }
}
