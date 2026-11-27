'use strict';
var v;
// vector 30: ES5.1 15.8.2
v = Math.acos(0); if (!(v === 1.5707963267948966)) { throw 1030; }
// vector 31: ES5.1 15.8.2
v = Math.acos(-0); if (!(v === 1.5707963267948966)) { throw 1031; }
// vector 32: ES5.1 15.8.2
v = Math.acos(0.5); if (!(v === 1.0471975511965979)) { throw 1032; }
// vector 33: ES5.1 15.8.2
v = Math.acos(-0.5); if (!(v === 2.0943951023931957)) { throw 1033; }
// vector 34: ES5.1 15.8.2
v = Math.acos(1); if (!(v === 0 && 1/v === Infinity)) { throw 1034; }
// vector 35: ES5.1 15.8.2
v = Math.acos(-1); if (!(v === 3.141592653589793)) { throw 1035; }
// vector 36: ES5.1 15.8.2
v = Math.acos(2); if (!(v !== v)) { throw 1036; }
// vector 37: ES5.1 15.8.2
v = Math.acos(-2); if (!(v !== v)) { throw 1037; }
// vector 38: ES5.1 15.8.2
v = Math.acos(Infinity); if (!(v !== v)) { throw 1038; }
// vector 39: ES5.1 15.8.2
v = Math.acos(-Infinity); if (!(v !== v)) { throw 1039; }
// vector 40: ES5.1 15.8.2
v = Math.acos(NaN); if (!(v !== v)) { throw 1040; }
// vector 41: ES5.1 15.8.2
v = Math.acos(" 2 "); if (!(v !== v)) { throw 1041; }
// vector 42: ES5.1 15.8.2
v = Math.acos("bad"); if (!(v !== v)) { throw 1042; }
// vector 43: ES5.1 15.8.2
v = Math.acos(); if (!(v !== v)) { throw 1043; }
// vector 44: ES5.1 15.8.2
v = Math.asin(undefined); if (!(v !== v)) { throw 1044; }
// vector 45: ES5.1 15.8.2
v = Math.asin(null); if (!(v === 0 && 1/v === Infinity)) { throw 1045; }
// vector 46: ES5.1 15.8.2
v = Math.asin(true); if (!(v === 1.5707963267948966)) { throw 1046; }
// vector 47: ES5.1 15.8.2
v = Math.asin(false); if (!(v === 0 && 1/v === Infinity)) { throw 1047; }
// vector 48: ES5.1 15.8.2
v = Math.asin(0); if (!(v === 0 && 1/v === Infinity)) { throw 1048; }
// vector 49: ES5.1 15.8.2
v = Math.asin(-0); if (!(v === 0 && 1/v === -Infinity)) { throw 1049; }
// vector 50: ES5.1 15.8.2
v = Math.asin(0.5); if (!(v === 0.5235987755982989)) { throw 1050; }
// vector 51: ES5.1 15.8.2
v = Math.asin(-0.5); if (!(v === -0.5235987755982989)) { throw 1051; }
// vector 52: ES5.1 15.8.2
v = Math.asin(1); if (!(v === 1.5707963267948966)) { throw 1052; }
// vector 53: ES5.1 15.8.2
v = Math.asin(-1); if (!(v === -1.5707963267948966)) { throw 1053; }
// vector 54: ES5.1 15.8.2
v = Math.asin(2); if (!(v !== v)) { throw 1054; }
// vector 55: ES5.1 15.8.2
v = Math.asin(-2); if (!(v !== v)) { throw 1055; }
// vector 56: ES5.1 15.8.2
v = Math.asin(Infinity); if (!(v !== v)) { throw 1056; }
// vector 57: ES5.1 15.8.2
v = Math.asin(-Infinity); if (!(v !== v)) { throw 1057; }
// vector 58: ES5.1 15.8.2
v = Math.asin(NaN); if (!(v !== v)) { throw 1058; }
// vector 59: ES5.1 15.8.2
v = Math.asin(" 2 "); if (!(v !== v)) { throw 1059; }
true;
