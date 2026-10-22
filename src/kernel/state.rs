//! Firmware-bound history and derived state.
//!
//! Storage is supplied as a static, typed record. Installation binds it once to
//! an initialized appliance, before kernel startup. Each registered source change
//! appends one captured post-value and runs its compiled calculation. No IR is
//! interpreted, hardware is reread, or graph work is queued.
//!
//! History age zero is the newest registered observation. Missing samples and
//! unavailable observations read as None; len distinguishes occupied history.
//! The age views and computed value are read-only mathematical states. Their
//! changes use normal delta transport; local cascading installations and remote
//! program loading are separate work. The native updater is supplied by firmware;
//! this helper is not an externally authored program installation interface.

use super::InstallError;
use crate::{
    ir::{self, Value},
    surface::{Predicate, Read, Scalar, StateHandle},
};
use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

// The surface keeps an opaque location. Typed storage erasure and its single
// publication belong here, beside the installer that establishes the invariant.
pub(crate) struct Reader<A> {
    read: unsafe fn(*const (), &A, usize) -> Option<Value>,
    storage: AtomicPtr<()>,
    index: usize,
}
impl<A> Reader<A> {
    pub(crate) const fn new(
        index: usize,
        read: unsafe fn(*const (), &A, usize) -> Option<Value>,
    ) -> Self {
        Self {
            read,
            storage: AtomicPtr::new(ptr::null_mut()),
            index,
        }
    }
    pub(crate) fn bind(&self, storage: *mut ()) {
        self.storage.store(storage, Ordering::Release);
    }
    pub(crate) fn read(&self, appliance: &A) -> Option<Value> {
        let storage = self.storage.load(Ordering::Acquire);
        if storage.is_null() {
            None
        } else {
            // Only the typed install path binds storage, permanently resident
            // and of the concrete type used to construct this read function.
            unsafe { (self.read)(storage.cast_const(), appliance, self.index) }
        }
    }
}

/// A fixed ring of qualified observations, read by the compiled state updater.
pub struct History<T: Scalar, const N: usize> {
    samples: [Option<T>; N],
    next: usize,
    len: usize,
}
impl<T: Scalar, const N: usize> History<T, N> {
    const fn new() -> Self {
        Self {
            samples: [None; N],
            next: 0,
            len: 0,
        }
    }
    /// Number of retained observations, including unavailable observations.
    pub fn len(&self) -> usize {
        self.len
    }
    /// Whether no observation has been registered since installation.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Read by age: zero is newest. Unavailable/missing samples return None.
    pub fn value(&self, age: usize) -> Option<T> {
        if age >= self.len {
            return None;
        }
        self.samples[(self.next + N - 1 - age) % N]
    }
    fn append(&mut self, value: Option<T>) {
        self.samples[self.next] = value;
        self.next = (self.next + 1) % N;
        self.len = (self.len + 1).min(N);
    }
}

struct Values<T: Scalar, U: Scalar, const N: usize> {
    history: History<T, N>,
    computed: Option<U>,
    update: Option<fn(&History<T, N>) -> Option<U>>,
}

/// Statically reserved storage for one source history and its computed value.
///
/// T and U are sealed scalar types, so history uses native-width samples rather
/// than a full tagged IR Value per entry. The updater is already compiled for the
/// target and receives only a read-only history. Its mathematical correspondence
/// is supplied by the firmware author, like a native transformer's IR definition.
pub struct RetainedState<A: 'static, T: Scalar, U: Scalar, const N: usize> {
    source: &'static StateHandle<A>,
    samples: [ir::Attribute<Read<A>>; N],
    result: ir::Attribute<Read<A>>,
    owner: AtomicPtr<A>,
    installed: AtomicBool,
    access: AtomicBool,
    values: UnsafeCell<Values<T, U, N>>,
    installation: UnsafeCell<Option<Installation<A>>>,
}

