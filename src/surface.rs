//! Appliance state, transformations, and mathematical operating constraints.
//!
//! Implementors declare concrete [`crate::kernel::State`] fields and native methods.
//! Their native types determine the scalar boundary; static handles make those
//! implementations enumerable, with optional authored IR, without an allocator or registry.
//! Device-wide axioms and invariants remain visible mathematical predicates.
//! Bind the static [`Surface`] and native instance in `kernel::Appliance` and pass
//! that to `kernel::start` on an MCU port. The kernel serves definitions, invokes
//! native operations, and transmits registered observations without appliance packet handling.
//!
//! This module supplies local declarations and hardware function adapters.
//! Kernel-owned state/event primitives register qualified changes automatically;
//! firmware supplies device behavior, not a report loop. IR interpretation and program installation are not provided here.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod value;
pub use value::{ArgumentError, Arguments, Scalar};

use crate::ir::{self, Type, Value};

// Stable wire identity, not a security hash. The top two bits distinguish native
// state, transformer and retained-state bindings from inline mathematical records.
// FNV-1a over UTF-8 name bytes, a NUL separator and a little-endian u16 member.
// Duplicate references (including hash collisions) are rejected before use.
pub(crate) const fn binding_reference(kind: u64, name: &str, member: u16) -> u64 {
    assert!(!name.is_empty(), "empty appliance binding name");
    let bytes = name.as_bytes();
    let mut hash = 0xcbf29ce484222325u64;
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i] != 0, "NUL in appliance binding name");
        hash = (hash ^ bytes[i] as u64).wrapping_mul(0x100000001b3);
        i += 1;
    }
    hash = hash.wrapping_mul(0x100000001b3);
    hash = (hash ^ (member & 0xff) as u64).wrapping_mul(0x100000001b3);
    hash = (hash ^ (member >> 8) as u64).wrapping_mul(0x100000001b3);
    let reference = (kind << 62) | (hash & 0x3fffffffffffffff);
    // All-ones is reserved by the surface/read codecs.
    if reference == u64::MAX {
        reference - 1
    } else {
        reference
    }
}

/// A scalar state location: a native reader or an installed retained value.
/// Reading never evaluates IR or triggers state maintenance.
pub struct Read<Appliance: 'static> {
    reference: u64,
    #[cfg(feature = "use-mutexes")]
    pub(crate) dependencies: core::sync::atomic::AtomicU16,
    source: Reader<Appliance>,
}
enum Reader<Appliance: 'static> {
    Native(crate::kernel::state::Access<Appliance>),
    #[cfg(feature = "use-mutexes")]
    Retained(crate::kernel::history::Reader<Appliance>),
}
impl<Appliance: 'static> Read<Appliance> {
    #[cfg(feature = "use-mutexes")]
    pub(crate) const fn retained(
        reference: u64,
        index: usize,
        read: unsafe fn(*const (), &Appliance, usize) -> Option<Value>,
    ) -> Self {
        Self {
            reference,
            dependencies: core::sync::atomic::AtomicU16::new(u16::MAX),
            source: Reader::Retained(crate::kernel::history::Reader::new(index, read)),
        }
    }
    /// Durable owner-local binding, independent of this declaration's address.
    pub const fn reference(&self) -> u64 {
        self.reference
    }
    #[cfg(feature = "use-mutexes")]
    pub(crate) fn bind_retained(&self, storage: *mut ()) {
        let Reader::Retained(reader) = &self.source else {
            unreachable!()
        };
        reader.bind(storage);
    }
    /// Read the original state source; this does not execute its IR definition.
    pub fn read(&self, appliance: &Appliance) -> Option<Value> {
        match &self.source {
            Reader::Native(access) => access.read(appliance),
            #[cfg(feature = "use-mutexes")]
            Reader::Retained(reader) => reader.read(appliance),
        }
    }
}

/// A firmware-resident mathematical definition over this appliance's state.
pub type Definition<Appliance> = ir::Definition<'static, 'static, Read<Appliance>>;

/// A mathematical predicate referring directly to its state inputs.
pub type Predicate<Appliance> = ir::Predicate<'static, 'static, Read<Appliance>>;

/// The precondition and coupled effects of one appliance operation.
pub type Operation<Appliance> = ir::Operation<'static, 'static, Read<Appliance>>;

/// An enumerable state definition with a typed native reader.
///
/// The handle stores its IR attribute and kernel-maintained history linkage.
/// It neither owns nor duplicates the appliance's observed value.
pub struct StateHandle<Appliance: 'static> {
    definition: ir::Attribute<Read<Appliance>>,
    #[cfg(feature = "use-mutexes")]
    pub(crate) maintained:
        core::sync::atomic::AtomicPtr<crate::kernel::history::Installation<Appliance>>,
}

