//! Appliance state, transformations, and mathematical operating constraints.
//!
//! Implementors provide [`State`] readers and [`StateTransformer`] operations.
//! Their native types determine the scalar boundary; static handles make those
//! implementations enumerable alongside their IR without an allocator or registry.
//! Device-wide axioms and invariants remain visible mathematical predicates.
//! Bind the static [`Surface`] and native instance in `kernel::Appliance` and pass
//! that to `kernel::start` on an MCU port. The kernel serves definitions, invokes
//! native operations, and transmits registered observations without appliance packet handling.
//!
//! This module supplies local declarations and hardware function adapters.
//! The kernel owns acquisition and change registration; firmware supplies no
//! report callback. IR interpretation and program installation are not provided here.

#![forbid(unsafe_code)]
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
    Native(fn(&Appliance) -> Option<Value>),
    #[cfg(feature = "use-mutexes")]
    Retained(crate::kernel::state::Reader<Appliance>),
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
            source: Reader::Retained(crate::kernel::state::Reader::new(index, read)),
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
            Reader::Native(read) => read(appliance),
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

/// One observed state of an encapsulated appliance.
///
/// The native value type supplies the IR type automatically. Readers observe
/// actual appliance state; they must not issue commands or invent a desired
/// value. Kernel acquisition calls the getter independently of native mutations;
/// hardware bindings protect their own shared peripheral access.
pub trait State {
    /// Concrete appliance encapsulation, such as a lamp or valve.
    type Appliance: 'static;
    /// Native scalar type of this state.
    type Value: Scalar;

    /// Current established observation, or `None` if unavailable.
    fn read(appliance: &Self::Appliance) -> Option<Self::Value>;
}

/// An enumerable state definition with a typed native reader.
///
/// The handle stores its IR attribute and kernel-maintained history linkage.
/// It neither owns nor duplicates the appliance's observed value.
pub struct StateHandle<Appliance: 'static> {
    definition: ir::Attribute<Read<Appliance>>,
    #[cfg(feature = "use-mutexes")]
    pub(crate) maintained:
        core::sync::atomic::AtomicPtr<crate::kernel::state::Installation<Appliance>>,
}

