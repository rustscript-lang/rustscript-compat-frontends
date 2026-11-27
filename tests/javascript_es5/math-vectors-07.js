'use strict';
var v;
// vector 210: ES5.1 15.8.2
v = Math.sqrt(0); if (!(v === 0 && 1/v === Infinity)) { throw 1210; }
// vector 211: ES5.1 15.8.2
v = Math.sqrt(-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1211; }
// vector 212: ES5.1 15.8.2
v = Math.sqrt(0.5); if (!(v === 0.7071067811865476)) { throw 1212; }
// vector 213: ES5.1 15.8.2
v = Math.sqrt(-0.5); if (!(v !== v)) { throw 1213; }
// vector 214: ES5.1 15.8.2
v = Math.sqrt(1); if (!(v === 1)) { throw 1214; }
// vector 215: ES5.1 15.8.2
v = Math.sqrt(-1); if (!(v !== v)) { throw 1215; }
// vector 216: ES5.1 15.8.2
v = Math.sqrt(2); if (!(v === 1.4142135623730951)) { throw 1216; }
// vector 217: ES5.1 15.8.2
v = Math.sqrt(-2); if (!(v !== v)) { throw 1217; }
// vector 218: ES5.1 15.8.2
v = Math.sqrt(Infinity); if (!(v === Infinity)) { throw 1218; }
// vector 219: ES5.1 15.8.2
v = Math.sqrt(-Infinity); if (!(v !== v)) { throw 1219; }
// vector 220: ES5.1 15.8.2
v = Math.sqrt(NaN); if (!(v !== v)) { throw 1220; }
// vector 221: ES5.1 15.8.2
v = Math.sqrt(" 2 "); if (!(v === 1.4142135623730951)) { throw 1221; }
// vector 222: ES5.1 15.8.2
v = Math.sqrt("bad"); if (!(v !== v)) { throw 1222; }
// vector 223: ES5.1 15.8.2
v = Math.sqrt(); if (!(v !== v)) { throw 1223; }
// vector 224: ES5.1 15.8.2
v = Math.tan(undefined); if (!(v !== v)) { throw 1224; }
// vector 225: ES5.1 15.8.2
v = Math.tan(null); if (!(v === 0 && 1/v === Infinity)) { throw 1225; }
// vector 226: ES5.1 15.8.2
v = Math.tan(true); if (!(v === 1.5574077246549023)) { throw 1226; }
// vector 227: ES5.1 15.8.2
v = Math.tan(false); if (!(v === 0 && 1/v === Infinity)) { throw 1227; }
// vector 228: ES5.1 15.8.2
v = Math.tan(0); if (!(v === 0 && 1/v === Infinity)) { throw 1228; }
// vector 229: ES5.1 15.8.2
v = Math.tan(-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1229; }
// vector 230: ES5.1 15.8.2
v = Math.tan(0.5); if (!(v === 0.5463024898437905)) { throw 1230; }
// vector 231: ES5.1 15.8.2
v = Math.tan(-0.5); if (!(v === -0.5463024898437905)) { throw 1231; }
// vector 232: ES5.1 15.8.2
v = Math.tan(1); if (!(v === 1.5574077246549023)) { throw 1232; }
// vector 233: ES5.1 15.8.2
v = Math.tan(-1); if (!(v === -1.5574077246549023)) { throw 1233; }
// vector 234: ES5.1 15.8.2
v = Math.tan(2); if (!(v === -2.185039863261519)) { throw 1234; }
// vector 235: ES5.1 15.8.2
v = Math.tan(-2); if (!(v === 2.185039863261519)) { throw 1235; }
// vector 236: ES5.1 15.8.2
v = Math.tan(Infinity); if (!(v !== v)) { throw 1236; }
// vector 237: ES5.1 15.8.2
v = Math.tan(-Infinity); if (!(v !== v)) { throw 1237; }
// vector 238: ES5.1 15.8.2
v = Math.tan(NaN); if (!(v !== v)) { throw 1238; }
// vector 239: ES5.1 15.8.2
v = Math.tan(" 2 "); if (!(v === -2.185039863261519)) { throw 1239; }
true;