impl<Appliance: 'static> StateHandle<Appliance> {
    /// Derive the mathematical type and scalar adapter from a state implementation.
    /// `name` is a permanent appliance binding name, not a display label or Rust
    /// type name. Keep it across compatible releases; change it when repurposing
    /// the physical state. Only its numeric fingerprint is retained/transmitted.
    ///
    /// # Panics
    /// Rejects an empty name or a name containing NUL.
    /// Select one fixed field; it must name the same state object for this
    /// appliance's lifetime. The kernel handles registration automatically.
    pub const fn new<T: Scalar>(
        name: &str,
        field: fn(&Appliance) -> &crate::kernel::State<T>,
    ) -> Self {
        Self::from_access(name, T::TYPE, crate::kernel::state::Access::memory(field))
    }
    /// Declare a bounded hardware/state reader and its qualified change event.
    /// The HAL supplies the read/capture behavior; the kernel connects the event.
    ///
    /// ```compile_fail
    /// use lattice_vertex::{kernel::StateEvent, surface::StateHandle};
    /// struct Device { changed: StateEvent<bool> }
    /// let wrong = StateHandle::<Device>::hardware("power",
    ///     |_| Some(1u8), |device| &device.changed);
    /// ```
    pub const fn hardware<T: Scalar>(
        name: &str,
        reader: fn(&Appliance) -> Option<T>,
        event: fn(&Appliance) -> &crate::kernel::StateEvent<T>,
    ) -> Self {
        Self::from_access(
            name,
            T::TYPE,
            crate::kernel::state::Access::hardware(reader, event),
        )
    }
    const fn from_access(
        name: &str,
        ty: Type,
        access: crate::kernel::state::Access<Appliance>,
    ) -> Self {
        Self {
            definition: ir::Attribute {
                ty,
                location: Read {
                    reference: binding_reference(1, name, 0),
                    #[cfg(feature = "use-mutexes")]
                    dependencies: core::sync::atomic::AtomicU16::new(u16::MAX),
                    source: Reader::Native(access),
                },
            },
            #[cfg(feature = "use-mutexes")]
            maintained: core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// Durable reference used by state reads, deltas and installed bindings.
    pub const fn reference(&self) -> u64 {
        self.definition.location.reference()
    }

    /// The original state definition, for direct references in expressions/effects.
    pub const fn definition(&self) -> &ir::Attribute<Read<Appliance>> {
        &self.definition
    }

    /// Read live state. Unavailable observations remain distinct from false or zero.
    pub fn read(&self, appliance: &Appliance) -> Option<Value> {
        self.definition.location.read(appliance)
    }

    #[cfg(feature = "use-mutexes")]
    pub(crate) fn bind(
        &self,
        appliance: &'static Appliance,
        events: &'static crate::kernel::appliance::changes::Events,
        index: u16,
    ) {
        let Reader::Native(access) = &self.definition.location.source else {
            unreachable!()
        };
        access.connect(appliance, events, index);
    }

    pub(crate) fn record_change(&self, appliance: &Appliance, value: Option<Value>) {
        #[cfg(all(
            feature = "use-mutexes",
            any(
                feature = "port-esp32c6",
                feature = "port-nrf52840",
                feature = "port-test"
            )
        ))]
        crate::kernel::change::record(
            core::ptr::from_ref(appliance).cast(),
            self.reference(),
            value,
        );
        let _ = (appliance, value);
    }
}

/// One native operation and its inspectable mathematical meaning.
///
/// This optional behavioral interface supplies native code and its argument type.
/// Mathematical metadata is attached separately; this trait does not infer it.
pub trait StateTransformer {
    /// Concrete appliance encapsulation acted on by this operation.
    type Appliance: 'static;
    /// Native invocation arguments, decoded by the kernel's scalar adapter.
    type Arguments: Arguments;
    /// Error reported by the appliance operation.
    type Error;

    /// Perform the finite hardware operation once, enforcing native limits.
    /// The kernel supplies its task; getters can run concurrently. Protect only
    /// actual shared peripheral access inside the adapter, not the whole call.
    /// Success does not manufacture observations or imply physical convergence.
    ///
    /// # Errors
    /// Returns the appliance's own invocation error.
    fn invoke(
        context: &crate::sync::TaskContext,
        appliance: &Self::Appliance,
        arguments: Self::Arguments,
    ) -> Result<(), Self::Error>;
}

