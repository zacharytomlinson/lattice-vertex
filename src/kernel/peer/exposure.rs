//! Encode this appliance's immutable IR surface for transient peer publication.
//! No reader, transformer, or IR expression is executed. References identify
//! original local declarations; no entity IDs or remote-surface catalog is built.

use crate::{
    ir::{self, Binary, Expr, Integer, Type, Unary},
    surface::{Read, Surface},
};

const MAX_DEPTH: u8 = 32;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EncodeError {
    TooLarge,
    TooDeep,
    Full,
}

trait Encode: Sync {
    fn encode(&self, writer: &mut Writer<'_>) -> Result<(), EncodeError>;
}

/// Borrow the authored surface and its encoded length. Byte ranges are emitted
/// on demand into the existing frame buffer, without a second resident IR copy.
pub(crate) struct Publication {
    source: &'static dyn Encode,
    length: u16,
}

impl Publication {
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

    pub(super) fn len(&self) -> u16 {
        self.length
    }

    pub(super) fn encode_part(&self, offset: u16, output: &mut [u8]) -> usize {
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
    fn reference<T>(&mut self, object: &T) -> Result<(), EncodeError> {
        self.bytes(&(core::ptr::from_ref(object) as usize as u64).to_le_bytes())
    }
    fn ty(&mut self, ty: Type) -> Result<(), EncodeError> {
        self.byte(match ty {
            Type::Bool => 0,
            Type::Integer(i) => integer_tag(i),
        })
    }
    fn types(&mut self, types: &[Type]) -> Result<(), EncodeError> {
        self.count(types.len())?;
        for ty in types {
            self.ty(*ty)?;
        }
        Ok(())
    }
    fn attribute<A>(&mut self, attribute: &ir::Attribute<Read<A>>) -> Result<(), EncodeError> {
        self.reference(attribute)?;
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
        self.reference(definition)?;
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
                self.ty(value.ty())?;
                match value.ty() {
                    Type::Bool => self.byte(u8::from(value.as_bool().unwrap())),
                    Type::Integer(ty) => self.bytes(
                        &(value.as_integer().unwrap() as u64).to_le_bytes()
                            [..ty.width() as usize / 8],
                    ),
                }
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
                        self.byte(integer_tag(*ty))?;
                    }
                    Unary::SignExtend(ty) => {
                        self.byte(3)?;
                        self.byte(integer_tag(*ty))?;
                    }
                    Unary::Truncate(ty) => {
                        self.byte(4)?;
                        self.byte(integer_tag(*ty))?;
                    }
                    Unary::Reinterpret(ty) => {
                        self.byte(5)?;
                        self.byte(integer_tag(*ty))?;
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

fn integer_tag(ty: Integer) -> u8 {
    match ty {
        Integer::U8 => 1,
        Integer::U16 => 2,
        Integer::U32 => 3,
        Integer::U64 => 4,
        Integer::I8 => 5,
        Integer::I16 => 6,
        Integer::I32 => 7,
        Integer::I64 => 8,
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
        writer.bytes(b"LSF\x01")?;
        writer.byte(usize::BITS as u8)?;
        for count in [
            self.states.len(),
            self.transformers.len(),
            self.axioms.len(),
            self.invariants.len(),
        ] {
            writer.count(count)?;
        }
        for state in self.states {
            writer.attribute(state.definition())?;
        }
        for transformer in self.transformers {
            writer.reference(*transformer)?;
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
        for predicates in [self.axioms, self.invariants] {
            for predicate in predicates {
                writer.reference(*predicate)?;
                writer.predicate(predicate)?;
            }
        }
        Ok(())
    }
}

/// Only local transmission progress is retained. Arrivals during publication
/// request a subsequent full pass; incoming surface pieces never trigger one.
pub(super) struct Publisher {
    pub(super) publication: Publication,
    pub(super) sequence: u16,
    pub(super) offset: u16,
    pending: bool,
    active: bool,
}

impl Publisher {
    pub(super) fn new(publication: Publication) -> Self {
        Self {
            publication,
            sequence: 0,
            offset: 0,
            pending: true,
            active: false,
        }
    }
    pub(super) fn arrival(&mut self) {
        self.pending = true;
    }
    pub(super) fn has_work(&self) -> bool {
        self.pending || self.active
    }
    pub(super) fn prepare_part(&mut self, output: &mut [u8]) -> usize {
        if !self.active {
            if !self.pending {
                return 0;
            }
            self.pending = false;
            self.active = true;
            self.offset = 0;
            self.sequence = self.sequence.wrapping_add(1);
        }
        self.publication.encode_part(self.offset, output)
    }
    pub(super) fn transmitted(&mut self, length: usize) {
        self.offset += length as u16;
        if self.offset == self.publication.len() {
            self.active = false;
        }
    }
    pub(super) fn transmit_failed(&mut self) {
        self.active = false;
    }
}

#[cfg(test)]
mod tests {
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
            panic!("publication must not read hardware")
        }
    }
    static ON: StateHandle<Switch> = StateHandle::new::<On>();
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
        fn invoke(_: &mut Switch, _: ()) -> Result<(), ()> {
            panic!("publication must not invoke hardware")
        }
    }
    static TOGGLE: TransformerHandle<Switch, ()> = TransformerHandle::new::<Toggle>();
    static SURFACE: Surface<'static, Switch, ()> = Surface {
        states: &[&ON],
        transformers: &[&TOGGLE],
        axioms: &[&AXIOM],
        invariants: &[&INVARIANT],
    };

