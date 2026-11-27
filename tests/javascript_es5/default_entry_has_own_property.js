// Public Object.prototype.hasOwnProperty calls on the normal default file
// route. The last expression is the boxed Boolean completion read by the entry
// test; every check is a real public Reference through the prototype chain.
var n = 0;
var o = {x: 1, y: 2};
if (!o.hasOwnProperty("x")) n = 1;
if (n === 0 && o.hasOwnProperty("z")) n = 2;
var a = [7, 8];
if (n === 0 && !a.hasOwnProperty("0")) n = 3;
if (n === 0 && !a.hasOwnProperty("1")) n = 4;
if (n === 0 && a.hasOwnProperty("2")) n = 5;
if (n === 0 && !a.hasOwnProperty("length")) n = 6;
var t = [1, "x"];
if (n === 0 && !t.hasOwnProperty("1")) n = 7;
var numeric = {"5": 1};
if (n === 0 && !numeric.hasOwnProperty(5)) n = 8;
var key = {toString: function () { "use strict"; return "x"; }};
if (n === 0 && !o.hasOwnProperty(key)) n = 9;
if (n === 0 && !Object.prototype.hasOwnProperty("valueOf")) n = 10;
if (n === 0 && o.hasOwnProperty.length !== 1) n = 11;
n === 0;
