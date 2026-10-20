//! Compiled local conditions and local/peer transformer continuations.
//!
//! An installed fragment responds synchronously to its native trigger after the
//! report's retained state has been maintained. It captures declared operands,
//! evaluates compiled code, then invokes locally or submits one encoded peer call.
//! No mathematical IR is interpreted and no graph work is queued.
//! Installations are local and immutable; remote code installation is separate work.
//! Compiler-supplied code and continuation relationships must terminate.

use super::InstallError;
use crate::{
    ir::{Attribute, Type, Value},
    observation::Transition,
    surface::{InvokeError, Read, StateHandle, TransformerHandle},
};
use core::{
    ptr,
    sync::atomic::{AtomicPtr, AtomicU8, Ordering},
};

/// Outcome of the last local activation/handoff, not a physical-state result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Outcome {
    /// No change has activated this fragment.
    NotActivated,
    /// At least one declared operand was unavailable; no calculation ran.
    Unavailable,
    /// The compiled condition did not yield continuation arguments.
    Inactive,
    /// The native transformer accepted the invocation.
    Invoked,
    /// Computed argument types did not match the bound transformer.
    Arguments,
    /// The appliance reported an invocation error; it is never retried here.
    ApplianceError,
    /// Encoded arguments were handed to bounded transport, not yet confirmed.
    Submitted,
    /// The peer runtime has not formed/joined a network yet.
    NotReady,
    /// A peer continuation names this vertex or an invalid address/reference.
    InvalidTarget,
    /// This boot has exhausted its invocation ID space; IDs are never recycled.
    IdentityExhausted,
}

