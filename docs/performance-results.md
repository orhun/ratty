# Performance evidence (work in progress)

The VT comparison below is established. Controlled application and desktop
comparisons, final measured commit identities, and PR verification remain
incomplete. Do not interpret the validation runs as end-to-end speedup claims.

## VT result

| Workload | Configuration | Metric | Baseline | Candidate | Change | Samples and variability |
| --- | --- | --- | ---: | ---: | ---: | --- |
| ASCII processing | 120×40, 2,000 history rows, 4,096-byte chunks, 4 MiB/sample | Median of five run medians, ms | 131.162 | 83.454 | −36.37% | Five alternating pairs, seven samples/run; run medians span 129.186–133.672 ms baseline and 82.335–85.288 ms candidate |

Individual paired time reductions were 35.40%, 34.98%, 38.41%, 35.42%, and
37.30%. These exceeded the preselected 10% repeatability threshold. The complete
matrix had 93 valid baseline/candidate comparisons with no median regression
above 5%. Three fragmented Unicode cases failed baseline correctness, so they
cannot establish speedups; all 96 candidate cases passed validation.

The retained fast path bypasses grapheme-boundary analysis when both the prior
cell's final byte and new character are printable ASCII. Non-ASCII behavior
continues through the existing Unicode path. Baseline CPU sampling identified
boundary analysis as a substantial cost; exhaustive printable-ASCII pair tests
and mixed Unicode-context tests check the shortcut's correctness. A separate
UTF-8 boundary workaround addresses byte loss in the pinned parser dependency.

Baseline source is commit `05308cd6703be24441f6913a537a16d35ed2c117` with the
common benchmark harness added. Candidate source also includes the local parser
fix and ASCII shortcut; these measurements precede a candidate commit. Preserved
executables have SHA-256 identities:

- Baseline: `13f4ba79a067069338de4c9fc1f9fb8e4c4d82836731b2c4ab1ae75740933c02`
- Candidate: `1954fdef565400938535d4b9c700267ef59a413c1601d32788c1060779292017`

Local evidence is `/private/tmp/ratty-performance-evidence`: raw
`paired-baseline-{1..5}.json`, `paired-ascii-fast-path-{1..5}.json`, full matrix
`vt-ascii-fast-path.json`, comparison summary `vt-comparison-summary.json`, and
CPU stack sample `vt-cpu-sample.txt`. These are optimized Apple M2 Max CPU runs,
not renderer or GPU measurements. See [the guide](performance.md) for commands,
measurement conditions, and tooling migration details.

## Validation and outstanding measurements

Both preserved optimized application binaries pass the 21-case CPU drawing/PTY
matrix and sustained-flood validation. All four application noise pilots are
excluded: the first three overlapped external compilation, and a broader audit
found an external `fux-xtask` process using 97.5–100% of one CPU core throughout
the fourth. The initial compiler-name filter missed this workload. Thresholds
derived from that pilot (`app-acceptance-thresholds.json`) were set before
candidate evaluation, but are invalid for accepting results and must be replaced
after a quiet baseline run. Load audits must inspect all busy processes, not
just compiler and test executable names.

The subsequent five alternating baseline/candidate pairs all completed the 18
drawing cases (300 updates each) and three PTY cases. Every PTY case consumed
1,802,263 bytes; final-state hashes agreed across both variants and all trials.
Observed peak outstanding PTY accounting was 17,408–18,432 bytes. These are
finite-workload correctness and queue observations, not a general memory bound.
However, process logs captured external compilation or test workloads in pairs
1–3, including a process at 99.7% of one CPU core. The entire batch is excluded
from performance claims; the two remaining pairs do not meet the five-run
requirement. Raw `app-paired-{baseline,candidate}-{1..5}.json`, `.log`, and
`-processes.txt` artifacts are retained. A controlled replacement comparison
remains outstanding.

Real Metal windows have verified 80×24 and 240×80 configurations. A three-cycle
graphics validation observed all twelve live RGP yaw updates and exact upward
anchor movement in every cycle; object and main-world asset counts returned to
their initial values after deletion. This validates the workload and observed
collection cleanup, not process or GPU memory retention. Earlier blank-row
fixtures lost anchors through ambiguous existing scroll inference and are
superseded for animation/update coverage.

A separate optimized 20-cycle graphics run without CSV recording sampled macOS
`top`'s `MEM` metric (process physical footprint, not RSS). After startup it was
approximately 346–349 MB during repeated updates and about 343 MB during cleanup.
The child completed all 20 registrations and exited successfully. Raw output is
`graphics-memory-top.txt` with application log `graphics-memory-diagnostic.log`.
This single diagnostic run occurred under external load; it supports an observed
plateau for this workload, not a controlled memory improvement or a general
no-leak claim.

Most activation probes recorded unfocused windows; a later probe recorded 92
focused updates over three seconds in low-power mode. Its Apple Events activation
command was denied, so this does not establish reliable focus automation. Every
measurement must check its recorded focus state. Unfocused scheduling remains
continuous. Controlled window timings, RSS comparisons, recorder/profiler overhead,
and repeated input-latency measurements remain outstanding. GPU timestamps are unavailable through the pinned renderer's
diagnostics on Metal; CPU markers do not substitute for them.
An OS-state audit subsequently found the session locked (`CGSSessionScreenIsLocked=Yes`,
`loginwindow` frontmost). Earlier probes did not record OS lock state; their
visible/focused flags establish application state only. Treat them as pipeline
validation rather than evidence of an actively presented desktop. Remaining
desktop measurements require an unlocked session and a quiet machine.

The opt-in font/DPI/window scenario passes all seven stages and restores its
original grid and physical dimensions. Its phase field identifies a requested
stage; stable stretches establish settling. Image asset counts rise from 14 to
22 over the first cycle and remain there after restoration. A subsequent three-cycle
run stayed at 22 in later cycles and restored the grid each time, consistent with
a cache plateau over this finite test. This does not prove bounded GPU memory
or exclude retention in other workloads. No memory-restoration claim follows
from grid restoration.

The symbol-preserving baseline application profile (`desktop-full-profile-repeat.txt`)
captured 813 snapshots per thread over ten seconds with 30 Hz full-text updates
at 80×24. It reaches PTY draining/filtering and widget/renderer synchronization.
Most collapsed stack counts represent waiting threads; active stacks frequently
include Bevy task scheduling. Few snapshots land in parsing at this input rate,
so the VT gain cannot be extrapolated to the entire application. These are
qualitative sampling observations, not CPU-share percentages or an optimization
justification for changing scheduling. The first two-snapshot profile is excluded.

The graphics profile (`desktop-graphics-profile.txt`) captured 871 snapshots per
thread and reached inline filtering and synchronization. Row-to-text conversion
appears among the more frequent active leaf samples (26 collapsed samples),
consistent with the visible-row snapshots used for anchor tracking. This is a
candidate for further investigation, not a retained optimization: the existing
scroll-inference ambiguity and missing controlled comparisons make a batching
change premature.

`ARTIFACT-STATUS.md` in the evidence directory identifies superseded and
contaminated artifacts. No application or desktop gain is claimed here.
