# Inactive image storage

`lattice-image-storage` owns the bounded staging mechanics shared by the Nordic
and C6 update ports: sequential chunk acceptance, page erase progress, duplicate
comparison, final-word padding, failed-write rejection, and SHA-256 verification.
It has no allocator or image-sized buffer. Work uses a 256-byte aligned buffer;
the candidate record holds bounds, volatile write progress, and incremental
SHA-256 state (or the completed digest).

This small `no_std` crate can be consumed by port boot-support crates without
linking an RTOS into the separately built Nordic loader. It is part of the common
kernel source, not an application update loop. The parent `kernel::update` service
owns signed packages, persistent page checkpoints, and local activation. This
crate resumes only from a caller-validated durable offset; it never interprets
network packets or trusts a remote offset itself. Admission and activation policy
remain in the parent updater.

The port owns the inactive reservation and the decision to change boot metadata.
Nordic erases the full active-sized candidate area because its loader swaps the
whole slot. C6 erases only the candidate's sectors and boots that slot directly.
Both ports reject staging during an unconfirmed trial so the rollback image stays
available. Signature authorization remains distinct from a matching byte digest.

The port integration checks exercise this component through their real image
handling paths: [Nordic](../../../port/nrf52840/boot/src/tests.rs) and
[C6](../../../port/esp32c6/boot/src/tests.rs). Flash geometry and address ranges
are checked before erase or write. A durable page boundary resumes the written
prefix while requiring the remaining erase pages to be prepared again.


`verify_step` reads at most the requested byte count and reports completion only
after the entire digest matches. Resuming or modifying the candidate invalidates
its hash state. Ports recheck their small image prefix before readiness/selection.
A verified unchanged region reuses its digest at selection; there is no second
full-image read hidden inside that transition. The synchronous `verify` convenience
is reserved for direct local callers; common staging uses bounded steps.