/// A peer-owned transformer reference and its declared scalar argument types.
/// The installer obtains these from the owner's surface. The receiver remains
/// authoritative for the reference, signature and native hardware limits.
pub struct RemoteTransformer<const N: usize> {
    /// Address of the peer that owns the transformer.
    pub owner: u64,
    /// Owner-local reference learned from its exposed surface; never dereferenced here.
    pub transformer: u64,
    /// Exact declared scalar types, in argument order.
    pub arguments: [Type; N],
}
pub(super) enum Continuation<A: 'static, E: 'static, const N: usize> {
    Local(&'static TransformerHandle<A, E>),
    Peer(RemoteTransformer<N>),
}

/// A fixed local fragment, supplied by the compiler/installer before startup.
///
/// The compiled function sees the captured trigger and declared scalar operands.
/// `None` stops this activation; `Some` supplies the target's arguments. Inputs
/// may reference native or already-installed retained state of the same owner.
/// Fragments do not add hardware capabilities or new surface declarations.
///
/// A declared trigger operand uses the captured post-value. Other inputs are
/// read when this fragment is reached, then held unchanged for its evaluation.
/// Siblings execute in installation order before this source's native callbacks;
/// earlier continuations may change non-trigger inputs of later fragments. This
/// is sequential local execution, not a frozen snapshot of an entire report.
pub struct Fragment<A: 'static, E: 'static, const INPUTS: usize, const ARGUMENTS: usize> {
    pub(super) source: &'static StateHandle<A>,
    pub(super) inputs: [&'static Attribute<Read<A>>; INPUTS],
    pub(super) target: Continuation<A, E, ARGUMENTS>,
    evaluate: fn(Transition, &[Value; INPUTS]) -> Option<[Value; ARGUMENTS]>,
    owner: AtomicPtr<A>,
    outcome: AtomicU8,
    binding: Binding<A>,
}

impl<A, E, const I: usize, const O: usize> Fragment<A, E, I, O> {
    /// Reserve an immutable local condition/continuation; this allocates nothing
    /// and does not activate it. Use `Appliance::install_fragment` to bind it.
    pub const fn new(
        source: &'static StateHandle<A>,
        inputs: [&'static Attribute<Read<A>>; I],
        evaluate: fn(Transition, &[Value; I]) -> Option<[Value; O]>,
        target: &'static TransformerHandle<A, E>,
    ) -> Self {
        Self::from_parts(source, inputs, evaluate, Continuation::Local(target))
    }

    /// Reserve a fragment whose computed arguments go to a peer transformer.
    /// Each activation must fit one existing invocation frame. It does not
    /// install code at the receiver or route through a tablet/nucleus.
    pub const fn to_peer(
        source: &'static StateHandle<A>,
        inputs: [&'static Attribute<Read<A>>; I],
        evaluate: fn(Transition, &[Value; I]) -> Option<[Value; O]>,
        target: RemoteTransformer<O>,
    ) -> Self {
        Self::from_parts(source, inputs, evaluate, Continuation::Peer(target))
    }

    const fn from_parts(
        source: &'static StateHandle<A>,
        inputs: [&'static Attribute<Read<A>>; I],
        evaluate: fn(Transition, &[Value; I]) -> Option<[Value; O]>,
        target: Continuation<A, E, O>,
    ) -> Self {
        Self {
            source,
            inputs,
            target,
            evaluate,
            owner: AtomicPtr::new(ptr::null_mut()),
            outcome: AtomicU8::new(Outcome::NotActivated as u8),
            binding: Binding {
                storage: AtomicPtr::new(ptr::null_mut()),
                next: AtomicPtr::new(ptr::null_mut()),
                activate: activate::<A, E, I, O>,
            },
        }
    }

    /// Last local outcome. `Submitted` records admission to transport only: it
    /// remains so after eviction, transmission failure or a peer response. This
    /// is one diagnostic value, not a remote result or an event log.
    pub fn outcome(&self) -> Outcome {
        match self.outcome.load(Ordering::Acquire) {
            0 => Outcome::NotActivated,
            1 => Outcome::Unavailable,
            2 => Outcome::Inactive,
            3 => Outcome::Invoked,
            4 => Outcome::Arguments,
            5 => Outcome::ApplianceError,
            6 => Outcome::Submitted,
            7 => Outcome::NotReady,
            8 => Outcome::InvalidTarget,
            9 => Outcome::IdentityExhausted,
            _ => unreachable!(),
        }
    }

    pub(super) fn install(&'static self, appliance: &A) -> Result<(), InstallError> {
        self.owner
            .compare_exchange(
                ptr::null_mut(),
                ptr::from_ref(appliance).cast_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| InstallError::AlreadyInstalled)?;
        self.binding
            .storage
            .store(ptr::from_ref(self).cast_mut().cast(), Ordering::Relaxed);
        // Append in registration order. Each immutable resident binding is
        // published once; replacement of firmware callbacks cannot remove it.
        let mut tail = &self.source.fragments;
        loop {
            match tail.compare_exchange(
                ptr::null_mut(),
                ptr::from_ref(&self.binding).cast_mut(),
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                // Every non-null link was published from a permanent binding.
                Err(next) => tail = unsafe { &(*next).next },
            }
        }
    }

    fn evaluate_and_invoke(&self, appliance: &mut A, transition: Transition) -> Outcome {
        let mut operands = [Value::boolean(false); I];
        for (attribute, operand) in self.inputs.iter().zip(&mut operands) {
            let value = if ptr::eq(*attribute, self.source.definition()) {
                transition.after
            } else {
                attribute.location.read(appliance)
            };
            let Some(value) = value else {
                return Outcome::Unavailable;
            };
            *operand = value;
        }
        let Some(arguments) = (self.evaluate)(transition, &operands) else {
            return Outcome::Inactive;
        };
        match &self.target {
            Continuation::Local(target) => match target.invoke(appliance, &arguments) {
                Ok(()) => Outcome::Invoked,
                Err(InvokeError::Arguments(_)) => Outcome::Arguments,
                Err(InvokeError::Appliance(_)) => Outcome::ApplianceError,
            },
            Continuation::Peer(target) => {
                if arguments
                    .iter()
                    .zip(target.arguments)
                    .any(|(v, ty)| v.ty() != ty)
                {
                    return Outcome::Arguments;
                }
                use super::peer::continuation::{submit, SubmitError};
                match submit(target.owner, target.transformer, &arguments) {
                    Ok(()) => Outcome::Submitted,
                    Err(SubmitError::NotReady) => Outcome::NotReady,
                    Err(SubmitError::Target) => Outcome::InvalidTarget,
                    Err(SubmitError::Arguments) => Outcome::Arguments,
                    Err(SubmitError::Exhausted) => Outcome::IdentityExhausted,
                }
            }
        }
    }
}

/// Number of already-evaluated continuation packets lost to transport overflow.
/// This does not count RF loss or establish whether a peer executed a call.
pub fn evicted_continuations() -> u32 {
    super::peer::continuation::evicted()
}

// Type-erased linkage only; the concrete fragment keeps its references and
// compile-time input/output extents. Publication establishes the storage type.
pub(crate) struct Binding<A> {
    storage: AtomicPtr<()>,
    next: AtomicPtr<Self>,
    activate: unsafe fn(*const (), &mut A, Transition),
}

unsafe fn activate<A: 'static, E: 'static, const I: usize, const O: usize>(
    storage: *const (),
    appliance: &mut A,
    transition: Transition,
) {
    // Only Fragment<A,E,I,O>::install publishes this callback/storage pair.
    let fragment = unsafe { &*storage.cast::<Fragment<A, E, I, O>>() };
    if fragment.owner.load(Ordering::Acquire).cast_const() == ptr::from_ref(appliance) {
        let outcome = fragment.evaluate_and_invoke(appliance, transition);
        fragment.outcome.store(outcome as u8, Ordering::Release);
    }
}

pub(crate) fn dispatch<A>(source: &StateHandle<A>, appliance: &mut A, transition: Transition) {
    let mut next = source.fragments.load(Ordering::Acquire);
    while let Some(binding) = unsafe { next.as_ref() } {
        unsafe {
            (binding.activate)(
                binding.storage.load(Ordering::Relaxed).cast_const(),
                appliance,
                transition,
            );
        }
        next = binding.next.load(Ordering::Acquire);
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests;
