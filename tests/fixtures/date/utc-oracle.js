// Independent numeric Date oracle. No strings, clock, timezone, or product fallback.
'use strict';
const fs = require('fs');
const vectors = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const number = x => x === 'NaN' ? NaN : x === '+Infinity' ? Infinity : x === '-Infinity' ? -Infinity : Number(x);
const encode = x => Number.isNaN(x) ? 'NaN' : x === Infinity ? '+Infinity' : x === -Infinity ? '-Infinity' : Object.is(x, -0) ? '-0' : String(x);
const parts = t => {
  const d = new Date(t);
  return {valid: !Number.isNaN(d.getTime()), year: encode(d.getUTCFullYear()), month: encode(d.getUTCMonth()), date: encode(d.getUTCDate()), weekday: encode(d.getUTCDay()), hour: encode(d.getUTCHours()), minute: encode(d.getUTCMinutes()), second: encode(d.getUTCSeconds()), millisecond: encode(d.getUTCMilliseconds())};
};
function utc(a) {
  // Only these fixtures' literal integral 0..99 years use the adapter.
  // A 400-year shift preserves leap/carry behavior; undo after construction.
  if (a[0] >= 0 && a[0] <= 99 && Number.isInteger(a[0])) {
    const d = new Date(Date.UTC(a[0] + 400, ...a.slice(1)));
    d.setUTCFullYear(d.getUTCFullYear() - 400);
    return d.getTime();
  }
  return Date.UTC(...a);
}
const observations = vectors.map(v => {
  const a = v.args.map(number);
  let result;
  switch (v.op) {
    case 0: result = encode(new Date(a[0]).getTime()); break;
    case 1: result = encode(Date.UTC(1970, 0, 1, ...a)); break;
    case 2: result = encode(utc([...a, 0, 0, 0, 0]) / 86400000); break;
    case 3: result = encode(new Date(a[0] * 86400000 + a[1]).getTime()); break;
    case 4: result = encode(utc(a)); break;
    case 5: result = parts(a[0]); break;
    default: throw new Error('operation');
  }
  return {id: v.id, op: v.op, args: v.args, result, ...(v.op === 4 ? {utcParts: parts(number(result))} : {})};
});
console.log(JSON.stringify({version: process.version, execPath: process.execPath, argv: process.argv, execArgv: process.execArgv, observations}));
