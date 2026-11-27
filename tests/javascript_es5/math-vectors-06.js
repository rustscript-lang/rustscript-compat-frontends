'use strict';
var v;
// vector 180: ES5.1 15.8.2
v = Math.round(2); if (!(v === 2)) { throw 1180; }
// vector 181: ES5.1 15.8.2
v = Math.round(-2); if (!(v === -2)) { throw 1181; }
// vector 182: ES5.1 15.8.2
v = Math.round(Infinity); if (!(v === Infinity)) { throw 1182; }
// vector 183: ES5.1 15.8.2
v = Math.round(-Infinity); if (!(v === -Infinity)) { throw 1183; }
// vector 184: ES5.1 15.8.2
v = Math.round(NaN); if (!(v !== v)) { throw 1184; }
// vector 185: ES5.1 15.8.2
v = Math.round(" 2 "); if (!(v === 2)) { throw 1185; }
// vector 186: ES5.1 15.8.2
v = Math.round("bad"); if (!(v !== v)) { throw 1186; }
// vector 187: ES5.1 15.8.2
v = Math.round(); if (!(v !== v)) { throw 1187; }
// vector 188: ES5.1 15.8.2
v = Math.sin(undefined); if (!(v !== v)) { throw 1188; }
// vector 189: ES5.1 15.8.2
v = Math.sin(null); if (!(v === 0 && 1/v === Infinity)) { throw 1189; }
// vector 190: ES5.1 15.8.2
v = Math.sin(true); if (!(v === 0.8414709848078965)) { throw 1190; }
// vector 191: ES5.1 15.8.2
v = Math.sin(false); if (!(v === 0 && 1/v === Infinity)) { throw 1191; }
// vector 192: ES5.1 15.8.2
v = Math.sin(0); if (!(v === 0 && 1/v === Infinity)) { throw 1192; }
// vector 193: ES5.1 15.8.2
v = Math.sin(-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1193; }
// vector 194: ES5.1 15.8.2
v = Math.sin(0.5); if (!(v === 0.479425538604203)) { throw 1194; }
// vector 195: ES5.1 15.8.2
v = Math.sin(-0.5); if (!(v === -0.479425538604203)) { throw 1195; }
// vector 196: ES5.1 15.8.2
v = Math.sin(1); if (!(v === 0.8414709848078965)) { throw 1196; }
// vector 197: ES5.1 15.8.2
v = Math.sin(-1); if (!(v === -0.8414709848078965)) { throw 1197; }
// vector 198: ES5.1 15.8.2
v = Math.sin(2); if (!(v === 0.9092974268256817)) { throw 1198; }
// vector 199: ES5.1 15.8.2
v = Math.sin(-2); if (!(v === -0.9092974268256817)) { throw 1199; }
// vector 200: ES5.1 15.8.2
v = Math.sin(Infinity); if (!(v !== v)) { throw 1200; }
// vector 201: ES5.1 15.8.2
v = Math.sin(-Infinity); if (!(v !== v)) { throw 1201; }
// vector 202: ES5.1 15.8.2
v = Math.sin(NaN); if (!(v !== v)) { throw 1202; }
// vector 203: ES5.1 15.8.2
v = Math.sin(" 2 "); if (!(v === 0.9092974268256817)) { throw 1203; }
// vector 204: ES5.1 15.8.2
v = Math.sin("bad"); if (!(v !== v)) { throw 1204; }
// vector 205: ES5.1 15.8.2
v = Math.sin(); if (!(v !== v)) { throw 1205; }
// vector 206: ES5.1 15.8.2
v = Math.sqrt(undefined); if (!(v !== v)) { throw 1206; }
// vector 207: ES5.1 15.8.2
v = Math.sqrt(null); if (!(v === 0 && 1/v === Infinity)) { throw 1207; }
// vector 208: ES5.1 15.8.2
v = Math.sqrt(true); if (!(v === 1)) { throw 1208; }
// vector 209: ES5.1 15.8.2
v = Math.sqrt(false); if (!(v === 0 && 1/v === Infinity)) { throw 1209; }
true;
