//! Statically provisioned RTOS contexts for acquisition and finite native calls.
//! Ready records belong to a reserved task; there is no waiting-job queue.

use super::Native;
use crate::{ir::Value, kernel::peer::frame::InvokeStatus, port, sync::TaskContext};
use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use lattice_peer_wire::{scalar::decode_arguments, CALL_ARGUMENT_BYTES, MAX_CALL_ARGUMENTS};

const COUNT: usize = crate::configLATTICE_NATIVE_TASKS;
const _: () = assert!(
    crate::configLATTICE_PROGRAM_ARGUMENTS >= MAX_CALL_ARGUMENTS
        && crate::configLATTICE_PROGRAM_ARGUMENTS <= u8::MAX as usize
);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ticket {
    slot: usize,
    generation: u32,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Ready,
    Running,
    Finished,
}
struct Call {
    native: &'static dyn Native,
    transformer: u64,
    values: [Value; crate::configLATTICE_PROGRAM_ARGUMENTS],
    length: usize,
    valid: bool,
    replaceable: bool,
    #[cfg(feature = "firmware-update")]
    _hold: crate::kernel::update::UpdateHold,
}
struct Slot {
    generation: u32,
    phase: Phase,
    call: Option<Call>,
    result: InvokeStatus,
    detached: bool,
}
impl Slot {
    const fn new() -> Self {
        Self {
            generation: 0,
            phase: Phase::Idle,
            call: None,
            result: InvokeStatus::Busy,
            detached: false,
        }
    }
}
static mut SLOTS: [Slot; COUNT] = [const { Slot::new() }; COUNT];
static WAKE: [AtomicPtr<c_void>; COUNT] = [const { AtomicPtr::new(ptr::null_mut()) }; COUNT];
fn access<R>(f: impl FnOnce(&mut [Slot; COUNT]) -> R) -> R {
    struct Restore(crate::types::UBaseType_t);
    impl Drop for Restore {
        fn drop(&mut self) {
            port::portCLEAR_INTERRUPT_MASK_FROM_ISR(self.0);
        }
    }
    let _restore = Restore(port::portSET_INTERRUPT_MASK_FROM_ISR());
    // Selected ports are single-core. Only bounded metadata moves occur here;
    // native functions and guard destruction run outside the masked section.
    f(unsafe { &mut *ptr::addr_of_mut!(SLOTS) })
}
pub(crate) fn submit(
    native: &'static dyn Native,
    context: &TaskContext,
    transformer: u64,
    arguments: &[u8],
) -> Result<Ticket, InvokeStatus> {
    if arguments.len() > CALL_ARGUMENT_BYTES {
        return Err(InvokeStatus::Arguments);
    }
    let mut values = [Value::boolean(false); MAX_CALL_ARGUMENTS];
    match decode_arguments(arguments, &mut values) {
        Some(values) => submit_prepared(native, context, transformer, values, true, false),
        None => submit_prepared(native, context, transformer, &[], false, false),
    }
}

