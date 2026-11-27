'use strict';
// Exact-bit reference only. No guest implementation or fallback.
const fs = require('fs');
const request = JSON.parse(fs.readFileSync(0, 'utf8'));
const view = new DataView(new ArrayBuffer(8));
const vectors = request.vectors.map(row => {
  view.setBigUint64(0, BigInt('0x' + row.args.bits), false);
  const number = view.getFloat64(0, false);
  return {...row, reference: {raw_string: String(number), negative_zero: Object.is(number, -0), is_nan: Number.isNaN(number)}};
});
const int_vectors = (request.int_vectors || []).map(row => {
  const number = Number(BigInt(row.args.integer));
  return {...row, reference: {raw_string: String(number), negative_zero: Object.is(number, -0), is_nan: Number.isNaN(number)}};
});
process.stdout.write(JSON.stringify({version: process.version, argv: process.argv, vectors, int_vectors}, null, 2) + '\n');
