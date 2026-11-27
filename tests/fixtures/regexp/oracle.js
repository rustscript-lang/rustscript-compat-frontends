// Independent reference only. The extra modern d flag observes offsets;
// the original pattern/flags/subject and ordinary exec captures are retained.
const fs = require('fs');
const cases = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
function units(s) { return Array.from({length:s.length}, (_,i)=>s.charCodeAt(i)); }
const vectors = cases.map(c => {
  const source = c.subject_units ? String.fromCharCode(...c.subject_units) : c.subject;
  const r = new RegExp(c.pattern, c.flags);
  r.lastIndex = c.search_start || 0;
  const m = r.exec(source);
  const indexed = new RegExp(c.pattern, c.flags + 'd');
  indexed.lastIndex = c.search_start || 0;
  const d = indexed.exec(source);
  if (!!m !== !!d || (m && m.some((v,i)=>v!==d[i]))) throw new Error('index observation changed match');
  return {id:c.id, original_js:c.original_js, pattern:c.pattern, flags:c.flags, subject_units:units(source), search_start:c.search_start||0,
    match:m ? {index:m.index, end:m.index+m[0].length, captures:Array.from(m,v=>v===undefined?null:units(v)), pairs:d.indices.map(p=>p===undefined?[-1,-1]:p)} : null,
    lastIndex:r.lastIndex, classification:c.classification};
});
process.stdout.write(JSON.stringify({node:process.version,argv:process.argv,vectors},null,2)+'\n');
