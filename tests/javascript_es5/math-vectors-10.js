'use strict';
var v;
// vector 300: ES5.1 15.1.2.2
v = parseInt(undefined); if (!(v !== v)) { throw 1300; }
// vector 301: ES5.1 15.1.2.2
v = parseInt(1e21); if (!(v === 1)) { throw 1301; }
// vector 302: ES5.1 15.1.2.2
v = parseInt(1e-7); if (!(v === 1)) { throw 1302; }
// vector 303: ES5.1 15.1.2.2
v = parseInt(-0); if (!(v === 0 && 1/v === Infinity)) { throw 1303; }
// vector 304: ES5.1 15.1.2.3
v = parseFloat(" -0 "); if (!(v === 0 && 1/v === -Infinity)) { throw 1304; }
// vector 305: ES5.1 15.1.2.3
v = parseFloat("1.25tail"); if (!(v === 1.25)) { throw 1305; }
// vector 306: ES5.1 15.1.2.3
v = parseFloat("1e+"); if (!(v === 1)) { throw 1306; }
// vector 307: ES5.1 15.1.2.3
v = parseFloat(".5"); if (!(v === 0.5)) { throw 1307; }
// vector 308: ES5.1 15.1.2.3
v = parseFloat("1.e2"); if (!(v === 100)) { throw 1308; }
// vector 309: ES5.1 15.1.2.3
v = parseFloat("Infinitytail"); if (!(v === Infinity)) { throw 1309; }
// vector 310: ES5.1 15.1.2.3
v = parseFloat("-Infinity"); if (!(v === -Infinity)) { throw 1310; }
// vector 311: ES5.1 15.1.2.3
v = parseFloat("0x10"); if (!(v === 0 && 1/v === Infinity)) { throw 1311; }
// vector 312: ES5.1 15.1.2.3
v = parseFloat("bad"); if (!(v !== v)) { throw 1312; }
// vector 313: ES5.1 15.1.2.3
v = parseFloat("\u180e2"); if (!(v !== v)) { throw 1313; }
// vector 314: ES5.1 15.1.2.3
v = parseFloat("\u200b2"); if (!(v === 2)) { throw 1314; }
// vector 315: ES5.1 15.1.2.3
v = parseFloat(1e21); if (!(v === 1e+21)) { throw 1315; }
// vector 316: ES5.1 15.1.2.3
v = parseFloat(-0); if (!(v === 0 && 1/v === Infinity)) { throw 1316; }
// vector 317: ES5.1 15.1.2.3
v = parseFloat(true); if (!(v !== v)) { throw 1317; }
// vector 318: ES5.1 15.1.2.3
v = parseFloat(null); if (!(v !== v)) { throw 1318; }
// vector 319: ES5.1 15.1.2.3
v = parseFloat(undefined); if (!(v !== v)) { throw 1319; }
// vector 320: ES5.1 15.1.2.4
v = isNaN(); if (!(v === true)) { throw 1320; }
// vector 321: ES5.1 15.1.2.4
v = isNaN(undefined); if (!(v === true)) { throw 1321; }
// vector 322: ES5.1 15.1.2.4
v = isNaN(null); if (!(v === false)) { throw 1322; }
// vector 323: ES5.1 15.1.2.4
v = isNaN(true); if (!(v === false)) { throw 1323; }
// vector 324: ES5.1 15.1.2.4
v = isNaN(false); if (!(v === false)) { throw 1324; }
// vector 325: ES5.1 15.1.2.4
v = isNaN(""); if (!(v === false)) { throw 1325; }
// vector 326: ES5.1 15.1.2.4
v = isNaN("  "); if (!(v === false)) { throw 1326; }
// vector 327: ES5.1 15.1.2.4
v = isNaN("0x10"); if (!(v === false)) { throw 1327; }
// vector 328: ES5.1 15.1.2.4
v = isNaN("0b10"); if (!(v === true)) { throw 1328; }
// vector 329: ES5.1 15.1.2.4
v = isNaN("0o10"); if (!(v === true)) { throw 1329; }
true;
