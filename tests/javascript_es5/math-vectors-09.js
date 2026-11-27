'use strict';
var v;
// vector 270: ES5.1 15.8.2.13
v = Math.pow(1,Infinity); if (!(v !== v)) { throw 1270; }
// vector 271: ES5.1 15.8.2.13
v = Math.pow(-1,Infinity); if (!(v !== v)) { throw 1271; }
// vector 272: ES5.1 15.8.2.13
v = Math.pow(0,-3); if (!(v === Infinity)) { throw 1272; }
// vector 273: ES5.1 15.8.2.13
v = Math.pow(-0,-3); if (!(v === -Infinity)) { throw 1273; }
// vector 274: ES5.1 15.8.2.13
v = Math.pow(-0,3); if (!(v === 0 && 1/v === -Infinity)) { throw 1274; }
// vector 275: ES5.1 15.8.2.13
v = Math.pow(-0,2); if (!(v === 0 && 1/v === Infinity)) { throw 1275; }
// vector 276: ES5.1 15.8.2.13
v = Math.pow(-Infinity,-3); if (!(v === 0 && 1/v === -Infinity)) { throw 1276; }
// vector 277: ES5.1 15.8.2.13
v = Math.pow(-Infinity,3); if (!(v === -Infinity)) { throw 1277; }
// vector 278: ES5.1 15.8.2.13
v = Math.pow(-2,0.5); if (!(v !== v)) { throw 1278; }
// vector 279: ES5.1 15.8.2.13
v = Math.pow(2,3); if (!(v === 8)) { throw 1279; }
// vector 280: ES5.1 15.8.2.13
v = Math.pow(Infinity,-1); if (!(v === 0 && 1/v === Infinity)) { throw 1280; }
// vector 281: ES5.1 15.1.2.2
v = parseInt("  -0x10"); if (!(v === -16)) { throw 1281; }
// vector 282: ES5.1 15.1.2.2
v = parseInt("08"); if (!(v === 8)) { throw 1282; }
// vector 283: ES5.1 15.1.2.2
v = parseInt("0b11"); if (!(v === 0 && 1/v === Infinity)) { throw 1283; }
// vector 284: ES5.1 15.1.2.2
v = parseInt("11",2); if (!(v === 3)) { throw 1284; }
// vector 285: ES5.1 15.1.2.2
v = parseInt("z",36); if (!(v === 35)) { throw 1285; }
// vector 286: ES5.1 15.1.2.2
v = parseInt("123x",10); if (!(v === 123)) { throw 1286; }
// vector 287: ES5.1 15.1.2.2
v = parseInt("-0",10); if (!(v === 0 && 1/v === -Infinity)) { throw 1287; }
// vector 288: ES5.1 15.1.2.2
v = parseInt("12",4294967298); if (!(v === 1)) { throw 1288; }
// vector 289: ES5.1 15.1.2.2
v = parseInt("12",1); if (!(v !== v)) { throw 1289; }
// vector 290: ES5.1 15.1.2.2
v = parseInt("12",37); if (!(v !== v)) { throw 1290; }
// vector 291: ES5.1 15.1.2.2
v = parseInt("12",null); if (!(v === 12)) { throw 1291; }
// vector 292: ES5.1 15.1.2.2
v = parseInt("12",true); if (!(v !== v)) { throw 1292; }
// vector 293: ES5.1 15.1.2.2
v = parseInt("0x10",10); if (!(v === 0 && 1/v === Infinity)) { throw 1293; }
// vector 294: ES5.1 15.1.2.2
v = parseInt("0x10",16); if (!(v === 16)) { throw 1294; }
// vector 295: ES5.1 15.1.2.2
v = parseInt("\u200012"); if (!(v === 12)) { throw 1295; }
// vector 296: ES5.1 15.1.2.2
v = parseInt("\u180e12"); if (!(v !== v)) { throw 1296; }
// vector 297: ES5.1 15.1.2.2
v = parseInt("\u200b12"); if (!(v === 12)) { throw 1297; }
// vector 298: ES5.1 15.1.2.2
v = parseInt(true); if (!(v !== v)) { throw 1298; }
// vector 299: ES5.1 15.1.2.2
v = parseInt(null); if (!(v !== v)) { throw 1299; }
true;
