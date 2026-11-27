# Private ES5.1 URI kernel controls

`vectors.json` contains raw UTF-16 code-unit inputs, not precomputed product outputs.
`oracle.js` runs the four built-in Node operations independently, retaining raw
code units and URIError identity. Node is reference-only and contributes no
frontend semantic pass counts. The Rust tests feed those raw arrays to a real
append-produced typed guest callable. No JS engine participates in production.

Normative source: ECMA-262 5.1 (June 2011), §15.1.3 shared Encode/Decode,
§15.1.3.1–4 and Table 21:
https://262.ecma-international.org/5.1/
https://ecma-international.org/wp-content/uploads/ECMA-262_5.1_edition_june_2011.pdf
The full official HTML and original copyright/license notices are retained in
the assigned evidence OUT/ecma-262-5.1.html; extracted algorithms in uri-spec.txt.
No normative source text is copied into the production implementation.

Modes: 0 encodeURI, 1 encodeURIComponent, 2 decodeURI, 3 decodeURIComponent.
Input is already coerced Array(Int), each unit 0..65535. Status 0 is success,
1 is malformed URI, 2 is private representation/size refusal. Input/output
ceilings are 65536 code units, limit parameter is 0..65536. These policies do
not increase global runtime quotas. No public ToString, URIError construction,
global binding, callable identity/shadowing, or public member support is claimed.
