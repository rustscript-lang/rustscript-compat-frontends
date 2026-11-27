'use strict';
var v;
// vector 120: ES5.1 15.8.2
v = Math.exp(0); if (!(v === 1)) { throw 1120; }
// vector 121: ES5.1 15.8.2
v = Math.exp(-0); if (!(v === 1)) { throw 1121; }
// vector 122: ES5.1 15.8.2
v = Math.exp(0.5); if (!(v === 1.6487212707001282)) { throw 1122; }
// vector 123: ES5.1 15.8.2
v = Math.exp(-0.5); if (!(v === 0.6065306597126334)) { throw 1123; }
// vector 124: ES5.1 15.8.2
v = Math.exp(1); if (!(v === 2.718281828459045)) { throw 1124; }
// vector 125: ES5.1 15.8.2
v = Math.exp(-1); if (!(v === 0.36787944117144233)) { throw 1125; }
// vector 126: ES5.1 15.8.2
v = Math.exp(2); if (!(v === 7.38905609893065)) { throw 1126; }
// vector 127: ES5.1 15.8.2
v = Math.exp(-2); if (!(v === 0.1353352832366127)) { throw 1127; }
// vector 128: ES5.1 15.8.2
v = Math.exp(Infinity); if (!(v === Infinity)) { throw 1128; }
// vector 129: ES5.1 15.8.2
v = Math.exp(-Infinity); if (!(v === 0 && 1/v === Infinity)) { throw 1129; }
// vector 130: ES5.1 15.8.2
v = Math.exp(NaN); if (!(v !== v)) { throw 1130; }
// vector 131: ES5.1 15.8.2
v = Math.exp(" 2 "); if (!(v === 7.38905609893065)) { throw 1131; }
// vector 132: ES5.1 15.8.2
v = Math.exp("bad"); if (!(v !== v)) { throw 1132; }
// vector 133: ES5.1 15.8.2
v = Math.exp(); if (!(v !== v)) { throw 1133; }
// vector 134: ES5.1 15.8.2
v = Math.floor(undefined); if (!(v !== v)) { throw 1134; }
// vector 135: ES5.1 15.8.2
v = Math.floor(null); if (!(v === 0 && 1/v === Infinity)) { throw 1135; }
// vector 136: ES5.1 15.8.2
v = Math.floor(true); if (!(v === 1)) { throw 1136; }
// vector 137: ES5.1 15.8.2
v = Math.floor(false); if (!(v === 0 && 1/v === Infinity)) { throw 1137; }
// vector 138: ES5.1 15.8.2
v = Math.floor(0); if (!(v === 0 && 1/v === Infinity)) { throw 1138; }
// vector 139: ES5.1 15.8.2
v = Math.floor(-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1139; }
// vector 140: ES5.1 15.8.2
v = Math.floor(0.5); if (!(v === 0 && 1/v === Infinity)) { throw 1140; }
// vector 141: ES5.1 15.8.2
v = Math.floor(-0.5); if (!(v === -1)) { throw 1141; }
// vector 142: ES5.1 15.8.2
v = Math.floor(1); if (!(v === 1)) { throw 1142; }
// vector 143: ES5.1 15.8.2
v = Math.floor(-1); if (!(v === -1)) { throw 1143; }
// vector 144: ES5.1 15.8.2
v = Math.floor(2); if (!(v === 2)) { throw 1144; }
// vector 145: ES5.1 15.8.2
v = Math.floor(-2); if (!(v === -2)) { throw 1145; }
// vector 146: ES5.1 15.8.2
v = Math.floor(Infinity); if (!(v === Infinity)) { throw 1146; }
// vector 147: ES5.1 15.8.2
v = Math.floor(-Infinity); if (!(v === -Infinity)) { throw 1147; }
// vector 148: ES5.1 15.8.2
v = Math.floor(NaN); if (!(v !== v)) { throw 1148; }
// vector 149: ES5.1 15.8.2
v = Math.floor(" 2 "); if (!(v === 2)) { throw 1149; }
true;