pub(crate) fn submit_values(
    native: &'static dyn Native,
    context: &TaskContext,
    transformer: u64,
    arguments: &[Value],
) -> Result<Ticket, InvokeStatus> {
    submit_prepared(native, context, transformer, arguments, true, false)
}
/// A scalar control target remains replaceable until its native function starts.
/// Ordinary native calls and program invocations never use this admission mode.
pub(crate) fn submit_target(
    native: &'static dyn Native,
    context: &TaskContext,
    transformer: u64,
    value: Value,
) -> Result<Ticket, InvokeStatus> {
    submit_prepared(native, context, transformer, &[value], true, true)
}
pub(crate) fn replace_target(ticket: Ticket, value: Value) -> bool {
    access(|slots| {
        let Some(s) = slots
            .get_mut(ticket.slot)
            .filter(|s| s.generation == ticket.generation && s.phase == Phase::Ready)
        else {
            return false;
        };
        let Some(c) = s
            .call
            .as_mut()
            .filter(|c| c.replaceable && c.length == 1 && c.values[0].ty() == value.ty())
        else {
            return false;
        };
        c.values[0] = value;
        true
    })
}
pub(crate) fn cancel_target(ticket: Ticket) -> bool {
    let removed = access(|slots| {
        let s = slots
            .get_mut(ticket.slot)
            .filter(|s| s.generation == ticket.generation && s.phase == Phase::Ready)?;
        if !s.call.as_ref()?.replaceable {
            return None;
        }
        s.phase = Phase::Idle;
        s.call.take()
    });
    // Releasing the update hold must happen outside the interrupt mask.
    let canceled = removed.is_some();
    drop(removed);
    if canceled {
        capacity_changed();
    }
    canceled
}
/// Explicit graph removal relinquishes this call's continuation. Ready calls
/// have not touched hardware; running calls finish normally in their own task.
pub(crate) fn detach(ticket: Ticket) {
    let removed = access(|slots| {
        let s = slots
            .get_mut(ticket.slot)
            .filter(|s| s.generation == ticket.generation)?;
        if s.phase == Phase::Running {
            s.detached = true;
            return None;
        }
        s.phase = Phase::Idle;
        s.call.take()
    });
    drop(removed);
    capacity_changed();
}
fn submit_prepared(
    native: &'static dyn Native,
    context: &TaskContext,
    transformer: u64,
    arguments: &[Value],
    valid: bool,
    replaceable: bool,
) -> Result<Ticket, InvokeStatus> {
    if arguments.len() > crate::configLATTICE_PROGRAM_ARGUMENTS {
        return Err(InvokeStatus::Arguments);
    }
    #[cfg(feature = "firmware-update")]
    let hold = crate::kernel::update::hold_updates(context).map_err(|_| InvokeStatus::Busy)?;
    let _ = context;
    let mut values = [Value::boolean(false); crate::configLATTICE_PROGRAM_ARGUMENTS];
    values[..arguments.len()].copy_from_slice(arguments);
    let mut call = Some(Call {
        native,
        transformer,
        values,
        length: arguments.len(),
        valid,
        replaceable,
        #[cfg(feature = "firmware-update")]
        _hold: hold,
    });
    let ticket = access(|slots| {
        let (index, slot) = slots
            .iter_mut()
            .enumerate()
            .find(|(_, s)| s.phase == Phase::Idle && s.generation != u32::MAX)?;
        slot.generation += 1;
        slot.detached = false;
        slot.call = call.take();
        slot.phase = Phase::Ready;
        Some(Ticket {
            slot: index,
            generation: slot.generation,
        })
    })
    .ok_or(InvokeStatus::Busy)?;
    let task = WAKE[ticket.slot].load(Ordering::Acquire);
    if !task.is_null() {
        unsafe {
            crate::kernel::tasks::xTaskGenericNotify(
                task,
                0,
                1,
                crate::kernel::tasks::eNotifyAction::eSetBits as i32,
                ptr::null_mut(),
            );
        }
    }
    Ok(ticket)
}
pub(crate) fn take_result(ticket: Ticket) -> Option<InvokeStatus> {
    let result = access(|slots| {
        let slot = slots.get_mut(ticket.slot)?;
        if slot.generation != ticket.generation || slot.phase != Phase::Finished {
            return None;
        }
        slot.phase = Phase::Idle;
        Some(slot.result)
    });
    if result.is_some() {
        capacity_changed();
    }
    result
}
fn capacity_changed() {
    #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
    crate::kernel::radio::task::notify_work();
}
pub(crate) fn completion_pending() -> bool {
    access(|slots| slots.iter().any(|s| s.phase == Phase::Finished))
}
fn run_one(index: usize, context: &TaskContext) -> bool {
    let Some(call) = access(|slots| {
        let slot = &mut slots[index];
        if slot.phase != Phase::Ready {
            return None;
        }
        slot.phase = Phase::Running;
        slot.call.take()
    }) else {
        return false;
    };
    let result = if call.valid {
        call.native
            .invoke(context, call.transformer, &call.values[..call.length])
    } else {
        InvokeStatus::Arguments
    };
    // Finish all hardware work before making the context reusable. The update
    // hold covers admission through execution, including a delayed task start.
    drop(call);
    access(|slots| {
        slots[index].result = result;
        slots[index].phase = if slots[index].detached {
            Phase::Idle
        } else {
            Phase::Finished
        };
    });
    #[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
    crate::kernel::radio::task::notify_work();
    true
}

