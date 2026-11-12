# Writing appliance firmware

Start with the [dependency and port selection](../../../README.md#start-an-appliance).

Firmware describes the appliance and implements its behavior. The kernel connects
its declared states and operations automatically. Static storage, references and
ordinary methods suffice; no state-registration handle or per-state trait is public.
The [lamp example](../../../examples/surface.rs) is a complete native-only declaration.

## Appliance declaration

A configured name belongs to the ordinary kernel appliance declaration:

```rust,ignore
static APPLIANCE: Appliance<Relay, Error> = lattice_vertex::appliance! {
    name: env!("VERTEX_APPLIANCE_NAME"),
    surface: &SURFACE,
};
```

Firmware can supply a literal, a constant, or a build-provided `env!` value.
The macro expands to `Appliance::new(&SURFACE).with_name(name)`; static initialization
evaluates that constructor at compile time. The kernel retains the borrowed name
alongside the surface, and `APPLIANCE.name()` returns it. No generated binary blob,
custom linker section or runtime registration is required. Existing unnamed
`Appliance::new` declarations remain valid and return `None` from `name()`.

The name is descriptive. It does not change state/operation binding references
or the compatibility fingerprint. The kernel exposes it through the ordinary
LSF v4 surface read; firmware needs no extra publication call. Names consume only
their UTF-8 bytes plus a two-byte length in that read and must fit the total
65,535-byte description bound. They add no discovery broadcast traffic. See the
[surface format](../peer/EXPOSURE.md#surface-format).

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
`State<T>` can own RAM or a `RegisterField`; both use the same read/set/clear/update
operations, with Boolean toggle. `State::mapped` reads the peripheral directly and
keeps no value mirror. `port::nrf52840::output` and `port::esp32c6::output` consume an
owned GPIO pin and produce `State<bool>` backed by its output latch/SET/CLR aliases.
True is electrical HIGH. The port initializes the pin once; it stays owned until reset.
Custom MMIO adapters can use the unsafe `RegisterField` constructors, whose access,
alignment and ownership contracts must be met. Out-of-range field writes reject
without truncating; a mapped field cannot represent unavailable state.

Mutation is synchronous and nonblocking, including in a kernel-callable IRQ. Capture
occurs after the value changes. An unexpected capture overflow is a kernel fault;
it does not return a rejected mutation for firmware to retry. Reentry also faults
instead of spinning while interrupts are masked. Both ports provide `on_edge(pin,
pull, edge, action)`: it consumes the pin, configures rising/falling/either-edge
qualification and reserves the IRQ handler. The kernel acknowledges the event and
runs the bounded nonblocking action; it never samples a previous input level.
`Interrupt::enable/disable` controls that source; `level()` reads its actual GPIO
level when an either-edge action needs it, without retaining a software mirror.
Inputs are armed after restoration.
The Nordic implementation owns GPIOTE; the C6 implementation owns the GPIO dispatcher.
Do not also install a HAL dispatcher for that peripheral.

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
    fn set_power(&'static self, on: bool) -> Result<(), Error> {
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

For a direct field operation, use `TransformerHandle::setter("set-power", |r| &r.power)`
or `TransformerHandle::toggle("toggle", |r| &r.power)`. These bind the kernel's typed
write/toggle; firmware does not implement an extra callback for a register write.
Custom methods and `appliance.call(|device| device.set_power(on))` run at the same
atomic mutation boundary, including IRQ context. They receive no `TaskContext` and
must be bounded and nonblocking. Use `work::defer` or `work::Timer` for subsequent
steps. `Appliance::device()` provides the stable owner for those callbacks.

CRC-valid, owner-addressed RF calls are validated/deduplicated before immediate
dispatch. Compact targets also require the established last-hop/session binding.
Their result/feedback remains in the existing peer owners for later transmission.
Older unhandled receive data forms an ordering barrier: the immediate path cannot
overtake it. Setup, forwarding and rejected packets remain peer-task work. MAC ACK
turnaround remains a port responsibility; Nordic dispatches after ACK PHYEND in
the same IRQ path. There is no RF transmission from the
appliance callback. Native worker tasks and their stacks have been removed.

A state transition is registered when its primitive runs, including changes after
a setter returns or before a later error. Success alone creates no state delta.
There are no before/after getter scans. A diffuser's duty-cycle controller, a fan's
appliance RF protocol and a dimmer's calibrated output mapping remain firmware code.

## Startup

1. Initialize the selected board/HAL and prepare initial or saved device state.
2. Declare the state fields and native methods, then construct the static `Surface`
   and `Appliance`. A basic firmware imports no IR definitions.
3. Call `initialize` once. It places the device in stable storage and connects all
   declared state/event sources. Declare board input actions with `port::on_edge`;
   the kernel enables them after protected settings have been restored.
4. Declare bounded controller steps using `work::defer` and `work::Timer`. Optional
   appliance tasks may use the RTOS, but native command hooks must not block.
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
Native methods and IRQ hooks do not receive or manufacture a task context.

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

`kernel::work` owns one fixed pool with separate capacity bounds: 32 captured state
changes (`configLATTICE_STATE_CHANGES`) and 16 deferred/timed callbacks. General
callbacks cannot consume capture capacity. One priority-2 dispatcher replaces the
old state task. It selects Foreground, Normal, then Background work and preserves
FIFO within each priority, reconsidering after every invocation. No heap or per-
appliance thread is created for a callback. Native operations are immediate, without
worker tasks or waiting-call tickets.

`work::defer(priority, owner, callback)` accepts a static owner and a bounded,
nonblocking function. `work::Timer<A>` takes such an owner and a callback returning
`Some(delay_ms)` to rearm or `None` to stop. Start replaces the timer's deadline;
cancel invalidates armed/queued work and suppresses automatic rearm by an old
invocation. Already-running Rust code finishes; callbacks remain interruptible.
Hardware interlocks must be checked/applied in an atomic state operation. No
TaskContext is supplied to callbacks. The dispatcher blocks on notifications or
its earliest deadline when idle.

Captures preserve values and FIFO order. The owner remains read-Busy through
capture processing, including the interval after removal from the work queue.
Publication follows metadata/history completion. Nonblocking `StateEvent` capture
still reports its overflow counter; ordinary state mutation faults on that condition
rather than allowing a replay of an already-performed operation. Numeric worst-case
response guarantees and actual appliance actuation remain unvalidated. Selected
GPIO IRQ and radio-command paths have been exercised on both ports.

### Nonvolatile settings

`StateHandle::new("stable-name", field).nonvolatile()` selects automatic persistence;
ordinary bindings remain volatile. Exposed settings stay in the normal surface.
Declare private settings with `Appliance::with_settings(&[&STARTUP_DELAY, &CYCLE_LENGTH])`;
they are not enumerated or broadcast. Both use the same authoritative `State<T>`.
Read-only external observations cannot be marked nonvolatile.

`start_with_storage`/`start_with_updates` restores settings by durable name and type
before arming kernel input IRQs. Missing settings retain their initialized defaults;
a setting binding that cannot be reconstructed prevents startup. Incompatible saved
programs remain retained and inactive; they do not prevent native controls or valid
settings from operating. Program compatibility status remains inspectable. Restoration writes
the field directly, without invoking a command or emitting a fabricated delta.

Live writes are immediate. Dirty settings join the existing protected configuration
snapshot after one second without another setting change, with only the latest
values copied. Continuous adjustments postpone starting that snapshot. Changes during
a snapshot remain dirty for a later commit. One erase page or 256-byte write advances
per flash step; foreground work and queued host commands take precedence, and steps
leave a receive turn between them. An open continuous-control session defers
configuration and OTA staging until it closes or expires. This does not make flash stalls preemptible.
Power loss restores the last complete snapshot. Uncommitted setting changes may be
lost. Storage/capacity failure is visible in configuration status; no silent fallback
to claimed durability is provided. Firmware must use stable names for compatible
settings and change a name when repurposing its type/meaning.

Stored settings consume 36 bytes each in the existing arena; no per-setting flash
page or second flash owner is allocated. Retired settings remain readable for a
compatible firmware return and consume capacity until a future reclamation policy.
These records are configuration, not a historical ledger.

An explicit peer/native read returns Busy while captured changes await registration
during derived-state maintenance. Firmware reads its own State
normally. Register access and kernel sequence publication remain separate from
physical feedback accuracy and application persistence policy.

Host checks cover automatic registration, typing, captured FIFO values, ISR handoff,
capacity, native-only invocation, metadata availability and scheduler exclusion.
Both maintained MCU images are build targets. The four production appliance
implementations and electrical qualification on those appliances remain the next
integration exercise; this API is not a claim that those have been completed.

## Hardware qualification scope

Selected C6/nRF52840 bench checks exercised register-backed output, GPIO loopback,
bounded rearming timers, reciprocal native commands and compact continuous control.
Both IRQ dispatch and ordered task-context handling were observed without native
worker tasks. Saved values survived application replacement/restart and a full
unplug/reconnect with zero replayed calls. Original saved program records were
preserved; incompatible definitions remained inactive.

This run exposed and corrected two policy boundaries: incompatible saved programs
must not block otherwise valid settings/native startup, and background persistence
must wait for quiet settings and an inactive manual-control session. Earlier RF
exchanges timed out; a later reciprocal native run and both continuous-control runs
completed after those changes. This is selected-run evidence, not a lossless-radio
claim or a bound on flash-time interrupt latency. Full OTA transfer/activation was
not repeated. The adapter, measurements and raw captures remain in the external lab.
