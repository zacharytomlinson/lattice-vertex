# Circuit implementation plan

The [architecture](ARCHITECTURE.md#maintained-circuits-and-delta-propagation) owns
the accepted semantics. Maintained software connections are implemented for
**nRF52840 and ESP32-C6**, with selected two-board validation complete. LLVM lowering
on both targets is next.
Hardware selection and autonomous signal/RF paths remain a separate investigation.
The [implementation map](KERNEL_DIRECTION.md#implementation-map) distinguishes
current owners from planned relationships. The [program component](../src/kernel/program/README.md)
owns the currently implemented LPD/LPG formats and checks.

The software backend has host/property checks, release builds and a two-board
retained-input check in both chip directions. It established local resume, argument
updates, HIGH/LOW, stale/duplicate rejection and reconstruction using software-backed
appliance states. See the component notes for evidence and limits. Local record
interpretation remains until native lowering; physical circuit offload is unproven.

## Shared channel foundation

The shared logical channel is implemented in [radio/channel.rs](../src/kernel/radio/channel.rs) with Unique,
Temporal and Complete policies. The existing wire codec supplies a decoded update
or indexed piece; the supplied definition exposes its metadata without a second
decode. This is one mechanism over the existing transport, not a transceiver or
task per message type. The [policy contract](ARCHITECTURE.md#composable-channel-policies)
remains independent of the physical radio.

The channel binds a message codec, stream identity, policy composition and bounded
storage. Resolve policy combinations at channel setup/build time. The shared
mechanism owns inspection and acceptance; commit its sequence/coverage evidence
only when storage has accepted the data. Duplicate/stale results can produce a
receipt without delivering the message to the handler again.

Control targets and the bounded state-notice forwarding cache now use Unique +
Temporal. Program installation and firmware/header reception use Unique + Complete.
Fragments use their existing 512-byte staging buffer and a 64-byte coverage map;
firmware uses a 192-byte receive window before contiguous flash writes. Signature
verification, protected staging and whole-image verification remain in the updater.
Retained connection inputs now use Unique + Temporal directly. Signal updates
replace the old visit protocol. The bounded relay-notice cache remains separate
from these per-connection inputs.

Existing codecs own encoding/decoding; channel definitions expose sequence/extent
metadata from decoded messages. Stream identity stays in the owner's existing
validated binding. Appliance implementations gain no protocol knowledge.
Keep received-state ownership with existing consumers and allocated
bindings rather than creating a replica of every attribute in the network.

## Activation propagation

Each DAG describes predicates leading to a discrete system-state mutation. A user
schedule such as sunset-on and midnight-off compiles to two DAGs. Installed
relationships and activation persist between changes; the automation is not a
long-running call, and LOW does not imply a compensating off/zero mutation.

Activation is retained between reliably delivered deltas in this first stage.
There is no periodic continuity carrier or guessed expiry. A qualified local-state
change, local deadline or accepted incoming signal update recomputes the affected
connection directly from its established inputs, without walking upstream to the head.
Incoming activation starts LOW and remains volatile. A local predicate may evaluate
true without propagating; an upstream HIGH triggers re-evaluation. Do not restore
pre-reboot adjacent activation or values. A missing scalar is not replaced with zero.
An active-path end-to-end heartbeat is deferred until after this migration. The
interim software model can retain HIGH after an isolated upstream failure; the
current shared-power test setup accepts that limitation, not a general reliability
guarantee. ACK/retry handles issued updates, not silence from a failed origin.
Only a true, enabled head originates initial HIGH. A false head leaves the default
LOW unchanged. No startup neighbor-state query or reconnect announcement is implied.

Each addressed connection retains the activation/value context needed to resume its
existing local fragment directly. Preserve the existing expressions, comparisons,
AND/OR branch decisions and mutation calls. The change is where evaluation starts
and how far its effects propagate, not a new logic model or a linear-only subset.
Shared computation does not merge the addressed pairs. Common receive policy rejects
a newly arriving obsolete update; it does not cancel an earlier accepted evaluation.

Existing bounded arithmetic and the local record reader may serve this explicit
first migration stage. Incoming messages become connection updates, not visits
with a program counter and carried stack. Native calls remain finite RTOS work;
already-running calls finish. LLVM subsequently replaces local record dispatch
while retaining connection ownership and propagation semantics.

Sources are qualified appliance states, including binary, enumerated and bounded
scalar values. Firmware gains no networking, evaluator or publication responsibility.
No periodic scan of all predicates or new scheduler is introduced.

The activation queue stores dependency cursors and reads current owned state at
evaluation time. It is not a sample history. Incoming carried values, output payloads
and working data share the existing 4 KiB arena; declaration-based admission leaves
space for concurrent updates. Exact capacities, indexed reception and compaction
belong in the [component](../src/kernel/program/README.md#storage-and-capacity).

No old test automation is live, so compatibility with deployed automations is not
a migration constraint. Do not redesign the program format merely on that basis;
keep the existing logic and change only representation required by the migration.
Preserve unrelated appliance and clock configuration. Do not infer new mutations
or resting actions from existing false-stop paths.

## Appliance integration exercise

Implement and validate the revised kernel primitives first. The published interface
in [the appliance component](../src/kernel/appliance/README.md) is the starting
point, not a completed immediate-mutation contract. Only after kernel readiness
should the four representative appliances below consume it. Mathematical
surface descriptions serve FSPS/authoring; they do not make the kernel responsible
for appliance calibration, scalar limits or control algorithms. Reuse the declared
state/operation bindings when attaching that metadata, rather than adding a second
registry or imposing a mathematical interpreter on firmware operation.

| Capability to support | Why it is needed | Responsibility |
| --- | --- | --- |
| Typed state backed by registers, buffers or ordinary memory | Relay output, dimmer setting, fan/TV command model | Firmware supplies the location/access and meaning; kernel primitives provide integration independently of backing storage |
| Initial state, unavailable values and later corrections | Hardware startup and externally controlled or partly observable appliances | Firmware establishes what it knows; initialization does not replay actuator commands |
| Qualified external change events | Switch input, sensor/feedback IRQ or device response | Hardware/firmware establishes the change; kernel wires handling and delta production without a second getter-comparison loop |
| Firmware-originated state transitions | Commands, controller phases and software-maintained models | Normal state/operation primitives automatically register changes; no separate Lattice publish call |
| One operation affecting several states | Fan speed also changes power; dimmer behavior may couple power and brightness | Firmware defines the relationship; kernel must not impose a universal scalar-to-power rule |
| Finite native operations and completion/failure | Relay writes, dimmer configuration, radio/TV commands | Appliance hook performs the operation inside a kernel-owned wrapper; request acceptance is not itself a state transition |
| Bounded controller invocations and rearmable deadlines | Diffuser duty cycling, ramping or appliance-specific timing | Firmware owns the algorithm; kernel timers invoke finite steps without a sleeping controller task |
| Appliance I/O independent of Lattice transport | Fan OOK radio, serial/bus hardware, TV protocol | Firmware/driver owns appliance communication; the kernel owns graph-network communication |
| Shared-resource access and event handoff | ISR/task interactions and concurrent native calls/controller activity | Reuse kernel synchronization and bounded queues; state capture must not depend on later resampling |
| Volatile/nonvolatile state policy | Calibration, learned estimates and controller settings | Declare retention; the kernel commits and restores data through its existing flash owner, independently of mutation urgency |
| Declared math metadata alongside operational bindings | FSPS needs bounds, effects and invariants | Appliance defines semantics; authoring/synthesis consumes them; kernel keeps transport/type/resource checks distinct from device policy |

The four implementation cases are:

| Firmware | Concrete behavior the interface must accommodate |
| --- | --- |
| Binary relay | A command and a qualified physical switch event use the same relay control path. The Power transition automatically emits a delta. Switch interpretation is appliance-specific. |
| Scalar dimmer | Firmware maps a declared level to the actual electrical control. A fade, if supported, changes levels through the same primitives. Kernel integration does not assume that off discards the selected brightness. |
| Fan | The initial command-model implementation maps operations to appliance RF and changes its local state, including coupled speed/power changes. A feedback-equipped version binds observed changes through the same interface. Lack of feedback in the existing Jarvis implementation is not a fan/vertex limitation. |
| Diffuser | SetIntensity configures a firmware-owned controller. Fan-running events and deadlines drive startup-compensated cycling. Intensity can remain unchanged while Power changes after the setter returns; every declared transition is integrated automatically. |

Use the TV command model as an additional interface review case for software state,
asynchronous replies and imperfect observability. This is not a requirement to add
new TV hardware or implement a TV driver as part of the first four integrations.

Use all four requirements to judge the kernel interface. Implement the common
primitives in the kernel and qualify both ports before touching consumer firmware.
Then integrate relay/dimmer, followed by fan/diffuser. Keep device logic with its appliance owner; generic kernel tests
must not depend on those repositories. Board-specific proof waits for actual bindings
and hardware; a software stand-in does not establish physical integration.

For each blocker, identify the missing operation and where it belongs. First check
whether an existing kernel/HAL facility or a simpler firmware composition resolves
it. Extend the kernel only for a demonstrated missing primitive; an appliance rule,
temporary board limitation or awkward provisional interface is not automatically a
new kernel concept. Resolve implementation-defining ambiguity before adding code.

After these integrations, review the kernel against what they actually use. Remove
redundant state storage, duplicate event paths, polling fallbacks superseded by the
new boundary, unused abstractions and temporary hooks. Preserve facilities justified
by concrete kernel obligations such as recovery, even if no example directly calls
them. Archive useful experimental material outside source. Apply the same review to
tests and fixtures, and update the ownership map before moving to native lowering.

## Declaration and model availability

State is data, selected once through `StateHandle`; the old per-state trait,
public StateInput and manual bind forwarding are removed. Kernel initialization
connects software state and hardware events. Native methods register directly, or
through the optional behavioral StateTransformer trait. Neither requires IR.

IR operations and appliance mathematics are explicit attachments. Absence is
unknown behavior for synthesis, never a default empty effect or permissive condition.
The cheap discovery marker lets nucleus skip absent/incomplete models before a
full transfer; declared complete still needs its assessment. LVT v8/LSF v4 encode
this separation and the descriptive appliance name, with the exact format owned
by the peer component. The name is excluded from compatibility identity.

## Compilation, setup and operation

The appliance implementor builds its device state machine using the kernel's primitives
and supplies its hardware/state bindings. Mathematical surface metadata describes those
members for synthesis and authoring. Declared state transitions and operation/event
hooks carry automatic Lattice behavior through kernel-owned wrappers. Sealed internal
interfaces protect required integration; appliance hooks remain implementable externally.
Firmware owns device behavior, not reporting, graph evaluation or network participation.

The getter-sampling boundary has been replaced by typed state/event primitives.
Complete the appliance integrations and pruning pass before the native backend.
Broadcast appliance deltas and directed circuit continuations keep their separate jobs;
receiving a general appliance broadcast is not an instruction to evaluate a foreign DAG.

- The appliance build binds operational state, events and actions through the supplied
  HAL and kernel primitives. Opaque getter code alone cannot identify physical resources.
- Circuit authoring supplies owner-local logic and connections using existing IR
  semantics. The tablet compiles its user-authored graph; nucleus synthesizes a
  program and then uses the same compiler. The tablet does not run FSPS.
  No per-automation firmware rebuild is required.
- Boot/install validates the compiled artifact, relocates and binds resources,
  applies setup and establishes input validity before activation.
- Ordinary operation propagates input/output changes through configured paths.
  Required CPU handlers are explicit, bounded target implementations; there is no
  implicit fallback to the old interpreter or per-delta expression reconstruction.

The concrete artifact format and resource relocation ABI are not selected.
The shared LLVM backend belongs in a separate crate in the lattice-ir repository;
the firmware-facing core remains `no_std` without LLVM dependencies.
Keep semantics in lattice-ir, hardware recipes with their port owners,
wire encoding in the existing shared codec and installation in program/storage.
Do not create a parallel IR or a new generic runtime.

### Native compilation follows maintained connections

The selected next direction is IR lowering through LLVM to machine code for both
CPU targets. LLVM belongs in the compiler toolchain, not the MCU runtime. Before
that stage, specify the native entry ABI, typed input/output locations, authorized
native bindings, relocation and executable-memory placement for Thumb and RV32.
Preserve declared-width arithmetic, meaningful output changes and invocation
semantics. A generated instruction image must not restore whole-graph traversal.

Compiler crate ownership is settled; native artifact and interface details remain
to settle with vertex. The tablet and nucleus use this compiler before installation;
neither participates in ongoing execution. Future hardware lowering must use the same
mathematical definitions; it must not infer intent by disassembling native code.

## Following behavior and validation scope

Validate the retained-input implementation on both targets with installed predicates,
branch paths and native calls. A scalar example is:

    A: head enable and occupancy
    B: qualified illumination and local time-window signal
    C: SetLevel with the argument computed by B

    A.out = enabled AND occupied
    B.out = A.out AND (illumination < threshold) AND time_window
    C invokes when B.out rises, or its required argument changes while high

Withdrawal closes the path without synthesizing `SetLevel(0)`. If the user also
wants an off action under another condition or at another deadline, that is a
separate DAG with an explicit SetLevel(0) endpoint.

A, B and C are logical ownership roles. A two-board check can co-locate B and C;
swap target roles to exercise both ports without claiming a three-owner radio run.

An illumination change at B updates B from its maintained A input. It does not
travel backward to A. A time boundary does the same through a local deadline event.
A low from A propagates through B to C even if B's own predicate remains true.
At C, a high transition invokes the mutation, a low withdraws activation,
and a changed argument while high invokes with the new value. Unchanged effective
output and duplicate delivery do not cause another action. Required current inputs
must be established before this truth table applies; unavailable is not low.

The digital variant fixes the active argument. The analog variant changes it with
illumination. The transfer function, quantization and physical range must be authored
explicitly; this example does not invent a lux-to-brightness law. A scalar comparison
producing a Boolean result needs no "analog" flag on every edge.

Boolean composition also needs a serial short-circuit mapping: AND proceeds to the
second condition when the first is true; OR bypasses it when the first is true and
proceeds to it when false. Local versus remote placement changes the connection,
not this meaning. No parallel join or standalone AND/OR instruction is mandatory.
Account for selected-path handover and low propagation without reinstating a fresh
whole-DAG traversal for each change.

## Target capability map

This is reference material for the separate hardware investigation, not a prerequisite
for the software migration. These are **candidate mappings**, not completed recipes.
Compose peripherals before concluding that a primitive needs CPU work. PPI and ETM
are event interconnects, not proof by themselves of complete Boolean logic or data
movement. Free capacity must exclude resources already owned by the kernel,
radio and other installed components.

| Primitive or boundary | nRF52840 basis | ESP32-C6 basis | Mapping still to establish |
| --- | --- | --- | --- |
| Binary source/action | GPIO/GPIOTE edge events and SET/CLR tasks | GPIO edge events and ETM GPIO tasks | Polarity, initial level, qualification and pin ownership |
| Conditional path and retained continuity | PPI group enable/disable tasks and EGU event stages | ETM plus event/task capabilities of connected peripherals | Complete gates/latches, including local changes while upstream stays high; channel enable alone is not a complete gate |
| Analog predicate | LPCOMP/COMP references and crossing events; SAADC limits | SAR ADC digital threshold monitors and conversion events | Threshold representation, calibration, hysteresis, qualification and actual event routes |
| Numerical value movement | Peripheral registers and EasyDMA buffers | Peripheral registers and supported GDMA paths | Format/alignment, coherent updates, buffer ownership and completion |
| Time/counter predicate | TIMER/RTC compare/capture through PPI | GPTimer/SYSTIMER/RTC events through ETM where exposed | Reserved RTOS/radio resources and next deadline; retain bounded civil/solar calculation |
| Scalar output | PWM sequence/data and tasks | LEDC/MCPWM or another declared output peripheral | Value conversion, update boundary and explicit mutation |
| Radio input/output | RADIO events/tasks, SHORTS and EasyDMA | 802.15.4 MAC/DMA/interrupt paths | Packet validation, connection selection, value placement, TX ownership and receipts; do not assume C6 RADIO is an ETM endpoint |

Nordic's [PPI specification][ppi] documents 20 configurable channels, 12 fixed
channels and six channel groups; [GPIOTE][gpiote] has eight channels and a pin may
belong to only one channel at a time. [LPCOMP][lpcomp] and [COMP][comp] cannot run
simultaneously. LPCOMP and SAADC cannot simultaneously use the same analog pin.
Count these shared resources rather than assuming a comparator per state/predicate.
[SAADC][saadc] limit events also require distinguishing repeated qualifying samples
from actual output transitions.

C6 has 50 ETM channels with specified event/task endpoints. Its [continuous ADC
monitors][c6-adc] can report out-of-range on **every sample**, which is not automatically
one threshold-crossing delta. Use the [ETM documentation][etm] and [technical reference
manual][c6-trm] to establish concrete routes. Do not infer a continuous analog
comparator or particular DMA connection from the existence of ADC/ETM.

PPI carries events, not arbitrary scalars or Rust function calls. Compatible
producers/consumers may share DMA storage; generic memory copies, arithmetic and
packet-field tests still need an identified mechanism. Nordic's [wireless timer
example][nordic-example] demonstrates event-triggered radio work and a self-disabling
PPI group, with CPU setup/data work around it. It is a composition reference,
not proof of a CPU-free Lattice node.

## Implementation sequence

| Step | Concrete result | Owners and completion evidence |
| --- | --- | --- |
| 1. Consolidate channel policies (implemented) | Define codec/storage bindings and implement shared Unique, Temporal and Complete admission in actual consumers. State updates reject duplicates/older arrivals; complete transfers collect every piece. | Existing radio/peer/program/update owners. No unused generic harness or duplicated payload storage. |
| 2. Retain activation/value context (implemented) | Bind and budget the retained inputs needed to start evaluation at the affected local fragment. Preserve the existing operators, branches and mutations. | Existing program/image/validation and source/time owners. No backward activation requirement or new logic model. |
| 3. Validate propagation on both ports (selected bench check passed) | Exercise local and upstream changes, both activation directions, argument changes, duplicate/late arrivals, reconstruction and pair isolation. | Compact component properties plus maintained hardware fixtures. Report latency and CPU work without claiming hardware realization. |
| 4. Revise the kernel primitive boundary (in progress) | Implement authoritative state, immediate mutation, bounded work/timers and retention; validate both ports before consumer integration. | Existing state/surface/appliance, radio, storage and port owners. No firmware substitutes for missing kernel behavior. |
| 5. Replace local interpretation | Lower IR through LLVM to native code for both targets, using the agreed typed input/output and native-call ABI. | Compiler and existing loader/port owners. Same evaluation meaning, correct target execution and no per-delta opcode dispatch. |

Steps can share implementation work, but a packet-only demonstration does not
establish the complete circuit. Stage 2's local evaluation reuse is an explicit
intermediate implementation, not evidence of native compilation. No new firmware
callback or preinstalled appliance-specific automation substitutes for installation.

## Decisions to close before dependent implementation

Retained activation between delivered deltas, unchanged evaluation logic, and the
common channel policy meanings are settled. These mechanics remain implementation work:

- **Native artifact (following stage):** entry ABI, relocation,
  code placement and typed/native bindings for LLVM-generated Thumb/RV32 code.
- **Active heartbeat (deferred):** periodic head-to-tail traversal over the selected
  active connections, with a correlated pong addressed to the head. Existing
  connection identity distinguishes DAGs sharing one head; the MAC supplies the
  return address. Choose interval, missed-pulse allowance, local activation expiry
  and recovery/retrigger handling before implementation. A heartbeat is not an
  action request or an appliance delta. No expiry or carrier is implemented.
- **Hardware realization (separate investigation):** a complete receive/gate/transmit
  path and its resources. Adding a processor at each missing connection is not the
  selected solution, and a local comparator does not establish autonomous forwarding.

## Proportional validation

Use compact tables for Boolean truth/transitions, argument-change qualification,
connection ownership, encoding, capacity and resource release/reuse. Cover both
causes of a gate change, both directions, unchanged output, duplicate delivery and
unavailable inputs. Repeated threshold/sample events must not become repeated actions.

Use the same maintained check on both chips and cross-vendor signal delivery.
Measure event-to-output/packet latency, CPU interrupts/wakeups, allocated peripherals
and production RAM/image footprint. Distinguish direct hardware paths, bounded CPU
handlers, build-only evidence and physical behavior. Software-backed atomics and
packet transfer do not prove hardware circuit realization. C6 remains at 20 dBm;
RF-range work stays parked. Optional USB diagnostics are not ordinary-node costs.

Tests retain the repository's boundary/property discipline and 2:1 review threshold.
Reuse OTA, protected storage, discovery, manual continuous control, local time and
MAC reliability; changes to them need a concrete dependency.

[ppi]: https://docs.nordicsemi.com/r/bundle/ps_nrf52840/page/ppi.html
[gpiote]: https://docs.nordicsemi.com/r/bundle/ps_nrf52840/page/gpiote.html
[lpcomp]: https://docs.nordicsemi.com/r/bundle/ps_nrf52840/page/lpcomp.html
[comp]: https://docs.nordicsemi.com/r/bundle/ps_nrf52840/page/comp.html
[saadc]: https://docs.nordicsemi.com/r/bundle/ps_nrf52840/page/saadc.html
[etm]: https://docs.espressif.com/projects/esp-idf/en/v5.5.4/esp32c6/api-reference/peripherals/etm.html
[c6-adc]: https://docs.espressif.com/projects/esp-idf/en/v5.5.4/esp32c6/api-reference/peripherals/adc_continuous.html
[c6-trm]: https://documentation.espressif.com/esp32-c6_technical_reference_manual_en.pdf
[nordic-example]: https://devzone.nordicsemi.com/guides/short-range-guides/b/bluetooth-low-energy/posts/wireless-timer-synchronization-among-nrf5-devices