/// An invocation was rejected before dispatch, or failed in the appliance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokeError<Error> {
    /// The kernel cannot admit physical work while an update/flash step is active.
    Busy,
    /// The supplied scalar values do not match the operation's signature.
    Arguments(ArgumentError),
    /// The appliance returned its own error.
    Appliance(Error),
}

type Invoke<Appliance, Error> = unsafe fn(
    *const (),
    &crate::sync::TaskContext,
    &Appliance,
    &[Value],
) -> Result<(), InvokeError<Error>>;

/// A declared native callable with a signature, optional model and invocation adapter.
/// Authored models remain borrowed. Sharing a model does not merge native callables.
pub struct TransformerHandle<Appliance: 'static, Error> {
    reference: u64,
    arguments: &'static [Type],
    model: Option<&'static Operation<Appliance>>,
    function: *const (),
    invoke: Invoke<Appliance, Error>,
}

// Only immutable function pointers and authored metadata are retained, not an
// appliance instance. Invocation still requires a valid shared appliance reference.
unsafe impl<A, E> Sync for TransformerHandle<A, E> {}

impl<Appliance: 'static, Error> TransformerHandle<Appliance, Error> {
    /// Declare a behavior supplied through the trait. No mathematical model is inferred.
    pub const fn new<T: StateTransformer<Appliance = Appliance, Error = Error>>(
        name: &str,
    ) -> Self {
        Self {
            reference: binding_reference(2, name, 0),
            arguments: T::Arguments::TYPES,
            model: None,
            function: core::ptr::null(),
            invoke: invoke_transformer::<T>,
        }
    }
    /// Declare an ordinary firmware method directly. The native argument type
    /// supplies its signature; no marker type, forwarding trait or IR is required.
    pub const fn from_fn<Args: Arguments>(
        name: &str,
        function: fn(&Appliance, &crate::sync::TaskContext, Args) -> Result<(), Error>,
    ) -> Self {
        Self {
            reference: binding_reference(2, name, 0),
            arguments: Args::TYPES,
            model: None,
            function: function as *const (),
            invoke: invoke_function::<Appliance, Args, Error>,
        }
    }
    /// Attach an explicit authored mathematical model. Signature agreement is a
    /// representation check, not proof that the model describes the hardware.
    pub const fn with_model(mut self, model: &'static Operation<Appliance>) -> Self {
        assert!(
            same_signature(model.effect.arguments, self.arguments),
            "model/native argument types disagree"
        );
        assert!(
            model.precondition.definition.arguments.is_empty()
                || same_signature(model.precondition.definition.arguments, self.arguments),
            "precondition/native argument types disagree"
        );
        self.model = Some(model);
        self
    }
    /// Native signature, available independently of any mathematical description.
    pub const fn arguments(&self) -> &'static [Type] {
        self.arguments
    }

    /// Durable reference resolved to this build's native invocation adapter.
    pub const fn reference(&self) -> u64 {
        self.reference
    }

    /// Explicit authored model, if supplied. None means unknown effects, not none.
    pub const fn definition(&self) -> Option<&Operation<Appliance>> {
        self.model
    }

    /// Decode arguments and invoke once. IR conditions are not interpreted here.
    /// Observed changes remain distinct from the operation's modeled effects.
    ///
    /// # Errors
    /// Rejects malformed arguments before dispatch or propagates the appliance error.
    pub fn invoke(
        &self,
        context: &crate::sync::TaskContext,
        appliance: &Appliance,
        arguments: &[Value],
    ) -> Result<(), InvokeError<Error>> {
        // Constructors pair each function pointer with its exact typed adapter.
        unsafe { (self.invoke)(self.function, context, appliance, arguments) }
    }
}

unsafe fn invoke_transformer<T: StateTransformer>(
    _: *const (),
    context: &crate::sync::TaskContext,
    appliance: &T::Appliance,
    arguments: &[Value],
) -> Result<(), InvokeError<T::Error>> {
    value::check(T::Arguments::TYPES, arguments).map_err(InvokeError::Arguments)?;
    let arguments = T::Arguments::decode(arguments).map_err(InvokeError::Arguments)?;
    T::invoke(context, appliance, arguments).map_err(InvokeError::Appliance)
}

unsafe fn invoke_function<A, Args: Arguments, E>(
    function: *const (),
    context: &crate::sync::TaskContext,
    appliance: &A,
    values: &[Value],
) -> Result<(), InvokeError<E>> {
    value::check(Args::TYPES, values).map_err(InvokeError::Arguments)?;
    let arguments = Args::decode(values).map_err(InvokeError::Arguments)?;
    // The private pointer only originates in from_fn::<Args> and is restored to
    // that same signature. Noncapturing methods/closures require no heap storage.
    let function: fn(&A, &crate::sync::TaskContext, Args) -> Result<(), E> =
        unsafe { core::mem::transmute(function) };
    function(appliance, context, arguments).map_err(InvokeError::Appliance)
}

