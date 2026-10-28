# Maintained peer links and traversal

Every vertex maintains adjacent peer relationships as common kernel behavior.
`ReadLinks` exposes those relationships in a fixed numeric protocol understood
before an appliance's IR is read. Appliance authors supply no link definitions,
maintenance callbacks, network configuration, or enumeration handler.

A link is distinct from a learned path toward a farther owner and from an
installed program edge. The local link records provide a bounded piece of network
memory and a direct forwarding direction. Destination paths can be learned through
those links. The destination cache now retains two directions per owner, ranks
normalized directional time per payload byte, and supports a bounded invocation split.
See [invocation forwarding](INVOCATION.md#forwarding-and-duplicate-commands).
Link measurements include ACK/retry service time. They do not establish independent
end-to-end failure probabilities. Unknown cost is distinct from a zero-cost path.

## Establishment and maintenance

After channel formation, the existing link-local discovery exchange supplies
candidate addresses. Presence alone does not make a candidate enumerable. One-hop
`Offer -> Accept -> Confirm` messages establish retained relationships:

1. The initiator reserves a candidate and sends an Offer with a nonzero token.
2. The recipient reserves storage and returns Accept, or Reject if full.
3. A matching Accept confirms the initiator's link and produces Confirm. The
   recipient confirms its link upon receiving that matching Confirm.

Only established entries are enumerated. No confirmation echoes another
confirmation. Simultaneous exchanges in both directions are independent. A duplicate
Accept can elicit the same Confirm within its exchange window, but does not keep
renewing the initiator's confirmation time. Lost messages can temporarily leave
asymmetric views; maintenance repairs or expires them. This is not membership
consensus or authenticated admission.

The current storage has eight entries shared by candidates and established links.
Full storage rejects a new offer instead of silently evicting a healthy link.
A vertex may remain without maintained links if no neighbor has capacity. Coordinated
replacement/degree optimization is not implemented. A solitary vertex continues
local operation and discovery.

At most one offer is initiated per existing 2–3 second discovery interval. A
confirmed link becomes eligible for refresh after 15 seconds; oldest attempted
eligible neighbors are serviced first. Pending exchange tokens expire after six
seconds. Entries expire 60 seconds after reservation or their latest successful
confirmation. Mere presence does not renew them. Expired confirmed links also
invalidate learned directions through that peer. The clock arithmetic wraps.
No additional RTOS task, graph work queue, or state-report poller is introduced.
These control exchanges never invoke or retry appliance transformers.

The retained round-trip sample measures the local maintenance exchange in
milliseconds (minimum 1), including processing/scheduling delay. It is useful
a diagnostic. The separate `cost` field is outgoing microseconds per acknowledged
MAC payload byte (Q24.8); it comes from actual sends, not from dividing this RTT.
Its records expire with the neighbor, or become unknown after 60 seconds without
a transmission sample. Neither radio's power configuration changes.

## Reading and following references

`ReadLinks` names a reader, owner, fresh read token, exclusive `after` peer-address
cursor, and optional `via` vertex. With `after=0`, the owner returns its lowest
established peer address. Reusing the returned peer as the next cursor obtains the
next link. An end record indicates that no greater peer address is currently
retained. Every entry or end record fits one frame and includes a membership
revision. The revision changes when links enter/leave the enumerable set; refresh
of a timing sample does not change membership. Cursor order is independent of
storage positions. A consumer can detect changed membership while enumerating;
there is no retained list snapshot or complete network-census claim.

`via=0` uses ordinary owner-directed forwarding. When A advertises B, the consumer
can send `ReadLinks(owner=B, via=A)`. Forwarding heads toward A, then A hands the
request directly to B using its maintained link. If that link has expired, A drops
the stale request rather than turning the handoff into another flood. Reaching B
earlier can also satisfy the read; the hint is a route aid, not an authorization
constraint. A nonzero via must differ from the reader and owner.

The owner's reply follows the retained incoming directions and teaches a path
toward the owner at each relay. The next step can therefore approach C through B,
while subsequent surface reads, state reads, or commands can use the learned path
to B. A crawler keeps its own visited set and requests surfaces only where useful.
It starts from one reachable contact without a preexisting global registry.
Transport still limits a request/reply to eight links; traversal does not bypass
that bound or prove continued reachability after a reply.

All read kinds share the same eight reverse-path records and reader/token
namespace. Link-read correlation includes owner, cursor, and via. Duplicate requests
are suppressed within retained history; a retry requires a fresh read token. The
existing expiry, oldest eviction, and TX-failure rules apply. A failed request
heading toward a via invalidates that direction, not an unrelated target route.
Direct maintained neighbors and cached indirect directions compete on the same
normalized cost; hop count and address only break ties.

## Wire format

These use **LVT v7**. Link replies now include the advertised neighbor cost and
the remaining return-sender-to-owner path cost. Older wire versions are rejected.
The common payload prefix remains magic/version (4 bytes), opcode (1), network
founder (8), and channel (1). Integers are little-endian.

| Opcode | Fields after prefix | MAC bytes including payload, before FCS |
| --- | --- | --- |
| 13: link exchange | Phase `u8` (0 Offer, 1 Accept, 2 Confirm, 3 Reject), token `u32` | 40, addressed one-hop only |
| 14: read links | Reader `u64`, owner `u64`, request `u16`, after `u64`, via `u64`, remaining scope `u8` | 70 addressed; 64 broadcast exploration |
| 15: link entry | Same reader/owner/request/after/via, distance `u8`, membership revision `u32`, entry tag `u8`; tag 1 adds peer `u64` and round-trip milliseconds `u16`, outgoing cost `u32`; tag 0 is end. Both append path cost `u32` | 93 for an entry; 79 for end, addressed reverse path |

Link exchanges are never relayed. Zero tokens, unknown phases, foreign network
records, and broadcast exchanges are rejected. Link entries reject self links,
invalid addresses, zero round-trip samples, peers at/before the cursor, trailing
bytes, and invalid scope. Pending candidates never appear as established entries.
Tokens/revisions are volatile; boot/session identity and admission authentication
remain future protocol work.

## Ownership and checks

`links.rs` owns the resident adjacency records, confirmation, maintenance, and
cursor enumeration. `forwarding.rs` owns their connection to routing and read
return paths. The resident peer task owns both and performs periodic link work
on its existing discovery wakeup. The old automatic startup `FindOwners` search
is removed; explicit bounded owner searches and compact arrivals remain supported.

Link storage has a 608-byte compile-time ceiling for eight neighbors, their
measurement totals/freshness, and bounded update-absence records. It is included
in the forwarder's 2,912-byte ceiling, including four bound control paths. No allocator, per-peer task, foreign-surface store,
new hardware experiment, or test-only receive path is added.

Colocated host checks cover phase/token correlation, simultaneous opens, lost
confirmation recovery, refresh/expiry, full capacity, wrapping clocks, cursor
ordering, and wire bounds. A chained walk uses encoded frames, follows advertised
links beyond the consumer's radio range, and then reads the encountered owner's
surface through the learned direction. These are host/build checks; no new board
run or RF investigation has been performed.


## Update absence

Each retained neighbor has one bounded update-absence record in the same link
storage. Only confirmed neighbors can supply it. Proposed, leaving and cancel
notices stay one hop; they neither reserve a route nor establish a new neighbor.
An announced neighbor remains retained until its bounded absence expires, even
if ordinary refresh expires meanwhile. Duplicate notices do not extend the timer.
The local updater uses these records to defer competing departures, with an
address tie-break for simultaneous proposals. See [the activation policy](../../../docs/FIRMWARE_UPDATES.md#local-activation-policy) for the exact timing and limits.
