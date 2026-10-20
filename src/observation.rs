//! Register local observation changes and activate their resident dependents.
//!
//! Dependency links live with the state they depend on. Activation is immediate
//! in the caller's context; there is no graph work queue, polling task, or IR
//! interpreter. The kernel updates installed history and computed state before
//! evaluating installed fragments and running native dependents, using this
//! report's captured values. A callback
//! represents a compiled local activation entrypoint. Bound appliance changes
//! also enter the kernel's fixed outgoing delta ring. Loadable fragment installation remains separate work;
//! this local list is not its wire format or final executable ABI.
//! The resident pointer is only local fanout-list linkage. It is not a DAG entry
//! reference, an upstream peer reference, or a distributed continuation.

#![warn(missing_docs)]

use core::{
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{ir::Value, surface::StateHandle};

/// Whether the report captured the value before its update. An unavailable
/// captured value is different from a report with no before-value at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Previous {
    /// The caller explicitly reported a change without a before-value.
    Unknown,
    /// Value captured before applying the report, possibly unavailable.
    Captured(Option<Value>),
}

/// The values belonging to one registered change, preserved across callbacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    /// Captured pre-update value, or an explicit absence of that information.
    pub before: Previous,
    /// Captured post-update value, possibly unavailable.
    pub after: Option<Value>,
}

/// One resident local activation binding for a state, with its next sibling link.
///
/// Links are immutable and statically resident. Installation code supplies an
/// acyclic list in dispatch order. Each callback receives the captured transition
/// even if an earlier callback has already changed the appliance again.
pub struct Dependent<Appliance: 'static> {
    /// Compiled/native activation entrypoint. It returns without a graph result.
    pub activate: fn(&mut Appliance, &StateHandle<Appliance>, Transition),
    /// The next local dependent of this state, not the next edge of a DAG traversal.
    pub next: Option<&'static Self>,
}

/// The state's pointer to its first local dependent.
pub(crate) struct Dependents<Appliance: 'static> {
    first: AtomicPtr<Dependent<Appliance>>,
}

impl<Appliance: 'static> Dependents<Appliance> {
    pub(crate) const fn new() -> Self {
        Self {
            first: AtomicPtr::new(ptr::null_mut()),
        }
    }

    pub(crate) fn set(&self, first: Option<&'static Dependent<Appliance>>) {
        let first = first.map_or(ptr::null_mut(), |link| ptr::from_ref(link).cast_mut());
        self.first.store(first, Ordering::Release);
    }

    pub(crate) fn dispatch(
        &self,
        appliance: &mut Appliance,
        source: &StateHandle<Appliance>,
        transition: Transition,
    ) {
        let first = self.first.load(Ordering::Acquire);
        // SAFETY: the private atomic is either null or was installed from an
        // immutable 'static reference by set(). Replacement never frees a link.
        // Acquire observes publication, and the loaded list stays valid through
        // every callback even if a callback installs another list for future deltas.
        let mut next = unsafe { first.as_ref() };
        while let Some(dependent) = next {
            (dependent.activate)(appliance, source, transition);
            next = dependent.next;
        }
    }
}

/// Apply a local state update, compare before/after values, and notify dependents.
///
/// The caller supplies the update closure. State readers compare the appliance's
/// values around that update; the kernel handles delta transport for a bound
/// instance and synchronous local notification.
///
/// The array length is inferred from the supplied group: no fixed slot capacity
/// is imposed. Include each affected native state once, including any native
/// derived states. Installed histories/results are maintained automatically.
/// Before/after values exist on this call's stack; only changed post-values enter
/// transport records.
/// This group covers one hardware report, not a snapshot of the distributed graph.
///
/// All updates and comparisons complete before callbacks run. Once detected,
/// a delta is delivered even if an earlier callback changes that state again.
/// Callbacks receive this report's captured values rather than rereading them.
/// `None` becoming available or unavailable also counts as a change.
/// For each changed source, installed fragments run before its native callbacks.
/// Their non-trigger operands are sampled when each fragment is reached.
///
/// The update's return value is preserved, including an error after a partial
/// update: actual observed changes still produce notifications. Call separately
/// for successive observations whose intermediate changes must be represented.
/// Callers must use a context in which their installed callbacks may execute.
pub fn update<Appliance: 'static, R, const N: usize>(
    appliance: &mut Appliance,
    states: &[&StateHandle<Appliance>; N],
    update: impl FnOnce(&mut Appliance) -> R,
) -> R {
    let before = states.map(|state| state.read(appliance));
    let result = update(appliance);
    let after = states.map(|state| state.read(appliance));
    // Register the whole report before callbacks can cause later observations.
    // Otherwise a callback's newer value could precede an older sibling delta.
    for (index, state) in states.iter().enumerate() {
        if before[index] != after[index] {
            state.record_change(appliance, after[index]);
        }
    }
    // Maintenance cannot invoke appliance callbacks. Complete every history and
    // derived value before a callback can register a newer, nested observation.
    #[cfg(feature = "use-mutexes")]
    for (index, state) in states.iter().enumerate() {
        if before[index] != after[index] {
            crate::kernel::state::maintain(
                state,
                appliance,
                Transition {
                    before: Previous::Captured(before[index]),
                    after: after[index],
                },
            );
        }
    }
    for (index, state) in states.iter().enumerate() {
        if before[index] != after[index] {
            state.dispatch_dependents(
                appliance,
                Transition {
                    before: Previous::Captured(before[index]),
                    after: after[index],
                },
            );
        }
    }
    result
}
