var o = {x: 7};
var proto = {marker: 3};
Object.setPrototypeOf(o, proto);
var a = [1, 2, 3];
var s = 0;
for (var v of a) { s = s + v; }
var tagged = [1, 'two', o];
Math.floor(2.9) === 2 && s === 6 && o.x === 7 && o.marker === 3 && tagged.length === 3 && tagged[1] === 'two';
