//! Bounded deferred invocations. Immediate state mutations do not enter this queue.
//! One dispatcher reconsiders urgency after each invocation. Callbacks remain
//! interruptible and must never block; register/interlock commits use State's
//! atomic operations rather than masking interrupts for an entire callback.
use super::{
    appliance::{changes::Captured, Native},
    state, tasks,
};
use crate::{sync::TaskContext, types::*};
use core::{
    cell::UnsafeCell,
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

const CAPTURE_CAPACITY: usize = crate::configLATTICE_STATE_CHANGES;
const CALL_CAPACITY: usize = 16;
const CAPACITY: usize = CAPTURE_CAPACITY + CALL_CAPACITY;

/// Urgency of deferred work. FIFO ordering is preserved within each priority.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Background,
    Normal,
    Foreground,
}

// Every access is inside state::atomic; no target needs native 64-bit atomics.
struct Version(UnsafeCell<u64>);
unsafe impl Sync for Version {}
impl Version {
    const fn new() -> Self {
        Self(UnsafeCell::new(0))
    }
    fn get(&self) -> u64 {
        unsafe { *self.0.get() }
    }
    fn advance(&self) -> u64 {
        let next = self
            .get()
            .checked_add(1)
            .expect("timer generation exhausted");
        unsafe {
            *self.0.get() = next;
        }
        next
    }
}

/// One cancellable timer, with no sleeping task or allocation. Return Some(delay_ms)
/// to rearm after the callback, or None to stop. The callback is a bounded,
/// nonblocking invocation. Use atomic State operations for hardware interlocks.
pub struct Timer<A: Sync + 'static> {
    callback: fn(&'static A) -> Option<u32>,
    priority: Priority,
    version: Version,
}
impl<A: Sync + 'static> Timer<A> {
    pub const fn new(priority: Priority, callback: fn(&'static A) -> Option<u32>) -> Self {
        Self {
            callback,
            priority,
            version: Version::new(),
        }
    }
    /// Replace this timer's previous deadline/queued invocation. Safe in a
    /// kernel-callable IRQ. The supplied owner must live through every invocation.
    pub fn start(&'static self, owner: &'static A, after_ms: u32) {
        state::atomic(|| {
            let generation = self.version.advance();
            let at = now();
            access(|q| {
                q.cancel(&self.version);
                q.insert(
                    Item::Callback {
                        function: self.callback as *const (),
                        data: ptr::from_ref(owner).cast(),
                        invoke: invoke_timer::<A>,
                        version: Some(&self.version),
                        generation,
                    },
                    self.priority,
                    at.saturating_add(u64::from(after_ms)),
                    at,
                );
            });
        });
        wake();
    }
    /// Invalidate the armed deadline and any queued invocation. A callback cannot
    /// automatically rearm after canceling itself. No running code is aborted.
    pub fn cancel(&self) {
        state::atomic(|| {
            self.version.advance();
            access(|q| q.cancel(&self.version));
        });
        wake();
    }
}

/// Queue a finite nonblocking invocation. No TaskContext is supplied, and no
/// persistent thread is created for the owner. Excess capacity is a kernel fault.
pub fn defer<A: Sync + 'static>(priority: Priority, owner: &'static A, callback: fn(&'static A)) {
    let at = now();
    access(|q| {
        q.insert(
            Item::Callback {
                function: callback as *const (),
                data: ptr::from_ref(owner).cast(),
                invoke: invoke_once::<A>,
                version: None,
                generation: 0,
            },
            priority,
            at,
            at,
        )
    });
    wake();
}
unsafe fn invoke_once<A: 'static>(function: *const (), data: *const ()) -> Option<u32> {
    let function: fn(&'static A) = unsafe { core::mem::transmute(function) };
    function(unsafe { &*data.cast::<A>() });
    None
}
unsafe fn invoke_timer<A: 'static>(function: *const (), data: *const ()) -> Option<u32> {
    let function: fn(&'static A) -> Option<u32> = unsafe { core::mem::transmute(function) };
    function(unsafe { &*data.cast::<A>() })
}