// Mutable values are guarded independently of instance pointer identity (which
// alone is insufficient for zero-sized appliance types). Installation metadata
// is written once and published with installed's release/acquire pair.
unsafe impl<A, T: Scalar + Send, U: Scalar + Send, const N: usize> Sync
    for RetainedState<A, T, U, N>
{
}

impl<A, T: Scalar + Send, U: Scalar + Send, const N: usize> RetainedState<A, T, U, N> {
    /// Reserve a history and result without allocating or binding any appliance.
    /// The initial history is empty and the computed value unavailable.
    pub const fn new(source: &'static StateHandle<A>) -> Self {
        assert!(
            N > 0 && N < u16::MAX as usize,
            "invalid retained history extent"
        );
        let mut samples = [const {
            ir::Attribute {
                ty: T::TYPE,
                location: Read::retained(0, read::<A, T, U, N>),
            }
        }; N];
        let mut age = 0;
        while age < N {
            samples[age] = ir::Attribute {
                ty: T::TYPE,
                location: Read::retained(age, read::<A, T, U, N>),
            };
            age += 1;
        }
        Self {
            source,
            samples,
            result: ir::Attribute {
                ty: U::TYPE,
                location: Read::retained(N, read::<A, T, U, N>),
            },
            owner: AtomicPtr::new(ptr::null_mut()),
            installed: AtomicBool::new(false),
            access: AtomicBool::new(false),
            values: UnsafeCell::new(Values {
                history: History::new(),
                computed: None,
                update: None,
            }),
            installation: UnsafeCell::new(None),
        }
    }
    /// Mathematical reference for a history position. Zero is newest.
    pub const fn sample(&self, age: usize) -> &ir::Attribute<Read<A>> {
        &self.samples[age]
    }
    /// Mathematical reference for the computed state; readable through normal
    /// owner-directed state reads after this record has been installed.
    pub const fn result(&self) -> &ir::Attribute<Read<A>> {
        &self.result
    }

    pub(super) fn install(
        &'static self,
        owner: *mut A,
        update: fn(&History<T, N>) -> Option<U>,
        definition: &'static Predicate<A>,
        next_appliance: Option<&'static Installation<A>>,
    ) -> Result<&'static Installation<A>, InstallError> {
        if self.source.definition().ty != T::TYPE {
            return Err(InstallError::Type);
        }
        if !definition.definition.arguments.is_empty()
            || definition.definition.inputs.len() != definition.inputs.len()
            || !definition
                .definition
                .inputs
                .iter()
                .zip(definition.inputs)
                .all(|(ty, attr)| *ty == attr.ty)
        {
            return Err(InstallError::Definition);
        }
        self.owner
            .compare_exchange(ptr::null_mut(), owner, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| InstallError::AlreadyInstalled)?;
        // The owning Appliance serializes pre-start installation. No source list
        // can expose this node until its metadata and typed reader bindings exist.
        let storage = ptr::from_ref(self).cast::<()>().cast_mut();
        unsafe {
            (*self.values.get()).update = Some(update);
            *self.installation.get() = Some(Installation {
                samples: &self.samples,
                result: &self.result,
                definition,
                next_appliance,
                next_source: AtomicPtr::new(ptr::null_mut()),
                storage,
                update: maintain_record::<A, T, U, N>,
            });
        }
        for sample in &self.samples {
            sample.location.bind_retained(storage);
        }
        self.result.location.bind_retained(storage);
        self.installed.store(true, Ordering::Release);
        let node = self.node().unwrap();
        // A native declaration can be shared by distinct appliance instances.
        // Preserve both intrusive links if their pre-start installations race.
        let mut first = self.source.maintained.load(Ordering::Acquire);
        loop {
            node.next_source.store(first, Ordering::Relaxed);
            match self.source.maintained.compare_exchange(
                first,
                ptr::from_ref(node).cast_mut(),
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(changed) => first = changed,
            }
        }
        Ok(node)
    }
    pub(super) fn source(&self) -> &'static StateHandle<A> {
        self.source
    }
    fn node(&self) -> Option<&Installation<A>> {
        self.installed
            .load(Ordering::Acquire)
            .then(|| unsafe { (*self.installation.get()).as_ref().unwrap() })
    }
    fn lock(&self) -> Option<Access<'_>> {
        self.access
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Access(&self.access))
    }
}
struct Access<'a>(&'a AtomicBool);
impl Drop for Access<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

