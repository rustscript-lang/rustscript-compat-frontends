'use strict';
var v;
// vector 240: ES5.1 15.8.2
v = Math.tan("bad"); if (!(v !== v)) { throw 1240; }
// vector 241: ES5.1 15.8.2
v = Math.tan(); if (!(v !== v)) { throw 1241; }
// vector 242: ES5.1 15.8.2
v = Math.min(); if (!(v === Infinity)) { throw 1242; }
// vector 243: ES5.1 15.8.2
v = Math.min(0,-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1243; }
// vector 244: ES5.1 15.8.2
v = Math.min(-0,0); if (!(v === 0 && 1/v === -Infinity)) { throw 1244; }
// vector 245: ES5.1 15.8.2
v = Math.min(1,2,-3); if (!(v === -3)) { throw 1245; }
// vector 246: ES5.1 15.8.2
v = Math.min(NaN,1); if (!(v !== v)) { throw 1246; }
// vector 247: ES5.1 15.8.2
v = Math.min(Infinity,-Infinity); if (!(v === -Infinity)) { throw 1247; }
// vector 248: ES5.1 15.8.2
v = Math.min(true,"2",null); if (!(v === 0 && 1/v === Infinity)) { throw 1248; }
// vector 249: ES5.1 15.8.2
v = Math.max(); if (!(v === -Infinity)) { throw 1249; }
// vector 250: ES5.1 15.8.2
v = Math.max(0,-0); if (!(v === 0 && 1/v === Infinity)) { throw 1250; }
// vector 251: ES5.1 15.8.2
v = Math.max(-0,0); if (!(v === 0 && 1/v === Infinity)) { throw 1251; }
// vector 252: ES5.1 15.8.2
v = Math.max(1,2,-3); if (!(v === 2)) { throw 1252; }
// vector 253: ES5.1 15.8.2
v = Math.max(NaN,1); if (!(v !== v)) { throw 1253; }
// vector 254: ES5.1 15.8.2
v = Math.max(Infinity,-Infinity); if (!(v === Infinity)) { throw 1254; }
// vector 255: ES5.1 15.8.2
v = Math.max(true,"2",null); if (!(v === 2)) { throw 1255; }
// vector 256: ES5.1 15.8.2.5
v = Math.atan2(0,0); if (!(v === 0 && 1/v === Infinity)) { throw 1256; }
// vector 257: ES5.1 15.8.2.5
v = Math.atan2(-0,0); if (!(v === 0 && 1/v === -Infinity)) { throw 1257; }
// vector 258: ES5.1 15.8.2.5
v = Math.atan2(0,-0); if (!(v === 3.141592653589793)) { throw 1258; }
// vector 259: ES5.1 15.8.2.5
v = Math.atan2(-0,-0); if (!(v === -3.141592653589793)) { throw 1259; }
// vector 260: ES5.1 15.8.2.5
v = Math.atan2(1,Infinity); if (!(v === 0 && 1/v === Infinity)) { throw 1260; }
// vector 261: ES5.1 15.8.2.5
v = Math.atan2(-1,Infinity); if (!(v === 0 && 1/v === -Infinity)) { throw 1261; }
// vector 262: ES5.1 15.8.2.5
v = Math.atan2(1,-Infinity); if (!(v === 3.141592653589793)) { throw 1262; }
// vector 263: ES5.1 15.8.2.5
v = Math.atan2(-1,-Infinity); if (!(v === -3.141592653589793)) { throw 1263; }
// vector 264: ES5.1 15.8.2.5
v = Math.atan2(Infinity,Infinity); if (!(v === 0.7853981633974483)) { throw 1264; }
// vector 265: ES5.1 15.8.2.5
v = Math.atan2(-Infinity,-Infinity); if (!(v === -2.356194490192345)) { throw 1265; }
// vector 266: ES5.1 15.8.2.5
v = Math.atan2(Infinity,-Infinity); if (!(v === 2.356194490192345)) { throw 1266; }
// vector 267: ES5.1 15.8.2.5
v = Math.atan2(-Infinity,Infinity); if (!(v === -0.7853981633974483)) { throw 1267; }
// vector 268: ES5.1 15.8.2.5
v = Math.atan2(NaN,1); if (!(v !== v)) { throw 1268; }
// vector 269: ES5.1 15.8.2.13
v = Math.pow(NaN,0); if (!(v === 1)) { throw 1269; }
true;
