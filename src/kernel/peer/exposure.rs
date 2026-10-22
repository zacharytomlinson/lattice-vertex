//! Encode bounded reads of this appliance's immutable IR surface.
//! No reader, transformer, or IR expression is executed. References identify
//! durable native bindings or inline mathematical records. No remote catalog is built.

use crate::{
    ir::{self, Binary, Expr, Type, Unary},
    surface::{Read, Surface},
};

const MAX_DEPTH: u8 = 32;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EncodeError {
    TooLarge,
    TooDeep,
    Full,
    DuplicateBinding,
}

trait Encode: Sync {
    fn encode(&self, writer: &mut Writer<'_>) -> Result<(), EncodeError>;
}

/// Borrow the authored surface and its encoded length. Byte ranges are emitted
/// on demand into the existing frame buffer, without a second resident IR copy.
pub(crate) struct Description {
    source: &'static dyn Encode,
    length: u16,
}

impl Description {
    #[cfg(feature = "use-mutexes")]
    pub(crate) fn with_installed<A: Send + Sync + 'static, E: 'static>(
        appliance: &'static crate::kernel::Appliance<A, E>,
    ) -> Result<Self, EncodeError> {
        appliance.seal();
        let mut writer = Writer {
            output: &mut [],
            offset: 0,
            position: 0,
            copied: 0,
            counting: true,
        };
        appliance.encode(&mut writer)?;
        Ok(Self {
            source: appliance,
            length: writer.position as u16,
        })
    }
    pub(crate) fn new<A: 'static, E: 'static>(
        surface: &'static Surface<'static, A, E>,
    ) -> Result<Self, EncodeError> {
        let mut writer = Writer {
            output: &mut [],
            offset: 0,
            position: 0,
            copied: 0,
            counting: true,
        };
        surface.encode(&mut writer)?;
        Ok(Self {
            source: surface,
            length: writer.position as u16,
        })
    }

    pub(crate) fn len(&self) -> u16 {
        self.length
    }

    pub(crate) fn identity(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        let mut part = [0; 64];
        let mut offset = 0;
        while offset < self.length {
            let n = self.encode_part(offset, &mut part);
            hash.update(&part[..n]);
            offset += n as u16;
        }
        hash.finalize().into()
    }

    pub(crate) fn encode_part(&self, offset: u16, output: &mut [u8]) -> usize {
        if offset >= self.length || output.is_empty() {
            return 0;
        }
        let mut writer = Writer {
            output,
            offset: offset as usize,
            position: 0,
            copied: 0,
            counting: false,
        };
        match self.source.encode(&mut writer) {
            Ok(()) | Err(EncodeError::Full) => writer.copied,
            // The immutable surface was checked in full before scheduler startup.
            Err(error) => panic!("validated surface changed: {:?}", error),
        }
    }
}

struct Writer<'a> {
    output: &'a mut [u8],
    offset: usize,
    position: usize,
    copied: usize,
    counting: bool,
}

