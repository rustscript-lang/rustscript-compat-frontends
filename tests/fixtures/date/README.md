# Private UTC arithmetic kernel — pending independent review

`prelude/date.rs::append` emits typed guest IR for already-coerced Number
arguments. It does not register a public Date implementation. Coercion, the
public Date.UTC year-0..99 adapter, objects/prototypes, ISO strings, local time,
timezones and clocks are outside this kernel.

## Exact intermediate contract

- `TimeClip(Number) -> Number` retains the ES5.1 §15.9.1.14 positive-zero choice.
- `MakeTime(Number × 4) -> Number` retains the mandated IEEE multiplication and
  left-associated addition order, including IEEE overflow/cancellation.
- `MakeDay(Number × 3) -> Exact` uses exact integer ToInteger inputs, signed
  floor month normalization, Gregorian year/leap/month arithmetic and the
  mathematical first-of-month witness.
- `MakeDate(Number, Number) -> Exact` decodes both finite binary64 values and
  computes the exact multiply-add. Nonfinite input produces invalid Exact.
- `compose(Exact day, Number time) -> Exact` carries MakeDay directly into the
  timestamp calculation, without any day-to-Number round trip.
- `to_number(Exact) -> Number` is an explicitly named private §8.5 RNE boundary.
  UTC construction calls it once after composition, then calls TimeClip.
- UTC parts first applies TimeClip to its Number input. Its named result has
  `valid: Bool` and eight Number fields; invalid fields are NaN.

The private `Exact` Named schema is `{ valid: Bool, negative: Bool,
exponent: Int, limbs: Array(Int) }`. It denotes signed magnitude ×2^exponent.
Magnitude limbs are little-endian radix2^30, active length at most80. A nonzero
coefficient is odd and has a nonzero high limb. Zero and invalid values have
empty limbs, exponent0 and nonnegative sign. Each operation builds fresh arrays
and leaves input records/aliases unchanged. There are nine reusable exact guest
helpers plus seven Date entries per append (119 local slots). Existing caller
IR, declarations, captures and schemas survive append; refused appends are
transactional. Function/local/schema capacity and schema collisions are checked.

All operand-dependent work runs in existing guest Int/Float/Array/Named IR.
Float decoding uses exact powers-of-two scaling, comparison and at most53 bits;
there is no assumed Float-to-Int conversion primitive. Addition aligns binary
exponents exactly. Small division implements negative floor and nonnegative
modulo. RNE retains53 significant bits with guard/sticky/parity and reconstructs
using exact successive scaling. No arithmetic host evaluator, string decoder,
new dependency or Core contract is involved.

The finite-input bound is1024 integer bits. Conservative day/millisecond bounds
are2^1038 and2^1066; with finest input quantum2^-1074 the common coefficient needs
fewer than2140 bits, below the2400-bit capacity. Small limb multiplication and
carry are below2^57. Internal assertions detect bound violations; capacity is
not extended and high limbs are never discarded. Parts uses only clipped times;
its integer calendar expressions and division floors are exact within that
bounded domain.

## Domain and oracle history

ECMA-2625.1 §5.2 requires mathematical operations on the exact real represented
by finite Number inputs. MakeTime explicitly specifies IEEE arithmetic;
MakeDay/MakeDate do not. The chosen MakeDay step7 witness is the mathematical
Gregorian instant, without binary64 precision-gap or Date-object year rejection.
This interpretation is explicit: the specification does not state that witness
domain verbatim. The private Number conversion is separately named, not an
additional unwritten MakeDate step.

The earlier Number-only draft and its original three failed receipts remain
historical failures. The corrected expectation mapping preserves source/oracle
IDs and `primary_previous` fields:

| ID | Earlier primary | Current expectation |
|---|---|---|
| primary-day-0 | NaN | Exact365242499999280472; separate RNE image365242499999280448 |
| primary-day-1 | NaN | Exact mathematical February start; separate RNE image |
| primary-day-3 | NaN | Finite exact integer; RNE image+Infinity |
| primary-day-6 | NaN | Exact mathematical day; separate RNE image |
| primary-date-0 | 8.537504156112112e291 | Same Number bits, explicitly the private RNE image of Exact |
| primary-date-3 | -0 | Mathematical0, Number image+0 |

Keep primary-day-2/4/5, primary-utc-0 and primary-time-0/1 unchanged. In particular,
primary-utc-0 is the valid clipped time-62167219200000 even though modern Node
returns NaN. Modern refusals never replace valid primary expectations.

Three regression controls distinguish the old IEEE intermediate path:

- year2^53, month bits`c377fffffffffa3a`, date1 -> UTC20995200000;
- day2^40+1, time bits`c414997000001499` -> TimeClip7168;
- year10^15, month0, date1, hour bits`c3de699cb62fff10` -> UTC257662976
  after the unchanged IEEE MakeTime.

## Fixtures and execution

`utc-vectors.json` contains4225 records: all4197 original IDs/arguments plus28
controls. All2090 MakeDay/MakeDate records carry independently generated exact
canonical payload expectations. The evidence-only Python int/Fraction generator
is not imported or executed by the product. The fixture retains separate
Number-image primary overrides and their oracle/domain labels.

`utc-oracle.js` captures actual Node version/argv and raw observations. It is a
modern numerical reference, not the normative exact evaluator. Its test-only
integral-year0..99 adapter shifts by400 years to bypass public Date.UTC's1900
mapping. Raw modern observations remain separate from primary differences and
are also fed to guest parts for2064 derived decomposition checks.

Coverage includes positive/zero/negative years, centuries, month/date carries,
clip boundaries, invalid arguments, max finite inputs, subnormals, extreme
negative values, cancellation and signed zero. The strict, ownership-enabled
compiler executes dynamic inputs with10000000 fuel,33554432-byte worker stack,
JIT disabled and asserted zero JIT/AOT native executions. Separate parsed-caller
controls check real captures, repeated append, schemas, aliases, repeated helper
calls and transactional allocation refusal. These are private arithmetic tests,
not public Date or whole-ES5 coverage. Acceptance remains pending independent
review of the frozen candidate and source-bound execution receipts.
