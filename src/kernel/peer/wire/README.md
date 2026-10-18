# Shared numeric wire definitions

`lattice-peer-wire` owns the declared-width scalar/argument codec used by peer
frames, surface literals, and attached-host messages. The current consumers use LVT v7, host v4, and LSF v1. Values and integer domains
come from `lattice-ir`; protocol status values live here so the host need not invent
a second interpretation of native dispatch or state-read failures. The registered
`StateChange` record is also shared by RF deltas and host events.

The optional `host` feature adds the bounded local packet records and COBS stream
codec described in [the host interface](../host/README.md). This replaces the former
`lattice-host-link` crate under `host/wire`: scalar encoding now has both radio and
USB consumers, so its owner is the common peer wire crate. The crate contains no
kernel execution, platform support, solver, or interpreter.

`cost::TimePerByte` is the shared normalized time unit: microseconds per
acknowledged MAC payload byte in Q24.8. It supports path composition, exact
relative ratios, and quantity estimates without allocation or floating point.
Zero represents an empty path, `0xffffffff` unknown; unknown/overflow never
becomes a free path. Measurement and routing policy belong to the kernel.


`update` owns the bounded header/data/preparation/status request grammar and
progress reply shared by radio forwarding and USB. It also defines local-only
update maintenance controls. Signature verification, journals, flash access, and
activation decisions stay in `kernel::update` and the platform ports. The optional
host codec adds kinds 5/6 and 133/134 without altering existing host-v4 records.
See [the updater](../../../../docs/FIRMWARE_UPDATES.md#implemented-signed-transfer)
for the signed package and byte layout.
