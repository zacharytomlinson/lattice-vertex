//! Declared-width scalar and argument encoding shared by radio and USB.
use crate::{Integer, Type, Value, CALL_ARGUMENT_BYTES, MAX_CALL_ARGUMENTS};

pub fn type_tag(ty: Type) -> u8 {
    match ty {
        Type::Bool => 0,
        Type::Integer(i) => match i {
            Integer::U8 => 1,
            Integer::U16 => 2,
            Integer::U32 => 3,
            Integer::U64 => 4,
            Integer::I8 => 5,
            Integer::I16 => 6,
            Integer::I32 => 7,
            Integer::I64 => 8,
        },
    }
}

pub fn decode_type(tag: u8) -> Option<Type> {
    Some(match tag {
        0 => Type::Bool,
        1 => Type::Integer(Integer::U8),
        2 => Type::Integer(Integer::U16),
        3 => Type::Integer(Integer::U32),
        4 => Type::Integer(Integer::U64),
        5 => Type::Integer(Integer::I8),
        6 => Type::Integer(Integer::I16),
        7 => Type::Integer(Integer::I32),
        8 => Type::Integer(Integer::I64),
        _ => return None,
    })
}

pub fn encode_value(value: Value, out: &mut [u8]) -> usize {
    out[0] = type_tag(value.ty());
    match value.ty() {
        Type::Bool => {
            out[1] = u8::from(value.as_bool().unwrap());
            2
        }
        Type::Integer(ty) => {
            let n = ty.width() as usize / 8;
            out[1..1 + n].copy_from_slice(&(value.as_integer().unwrap() as u64).to_le_bytes()[..n]);
            1 + n
        }
    }
}

pub fn decode_value(bytes: &[u8]) -> Option<(Value, usize)> {
    let tag = *bytes.first()?;
    if tag == 0 {
        return match bytes.get(1)? {
            0 => Some((Value::boolean(false), 2)),
            1 => Some((Value::boolean(true), 2)),
            _ => None,
        };
    }
    let Type::Integer(ty) = decode_type(tag)? else {
        return None;
    };
    let n = ty.width() as usize / 8;
    let mut raw = [0; 8];
    raw[..n].copy_from_slice(bytes.get(1..1 + n)?);
    let bits = u64::from_le_bytes(raw);
    let mut value = i128::from(bits);
    if ty.signed() && bits & (1u64 << (ty.width() - 1)) != 0 {
        value -= 1i128 << ty.width();
    }
    Some((Value::integer(ty, value).ok()?, 1 + n))
}

pub fn valid_arguments(bytes: &[u8]) -> bool {
    let Some(&count) = bytes.first() else {
        return false;
    };
    if bytes.len() > CALL_ARGUMENT_BYTES || usize::from(count) > MAX_CALL_ARGUMENTS {
        return false;
    }
    let mut offset = 1;
    for _ in 0..count {
        let Some((_, n)) = decode_value(&bytes[offset..]) else {
            return false;
        };
        offset += n;
    }
    offset == bytes.len()
}

pub fn decode_arguments<'a>(
    bytes: &[u8],
    out: &'a mut [Value; MAX_CALL_ARGUMENTS],
) -> Option<&'a [Value]> {
    if !valid_arguments(bytes) {
        return None;
    }
    let count = bytes[0] as usize;
    let mut offset = 1;
    for value in &mut out[..count] {
        let (v, n) = decode_value(&bytes[offset..])?;
        *value = v;
        offset += n;
    }
    Some(&out[..count])
}

pub fn encode_arguments(values: &[Value], out: &mut [u8; CALL_ARGUMENT_BYTES]) -> Option<usize> {
    if values.len() > MAX_CALL_ARGUMENTS {
        return None;
    }
    out[0] = values.len() as u8;
    let mut offset = 1;
    for &value in values {
        let mut scalar = [0; 9];
        let n = encode_value(value, &mut scalar);
        out.get_mut(offset..offset + n)?
            .copy_from_slice(&scalar[..n]);
        offset += n;
    }
    Some(offset)
}