// Only RetainedState's installer can construct these intrusive links. Source
// linkage drives maintenance; appliance linkage enumerates the same definitions.
pub(crate) struct Installation<A: 'static> {
    pub samples: &'static [ir::Attribute<Read<A>>],
    pub result: &'static ir::Attribute<Read<A>>,
    pub definition: &'static Predicate<A>,
    pub next_appliance: Option<&'static Self>,
    next_source: AtomicPtr<Self>,
    storage: *const (),
    update: unsafe fn(*const (), &A, Option<Value>),
}
// Records and pointers are permanently resident, immutable after publication.
unsafe impl<A> Sync for Installation<A> {}

impl<A> Installation<A> {
    pub fn states(&self) -> impl Iterator<Item = &ir::Attribute<Read<A>>> + Clone {
        self.samples.iter().chain(core::iter::once(self.result))
    }
}
pub(crate) fn maintain<A>(source: &StateHandle<A>, appliance: &A, value: Option<Value>) {
    let mut next = unsafe { source.maintained.load(Ordering::Acquire).as_ref() };
    while let Some(node) = next {
        unsafe {
            (node.update)(node.storage, appliance, value);
        }
        next = unsafe { node.next_source.load(Ordering::Acquire).as_ref() };
    }
}
unsafe fn read<A: 'static, T: Scalar + Send, U: Scalar + Send, const N: usize>(
    storage: *const (),
    appliance: &A,
    index: usize,
) -> Option<Value> {
    let state = unsafe { &*storage.cast::<RetainedState<A, T, U, N>>() };
    if state.owner.load(Ordering::Acquire).cast_const() != ptr::from_ref(appliance)
        || !state.installed.load(Ordering::Acquire)
    {
        return None;
    }
    let _access = state.lock()?;
    let values = unsafe { &*state.values.get() };
    if index == N {
        values.computed.map(Scalar::into_value)
    } else {
        values.history.value(index).map(Scalar::into_value)
    }
}
unsafe fn maintain_record<A: 'static, T: Scalar + Send, U: Scalar + Send, const N: usize>(
    storage: *const (),
    appliance: &A,
    value: Option<Value>,
) {
    let state = unsafe { &*storage.cast::<RetainedState<A, T, U, N>>() };
    if state.owner.load(Ordering::Acquire).cast_const() != ptr::from_ref(appliance) {
        return;
    }
    let _access = state
        .lock()
        .expect("state updater reentered its own retained storage");
    let values = unsafe { &mut *state.values.get() };
    let oldest = values.history.value(N - 1);
    let old_result = values.computed;
    let sample = value.map(|v| T::from_value(v).expect("source scalar changed type"));
    values.history.append(sample);
    values.computed = (values.update.unwrap())(&values.history);
    // Values are committed before publication. These are ordinary state deltas,
    // not local callback jobs; no callbacks run between source histories.
    let instance = ptr::from_ref(appliance).cast();
    for age in 0..N {
        // After shifting, the old value at age k is now at k+1. Only the
        // evicted tail needs saving; stack use does not grow with the history.
        let before = if age + 1 == N {
            oldest
        } else {
            values.history.value(age + 1)
        };
        let after = values.history.value(age);
        let (before, after) = (
            before.map(Scalar::into_value),
            after.map(Scalar::into_value),
        );
        if before != after {
            super::change::record(
                instance,
                ptr::from_ref(&state.samples[age]) as usize as u64,
                after,
            );
        }
    }
    let (before, after) = (
        old_result.map(Scalar::into_value),
        values.computed.map(Scalar::into_value),
    );
    if before != after {
        super::change::record(
            instance,
            ptr::from_ref(&state.result) as usize as u64,
            after,
        );
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests;