impl Writer<'_> {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        let end = self
            .position
            .checked_add(bytes.len())
            .ok_or(EncodeError::TooLarge)?;
        if end > u16::MAX as usize {
            return Err(EncodeError::TooLarge);
        }
        if !self.counting && end > self.offset {
            let skip = self.offset.saturating_sub(self.position);
            let n = (bytes.len() - skip).min(self.output.len() - self.copied);
            self.output[self.copied..self.copied + n].copy_from_slice(&bytes[skip..skip + n]);
            self.copied += n;
        }
        self.position = end;
        if !self.counting && self.copied == self.output.len() {
            Err(EncodeError::Full)
        } else {
            Ok(())
        }
    }

    fn byte(&mut self, byte: u8) -> Result<(), EncodeError> {
        self.bytes(&[byte])
    }
    fn count(&mut self, count: usize) -> Result<(), EncodeError> {
        self.bytes(
            &u16::try_from(count)
                .map_err(|_| EncodeError::TooLarge)?
                .to_le_bytes(),
        )
    }
    fn reference(&mut self, reference: u64) -> Result<(), EncodeError> {
        self.bytes(&reference.to_le_bytes())
    }
    fn inline_reference(&mut self) -> Result<(), EncodeError> {
        // Mathematical bodies are already inline. Their location in the encoded
        // description identifies this occurrence without any firmware address.
        self.reference(self.position as u64)
    }
    fn ty(&mut self, ty: Type) -> Result<(), EncodeError> {
        self.byte(super::frame::type_tag(ty))
    }
    fn types(&mut self, types: &[Type]) -> Result<(), EncodeError> {
        self.count(types.len())?;
        for ty in types {
            self.ty(*ty)?;
        }
        Ok(())
    }
    fn attribute<A>(&mut self, attribute: &ir::Attribute<Read<A>>) -> Result<(), EncodeError> {
        self.reference(attribute.location.reference())?;
        self.ty(attribute.ty)
    }
    fn attributes<A>(&mut self, attrs: &[&ir::Attribute<Read<A>>]) -> Result<(), EncodeError> {
        self.count(attrs.len())?;
        for attribute in attrs {
            self.attribute(attribute)?;
        }
        Ok(())
    }
    fn definition<A>(
        &mut self,
        definition: &ir::Definition<'_, '_, Read<A>>,
        depth: u8,
    ) -> Result<(), EncodeError> {
        self.inline_reference()?;
        self.types(definition.arguments)?;
        self.types(definition.inputs)?;
        self.expression(&definition.body, depth)
    }
    fn predicate<A>(
        &mut self,
        predicate: &ir::Predicate<'_, '_, Read<A>>,
    ) -> Result<(), EncodeError> {
        self.definition(predicate.definition, 0)?;
        self.attributes(predicate.inputs)
    }
    fn expression<A>(
        &mut self,
        expression: &Expr<'_, '_, Read<A>>,
        depth: u8,
    ) -> Result<(), EncodeError> {
        if depth >= MAX_DEPTH {
            return Err(EncodeError::TooDeep);
        }
        let next = depth + 1;
        match expression {
            Expr::Constant(value) => {
                self.byte(0)?;
                let mut scalar = [0; 9];
                let length = super::frame::encode_value(*value, &mut scalar);
                self.bytes(&scalar[..length])
            }
            Expr::Read(attribute) => {
                self.byte(1)?;
                self.attribute(attribute)
            }
            Expr::Argument(index) => {
                self.byte(2)?;
                self.count(*index)
            }
            Expr::Input(index) => {
                self.byte(3)?;
                self.count(*index)
            }
            Expr::Not(child) => {
                self.byte(4)?;
                self.expression(child, next)
            }
            Expr::And(a, b) | Expr::Or(a, b) => {
                self.byte(if matches!(expression, Expr::And(..)) {
                    5
                } else {
                    6
                })?;
                self.expression(a, next)?;
                self.expression(b, next)
            }
            Expr::Unary(op, child) => {
                self.byte(7)?;
                match op {
                    Unary::Not => self.byte(0)?,
                    Unary::Complement => self.byte(1)?,
                    Unary::ZeroExtend(ty) => {
                        self.byte(2)?;
                        self.ty(Type::Integer(*ty))?;
                    }
                    Unary::SignExtend(ty) => {
                        self.byte(3)?;
                        self.ty(Type::Integer(*ty))?;
                    }
                    Unary::Truncate(ty) => {
                        self.byte(4)?;
                        self.ty(Type::Integer(*ty))?;
                    }
                    Unary::Reinterpret(ty) => {
                        self.byte(5)?;
                        self.ty(Type::Integer(*ty))?;
                    }
                }
                self.expression(child, next)
            }
            Expr::Binary(op, a, b) => {
                self.byte(8)?;
                self.byte(binary_tag(*op))?;
                self.expression(a, next)?;
                self.expression(b, next)
            }
            Expr::Select(condition, yes, no) => {
                self.byte(9)?;
                self.expression(condition, next)?;
                self.expression(yes, next)?;
                self.expression(no, next)
            }
            Expr::Reference(definition) => {
                self.byte(10)?;
                self.definition(definition, next)
            }
        }
    }
}

