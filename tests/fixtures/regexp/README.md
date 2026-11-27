# Private ES5 RegExp matcher kernel

This fixture family tests a static-pattern compiler and a generated typed guest
matching kernel. It does not test public JavaScript `RegExp` objects.

## Contract

`javascript::regexp_lowering::append_matcher` is the private integration entry.
It validates UTF-16 pattern units and `g`, `i`, `m` flags, appends implemented
guest functions to the supplied `FrontendIr`, reserves disjoint local slots,
and returns the matcher function index. No intrinsic root is installed.

The guest signature is `(Array<Int> subject_utf16, Int search_start,
Int transition_budget, Int scratch_cells) -> __Es5RegExpKernelResult`.
The named result has Int `status`, `start`, `end`, `search_start`, `transitions`,
and `Array<Int> captures`. Statuses are 0 no-match, 1 match, 2 transition quota,
3 scratch quota, 4 invalid kernel input. Captures include group zero, in
start/end pairs, with -1/-1 for unmatched, when the call is admitted.
Positive lookahead captures may end after the whole-match end. Empty success
leaves start equal to end. This kernel does not mutate a JavaScript
`lastIndex` or advance it after empty success.
A public caller must select its search start according to its own object state.

Policy: 4096 pattern UTF-16 units, 256 captures, 1,000,000 transitions,
65,536 scratch Int cells. Caller-supplied lower budgets are supported; larger
budgets are rejected. All matching and backtracking execute in the existing VM,
with trace JIT disabled in these tests. Fuel, heap, worker-stack and VM limits
are unchanged. Pattern parsing, syntax validation and compilation run in Rust;
subjects arrive through a typed test-only callback after compilation.

The static instruction stream uses ten Int cells per instruction. Character
sets use immutable sorted inclusive UTF-16 ranges. Mutable scratch contains
current capture pairs and repetition counters/progress positions, followed by
fixed-width backtrack frames. A frame holds its kind, continuation, input
position and full mutable-state snapshot. Lookahead markers delimit atomic
assertions. Popping restores failed alternatives and negative assertions;
positive assertions retain successful captures and restore input/repetition
positions. Repetition resets its nested capture interval before every attempt.
Every instruction, search/progress step, comparison scan and snapshot-copy
iteration is charged; empty alternatives cannot avoid the transition quota.
Frame admission checks scratch bounds before any growth. The initial mutable
state and the full capture-output array are reserved together before matching.
If that reservation does not fit the selected scratch limit, or the kernel
input is invalid, `captures` is empty and no output Int cells are allocated.
Status 4 remains 4 when an invalid domain and a too-small scratch limit
coincide; quota and invalid input are not rewritten as match or no-match.
When the reservation fits and the input stays valid, match, no-match,
transition quota, and a later scratch quota still emit the full caplen pairs.
Actual scratch high water is the mutable state cells plus those output Int
cells, and that sum must fit the original selected limit. CODE and RANGES are
immutable pattern-program cells and stay outside this scratch limit. No JS heap
ID is stored in these arrays. Array reads use explicit IR `Borrow` receivers.

## Sources and reference execution

Normative source: https://262.ecma-international.org/5.1/ sections 6, 7.2,
7.6, 15.10.1 and 15.10.2. In particular, noncapturing `(?:...)` is ES5 syntax;
lookahead is atomic; unmatched backreferences succeed empty; RepeatMatcher
clears nested captures and refuses optional empty progress. The two repeat/
negative-look examples and positive-look examples in `spec_vectors.json` are
from sections 15.10.2.5 and 15.10.2.8.

`unicode-3.0-es5.txt` fixes Unicode 3.0 default unconditional uppercase,
IdentifierPart categories and whitespace. It is derived from the exact
UnicodeData-3.0.0.txt and SpecialCasing-2.txt URLs in its header. ES5
Canonicalize excludes multi-unit uppercase and non-ASCII-to-ASCII uppercase.
No running host locale or current Unicode table participates. Unicode 3.0 Zs
includes U+200B; modern Node differs. Later Georgian case mappings also differ.
Those vectors retain both modern reference observations and explicit normative
expectations. Data licensing is in UNICODE-LICENSE.txt.

`vectors.json` retains each original JS expression and exact pattern spelling
and subject UTF-16 units, including lone surrogates. `oracle.js` independently
executes ordinary Node RegExp and an otherwise identical `d`-flag observation
for capture offsets; `d` is instrumentation, not admitted ES5 pattern syntax.
The test emits Node version, actual argv, raw captures, return code, and each
guest comparison. Modern Node is not used as an ES5 grammar validator.

Public RegExp construction/exec/test/lastIndex, runtime patterns, String
interoperation, JSON and Date are separate integration scopes. Reserved
future P9 intrinsic codes 768..1023 and roots 16..31 are not registered here.
