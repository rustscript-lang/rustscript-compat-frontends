'use strict';
var v;
// vector 60: ES5.1 15.8.2
v = Math.asin("bad"); if (!(v !== v)) { throw 1060; }
// vector 61: ES5.1 15.8.2
v = Math.asin(); if (!(v !== v)) { throw 1061; }
// vector 62: ES5.1 15.8.2
v = Math.atan(undefined); if (!(v !== v)) { throw 1062; }
// vector 63: ES5.1 15.8.2
v = Math.atan(null); if (!(v === 0 && 1/v === Infinity)) { throw 1063; }
// vector 64: ES5.1 15.8.2
v = Math.atan(true); if (!(v === 0.7853981633974483)) { throw 1064; }
// vector 65: ES5.1 15.8.2
v = Math.atan(false); if (!(v === 0 && 1/v === Infinity)) { throw 1065; }
// vector 66: ES5.1 15.8.2
v = Math.atan(0); if (!(v === 0 && 1/v === Infinity)) { throw 1066; }
// vector 67: ES5.1 15.8.2
v = Math.atan(-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1067; }
// vector 68: ES5.1 15.8.2
v = Math.atan(0.5); if (!(v === 0.4636476090008061)) { throw 1068; }
// vector 69: ES5.1 15.8.2
v = Math.atan(-0.5); if (!(v === -0.4636476090008061)) { throw 1069; }
// vector 70: ES5.1 15.8.2
v = Math.atan(1); if (!(v === 0.7853981633974483)) { throw 1070; }
// vector 71: ES5.1 15.8.2
v = Math.atan(-1); if (!(v === -0.7853981633974483)) { throw 1071; }
// vector 72: ES5.1 15.8.2
v = Math.atan(2); if (!(v === 1.1071487177940904)) { throw 1072; }
// vector 73: ES5.1 15.8.2
v = Math.atan(-2); if (!(v === -1.1071487177940904)) { throw 1073; }
// vector 74: ES5.1 15.8.2
v = Math.atan(Infinity); if (!(v === 1.5707963267948966)) { throw 1074; }
// vector 75: ES5.1 15.8.2
v = Math.atan(-Infinity); if (!(v === -1.5707963267948966)) { throw 1075; }
// vector 76: ES5.1 15.8.2
v = Math.atan(NaN); if (!(v !== v)) { throw 1076; }
// vector 77: ES5.1 15.8.2
v = Math.atan(" 2 "); if (!(v === 1.1071487177940904)) { throw 1077; }
// vector 78: ES5.1 15.8.2
v = Math.atan("bad"); if (!(v !== v)) { throw 1078; }
// vector 79: ES5.1 15.8.2
v = Math.atan(); if (!(v !== v)) { throw 1079; }
// vector 80: ES5.1 15.8.2
v = Math.ceil(undefined); if (!(v !== v)) { throw 1080; }
// vector 81: ES5.1 15.8.2
v = Math.ceil(null); if (!(v === 0 && 1/v === Infinity)) { throw 1081; }
// vector 82: ES5.1 15.8.2
v = Math.ceil(true); if (!(v === 1)) { throw 1082; }
// vector 83: ES5.1 15.8.2
v = Math.ceil(false); if (!(v === 0 && 1/v === Infinity)) { throw 1083; }
// vector 84: ES5.1 15.8.2
v = Math.ceil(0); if (!(v === 0 && 1/v === Infinity)) { throw 1084; }
// vector 85: ES5.1 15.8.2
v = Math.ceil(-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1085; }
// vector 86: ES5.1 15.8.2
v = Math.ceil(0.5); if (!(v === 1)) { throw 1086; }
// vector 87: ES5.1 15.8.2
v = Math.ceil(-0.5); if (!(v === 0 && 1/v === -Infinity)) { throw 1087; }
// vector 88: ES5.1 15.8.2
v = Math.ceil(1); if (!(v === 1)) { throw 1088; }
// vector 89: ES5.1 15.8.2
v = Math.ceil(-1); if (!(v === -1)) { throw 1089; }
true;
