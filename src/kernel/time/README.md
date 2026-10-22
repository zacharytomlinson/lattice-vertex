# Local time and solar predicates

Every participating kernel supplies these inputs; an author places a temporal
record at the evaluation owner where it belongs in the program. There is no
special time vertex. Appliance HALs gain no timer-program or calendar callback.
The installed record and activation entry are described in
[program](../program/README.md#definition-format-lpd-v1).

`kernel::time` owns the volatile UTC anchor, cached predicate values and deadlines.
`program::Configuration` retains the coordinates and recurring civil rules beside
program definitions, using the existing protected flash owner. The peer task
registers installed temporal sources and waits until the nearest deadline through
its ordinary RTOS receive timeout. On expiry it refreshes due predicates; a truth
change enters the same bounded activation storage as native changes. Activation
travels upstream before normal forward traversal. Neither a short-circuit nor
head disablement removes the underlying temporal source.

There are at most 32 temporal records per vertex. The installer rejects excess
before committing, including after reconstruction. Storage addresses identify
sources; bit 15 distinguishes a temporal record from a native dependency cursor
inside the 8 KiB arena. No additional distributed identity is assigned.

## Calendar and solar behavior

Civil windows are start-inclusive/end-exclusive local seconds, from 0 through
86,400. Start must be below 86,400; equal bounds mean false. A start later than the
end crosses midnight. The recurring zone contains standard/daylight offsets in
seconds east of UTC, and two month/week/weekday transitions. Week 5 is the last
matching weekday; Sunday is 0. A transition's seconds are relative to local
midnight using the offset before the transition. Equal offsets disable DST.

A spring jump evaluates the window at the new civil time; a skipped window is
not an invented catch-up action. In autumn a window may become true twice if the
clock passes through it twice. These are current-state predicates, not cron jobs.
An anchor correction similarly recalculates current truth. First availability
establishes a baseline; enable/recovery visits already waiting for time can then
proceed. It does not manufacture an additional baseline delta. Deadline handling
is bounded by normal kernel/flash servicing latency; there is no historical
replay of every window missed while the CPU was unavailable.

Solar boundaries are sunrise, solar noon or sunset, each with an offset of at
most one day in either direction. Sunrise/sunset use the 90.833-degree zenith
approximation from Jarvis's pure solar arithmetic. Coordinates are integer
microdegrees. Solar arithmetic runs when finding/rearming a boundary, not per
clock tick. A daily boundary also refreshes seasonal availability. The zero-offset
sunrise-to-sunset window is true during polar day and false during polar night;
the inverse window is complementary. Other unavailable polar crossings remain
unavailable. These are calculated events, not claims about actual cloud cover,
terrain or a physical luminance sensor.

The UTC domain is 2000-01-01 through 9999-12-31. A commissioned rule calculates
future transitions while that rule remains valid; political changes require a
new profile. No IANA database, timezone name, GPS driver or Wi-Fi location service
runs on the MCU.

## Commissioning and reacquisition

Clock commands use the ordinary owner-directed program service and reply shape.
Every number is little-endian. No tablet/nucleus privilege or permanent clock
master is added.

| Command | Request after tag | Successful reply |
| --- | --- | --- |
| 15 profile | Empty | Generation = profile revision; 32-byte profile |
| 16 configure | Expected revision `u32`, profile | Persisting/Busy until committed, then same as profile |
| 17 reference | Empty | Generation = volatile anchor revision; receiver epoch `u64`, UTC milliseconds `i64` |
| 18 anchor | Receiver epoch `u64`, expected anchor revision `u32`, UTC milliseconds `i64` | New anchor revision |

The profile is latitude `i32`, longitude `i32`, standard offset `i32`, daylight
offset `i32`, then two 8-byte rules: month/week/weekday/reserved-zero bytes and
seconds `i32`. An absent profile replies Unavailable with generation zero. An
unanchored clock replies Unavailable with the receiver epoch alone; a receiver
whose startup incarnation has not committed replies Busy.

Configuration compares its expected revision and publishes only after flash
commit. An identical retry acknowledges the existing profile without another
write. Anchor requests compare the receiver epoch and revision; repeating the
same caller/revision/timestamp cannot re-anchor an old timestamp later. Conflicting
or old requests fail. These checks prevent accidental stale replay, not malicious
time injection; authentication follows the existing peer trust model.

After loss of power the profile survives, but UTC is unknown. The node visits its
maintained adjacent peers until one returns a valid reference, trying another
after unavailability/timeout. A half-round-trip estimate corrects transfer delay;
this is an approximate reference, not synchronized clocks or a latency guarantee.
Once anchored it does not continually poll neighbors. A commissioning client or
future GPS observational vertex can provide a newer reference using the same
messages. At whole-network cold start, one externally valid reference is still
needed. A saved timestamp cannot measure an unpowered interval.

## Elapsed counters

C6 reads the existing free-running SYSTIMER via esp-hal, independently of RTOS
tick loss. Nordic uses extended SysTick periods. Its physical flash adapter
measures synchronous NVMC operations with the core DWT counter and credits
coalesced SysTick periods to civil/solar elapsed time. No Nordic TIMER/RTC is
claimed and no scheduler policy is changed. DWT is used only during awake flash
operations shorter than its approximately 67-second wrap; oscillator accuracy,
other long interrupt masking, and debugger halts remain physical timing limits.
Neither port currently promises sub-millisecond reference accuracy or a backed RTC.

## Validation

Owner-local tables cover Gregorian leap cycles, northern/southern DST edges,
gaps/folds, civil and polar/solar bounds, numeric codecs, anchor idempotence,
capacity and deadline-source publication. Interrupted context checkpoints retain
the previous committed profile and preserve existing program references. C6 and
nRF52840 maintained radio and updater targets build with these paths.

The 2026-10-22 live C6/Nordic check verified:

- LPG v2-to-v3 reconstruction preserved prior definition bytes and offsets.
- Both Nordic civil-window boundaries activated upstream through C6 before normal
  forward evaluation/native invocation. Registered states changed 33 → 77 → 33.
- Nordic reboot retained configuration and program wiring. It reported unavailable
  UTC, acquired a reference from C6, and resumed evaluation without a host anchor.
- C6's calculated sunset generated a local deadline and changed 51 → 11. The test
  deliberately anchored UTC 15 seconds before the calculated boundary; this was
  not an observation of actual sunset. System UTC was restored afterward.
- Two Nordic context commits took 422 ms by both host and vertex elapsed clocks,
  exercising NVMC stall accounting. No execution faults, uncertain handoffs or
  local activation overflow were recorded in these runs.

The appliance states were software-backed. These checks establish the deadline,
traversal and clock plumbing, not physical appliance/solar accuracy, general RF
reliability, or power interruption during flash. Run sources, exact images and logs
live outside the kernel under
`/home/ty/PersonalCode/lattice-vertex-lab/experiments/time-solar-20261022/`.
Recurring civil calculations were additionally compared with the host timezone
database at 2,928 samples across nine supported zones with zero mismatches; an
irregular Morocco calendar was rejected. Boundary tests remain beside their owners,
not in that one-off hardware exercise.