#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
mod tasks {
    use super::*;
    use crate::{kernel::tasks, sync::TaskHandle, types::*};
    static mut STACKS: [[StackType_t; crate::configLATTICE_NATIVE_STACK_WORDS]; COUNT] =
        [[0; crate::configLATTICE_NATIVE_STACK_WORDS]; COUNT];
    static mut CONTROLS: [tasks::StaticTask_t; COUNT] =
        [const { tasks::StaticTask_t::new() }; COUNT];
    static mut ACQUISITION_STACK: [StackType_t; crate::configLATTICE_ACQUISITION_STACK_WORDS] =
        [0; crate::configLATTICE_ACQUISITION_STACK_WORDS];
    static mut ACQUISITION_CONTROL: tasks::StaticTask_t = tasks::StaticTask_t::new();
    static mut NATIVE: Option<&'static dyn Native> = None;
    pub(crate) fn start(context: &TaskContext, native: &'static dyn Native) {
        assert!(COUNT != 0);
        assert_eq!(
            tasks::xTaskGetSchedulerState(),
            tasks::taskSCHEDULER_NOT_STARTED
        );
        unsafe {
            assert!(NATIVE.is_none(), "native tasks already started");
            NATIVE = Some(native);
            for (index, (stack, tcb)) in STACKS.iter_mut().zip(CONTROLS.iter_mut()).enumerate() {
                TaskHandle::spawn_static_with_param(
                    context,
                    b"native",
                    stack,
                    tcb,
                    // Peer service and acquisition outrank user hardware work.
                    // Equal-priority host I/O still receives RTOS time slices.
                    1,
                    invoke,
                    index as *mut c_void,
                )
                .unwrap();
            }
            TaskHandle::spawn_static(
                context,
                b"acquire",
                &mut ACQUISITION_STACK,
                &mut ACQUISITION_CONTROL,
                2,
                acquire,
            )
            .unwrap();
        }
    }
    extern "C" fn invoke(parameter: *mut c_void) {
        let index = parameter as usize;
        let context = unsafe { TaskContext::assume() };
        WAKE[index].store(tasks::xTaskGetCurrentTaskHandle(), Ordering::Release);
        loop {
            if !run_one(index, &context) {
                unsafe {
                    tasks::xTaskNotifyWait(0, u32::MAX, ptr::null_mut(), portMAX_DELAY);
                }
            }
        }
    }
    extern "C" fn acquire(_: *mut c_void) {
        let context = unsafe { TaskContext::assume() };
        let native = unsafe { NATIVE.unwrap() };
        super::super::acquisition::run(native, &context)
    }
}
#[cfg(any(feature = "port-nrf52840", feature = "port-esp32c6"))]
pub(crate) use tasks::start;

#[cfg(all(test, feature = "port-test"))]
pub(crate) fn run_ready(context: &TaskContext) {
    for index in 0..COUNT {
        run_one(index, context);
    }
}
#[cfg(all(test, feature = "port-test"))]
pub(crate) fn reset() {
    for index in 0..COUNT {
        let old = access(|slots| core::mem::replace(&mut slots[index], Slot::new()));
        drop(old);
    }
}

#[cfg(all(test, feature = "port-test"))]
mod tests {
    use super::*;
    #[test]
    fn detachment_boundary() {
        crate::port::test_port_reset();
        reset();
        for phase in [Phase::Ready, Phase::Running, Phase::Finished] {
            access(|slots| {
                slots[0].generation = 1;
                slots[0].phase = phase;
                slots[0].detached = false;
            });
            detach(Ticket {
                slot: 0,
                generation: 2,
            });
            assert!(access(|slots| slots[0].phase == phase && !slots[0].detached));
            detach(Ticket {
                slot: 0,
                generation: 1,
            });
            assert!(access(|slots| if phase == Phase::Running {
                slots[0].phase == Phase::Running && slots[0].detached
            } else {
                slots[0].phase == Phase::Idle
            }));
        }
        reset();
    }
}