#[derive(Clone, Copy)]
enum Item {
    Capture(&'static dyn Native, Captured),
    Callback {
        function: *const (),
        data: *const (),
        invoke: unsafe fn(*const (), *const ()) -> Option<u32>,
        version: Option<&'static Version>,
        generation: u64,
    },
}
impl Item {
    fn current(self) -> bool {
        match self {
            Self::Callback {
                version: Some(v),
                generation,
                ..
            } => v.get() == generation,
            _ => true,
        }
    }
    fn source(self, source: &Version) -> bool {
        matches!(self, Self::Callback { version: Some(v), .. } if ptr::eq(v,source))
    }
}
#[derive(Clone, Copy)]
struct Entry {
    item: Item,
    priority: Priority,
    due: u64,
    order: u64,
    ready: bool,
    running: bool,
}
struct Pending {
    entries: [Option<Entry>; CAPACITY],
    serial: u64,
}
impl Pending {
    const fn new() -> Self {
        Self {
            entries: [None; CAPACITY],
            serial: 0,
        }
    }
    fn serial(&mut self) -> u64 {
        self.serial = self.serial.checked_add(1).expect("work ordering exhausted");
        self.serial
    }
    fn insert(&mut self, item: Item, priority: Priority, due: u64, at: u64) {
        self.promote(at);
        let capture = matches!(item, Item::Capture(..));
        let count = self
            .entries
            .iter()
            .flatten()
            .filter(|e| matches!(e.item, Item::Capture(..)) == capture)
            .count();
        assert!(
            count
                < if capture {
                    CAPTURE_CAPACITY
                } else {
                    CALL_CAPACITY
                },
            "deferred work capacity exhausted"
        );
        let order = self.serial();
        let slot = self.entries.iter_mut().find(|s| s.is_none()).unwrap();
        *slot = Some(Entry {
            item,
            priority,
            due,
            order,
            ready: due <= at,
            running: false,
        });
    }
    fn promote(&mut self, now: u64) {
        while let Some(index) = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                e.filter(|e| !e.ready && !e.running && e.due <= now)
                    .map(|e| (i, e))
            })
            .min_by_key(|(_, e)| (e.due, e.order))
            .map(|(i, _)| i)
        {
            let order = self.serial();
            let entry = self.entries[index].as_mut().unwrap();
            entry.ready = true;
            entry.order = order;
        }
    }
    fn select(&mut self, now: u64) -> Option<(usize, Entry)> {
        self.promote(now);
        let index = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.filter(|e| e.ready && !e.running).map(|e| (i, e)))
            .min_by_key(|(_, e)| (core::cmp::Reverse(e.priority), e.order))
            .map(|(i, _)| i)?;
        let entry = self.entries[index].as_mut().unwrap();
        entry.running = true;
        Some((index, *entry))
    }
    fn cancel(&mut self, source: &Version) {
        for slot in &mut self.entries {
            if slot.is_some_and(|e| e.item.source(source)) {
                *slot = None;
            }
        }
    }
    fn complete(&mut self, index: usize, entry: Entry, delay: Option<u32>, now: u64) {
        if !self.entries[index].is_some_and(|e| e.order == entry.order && e.running) {
            return;
        }
        self.entries[index] = None;
        if entry.item.current() {
            if let Some(delay) = delay {
                self.insert(
                    entry.item,
                    entry.priority,
                    now.saturating_add(u64::from(delay)),
                    now,
                );
            }
        }
    }
    fn wait_ms(&self, now: u64) -> Option<u64> {
        self.entries
            .iter()
            .flatten()
            .filter(|e| !e.running)
            .map(|e| e.due.saturating_sub(now))
            .min()
    }
}
static mut PENDING: Pending = Pending::new();
static WAKE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
fn access<R>(f: impl FnOnce(&mut Pending) -> R) -> R {
    state::atomic(|| f(unsafe { &mut *ptr::addr_of_mut!(PENDING) }))
}
fn now() -> u64 {
    super::time::monotonic_ms()
}
fn wake() {
    let target = WAKE.load(Ordering::Acquire);
    if target.is_null() {
        return;
    }
    state::atomic(|| {
        let mut woken = pdFALSE;
        // This atomic region provides the same exclusion as an allowed ISR.
        unsafe {
            tasks::xTaskNotifyFromISR(target, 1, tasks::eNotifyAction::eSetBits as i32, &mut woken);
        }
        crate::port::portYIELD_FROM_ISR(woken);
    });
}
pub(crate) fn capture(owner: &'static dyn Native, value: Captured) -> bool {
    let accepted = access(|q| {
        if q.entries
            .iter()
            .flatten()
            .filter(|e| matches!(e.item, Item::Capture(..)))
            .count()
            == CAPTURE_CAPACITY
        {
            return false;
        }
        let at = now();
        q.insert(Item::Capture(owner, value), Priority::Foreground, at, at);
        true
    });
    if accepted {
        wake();
    }
    accepted
}
pub(crate) fn foreground_pending() -> bool {
    let now = now();
    access(|q| {
        q.entries
            .iter()
            .flatten()
            .any(|e| e.priority > Priority::Background && (e.running || e.due <= now))
    })
}
pub(crate) fn run_one(context: &TaskContext, at: u64) -> bool {
    let Some((index, entry)) = access(|q| q.select(at)) else {
        return false;
    };
    match entry.item {
        Item::Capture(owner, value) => {
            access(|q| q.complete(index, entry, None, at));
            owner.process_capture(context, value);
        }
        Item::Callback {
            function,
            data,
            invoke,
            version: Some(_),
            ..
        } => {
            let current = state::atomic(|| entry.item.current());
            let delay = if current {
                unsafe { invoke(function, data) }
            } else {
                None
            };
            access(|q| q.complete(index, entry, delay, now()));
        }
        Item::Callback {
            function,
            data,
            invoke,
            ..
        } => {
            unsafe {
                invoke(function, data);
            }
            access(|q| q.complete(index, entry, None, now()));
        }
    }
    #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
    super::radio::task::notify_work();
    true
}