const fn same_signature(left: &[Type], right: &[Type]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut i = 0;
    while i < left.len() {
        let same = match (left[i], right[i]) {
            (Type::Bool, Type::Bool) => true,
            (Type::Integer(a), Type::Integer(b)) => {
                a.width() == b.width() && a.signed() == b.signed()
            }
            _ => false,
        };
        if !same {
            return false;
        }
        i += 1;
    }
    true
}

/// The appliance's operational declarations with optional authored mathematics.
///
/// Implements [`ir::Surface`] over explicitly modeled operations only. The native
/// callable catalog remains separate and must not be inferred from that subset. Mathematical consumers
/// use that shared interface without depending on the RTOS or invocation adapters.
/// The shared IR defines the roles of axioms, invariants, and preconditions.
///
/// All collections borrow existing definitions. They are enumeration roots, not
/// an ID registry or storage for installed graph edges. Appliances without a
/// particular category can use an empty slice. Surface::new supplies no IR model.
pub struct Surface<'a, Appliance: 'static, Error> {
    /// Exposed appliance states in implementor-defined enumeration order.
    pub states: &'a [&'a StateHandle<Appliance>],
    /// Exposed state-transforming operations.
    pub transformers: &'a [&'a TransformerHandle<Appliance, Error>],
    /// Optional authored model. Absent means unknown, not unrestricted/no effects.
    pub model: Option<&'a IrModel<'a, Appliance>>,
}

/// Optional appliance mathematics, authored outside the runtime. A complete
/// declaration is advertised for nucleus review; the kernel does not certify it.
pub struct IrModel<'a, A: 'static> {
    /// The implementor considers this model complete enough for synthesis review.
    pub complete: bool,
    /// Supplied facts and relationships.
    pub axioms: &'a [&'a Predicate<A>],
    /// Authored operating invariants.
    pub invariants: &'a [&'a Predicate<A>],
}
/// A radio-only participant has no mathematical model to advertise.
pub static EMPTY: Surface<'static, (), core::convert::Infallible> = Surface::new(&[], &[]);

impl<'a, A, E> Surface<'a, A, E> {
    /// Declare operational members with no default or generated IR model.
    pub const fn new(
        states: &'a [&'a StateHandle<A>],
        transformers: &'a [&'a TransformerHandle<A, E>],
    ) -> Self {
        Self {
            states,
            transformers,
            model: None,
        }
    }
    /// Attach authored appliance mathematics; native controls remain independent.
    pub const fn with_model(mut self, model: &'a IrModel<'a, A>) -> Self {
        self.model = Some(model);
        self
    }
    /// Cheap declaration for discovery. Missing operation models cannot be
    /// advertised as complete, even if the appliance declaration claims completeness.
    pub fn ir_status(&self) -> lattice_peer_wire::IrStatus {
        use lattice_peer_wire::IrStatus::*;
        match self.model {
            Some(model)
                if model.complete && self.transformers.iter().all(|t| t.definition().is_some()) =>
            {
                DeclaredComplete
            }
            Some(_) => Incomplete,
            None if self.transformers.iter().any(|t| t.definition().is_some()) => Incomplete,
            None => Absent,
        }
    }
    /// Authored facts, with no inferred defaults.
    pub fn axioms(&self) -> &[&Predicate<A>] {
        self.model.map_or(&[], |m| m.axioms)
    }
    /// Authored invariants, with no inferred defaults.
    pub fn invariants(&self) -> &[&Predicate<A>] {
        self.model.map_or(&[], |m| m.invariants)
    }
    pub(crate) const fn unique_bindings(&self) -> bool {
        let mut i = 0;
        while i < self.states.len() {
            let mut j = 0;
            while j < i {
                if self.states[i].reference() == self.states[j].reference() {
                    return false;
                }
                j += 1;
            }
            i += 1;
        }
        i = 0;
        while i < self.transformers.len() {
            let mut j = 0;
            while j < i {
                if self.transformers[i].reference() == self.transformers[j].reference() {
                    return false;
                }
                j += 1;
            }
            i += 1;
        }
        true
    }
}

impl<Appliance: 'static, Error> ir::Surface for Surface<'_, Appliance, Error> {
    type Location = Read<Appliance>;

