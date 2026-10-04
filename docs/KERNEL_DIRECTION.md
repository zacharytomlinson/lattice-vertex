# Lattice vertex kernel direction

This document records the implementation direction for extending the imported
FreeRTOS kernel into a cooperative distributed RTOS. Read it alongside the
[architecture white paper](Lattice-DRTOS-Architecture-White-Paper.docx). The scope,
local participation model, memory approach, and development approach below govern
work in this repository. They describe what we are building, not functionality
already implemented.

## Kernel and firmware responsibilities

The kernel implements the common behavior of a lattice node. Firmware authors
plug their appliance into that behavior through the interfaces we provide. They
should not assemble a protocol from an unstructured collection of primitives or
reimplement enumeration, change broadcasts, conditional evaluation, or forwarding.

The firmware implementor supplies:

- The appliance's typed mutable attributes: for example, a binary power state,
  scalar brightness, or enumerated fan speed.
- Its typed state-transforming function handles, backed by the concrete hardware
  operations that modify those attributes.
- The hardware facilities the kernel needs, including the platform's timing and
  interrupt support and primitive peer communication operations.

Express these requirements through small interfaces with static dispatch. A
transceiver trait is a likely requirement; its exact name and methods will follow
the communication code's actual needs. The kernel does not require a new HAL or
an appliance driver framework.

Hardware observations enter through the supplied attribute interface. From that
point, the kernel handles the dependent activity and communication. Firmware
authors provide access to their state and functions; they do not manually arrange
change propagation or evaluate graph conditions.

Migrating firmware such as `diffuser-fw` or `mhz-radio-fw` means retaining its
appliance-specific hardware code, exposing its attributes and functions through
these interfaces, and replacing its ad hoc networking and prior surface
enumeration with the kernel's common node behavior.

The shared [`lattice-ir`](https://github.com/zacharytomlinson/lattice-ir) crate
defines the mathematical surface of those attributes and transformations.
Firmware can expose these definitions as static data. The nucleus owns proof,
synthesis, and compilation, and delegates compiled fragments to participating
vertices. The shared crate contains definitions, not an interpreter or solver;
vertices execute the delegated fragments without hosting an IR interpreter.
Mathematical effect outputs describe post-state for reasoning. Runtime changes
are communicated through the node's change messages, not program return values.

## Cooperation produces the distributed kernel

The kernel exists collectively across participating nodes. Each node responds to
a local hardware change or an incoming message by doing the part assigned to it:

- Expose and enumerate its attributes and state-transforming functions.
- Broadcast changes and pass activation messages upstream toward the relevant
  program head through its resident dependencies.
- Execute its assigned compiled conditional and pass the continuation downstream.
- Invoke a reached local state-transforming function.
- Offer relay participation and forward peer messages.

These successive local responses produce the distributed execution. Each node
needs only its own state, functions, executable relationships, and the references
needed for its communications. There is no separate executor conducting a walk
over the distributed graph. The broader installation, recovery, and protection
behavior described in the white paper also belongs to the common kernel
implementation rather than being rebuilt by each appliance author.

Change is the unit of execution. Process local changes and received messages as
they arrive. There is no graph work queue, additional graph scheduler, or periodic
evaluation cycle. FreeRTOS already supplies the local execution facilities,
including tasks for overlapping evaluations when needed. Local timing remains
available for hardware, communication, and explicitly temporal operations; an
omnipresent clock does not drive graph evaluation.

## Structured memory and resident edges

An attribute is actual mutable appliance state exposed to the network. Edges
through attributes carry conditional evaluation; edges through function handles
carry state modification. Together they embody the distributed executable.

Keep an attribute, its dependent edges, and their upstream relationships tied
together in the memory structure. One-to-many relationships are fundamental.
Use structured memory and direct references with no dynamic allocation for the
lattice node machinery. Firmware-resident vertices are terminal hardware
participants, not objects that need relocation between machines.

Choose the concrete layout against the target's available memory and hardware.
A static structure is appropriate when its layout minimizes padding and its
access adds no unnecessary indirection. Do not impose a matrix, generic edge-slot
pool, auto-incremented object IDs, relational lookup layer, or ECS. Existing MCU
storage and registers may be useful where their hardware semantics permit it.
The particular field layout remains an implementation choice.

Fixed-width communication rings with hard eviction are transport storage. They
do not imply a queue of graph jobs. Reuse the RTOS facilities already present and
add the local mechanics that lattice participation actually requires.

## Implementation approach

ESP32 is the initial platform. The flashed firmware will include this kernel.
Existing ESP Rust HAL code and nearby firmware are platform references.
`lattice-ir` starts with the shared mathematical definitions extracted from
Jarvis's `zmt/rewrite` branch. Its formal development continues with the nucleus;
the kernel can use the shared definitions to build surface exposure independently.

Build working kernel code in meaningful increments, compile as we go, and learn
from concrete failures. Use focused checks where they answer a real question.
QEMU is available when useful; exhaustive simulations and speculative failure
models are not prerequisites to ordinary progress. Physical hardware testing
happens when the user chooses that the implementation is far enough along.

Start with the kernel interfaces and local participation behavior. Settle packing
and hardware integration through implementation. Relay tuning and hypothetical
storage-reuse problems must not displace that work. Keep the code small, direct,
and economical in memory; avoid extra abstractions and machinery without a
current purpose.
