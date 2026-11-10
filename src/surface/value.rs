//! Native scalar and argument conversion at the callable surface boundary.

use crate::ir::{Integer, Type, Value};

mod sealed {
    pub trait Scalar {}
}

/// Native types with an exact representation in the shared scalar IR.
///
/// Implemented for `bool` and signed/unsigned 8-, 16-, 32-, and 64-bit integers.
/// This trait is sealed so a state reader cannot declare one IR type and emit another.
pub trait Scalar: sealed::Scalar + Copy + PartialEq + 'static {
    /// Mathematical scalar type.
    const TYPE: Type;
    /// Convert a native observation to its exact IR value.
    fn into_value(self) -> Value;
    /// Decode a matching IR value without promotions or truncation.
    fn from_value(value: Value) -> Option<Self>;
}

impl sealed::Scalar for bool {}
impl Scalar for bool {
    const TYPE: Type = Type::Bool;

    fn into_value(self) -> Value {
        Value::boolean(self)
    }

    fn from_value(value: Value) -> Option<Self> {
        value.as_bool()
    }
}

macro_rules! integers {
    ($($native:ty => $ir:ident),+ $(,)?) => {$(
        impl sealed::Scalar for $native {}
        impl Scalar for $native {
            const TYPE: Type = Type::Integer(Integer::$ir);

            fn into_value(self) -> Value {
                Value::integer(Integer::$ir, i128::from(self))
                    .expect("native scalar fits its declared IR type")
            }

            fn from_value(value: Value) -> Option<Self> {
                if value.ty() != Self::TYPE {
                    return None;
                }
                Self::try_from(value.as_integer()?).ok()
            }
        }
    )+};
}
integers! {
    u8 => U8, u16 => U16, u32 => U32, u64 => U64,
    i8 => I8, i16 => I16, i32 => I32, i64 => I64,
}

/// Scalar arguments disagree with a native operation's signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgumentError {
    /// Wrong number of arguments.
    Count {
        /// Declared argument count.
        expected: usize,
        /// Supplied argument count.
        actual: usize,
    },
    /// Wrong scalar kind, width, or signedness.
    Type {
        /// Position in the argument list.
        index: usize,
        /// Declared scalar type.
        expected: Type,
        /// Supplied scalar type.
        actual: Type,
    },
}

/// A native argument product and its exact mathematical signature.
///
/// Unit, individual scalars, tuples of up to four scalars, and fixed arrays are provided.
/// Implementors can supply another fixed product with its own decoder; it must
/// reject values outside its declared signature and must not invoke hardware.
pub trait Arguments: Sized {
    /// Ordered scalar types of this product.
    const TYPES: &'static [Type];
    /// Decode the product without coercing scalar types.
    ///
    /// # Errors
    /// Rejects mismatched argument count or scalar types.
    fn decode(values: &[Value]) -> Result<Self, ArgumentError>;
}

impl<T: Scalar> Arguments for T {
    const TYPES: &'static [Type] = &[T::TYPE];
    fn decode(values: &[Value]) -> Result<Self, ArgumentError> {
        check(Self::TYPES, values)?;
        T::from_value(values[0]).ok_or(ArgumentError::Type {
            index: 0,
            expected: T::TYPE,
            actual: values[0].ty(),
        })
    }
}

pub(super) fn check(signature: &[Type], values: &[Value]) -> Result<(), ArgumentError> {
    if signature.len() != values.len() {
        return Err(ArgumentError::Count {
            expected: signature.len(),
            actual: values.len(),
        });
    }
    for (index, (&expected, value)) in signature.iter().zip(values).enumerate() {
        if expected != value.ty() {
            return Err(ArgumentError::Type {
                index,
                expected,
                actual: value.ty(),
            });
        }
    }
    Ok(())
}

impl Arguments for () {
    const TYPES: &'static [Type] = &[];
    fn decode(values: &[Value]) -> Result<Self, ArgumentError> {
        check(Self::TYPES, values)
    }
}

impl<T: Scalar, const N: usize> Arguments for [T; N] {
    const TYPES: &'static [Type] = &[T::TYPE; N];
    fn decode(values: &[Value]) -> Result<Self, ArgumentError> {
        check(Self::TYPES, values)?;
        Ok(core::array::from_fn(|i| T::from_value(values[i]).unwrap()))
    }
}

macro_rules! tuples {
    ($(($($native:ident:$index:tt),+)),+ $(,)?) => {$(
        impl<$($native: Scalar),+> Arguments for ($($native,)+) {
            const TYPES: &'static [Type] = &[$($native::TYPE),+];
            fn decode(values: &[Value]) -> Result<Self, ArgumentError> {
                check(Self::TYPES, values)?;
                Ok(($($native::from_value(values[$index]).unwrap(),)+))
            }
        }
    )+};
}
tuples! { (A:0), (A:0, B:1), (A:0, B:1, C:2), (A:0, B:1, C:2, D:3) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_domains() {
        fn roundtrip<T: Scalar + core::fmt::Debug>(values: &[T]) {
            for &value in values {
                let encoded = value.into_value();
                assert_eq!(
                    (encoded.ty(), T::from_value(encoded)),
                    (T::TYPE, Some(value))
                );
            }
        }
        roundtrip(&[false, true]);
        roundtrip(&[u8::MIN, u8::MAX]);
        roundtrip(&[u16::MIN, u16::MAX]);
        roundtrip(&[u32::MIN, u32::MAX]);
        roundtrip(&[u64::MIN, u64::MAX]);
        roundtrip(&[i8::MIN, i8::MAX]);
        roundtrip(&[i16::MIN, i16::MAX]);
        roundtrip(&[i32::MIN, i32::MAX]);
        roundtrip(&[i64::MIN, i64::MAX]);
        assert_eq!(bool::from_value(1_u8.into_value()), None);
        for value in [1_u16.into_value(), 1_i8.into_value(), true.into_value()] {
            assert_eq!(u8::from_value(value), None);
        }
    }

    #[test]
    fn argument_products() {
        assert_eq!(<()>::decode(&[]), Ok(()));
        assert_eq!(bool::decode(&[true.into_value()]), Ok(true));
        assert_eq!(u8::decode(&[255_u8.into_value()]), Ok(255));
        assert_eq!(<(u8,)>::decode(&[255_u8.into_value()]), Ok((255,)));
        assert_eq!(
            <(bool, u64, i64, u8)>::decode(&[
                true.into_value(),
                u64::MAX.into_value(),
                i64::MIN.into_value(),
                255_u8.into_value()
            ]),
            Ok((true, u64::MAX, i64::MIN, 255))
        );
        assert_eq!(
            <[u16; 5]>::decode(&[u16::MAX.into_value(); 5]),
            Ok([u16::MAX; 5])
        );
        assert_eq!(<[u16; 0]>::decode(&[]), Ok([]));
        for values in [&[][..], &[true.into_value(), false.into_value()][..]] {
            assert!(matches!(
                bool::decode(values),
                Err(ArgumentError::Count { .. })
            ));
        }
        assert!(matches!(
            bool::decode(&[1_u8.into_value()]),
            Err(ArgumentError::Type { .. })
        ));
    }
}
