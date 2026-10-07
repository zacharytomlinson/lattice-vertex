# Lattice vertex development guidance

Read [the kernel direction](docs/KERNEL_DIRECTION.md) before implementation.
It records the scope, execution model, memory approach, and development approach
for this repository. Use it with the linked architecture white paper.

This repository extends the RTOS with the common behavior of a lattice node.
Appliance firmware supplies its attributes, state-transforming functions, and
required hardware interfaces. Preserve that division of responsibility.

Keep changes concrete and small. Follow the development approach in the kernel
direction document; do not import workflow gates or architectural assumptions
from neighboring reference repositories.

## C6 RF investigation

Keep ESP32-C6 transmit power fixed at **20 dBm** for all runs, including vendor
reference comparisons. Do not lower or sweep TX power as a diagnostic step.
All three boards and their pairings have already produced similar RSSI; pursue
shared PHY, receiver, clock, coexistence, and initialization issues rather than
repeating board substitution or treating the selected power as an open choice.

Capture through USB serial. Do not scan or reconfigure the laptop's Wi-Fi adapter
for these RF tests; it carries the active internet connection, which an earlier
host scan interrupted.

## Naming and organization

Data-oriented code depends on precise names and a legible module structure.
Memory efficiency and readability are both requirements.

- Organize files and modules around concrete responsibilities. Keep related data
  and operations together so the module path, type, and function name explain
  what an operation does.
- Give structs names that describe the records they represent. Introduce a type
  or abstraction when the data or behavior requires it; avoid unnecessary layers
  and fragmentation into one file per type.
- Use verbs that distinguish reading state, updating it, signaling a change,
  evaluating a condition, invoking a transformer, and forwarding a message.
  Name the behavior actually implemented, with its scope clear from context.
- Use domain terms consistently across code and documentation. Distinguish local
  list links from distributed graph relationships; a list's first entry is not
  automatically a DAG's entry point. Comments should explain mechanics and
  constraints rather than rescue ambiguous names.
- Use Redox and Bitcoin as references for clear responsibility-based organization.
  Let Lattice's implemented responsibilities determine its modules; add structure
  as the code needs it.
- Treat `experiments/` as temporary bring-up scaffolding. Move reusable code into
  platform ports and useful checks into kernel tests as integration proceeds,
  then remove the directory. It is not part of the permanent kernel layout or
  an acceptable dependency of the kernel runtime.

## Review after each meaningful change

Use the [implementation map](docs/KERNEL_DIRECTION.md#implementation-map) as a
working graph of the code's concepts, ownership, and relationships.

1. Before a change, read the affected code and check the map against it. Establish
   the existing responsibilities and relationships before introducing new ones.
2. After the change, update the same map when those relationships change and
   compare the result with the starting structure.
3. Review naming and organization alongside behavior: look for concept collisions,
   duplicated responsibilities or storage, unclear ownership, and abstractions
   invented merely because a piece of code needed somewhere to go. Check whether
   module, type, and function names still describe what they contain and do.
4. Resolve concrete mismatches within the change and briefly report any remaining
   structural concern. Evolving the organization is a normal part of engineering.

Keep this proportional to the change: one maintained map and a focused review.
Show implemented relationships accurately and distinguish planned behavior. The
map supports iteration; it is not a fixed blueprint or an additional approval gate.