fn binary_tag(op: Binary) -> u8 {
    match op {
        Binary::Equal => 0,
        Binary::NotEqual => 1,
        Binary::Less => 2,
        Binary::LessEqual => 3,
        Binary::Greater => 4,
        Binary::GreaterEqual => 5,
        Binary::Add => 6,
        Binary::Subtract => 7,
        Binary::Multiply => 8,
        Binary::Divide => 9,
        Binary::Remainder => 10,
        Binary::BitAnd => 11,
        Binary::BitOr => 12,
        Binary::BitXor => 13,
        Binary::ShiftLeft => 14,
        Binary::ShiftRight => 15,
        Binary::ShiftArithmetic => 16,
    }
}

impl<A: 'static, E> Encode for Surface<'_, A, E> {
    fn encode(&self, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
        encode_surface(self, core::iter::empty(), core::iter::empty(), writer)
    }
}

#[cfg(feature = "use-mutexes")]
impl<A: Send + Sync + 'static, E: 'static> Encode for crate::kernel::Appliance<A, E> {
    fn encode(&self, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
        encode_surface(
            self.surface(),
            self.installations().flat_map(|i| i.states()),
            self.installations().map(|i| i.definition),
            writer,
        )
    }
}

fn encode_surface<A: 'static, E>(
    surface: &Surface<'_, A, E>,
    states: impl Iterator<Item = &'static ir::Attribute<Read<A>>> + Clone,
    axioms: impl Iterator<Item = &'static crate::surface::Predicate<A>> + Clone,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    if writer.counting && !surface.unique_bindings() {
        return Err(EncodeError::DuplicateBinding);
    }
    writer.bytes(b"LSF\x02")?;
    writer.byte(64)?; // Binding-reference width, independent of CPU pointer width.
    for count in [
        surface.states.len() + states.clone().count(),
        surface.transformers.len(),
        surface.axioms.len() + axioms.clone().count(),
        surface.invariants.len(),
    ] {
        writer.count(count)?;
    }
    for state in surface.states {
        writer.attribute(state.definition())?;
    }
    for state in states {
        writer.attribute(state)?;
    }
    for transformer in surface.transformers {
        writer.reference(transformer.reference())?;
        let definition = transformer.definition();
        writer.predicate(&definition.precondition)?;
        writer.types(definition.effect.arguments)?;
        writer.attributes(definition.effect.inputs)?;
        writer.attributes(definition.effect.outputs)?;
        writer.count(definition.effect.results.len())?;
        for expression in definition.effect.results {
            writer.expression(expression, 0)?;
        }
    }
    for predicate in surface.axioms {
        writer.inline_reference()?;
        writer.predicate(predicate)?;
    }
    for predicate in axioms {
        writer.inline_reference()?;
        writer.predicate(predicate)?;
    }
    for predicate in surface.invariants {
        writer.inline_reference()?;
        writer.predicate(predicate)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::ir::Integer;
    extern crate std;
    use self::std::vec::Vec;
    use super::*;
    use crate::surface::{
        Definition, Operation, Predicate, State, StateHandle, StateTransformer, TransformerHandle,
    };

    struct Switch;
    struct On;
    impl State for On {
        type Appliance = Switch;
        type Value = bool;
        fn read(_: &Switch) -> Option<bool> {
            panic!("exposure must not read hardware")
        }
    }
    static ON: StateHandle<Switch> = StateHandle::new::<On>("on");
    static TRUE: Definition<Switch> = Definition {
        body: Expr::Constant(ir::Value::boolean(true)),
        arguments: &[],
        inputs: &[],
    };
    static REFERENCE: Definition<Switch> = Definition {
        body: Expr::Reference(&TRUE),
        arguments: &[],
        inputs: &[],
    };
    static AXIOM: Predicate<Switch> = Predicate {
        definition: &TRUE,
        inputs: &[],
    };
    static INVARIANT: Predicate<Switch> = Predicate {
        definition: &REFERENCE,
        inputs: &[],
    };
    struct Toggle;
    impl StateTransformer for Toggle {
        type Appliance = Switch;
        type Arguments = ();
        type Error = ();
        const DEFINITION: Operation<Switch> = Operation {
            precondition: Predicate {
                definition: &TRUE,
                inputs: &[],
            },
            effect: ir::Effect {
                inputs: &[ON.definition()],
                outputs: &[ON.definition()],
                results: &[Expr::Not(&Expr::Input(0))],
                arguments: &[],
            },
        };
        fn invoke(_: &Switch, _: ()) -> Result<(), ()> {
            panic!("exposure must not invoke hardware")
        }
    }
    static TOGGLE: TransformerHandle<Switch, ()> = TransformerHandle::new::<Toggle>("toggle");
    static SURFACE: Surface<'static, Switch, ()> = Surface {
        states: &[&ON],
        transformers: &[&TOGGLE],
        axioms: &[&AXIOM],
        invariants: &[&INVARIANT],
    };

    fn reference(out: &mut Vec<u8>, value: u64) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    fn inline(out: &mut Vec<u8>) {
        reference(out, out.len() as u64);
    }

    #[test]
    fn range_encoding() {
        let mut expected = Vec::from(&b"LSF\x02"[..]);
        expected.extend_from_slice(&[64, 1, 0, 1, 0, 1, 0, 1, 0]);
        reference(&mut expected, ON.reference());
        expected.push(0); // Boolean state
        reference(&mut expected, TOGGLE.reference());
        inline(&mut expected);
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 0, 0]); // signatures, true literal, no bound inputs
        expected.extend_from_slice(&[0, 0, 1, 0]); // effect arguments and one input
        reference(&mut expected, ON.reference());
        expected.extend_from_slice(&[0, 1, 0]);
        reference(&mut expected, ON.reference());
        expected.extend_from_slice(&[0, 1, 0, 4, 3, 0, 0]); // one output, Not(Input(0))
        inline(&mut expected);
        inline(&mut expected);
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 0, 0]);
        inline(&mut expected);
        inline(&mut expected);
        expected.extend_from_slice(&[0, 0, 0, 0, 10]);
        inline(&mut expected);
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 0, 0]);
        let description = Description::new(&SURFACE).unwrap();
        assert_eq!(description.len() as usize, expected.len());
        for width in [1, 7, super::super::frame::SURFACE_BYTES, 256] {
            let mut result = Vec::new();
            let mut piece = [0; 256];
            while result.len() < description.len() as usize {
                let n = description.encode_part(result.len() as u16, &mut piece[..width]);
                assert!(n > 0);
                result.extend_from_slice(&piece[..n]);
            }
            assert_eq!(result, expected, "piece size {width}");
        }
    }

    #[test]
    fn scalar_encoding() {
        for (value, expected) in [
            (ir::Value::boolean(false), &[0, 0, 0][..]),
            (
                ir::Value::integer(Integer::I8, -1).unwrap(),
                &[0, 5, 255][..],
            ),
            (
                ir::Value::integer(Integer::U16, 0x1234).unwrap(),
                &[0, 2, 0x34, 0x12][..],
            ),
        ] {
            let mut bytes = [0; 64];
            let mut w = Writer {
                output: &mut bytes,
                offset: 0,
                position: 0,
                copied: 0,
                counting: false,
            };
            w.expression::<Switch>(&Expr::Constant(value), 0).unwrap();
            assert_eq!(&w.output[..w.copied], expected);
        }
        let expression = Expr::Select(
            &Expr::Read(ON.definition()),
            &Expr::Argument(2),
            &Expr::Input(3),
        );
        let mut expected = Vec::from(&[9, 1][..]);
        reference(&mut expected, ON.reference());
        expected.extend_from_slice(&[0, 2, 2, 0, 3, 3, 0]);
        let mut bytes = [0; 64];
        let mut w = Writer {
            output: &mut bytes,
            offset: 0,
            position: 0,
            copied: 0,
            counting: false,
        };
        w.expression(&expression, 0).unwrap();
        assert_eq!(&w.output[..w.copied], expected);
    }

    #[test]
    fn encoding_bounds() {
        static CYCLE: Definition<Switch> = Definition {
            body: Expr::Reference(&CYCLE),
            arguments: &[],
            inputs: &[],
        };
        static P: Predicate<Switch> = Predicate {
            definition: &CYCLE,
            inputs: &[],
        };
        static BAD: Surface<'static, Switch, ()> = Surface {
            states: &[],
            transformers: &[],
            axioms: &[&P],
            invariants: &[],
        };
        static BIG: Surface<'static, Switch, ()> = Surface {
            states: &[],
            transformers: &[],
            axioms: &[&AXIOM; 4096],
            invariants: &[],
        };
        assert!(matches!(Description::new(&BAD), Err(EncodeError::TooDeep)));
        assert!(matches!(Description::new(&BIG), Err(EncodeError::TooLarge)));
    }

    #[test]
    fn range_bounds() {
        let description = Description::new(&SURFACE).unwrap();
        let mut whole = self::std::vec![0; description.len() as usize];
        assert_eq!(description.encode_part(0, &mut whole), whole.len());
        for offset in [0, 1, 13, description.len() - 1, description.len()] {
            let mut first = [0; 19];
            let mut repeated = [0; 19];
            let n = description.encode_part(offset, &mut first);
            assert_eq!(description.encode_part(offset, &mut repeated), n);
            assert_eq!(first, repeated);
            assert_eq!(&first[..n], &whole[offset as usize..offset as usize + n]);
        }
        assert_eq!(
            description.encode_part(description.len() + 1, &mut [0; 19]),
            0
        );
    }

    #[test]
    fn binding_uniqueness() {
        use self::std::boxed::Box;
        for (states, transformers, valid) in [
            (false, false, true),
            (true, false, false),
            (false, true, false),
            (true, true, false),
        ] {
            // Distinct declarations with the same durable name must not alias.
            let on = Box::leak(Box::new(StateHandle::new::<On>("on")));
            let toggle = Box::leak(Box::new(TransformerHandle::new::<Toggle>("toggle")));
            let surface = Box::leak(Box::new(Surface {
                states: if states {
                    Box::leak(Box::new([&ON, &*on]))
                } else {
                    SURFACE.states
                },
                transformers: if transformers {
                    Box::leak(Box::new([&TOGGLE, &*toggle]))
                } else {
                    SURFACE.transformers
                },
                axioms: &[],
                invariants: &[],
            }));
            assert_eq!(
                Description::new(surface).map(|_| ()),
                if valid {
                    Ok(())
                } else {
                    Err(EncodeError::DuplicateBinding)
                }
            );
        }
    }

    #[test]
    fn relocation_identity() {
        use self::std::boxed::Box;
        let mut previous = None;
        for _ in 0..2 {
            // Separately allocated native declarations and mathematical nodes.
            // Relocation changes their addresses, never the encoded definition.
            let on = Box::leak(Box::new(StateHandle::new::<On>("on")));
            let toggle = Box::leak(Box::new(TransformerHandle::new::<Toggle>("toggle")));
            let definition = Box::leak(Box::new(Definition {
                arguments: &[],
                inputs: &[],
                body: Expr::Read(on.definition()),
            }));
            let axiom = Box::leak(Box::new(Predicate {
                definition,
                inputs: &[],
            }));
            let surface = Box::leak(Box::new(Surface {
                states: Box::leak(Box::new([&*on])),
                transformers: Box::leak(Box::new([&*toggle])),
                axioms: Box::leak(Box::new([&*axiom])),
                invariants: &[],
            }));
            let description = Description::new(surface).unwrap();
            let mut bytes = self::std::vec![0; description.len() as usize];
            description.encode_part(0, &mut bytes);
            if let Some((address, old_bytes)) = previous {
                assert_ne!(address, core::ptr::from_ref(on));
                assert_eq!(old_bytes, bytes);
            }
            previous = Some((core::ptr::from_ref(on), bytes));
        }
    }
}
