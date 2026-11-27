# Private Number-to-UTF16 qualification

`number_string::append_units` emits the guest Number -> Array(Int) producer.
`append` emits that identical producer plus a caller-root-owned managed wrapper
using the actual `utf16_bridge::Entries`. Neither entry is public JavaScript
ToString, a prototype method, general coercion, or a registered global.

## Algorithm and qualified primitive

The guest projects its argument once with `/ Float(1.0)`, including physical
Int carriers. It handles NaN, both zeros and infinities before finite display.
Positive finite Core Float Display supplies shortest fixed ASCII digits on
rustc 1.94.1 (`e408947bfd200af42db322daf0fadfe7e26d3bd1`) and pinned Core
`31973aae3c417826dacedc82a6e5ea19e585bbbb`. A bounded byte scan determines
significant digits and decimal position without packed Float significands or
logarithms. Guest IR applies ES5.1 9.8.1 notation and explicit exponent signs.
The qualified primitive bounds are 343 raw bytes, 17 significant digits,
-323 <= n <= 309, and at most 25 final UTF16 units.

Step-5 minimality and steps-6-through-10 spelling are required. Note 2's
recommended closest/even choice is not asserted. The actual tie input encoded
`430c6bf526340002` yields guest `1000000000000000.3`; Node yields
`1000000000000000.2`. The independent exact rational oracle validates each
selected decimal against asymmetric neighbor midpoints, endpoint parity,
every smaller significant count and possible decimal magnitude, and spelling.
Both choices are valid under the selected normative contract. Every observed
Node difference is retained by original id/args/reference fields.

## References and evidence boundaries

`oracle.js` reads exact binary64 bits with DataView and obtains actual Node
String(Number) results, including negative-zero and NaN flags. Its seven
physical-Int reference inputs use Number(BigInt(decimal)) rather than an
already-rounded JSON numeric argument. `vectors.json` preserves all 1942
original exact-bit rows and adds those seven integer rows; explicit
`normative_alternative` entries were established by the independent Fraction
proof, never by an unverified roundtrip exception.

`exact_bits_units_kernel` compiles the real guest producer once, supplies
runtime values and checks each invocation. This is units-kernel evidence,
not thousands of managed ES5 executions. The separate managed fixture uses
real ES5 parsing and certified `into_vm`, the production catalog/adapters,
caller-owned roots, actual checked_base and live B1 payload observation.
Its numeric property definitions remain numeric. Exact string queries are
supported member access; numeric computed-member coercion is unchanged.
Actual object-row key slot/generation handles are followed through checked_base
and compared with the guest producer, including the tie and notation thresholds.
Unrelated NUL/lone-surrogate payload, full object rows, aliases and prior results
remain live across forced GC. ABI negatives require transactional unchanged IR.

## Allocation and future integration obligations

Core String, Bytes and Array(Int) temporaries are separate from B1 accounting.
The raw-capacity probe concurrently retains actual Core display text, actual
BytesFromUtf8 output and the real formatter output; it is a separate probe,
not an instrumentation claim about optimizer-specific production lifetimes.
Lengths do not bound capacities. Measured examples include a 326-byte raw
string with capacity 528, a 326-byte Bytes buffer, and a 23-unit output with
capacity 32. These measurements are not universal capacity guarantees and
exclude allocator metadata, VM/compiler structures and unrelated live objects.

A managed String requests six header cells plus logical units: <=31 requested
raw cells. Actual occupied extent includes an unsplittable tail (up to three
additional cells), and directory creation can separately require 451 cells.
Tests observe occupied raw extent, reserved B1 bytes, active roots, pending
state and capacity refusal; logical String length is not a heap charge.
The existing B1 ceiling/quota and failure/reset contracts remain unchanged.

A future public caller must explicitly demand this private helper, grant the
already-existing `bytes::from_utf8` builtin in its actual restricted registry,
charge allocation sites through the existing static accounting pipeline, and
root every other live value before publication. Completed-IR append does not
supply those production charges. The private harness grants `math::floor`
and `bytes::from_utf8`; no production registry is changed. There is no helper
root frame or new permanent root. Public coercion, URI/Date globals and whole
ES5 conformance require their own integration and acceptance.
