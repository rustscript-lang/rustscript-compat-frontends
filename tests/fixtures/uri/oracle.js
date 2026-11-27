// Independent reference only. Never imported by the production codec.
'use strict';
const fs = require('fs');
const vectors = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const operations = [encodeURI, encodeURIComponent, decodeURI, decodeURIComponent];
const results = vectors.map(v => {
  const input = String.fromCharCode.apply(null, v.input);
  try {
    const output = operations[v.mode](input);
    const units = [];
    for (let i = 0; i < output.length; ++i) units.push(output.charCodeAt(i));
    return {id: v.id, mode: v.mode, input: v.input, status: 0, output: units};
  } catch (e) {
    if (!(e instanceof URIError)) throw e;
    return {id: v.id, mode: v.mode, input: v.input, status: 1, output: [], error: e.name};
  }
});
console.log(JSON.stringify({version: process.version, argv: process.argv, results}));
