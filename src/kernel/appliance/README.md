# Writing appliance firmware

Start with the [dependency and port selection](../../../README.md#start-an-appliance).

Firmware describes the appliance and implements its behavior. The kernel connects
its declared states and operations automatically. Static storage, references and
ordinary methods suffice; no state-registration handle or per-state trait is public.
The [lamp example](../../../examples/surface.rs) is a complete native-only declaration.

## State is data

```rust
use lattice_vertex::{kernel::State, surface::StateHandle};

struct Relay { power: State<bool> }
static POWER: StateHandle<Relay> = StateHandle::new("power", |relay| &relay.power);
```

The declaration names the field once. `State::new(false)` establishes a known
initial value; `State::unknown()` starts unavailable, and `State::restore(value)`
accepts a saved `Option<T>`. Initialization does not actuate hardware or emit a delta.
A selector identifies the same state object throughout that appliance's lifetime.

Normal firmware uses `set`, `unavailable`, `replace` or `update`. Actual software
transitions are registered automatically; assigning the current value does nothing.
`State<bool>::toggle` returns the new Boolean value and rejects unknown state.
`update` is a short, nonblocking atomic computation; it cannot perform I/O or
reenter the same state. `get` returns `Option<T>` because unavailable is distinct
from false or zero. This is authoritative storage, not a polled observation mirror.

Types are Boolean and signed/unsigned 8/16/32/64-bit integers. Firmware can encode
finite selections and scaled fractions using those types. Meaningful ranges,
calibration and hardware limits remain appliance behavior, not inferred kernel policy.

## Hardware state and events

Hardware-backed state needs no software-value copy. Declare its bounded reader
and qualified event together:

```rust,ignore
static POWER: StateHandle<Relay> = StateHandle::hardware(
    "power", Relay::read_power, |relay| &relay.power_changed,
);
```

`read_power(&Relay) -> Option<bool>` reads established state from the supplied
register/buffer; it must not start blocking acquisition. `power_changed` is a
`StateEvent<bool>`. The kernel connects it during initialization. There is no
`StateInput`, `State::bind` implementation or manual `.bind()` call.

The HAL callback captures/acknowledges its already-qualified event through
`power_changed.handle(|| Some(captured_value))`. A configured edge can supply its
known value directly. `None` registers loss of availability. The wrapper does no
getter reread or equality comparison. Capture and enqueue are short, nonblocking
critical work; slower acquisition/decoding belongs in the firmware's worker task.

Initialize the appliance before enabling callbacks that refer to its fields.
`Appliance::initialize` returns the stable device reference for that setup. The
HAL/board binding owns actual peripheral IRQ configuration and uses a kernel-callable
priority (5..=7 on nRF52840). The kernel checks BASEPRI priorities before entering
its ISR queue path. An event before initialization runs its capture/acknowledgment
hook, then returns `StateChangeError::NotReady` rather than claiming delivery.

## Operations are ordinary behavior

A normal firmware method can be registered directly:

```rust,ignore
impl Relay {
    fn set_power(&self, context: &TaskContext, on: bool) -> Result<(), Error> {
        // Perform the appliance-specific I/O and state transition.
    }
}
static SET_POWER: TransformerHandle<Relay, Error> =
    TransformerHandle::from_fn("set-power", Relay::set_power);
static SURFACE: Surface<Relay, Error> = Surface::new(&[&POWER], &[&SET_POWER]);
```

The argument type supplies the native signature. Single scalars, tuples and fixed
arrays use the existing `Arguments` conversion. An implementation that benefits
from a behavioral trait can still use `StateTransformer` and
`TransformerHandle::new::<Implementation>`; it has no required mathematical constant.
Kernel-owned argument checking and invocation behavior cannot be overridden by a
firmware default method.

For local guarded work, `appliance.call(&context, |device| device.set_power(&context, on))`
uses the same update exclusion as remote calls. `InvokeError::Busy` means the hook
has not run. `Appliance::device()` gives continuing controllers access without
holding an operation guard for their entire lifetime. The supplied `TaskContext`
lets native methods use ordinary task/timer/synchronization facilities.

A state transition is registered when its primitive runs, including changes after
a setter returns or before a later error. Success alone creates no state delta.
There are no before/after getter scans. A diffuser's duty-cycle controller, a fan's
appliance RF protocol and a dimmer's calibrated output mapping remain firmware code.

## Startup

1. Initialize the selected board/HAL and prepare initial or saved device state.
2. Declare the state fields and native methods, then construct the static `Surface`
   and `Appliance`. A basic firmware imports no IR definitions.
3. Call `initialize` once. It places the device in stable storage and connects all
   declared state/event sources. Use the returned device to enable HAL callbacks.
4. Create any device controller tasks/timers using existing `sync::TaskHandle`,
   queues, semaphores, event groups and `Timer` (the latter uses feature `timers`).
5. Enter `kernel::start` or its storage/update variant. Firmware supplies no Lattice
   service loop, message construction, network policy or graph evaluator.

The entry point is board-specific; the appliance declaration is not. The maintained
board entries demonstrate the initialization order:

- [C6](../../port/esp32c6/tests/src/bin/radio.rs): initialize `esp-hal`, then give
  SYSTIMER alarm 0 and software interrupt 0 to `port::esp32c6::initialize`. Transfer
  `IEEE802154` to `kernel::start`. Other appliance I/O stays with firmware.
- [nRF52840](../../port/nrf52840/tests/src/bin/radio.rs): give SysTick to
  `port::nrf52840::initialize`; it initializes and returns the Nordic HAL peripherals.
  Transfer `RADIO` to `kernel::start`. Do not separately initialize the Nordic HAL
  or install another executor/time driver. Kernel-callable IRQs use priorities 5–7.

Use the board's actual bootloader/flash layout when linking. The port-owned
[C6](../../port/esp32c6/tests/README.md) and
[Nordic](../../port/nrf52840/tests/README.md) procedures identify their maintained
layouts and upload mechanisms. Those fixtures' diagnostics are not firmware dependencies.
`TaskContext::assume` is the unsafe board-entry boundary: construct it only in the
initialized thread context, before the scheduler, as those entries do.
Native methods receive an existing context; an IRQ uses `StateEvent::handle` and
must not manufacture a task context.

`kernel::start` uses volatile program storage. Firmware that must retain installed
automations and commissioning over reboot selects `esp32c6-update` or
`nrf52840-update`, obtains the port's `update::image_storage`, and uses
`kernel::start_with_storage`. This requires the matching protected flash/boot layout;
the feature alone does not provision it. Signed OTA additionally selects
`firmware-update` and uses `start_with_updates` with its real release policy. See
[storage](../storage/README.md) and [updates](../../../docs/FIRMWARE_UPDATES.md).
USB/host attachment (`host-link`) and test support are independent, optional roles.

## Optional authored mathematics

`TransformerHandle::with_model(&operation)` attaches an explicit IR operation.
`Surface::with_model(&IrModel { complete, axioms, invariants })` attaches appliance
mathematics and the implementor's completeness declaration. No model is inferred
from Rust code, state assignments or a native signature. Kernel checks of types,
buffer bounds and references are not appliance proofs or synthesized preconditions.

Discovery advertises a small `IrStatus`: `Absent`, `Incomplete`, or
`DeclaredComplete`. Missing operation models prevent a complete advertisement.
A declaration beyond the encoder's size/depth bounds is served as a native catalog
with `Incomplete`, retaining native control instead of generating substitute IR.
Nucleus can skip absent/incomplete models before a full read; a complete claim still
requires its validation. No nucleus-presence tracking or basic-install error is added.
The [wire format](../peer/EXPOSURE.md#surface-format) distinguishes missing operation
mathematics from an explicit empty effect.

## Delivery, storage and scope

Each appliance has a FIFO of `configLATTICE_STATE_CHANGES` captured transitions
(default 32), at most 12 bytes per entry: at most 384 payload bytes plus RTOS control.
The state task blocks when empty. It registers captured values and performs only
explicitly installed native [history](../history.rs) maintenance. No heap, per-state
task, default polling interval or second current-value array is required.

Full storage returns `StateChangeError::Capacity` and increments
`Appliance::state_change_overflow()`. Queued changes remain intact; a rejected
software write leaves its value unchanged. Hardware cannot undo an event it already
produced, so its binding must retain/recover it or report the loss. Acceptance in
this local FIFO is not proof of RF delivery. Broadcast deltas and addressed circuit
continuations keep their distinct existing roles.

An explicit peer/native read returns Busy while captured changes await registration
or derived-state maintenance owns the read boundary. Firmware reads its own State
normally. Register access and kernel sequence publication remain separate from
physical feedback accuracy and application persistence policy.

Host checks cover automatic registration, typing, captured FIFO values, ISR handoff,
capacity, native-only invocation, metadata availability and scheduler exclusion.
Both maintained MCU images are build targets. The four production appliance
implementations and physical IRQ/actuator validation remain the next integration
exercise; this API is not a claim that those have been completed.