    fn reference<T>(out: &mut Vec<u8>, value: &T) {
        out.extend_from_slice(&(core::ptr::from_ref(value) as usize as u64).to_le_bytes());
    }

    #[test]
    fn mathematical_surface_matches_wire_vector_across_fragment_boundaries() {
        let mut expected = Vec::from(&b"LSF\x01"[..]);
        expected.extend_from_slice(&[usize::BITS as u8, 1, 0, 1, 0, 1, 0, 1, 0]);
        reference(&mut expected, ON.definition());
        expected.push(0); // Boolean state
        reference(&mut expected, &TOGGLE);
        reference(&mut expected, &TRUE);
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 0, 0]); // signatures, true literal, no bound inputs
        expected.extend_from_slice(&[0, 0, 1, 0]); // effect arguments and one input
        reference(&mut expected, ON.definition());
        expected.extend_from_slice(&[0, 1, 0]);
        reference(&mut expected, ON.definition());
        expected.extend_from_slice(&[0, 1, 0, 4, 3, 0, 0]); // one output, Not(Input(0))
        reference(&mut expected, &AXIOM);
        reference(&mut expected, &TRUE);
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 0, 0]);
        reference(&mut expected, &INVARIANT);
        reference(&mut expected, &REFERENCE);
        expected.extend_from_slice(&[0, 0, 0, 0, 10]);
        reference(&mut expected, &TRUE);
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 0, 0]);
        let publication = Publication::new(&SURFACE).unwrap();
        assert_eq!(publication.len() as usize, expected.len());
        for width in [1, 7, super::super::frame::SURFACE_BYTES, 256] {
            let mut result = Vec::new();
            let mut piece = [0; 256];
            while result.len() < publication.len() as usize {
                let n = publication.encode_part(result.len() as u16, &mut piece[..width]);
                assert!(n > 0);
                result.extend_from_slice(&piece[..n]);
            }
            assert_eq!(result, expected, "piece size {width}");
        }
    }

    #[test]
    fn literal_width_signedness_and_expression_control_remain_explicit() {
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
        reference(&mut expected, ON.definition());
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
    fn startup_rejects_unbounded_definitions_before_transmitting() {
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
            states: &[&ON; 8192],
            transformers: &[],
            axioms: &[],
            invariants: &[],
        };
        assert!(matches!(Publication::new(&BAD), Err(EncodeError::TooDeep)));
        assert!(matches!(Publication::new(&BIG), Err(EncodeError::TooLarge)));
    }

    #[test]
    fn publication_is_finite_and_arrival_during_send_requests_one_fresh_pass() {
        let mut publisher = Publisher::new(Publication::new(&SURFACE).unwrap());
        let mut buffer = [0; super::super::frame::SURFACE_BYTES];
        let n = publisher.prepare_part(&mut buffer);
        publisher.arrival();
        publisher.transmitted(n);
        while publisher.offset < publisher.publication.len() {
            let n = publisher.prepare_part(&mut buffer);
            publisher.transmitted(n);
        }
        assert!(publisher.has_work());
        let n = publisher.prepare_part(&mut buffer);
        assert_eq!(publisher.offset, 0);
        assert_eq!(publisher.sequence, 2);
        publisher.transmitted(n);
        while publisher.has_work() {
            let n = publisher.prepare_part(&mut buffer);
            publisher.transmitted(n);
        }
        assert_eq!(publisher.prepare_part(&mut buffer), 0);
        publisher.arrival();
        publisher.prepare_part(&mut buffer);
        publisher.transmit_failed();
        assert!(!publisher.has_work());
    }
}