#[cfg(test)]
pub(crate) fn take_capture(owner: &dyn Native) -> Option<Captured> {
    access(|q| {
        let index = q
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match (*e)?.item {
                Item::Capture(n, _) if ptr::addr_eq(n, owner) => Some((i, e.unwrap().order)),
                _ => None,
            })
            .min_by_key(|(_, order)| *order)?
            .0;
        match q.entries[index].take()?.item {
            Item::Capture(_, value) => Some(value),
            _ => unreachable!(),
        }
    })
}
#[cfg(test)]
pub(crate) fn reset() {
    access(|q| *q = Pending::new());
}

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub(crate) fn start(context: &TaskContext) {
    use crate::sync::TaskHandle;
    static mut STACK: [StackType_t; crate::configLATTICE_STATE_STACK_WORDS] =
        [0; crate::configLATTICE_STATE_STACK_WORDS];
    static mut CONTROL: tasks::StaticTask_t = tasks::StaticTask_t::new();
    unsafe {
        TaskHandle::spawn_static(context, b"work", &mut STACK, &mut CONTROL, 2, run).unwrap();
    }
}
#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
extern "C" fn run(_: *mut c_void) {
    let context = unsafe { TaskContext::assume() };
    WAKE.store(tasks::xTaskGetCurrentTaskHandle(), Ordering::Release);
    loop {
        if run_one(&context, now()) {
            continue;
        }
        let wait = access(|q| q.wait_ms(now())).map_or(portMAX_DELAY, |ms| {
            crate::milliseconds_to_ticks_checked(ms).unwrap_or(portMAX_DELAY)
        });
        unsafe {
            tasks::xTaskNotifyWait(0, u32::MAX, ptr::null_mut(), wait);
        }
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests {
    extern crate std;
    use super::*;
    use core::sync::atomic::AtomicU32;
    static IDS: [u32; 4] = [1, 2, 3, 4];
    fn item(id: &'static u32) -> Item {
        fn ignore(_: &u32) {}
        Item::Callback {
            function: ignore as *const (),
            data: ptr::from_ref(id).cast(),
            invoke: invoke_once::<u32>,
            version: None,
            generation: 0,
        }
    }
    fn id(entry: Entry) -> u32 {
        match entry.item {
            Item::Callback { data, .. } => unsafe { *data.cast::<u32>() },
            _ => unreachable!(),
        }
    }
    #[test]
    fn priority_and_fifo() {
        let mut q = Pending::new();
        for (id, priority) in IDS.iter().zip([
            Priority::Background,
            Priority::Normal,
            Priority::Foreground,
            Priority::Normal,
        ]) {
            q.insert(item(id), priority, 0, 0);
        }
        for expected in [3, 2, 4, 1] {
            let (index, entry) = q.select(0).unwrap();
            assert_eq!(id(entry), expected);
            q.complete(index, entry, None, 0);
        }
        assert!(q.select(u64::MAX).is_none());
        q.insert(item(&IDS[0]), Priority::Normal, 10, 0);
        q.insert(item(&IDS[1]), Priority::Normal, 9, 9);
        q.insert(item(&IDS[2]), Priority::Normal, 10, 10);
        for expected in [2, 1, 3] {
            let (index, entry) = q.select(10).unwrap();
            assert_eq!(id(entry), expected);
            q.complete(index, entry, None, 10);
        }
    }
    #[test]
    fn callback_capacity() {
        let mut q = Pending::new();
        for _ in 0..CALL_CAPACITY {
            q.insert(item(&IDS[0]), Priority::Background, 0, 0);
        }
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| q.insert(
                item(&IDS[0]),
                Priority::Background,
                0,
                0
            )))
            .is_err()
        );
        let (index, entry) = q.select(0).unwrap();
        q.complete(index, entry, None, 0);
        q.insert(item(&IDS[0]), Priority::Foreground, 0, 0);
    }
    #[test]
    fn cancellation_and_rearm() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        static TIMER: Timer<AtomicU32> = Timer::new(Priority::Normal, |calls| {
            calls.fetch_add(1, Ordering::Relaxed);
            TIMER.cancel();
            Some(1)
        });
        crate::port::test_port_reset();
        reset();
        let context = unsafe { TaskContext::assume() };
        for ready in [false, true] {
            TIMER.start(&CALLS, if ready { 0 } else { 10 });
            if ready {
                access(|q| q.promote(now()));
            }
            TIMER.cancel();
            assert!(!run_one(&context, now() + 10));
        }
        assert_eq!(CALLS.load(Ordering::Relaxed), 0);
        TIMER.start(&CALLS, 0);
        assert!(run_one(&context, now()));
        assert!(!run_one(&context, now() + 100));
        assert_eq!(CALLS.load(Ordering::Relaxed), 1);
        reset();
    }
}