    fn states(&self) -> impl Iterator<Item = &ir::Attribute<Self::Location>> {
        self.states.iter().map(|state| state.definition())
    }

    fn transformers(&self) -> impl Iterator<Item = &ir::Operation<'_, '_, Self::Location>> {
        self.transformers
            .iter()
            .filter_map(|transformer| transformer.definition())
    }

    fn axioms(&self) -> impl Iterator<Item = &ir::Predicate<'_, '_, Self::Location>> {
        self.axioms().iter().copied()
    }

    fn invariants(&self) -> impl Iterator<Item = &ir::Predicate<'_, '_, Self::Location>> {
        self.invariants().iter().copied()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    #[test]
    fn binding_encoding() {
        for (kind, name, member, expected) in [
            (1, "power", 0, 0x4bae567d81cc09a2),
            (2, "set-power", 0, 0xadc296a9111fe909),
            (3, "level-history", 0, 0xfaefc52f30099058),
            (3, "level-history", u16::MAX, 0xfa4a342f2f7db5ce),
        ] {
            assert_eq!(binding_reference(kind, name, member), expected);
            assert_eq!(expected >> 62, kind);
        }
    }

    #[test]
    fn signature_admission() {
        use core::cell::Cell;
        // A custom decoder that accepts anything must not bypass the native signature.
        struct Permissive;
        impl Arguments for Permissive {
            const TYPES: &'static [Type] = <(bool, u8)>::TYPES;
            fn decode(_: &[Value]) -> Result<Self, ArgumentError> {
                Ok(Self)
            }
        }
        struct Behavior;
        impl StateTransformer for Behavior {
            type Appliance = Cell<usize>;
            type Arguments = Permissive;
            type Error = ();
            fn invoke(
                _: &crate::sync::TaskContext,
                calls: &Cell<usize>,
                _: Permissive,
            ) -> Result<(), ()> {
                calls.set(calls.get() + 1);
                Ok(())
            }
        }
        let declared = TransformerHandle::new::<Behavior>("behavior");
        let direct =
            TransformerHandle::from_fn("method", |calls: &Cell<usize>, _, _: Permissive| {
                calls.set(calls.get() + 1);
                Ok(())
            });
        // No blocking, scheduler operation or hardware access occurs in these hooks.
        let context = unsafe { crate::sync::TaskContext::assume() };
        let valid = [true.into_value(), 128_u8.into_value()];
        for callable in [&declared, &direct] {
            for (arguments, admitted) in [
                (&[][..], false),
                (&valid[..1], false),
                (&[valid[0], valid[1], valid[0]][..], false),
                (&[1_u8.into_value(), valid[1]][..], false),
                (&[valid[0], 128_u16.into_value()][..], false),
                (&[valid[0], 127_i8.into_value()][..], false),
                (&valid[..], true),
            ] {
                let calls = Cell::new(0);
                let result = callable.invoke(&context, &calls, arguments);
                assert_eq!(result.is_ok(), admitted);
                assert_eq!(calls.get(), usize::from(admitted));
                if !admitted {
                    assert!(matches!(result, Err(InvokeError::Arguments(_))));
                }
            }
        }
    }

    #[test]
    fn model_signature_bounds() {
        const TYPES: &[Type] = <(bool, u8)>::TYPES;
        const PERMITTED: Definition<()> = Definition {
            body: ir::Expr::Constant(Value::boolean(true)),
            arguments: &[],
            inputs: &[],
        };
        const ACCEPTS_ARGS: Definition<()> = Definition {
            arguments: TYPES,
            ..PERMITTED
        };
        const MISMATCHED: Definition<()> = Definition {
            arguments: &[Type::Bool],
            ..PERMITTED
        };
        const fn model(
            arguments: &'static [Type],
            definition: &'static Definition<()>,
        ) -> Operation<()> {
            Operation {
                precondition: Predicate {
                    definition,
                    inputs: &[],
                },
                effect: ir::Effect {
                    arguments,
                    inputs: &[],
                    outputs: &[],
                    results: &[],
                },
            }
        }
        static CASES: [(Operation<()>, bool); 4] = [
            (model(TYPES, &PERMITTED), true),
            (model(TYPES, &ACCEPTS_ARGS), true),
            (model(&[Type::Bool], &PERMITTED), false),
            (model(TYPES, &MISMATCHED), false),
        ];
        for (model, accepted) in &CASES {
            let result = std::panic::catch_unwind(|| {
                TransformerHandle::from_fn("method", |_: &(), _, _: (bool, u8)| Ok::<_, ()>(()))
                    .with_model(model)
            });
            assert_eq!(result.is_ok(), *accepted);
        }
    }
}
