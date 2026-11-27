'use strict';
var v;
// vector 150: ES5.1 15.8.2
v = Math.floor("bad"); if (!(v !== v)) { throw 1150; }
// vector 151: ES5.1 15.8.2
v = Math.floor(); if (!(v !== v)) { throw 1151; }
// vector 152: ES5.1 15.8.2
v = Math.log(undefined); if (!(v !== v)) { throw 1152; }
// vector 153: ES5.1 15.8.2
v = Math.log(null); if (!(v === -Infinity)) { throw 1153; }
// vector 154: ES5.1 15.8.2
v = Math.log(true); if (!(v === 0 && 1/v === Infinity)) { throw 1154; }
// vector 155: ES5.1 15.8.2
v = Math.log(false); if (!(v === -Infinity)) { throw 1155; }
// vector 156: ES5.1 15.8.2
v = Math.log(0); if (!(v === -Infinity)) { throw 1156; }
// vector 157: ES5.1 15.8.2
v = Math.log(-0); if (!(v === -Infinity)) { throw 1157; }
// vector 158: ES5.1 15.8.2
v = Math.log(0.5); if (!(v === -0.6931471805599453)) { throw 1158; }
// vector 159: ES5.1 15.8.2
v = Math.log(-0.5); if (!(v !== v)) { throw 1159; }
// vector 160: ES5.1 15.8.2
v = Math.log(1); if (!(v === 0 && 1/v === Infinity)) { throw 1160; }
// vector 161: ES5.1 15.8.2
v = Math.log(-1); if (!(v !== v)) { throw 1161; }
// vector 162: ES5.1 15.8.2
v = Math.log(2); if (!(v === 0.6931471805599453)) { throw 1162; }
// vector 163: ES5.1 15.8.2
v = Math.log(-2); if (!(v !== v)) { throw 1163; }
// vector 164: ES5.1 15.8.2
v = Math.log(Infinity); if (!(v === Infinity)) { throw 1164; }
// vector 165: ES5.1 15.8.2
v = Math.log(-Infinity); if (!(v !== v)) { throw 1165; }
// vector 166: ES5.1 15.8.2
v = Math.log(NaN); if (!(v !== v)) { throw 1166; }
// vector 167: ES5.1 15.8.2
v = Math.log(" 2 "); if (!(v === 0.6931471805599453)) { throw 1167; }
// vector 168: ES5.1 15.8.2
v = Math.log("bad"); if (!(v !== v)) { throw 1168; }
// vector 169: ES5.1 15.8.2
v = Math.log(); if (!(v !== v)) { throw 1169; }
// vector 170: ES5.1 15.8.2
v = Math.round(undefined); if (!(v !== v)) { throw 1170; }
// vector 171: ES5.1 15.8.2
v = Math.round(null); if (!(v === 0 && 1/v === Infinity)) { throw 1171; }
// vector 172: ES5.1 15.8.2
v = Math.round(true); if (!(v === 1)) { throw 1172; }
// vector 173: ES5.1 15.8.2
v = Math.round(false); if (!(v === 0 && 1/v === Infinity)) { throw 1173; }
// vector 174: ES5.1 15.8.2
v = Math.round(0); if (!(v === 0 && 1/v === Infinity)) { throw 1174; }
// vector 175: ES5.1 15.8.2
v = Math.round(-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1175; }
// vector 176: ES5.1 15.8.2
v = Math.round(0.5); if (!(v === 1)) { throw 1176; }
// vector 177: ES5.1 15.8.2
v = Math.round(-0.5); if (!(v === 0 && 1/v === -Infinity)) { throw 1177; }
// vector 178: ES5.1 15.8.2
v = Math.round(1); if (!(v === 1)) { throw 1178; }
// vector 179: ES5.1 15.8.2
v = Math.round(-1); if (!(v === -1)) { throw 1179; }
true;