impl<Appliance: 'static> StateHandle<Appliance> {
    /// Derive the mathematical type and scalar adapter from a state implementation.
    /// `name` is a permanent appliance binding name, not a display label or Rust
    /// type name. Keep it across compatible releases; change it when repurposing
    /// the physical state. Only its numeric fingerprint is retained/transmitted.
    ///
    /// # Panics
    /// Rejects an empty name or a name containing NUL.
    pub const fn new<S: State<Appliance = Appliance>>(name: &str) -> Self {
        Self {
            definition: ir::Attribute {
                ty: S::Value::TYPE,
                location: Read {
                    reference: binding_reference(1, name, 0),
                    #[cfg(feature = "use-mutexes")]
                    dependencies: core::sync::atomic::AtomicU16::new(u16::MAX),
                    source: Reader::Native(read_state::<S>),
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

fn read_state<S: State>(appliance: &S::Appliance) -> Option<Value> {
    S::read(appliance).map(Scalar::into_value)
}

/// One native operation and its inspectable mathematical meaning.
///
/// Both the IR definition and native implementation are required. The implementor
/// supplies their correspondence; a Rust trait does not prove physical behavior.
pub trait StateTransformer {
    /// Concrete appliance encapsulation acted on by this operation.
    type Appliance: 'static;
    /// Native invocation arguments, decoded by the kernel's scalar adapter.
    type Arguments: Arguments;
    /// Error reported by the appliance operation.
    type Error;

    /// Mathematical precondition and joint state transition for this invocation.
    const DEFINITION: Operation<Self::Appliance>;

    /// Perform the finite hardware operation once, enforcing native limits.
    /// The kernel supplies its task; getters can run concurrently. Protect only
    /// actual shared peripheral access inside the adapter, not the whole call.
    /// Success does not manufacture observations or imply physical convergence.
    ///
    /// # Errors
    /// Returns the appliance's own invocation error.
    fn invoke(appliance: &Self::Appliance, arguments: Self::Arguments) -> Result<(), Self::Error>;
}

/// An invocation was rejected before dispatch, or failed in the appliance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokeError<Error> {
    /// The supplied scalar values do not match the operation's signature.
    Arguments(ArgumentError),
    /// The appliance returned its own error.
    Appliance(Error),
}

type Invoke<Appliance, Error> = fn(&Appliance, &[Value]) -> Result<(), InvokeError<Error>>;

/// An enumerable transformer with its IR definition and native invocation adapter.
/// Each handle owns its operation root; expressions and state references remain
/// borrowed. Sharing mathematical expressions does not merge native callables.
pub struct TransformerHandle<Appliance: 'static, Error> {
    reference: u64,
    definition: Operation<Appliance>,
    invoke: Invoke<Appliance, Error>,
}

impl<Appliance: 'static, Error> TransformerHandle<Appliance, Error> {
    /// Connect a transformer while checking its mathematical/native signatures.
    /// Use in static declarations to reject disagreement at compile time.
    /// `name` identifies this hardware operation across compatible firmware
    /// builds. Keep it for the same physical meaning; give a repurposed operation
    /// a new name. This is not a display label.
    ///
    /// # Panics
    /// Panics if the effect signature differs from the native arguments, or if a
    /// nonempty precondition signature differs from them. In a static initializer
    /// this is a compilation error. Full expression validation belongs to nucleus.
    /// Also rejects an empty name or a name containing NUL.
    pub const fn new<T: StateTransformer<Appliance = Appliance, Error = Error>>(
        name: &str,
    ) -> Self {
        let definition = T::DEFINITION;
        let signature = T::Arguments::TYPES;
        assert!(
            same_signature(definition.effect.arguments, signature),
            "transformer IR and native argument types disagree"
        );
        let precondition = definition.precondition.definition.arguments;
        assert!(
            precondition.is_empty() || same_signature(precondition, signature),
            "transformer precondition and native argument types disagree"
        );
        Self {
            reference: binding_reference(2, name, 0),
            definition,
            invoke: invoke_transformer::<T>,
        }
    }

    /// Durable reference resolved to this build's native invocation adapter.
    pub const fn reference(&self) -> u64 {
        self.reference
    }

    /// Original mathematical operation, including direct state references.
    pub const fn definition(&self) -> &Operation<Appliance> {
        &self.definition
    }

    /// Decode arguments and invoke once. IR conditions are not interpreted here.
    /// Observed changes remain distinct from the operation's modeled effects.
    ///
    /// # Errors
    /// Rejects malformed arguments before dispatch or propagates the appliance error.
    pub fn invoke(
        &self,
        appliance: &Appliance,
        arguments: &[Value],
    ) -> Result<(), InvokeError<Error>> {
        (self.invoke)(appliance, arguments)
    }
}

fn invoke_transformer<T: StateTransformer>(
    appliance: &T::Appliance,
    arguments: &[Value],
) -> Result<(), InvokeError<T::Error>> {
    value::check(T::Arguments::TYPES, arguments).map_err(InvokeError::Arguments)?;
    let arguments = T::Arguments::decode(arguments).map_err(InvokeError::Arguments)?;
    T::invoke(appliance, arguments).map_err(InvokeError::Appliance)
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

/// The appliance's native state and transformer handles with its IR predicates.
///
/// Implements [`ir::Surface`] over the existing handles. Mathematical consumers
/// use that shared interface without depending on the RTOS or invocation adapters.
/// The shared IR defines the roles of axioms, invariants, and preconditions.
///
/// All collections borrow existing definitions. They are enumeration roots, not
/// an ID registry or storage for installed graph edges. Appliances without a
/// particular category declare an empty slice explicitly.
pub struct Surface<'a, Appliance: 'static, Error> {
    /// Exposed appliance states in implementor-defined enumeration order.
    pub states: &'a [&'a StateHandle<Appliance>],
    /// Exposed state-transforming operations.
    pub transformers: &'a [&'a TransformerHandle<Appliance, Error>],
    /// Device-model facts supplied to synthesis as assumptions.
    pub axioms: &'a [&'a Predicate<Appliance>],
    /// Device-wide properties that synthesis must preserve.
    pub invariants: &'a [&'a Predicate<Appliance>],
}

/// An explicitly empty appliance surface, useful for radio-only port checks.
pub static EMPTY: Surface<'static, (), core::convert::Infallible> = Surface {
    states: &[],
    transformers: &[],
    axioms: &[],
    invariants: &[],
};

impl<A, E> Surface<'_, A, E> {
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
            .map(|transformer| transformer.definition())
    }

    fn axioms(&self) -> impl Iterator<Item = &ir::Predicate<'_, '_, Self::Location>> {
        self.axioms.iter().copied()
    }

    fn invariants(&self) -> impl Iterator<Item = &ir::Predicate<'_, '_, Self::Location>> {
        self.invariants.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::binding_reference;
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
}
