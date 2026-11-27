'use strict';
var v;
// vector 90: ES5.1 15.8.2
v = Math.ceil(2); if (!(v === 2)) { throw 1090; }
// vector 91: ES5.1 15.8.2
v = Math.ceil(-2); if (!(v === -2)) { throw 1091; }
// vector 92: ES5.1 15.8.2
v = Math.ceil(Infinity); if (!(v === Infinity)) { throw 1092; }
// vector 93: ES5.1 15.8.2
v = Math.ceil(-Infinity); if (!(v === -Infinity)) { throw 1093; }
// vector 94: ES5.1 15.8.2
v = Math.ceil(NaN); if (!(v !== v)) { throw 1094; }
// vector 95: ES5.1 15.8.2
v = Math.ceil(" 2 "); if (!(v === 2)) { throw 1095; }
// vector 96: ES5.1 15.8.2
v = Math.ceil("bad"); if (!(v !== v)) { throw 1096; }
// vector 97: ES5.1 15.8.2
v = Math.ceil(); if (!(v !== v)) { throw 1097; }
// vector 98: ES5.1 15.8.2
v = Math.cos(undefined); if (!(v !== v)) { throw 1098; }
// vector 99: ES5.1 15.8.2
v = Math.cos(null); if (!(v === 1)) { throw 1099; }
// vector 100: ES5.1 15.8.2
v = Math.cos(true); if (!(v === 0.5403023058681398)) { throw 1100; }
// vector 101: ES5.1 15.8.2
v = Math.cos(false); if (!(v === 1)) { throw 1101; }
// vector 102: ES5.1 15.8.2
v = Math.cos(0); if (!(v === 1)) { throw 1102; }
// vector 103: ES5.1 15.8.2
v = Math.cos(-0); if (!(v === 1)) { throw 1103; }
// vector 104: ES5.1 15.8.2
v = Math.cos(0.5); if (!(v === 0.8775825618903728)) { throw 1104; }
// vector 105: ES5.1 15.8.2
v = Math.cos(-0.5); if (!(v === 0.8775825618903728)) { throw 1105; }
// vector 106: ES5.1 15.8.2
v = Math.cos(1); if (!(v === 0.5403023058681398)) { throw 1106; }
// vector 107: ES5.1 15.8.2
v = Math.cos(-1); if (!(v === 0.5403023058681398)) { throw 1107; }
// vector 108: ES5.1 15.8.2
v = Math.cos(2); if (!(v === -0.4161468365471424)) { throw 1108; }
// vector 109: ES5.1 15.8.2
v = Math.cos(-2); if (!(v === -0.4161468365471424)) { throw 1109; }
// vector 110: ES5.1 15.8.2
v = Math.cos(Infinity); if (!(v !== v)) { throw 1110; }
// vector 111: ES5.1 15.8.2
v = Math.cos(-Infinity); if (!(v !== v)) { throw 1111; }
// vector 112: ES5.1 15.8.2
v = Math.cos(NaN); if (!(v !== v)) { throw 1112; }
// vector 113: ES5.1 15.8.2
v = Math.cos(" 2 "); if (!(v === -0.4161468365471424)) { throw 1113; }
// vector 114: ES5.1 15.8.2
v = Math.cos("bad"); if (!(v !== v)) { throw 1114; }
// vector 115: ES5.1 15.8.2
v = Math.cos(); if (!(v !== v)) { throw 1115; }
// vector 116: ES5.1 15.8.2
v = Math.exp(undefined); if (!(v !== v)) { throw 1116; }
// vector 117: ES5.1 15.8.2
v = Math.exp(null); if (!(v === 1)) { throw 1117; }
// vector 118: ES5.1 15.8.2
v = Math.exp(true); if (!(v === 2.718281828459045)) { throw 1118; }
// vector 119: ES5.1 15.8.2
v = Math.exp(false); if (!(v === 1)) { throw 1119; }
true;
