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
matrix and sustained-flood validation. The attempted five-sample application
noise baseline was contaminated by restarted external compilation and is
excluded from performance conclusions.

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

Focused low-power behavior is unmeasured: attempted activation still recorded
unfocused windows. Unfocused scheduling remains continuous. Controlled window
timings, RSS, recorder overhead, input latency, and broader application profiling
remain outstanding. GPU timestamps are unavailable through the pinned renderer's
diagnostics on Metal; CPU markers do not substitute for them.

The opt-in font/DPI/window scenario passes all seven stages and restores its
original grid and physical dimensions. Its phase field identifies a requested
stage; stable stretches establish settling. Image asset counts rise from 14 to
22 over the first cycle and remain there after restoration. This could include
font/scale caches; repeated cycles are needed to distinguish a bounded cache
from continued retention. No memory-restoration claim follows from grid restoration.

`ARTIFACT-STATUS.md` in the evidence directory identifies superseded and
contaminated artifacts. No application or desktop gain is claimed here.
