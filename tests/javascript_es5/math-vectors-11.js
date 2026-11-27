'use strict';
var v;
// vector 330: ES5.1 15.1.2.4
v = isNaN("+0x10"); if (!(v === true)) { throw 1330; }
// vector 331: ES5.1 15.1.2.4
v = isNaN("Infinity"); if (!(v === false)) { throw 1331; }
// vector 332: ES5.1 15.1.2.4
v = isNaN("-Infinity"); if (!(v === false)) { throw 1332; }
// vector 333: ES5.1 15.1.2.4
v = isNaN("1e2"); if (!(v === false)) { throw 1333; }
// vector 334: ES5.1 15.1.2.4
v = isNaN("\u180e2"); if (!(v === true)) { throw 1334; }
// vector 335: ES5.1 15.1.2.4
v = isNaN("\u200b2"); if (!(v === false)) { throw 1335; }
// vector 336: ES5.1 15.1.2.4
v = isNaN(NaN); if (!(v === true)) { throw 1336; }
// vector 337: ES5.1 15.1.2.4
v = isNaN(Infinity); if (!(v === false)) { throw 1337; }
// vector 338: ES5.1 15.1.2.4
v = isNaN(-0); if (!(v === false)) { throw 1338; }
// vector 339: ES5.1 15.1.2.5
v = isFinite(); if (!(v === false)) { throw 1339; }
// vector 340: ES5.1 15.1.2.5
v = isFinite(undefined); if (!(v === false)) { throw 1340; }
// vector 341: ES5.1 15.1.2.5
v = isFinite(null); if (!(v === true)) { throw 1341; }
// vector 342: ES5.1 15.1.2.5
v = isFinite(true); if (!(v === true)) { throw 1342; }
// vector 343: ES5.1 15.1.2.5
v = isFinite(false); if (!(v === true)) { throw 1343; }
// vector 344: ES5.1 15.1.2.5
v = isFinite(""); if (!(v === true)) { throw 1344; }
// vector 345: ES5.1 15.1.2.5
v = isFinite("  "); if (!(v === true)) { throw 1345; }
// vector 346: ES5.1 15.1.2.5
v = isFinite("0x10"); if (!(v === true)) { throw 1346; }
// vector 347: ES5.1 15.1.2.5
v = isFinite("0b10"); if (!(v === false)) { throw 1347; }
// vector 348: ES5.1 15.1.2.5
v = isFinite("0o10"); if (!(v === false)) { throw 1348; }
// vector 349: ES5.1 15.1.2.5
v = isFinite("+0x10"); if (!(v === false)) { throw 1349; }
// vector 350: ES5.1 15.1.2.5
v = isFinite("Infinity"); if (!(v === false)) { throw 1350; }
// vector 351: ES5.1 15.1.2.5
v = isFinite("-Infinity"); if (!(v === false)) { throw 1351; }
// vector 352: ES5.1 15.1.2.5
v = isFinite("1e2"); if (!(v === true)) { throw 1352; }
// vector 353: ES5.1 15.1.2.5
v = isFinite("\u180e2"); if (!(v === false)) { throw 1353; }
// vector 354: ES5.1 15.1.2.5
v = isFinite("\u200b2"); if (!(v === true)) { throw 1354; }
// vector 355: ES5.1 15.1.2.5
v = isFinite(NaN); if (!(v === false)) { throw 1355; }
// vector 356: ES5.1 15.1.2.5
v = isFinite(Infinity); if (!(v === false)) { throw 1356; }
// vector 357: ES5.1 15.1.2.5
v = isFinite(-0); if (!(v === true)) { throw 1357; }
true;
