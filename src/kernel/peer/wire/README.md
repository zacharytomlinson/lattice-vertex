# Shared numeric wire definitions

`lattice-peer-wire` owns the declared-width scalar/argument codec used by peer
frames, surface literals, and attached-host messages. The code was moved from the
peer frame module without changing LVT v6 or LSF v1 bytes. Values and integer domains
come from `lattice-ir`; protocol status values live here so the host need not invent
a second interpretation of native dispatch or state-read failures. The registered
`StateChange` record is also shared by RF deltas and host events.

The optional `host` feature adds the bounded local packet records and COBS stream
codec described in [the host interface](../host/README.md). This replaces the former
`lattice-host-link` crate under `host/wire`: scalar encoding now has both radio and
USB consumers, so its owner is the common peer wire crate. The crate contains no
kernel execution, platform support, solver, or interpreter.
