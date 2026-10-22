# Documentation

Each document has one owner and purpose:

| Document | Purpose |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Canonical system semantics, ownership, interfaces and architectural requirements |
| [KERNEL_DIRECTION.md](KERNEL_DIRECTION.md) | Current implementation map, capability gaps and immediate next work |
| [PROGRAM_DESIGN.md](PROGRAM_DESIGN.md) | Remaining execution integration and its worked traversal; implemented loader/ABI details live beside the component |
| [AGENTS.md](../AGENTS.md) | Collaboration, code organization, test discipline and review procedure |
| [FIRMWARE_UPDATES.md](FIRMWARE_UPDATES.md) | Implemented updater behavior, wire/storage boundaries and remaining limits |
| Component notes beside source | Current APIs, protocols and maintained usage/validation procedures |
| DOCX white paper | Generated architecture export; never an independently edited specification |

Architectural requirements do not imply implementation. The direction distinguishes
working facilities from missing ones. Current user decisions supersede older notes;
reconcile a disagreement into its owner rather than retaining two authorities.
As the program design is implemented, move its accepted mechanics beside their
code and retire the proposal; do not maintain a parallel specification.

Useful component entry points:

- [Peer discovery](../src/kernel/peer/TESTING.md), [links](../src/kernel/peer/LINKS.md),
  [surfaces](../src/kernel/peer/EXPOSURE.md) and [native control](../src/kernel/peer/INVOCATION.md).
- [Wire definitions and communication cost](../src/kernel/peer/wire/README.md),
  [host attachment](../src/kernel/peer/host/README.md) and
  [transport checks](../src/kernel/radio/TESTING.md).
- [Hardware checks](../src/port/TESTING.md), including port-owned build/upload recipes.
- [Package/transfer tools](../tools/README.md) and
  [shared flash storage](../src/kernel/storage/README.md).
- [Resident program records and installation](../src/kernel/program/README.md).

Historical run logs, device-specific captures and superseded investigations live
in the external local lab. They are not kernel dependencies. Maintained procedures
stay beside their source; the roadmap contains only a concise capability summary.

## Export the architecture

Install Pandoc 3.x, then run from the repository root:

```sh
python3 docs/export_architecture.py
```

This regenerates `Lattice-DRTOS-Architecture-White-Paper.docx` from
`ARCHITECTURE.md` and its illustrations under `assets/architecture/`.
`--pandoc PATH` selects a particular Pandoc executable. Edit the Markdown and
regenerate the export in the same change. The export does not consume the old
DOCX, hand-maintained page numbers or a private lab template.
