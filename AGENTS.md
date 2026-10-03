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
