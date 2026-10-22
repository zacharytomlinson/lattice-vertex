# Lattice vertex development guidance

Read [the architecture](docs/ARCHITECTURE.md) for system semantics and
[the kernel direction](docs/KERNEL_DIRECTION.md) for current status and next work
before implementation. [Documentation ownership](docs/README.md) identifies where
changes belong. The DOCX white paper is generated; never edit it independently.

We extend the inherited RTOS into one kernel. Preserve the architecture's stable
firmware boundary: appliance code encapsulates hardware; the kernel owns Lattice
participation. Do not make firmware authors precompile automations, construct
messages, or understand program/transport internals. Keep changes concrete and
small; neighboring repositories are references, not sources of workflow gates.

## Clarify direction before provisional implementation

Questions are preferred over provisional work. An assumed implementation followed
by its removal imposes avoidable double work.

- "What is next?" asks for an informed direction, not coding without understanding
  the next stage. A general go-ahead does not resolve implementation-defining gaps.
- Explain the intended end-to-end behavior, representation, ownership and activation
  before a new architectural step. Distinguish implemented, proposed and undecided.
- When an unresolved decision changes the implementation, stop that dependent work,
  explain the uncertainty and ask. Do not fill the gap with speculative APIs,
  firmware-baked substitutes or assumptions about future components.
- Testing and scaffolding are not substitutes for resolving a missing design. A
  narrower demonstration does not establish the requested capability.
- Agree on a disposable experiment's concrete question before building it. Keep it
  outside kernel source and do not promote its assumptions into the public API.
- Use meaningful increments. Resolve routine mechanical choices independently once
  the behavior is understood; approval is not required for every small edit.

## Naming and organization

- Organize modules around concrete responsibilities, keeping related records and
  operations together. Memory efficiency and readability are both requirements.
- Names must describe the record or operation. Distinguish reading state, changing
  it, registering an observation, evaluating a predicate, invoking and forwarding.
- Distinguish a local list link from a distributed graph relationship. Comments
  explain mechanics and constraints; they must not rescue ambiguous abstractions.
- Do not invent entities or one-file-per-type layers merely to house some code.
  Use Redox and Bitcoin as references for clear responsibility-based organization.
- Keep useful FreeRTOS-derived mechanisms and their attribution within this kernel.
  Use existing owners rather than adding parallel bookkeeping or execution systems.

## Tests and experiments

- Keep one-off experiments outside the repository. Ignored `experiments/` and
  `lab/` paths are only a backstop; never commit/package them or make kernel code,
  tests or builds depend on them. Archive useful work before removing it.
- Colocate portable checks and maintained hardware tests with their owners.
  Board-specific linker/upload support is legitimate. Vendor A/B images, RF
  diaries, flash capture recorders and appliance prototypes belong in the lab.
- Test-only feature gates do not make experimental code a permanent kernel API.
  Keep hooks only when maintained checks use them. Generic tests/examples must not
  depend on unrelated appliance repositories.
- Test component properties and prescribed bounds: capacity, domains, ownership,
  ordering, encoding and transitions. Name the property, not an application story.
- Use one focused assertion path with tables for equivalent partitions/boundaries.
  Extend its existing owner. A table of scripts is still a scenario harness.
- Keep setup smaller than the property. Do not recreate an appliance/network to
  check a local invariant or share fixtures across unrelated test owners.
- Test-to-operational code above **2:1 requires explicit substantiation**.
  Approaching **3:1 triggers structural review**. Count helpers and fixtures using
  the same formatted-line measure within the owning component; do not hide costs
  or dilute them with unrelated kernel code. Lower ratios do not excuse weak tests.
- Keep build success, runtime execution, packet delivery and physical behavior
  distinct. Run checks appropriate to the change, not tests for their own sake.

## C6 RF investigation

ESP32-C6 TX stays at **20 dBm**, including vendor references. Do not lower/sweep
power or repeat board substitution: all three boards/pairings already showed
similar RSSI. Pursue a concrete shared PHY/receiver/clock/initialization issue.
Capture through USB serial. Never scan or reconfigure the laptop Wi-Fi adapter;
it carries the active connection and an earlier scan interrupted it.

## Review after each meaningful change

Use the single [implementation map](docs/KERNEL_DIRECTION.md#implementation-map)
as the working graph of ownership and relationships.

1. Read affected code and check the existing map before changing responsibilities.
2. Update that map when relationships change; mark planned behavior honestly.
3. Review behavior, naming and organization for collisions, duplicated storage or
   responsibility, unclear ownership and abstractions invented just to house code.
4. Resolve concrete mismatches within the change and briefly report any remaining
   structural concern. Keep this proportional; the map is not another approval gate.

Keep each fact in its documented owner. Update the architecture for semantic
changes, the direction for status/next work, and component notes for implementation
mechanics. Do not append command transcripts or completed milestones to the roadmap.
