'use strict';
var v;
// vector 0: ES5.1 15.8.1
v = Math.E; if (!(v === 2.718281828459045)) { throw 1000; }
// vector 1: ES5.1 15.8.1
v = Math.LN10; if (!(v === 2.302585092994046)) { throw 1001; }
// vector 2: ES5.1 15.8.1
v = Math.LN2; if (!(v === 0.6931471805599453)) { throw 1002; }
// vector 3: ES5.1 15.8.1
v = Math.LOG2E; if (!(v === 1.4426950408889634)) { throw 1003; }
// vector 4: ES5.1 15.8.1
v = Math.LOG10E; if (!(v === 0.4342944819032518)) { throw 1004; }
// vector 5: ES5.1 15.8.1
v = Math.PI; if (!(v === 3.141592653589793)) { throw 1005; }
// vector 6: ES5.1 15.8.1
v = Math.SQRT1_2; if (!(v === 0.7071067811865476)) { throw 1006; }
// vector 7: ES5.1 15.8.1
v = Math.SQRT2; if (!(v === 1.4142135623730951)) { throw 1007; }
// vector 8: ES5.1 15.8.2
v = Math.abs(undefined); if (!(v !== v)) { throw 1008; }
// vector 9: ES5.1 15.8.2
v = Math.abs(null); if (!(v === 0 && 1/v === Infinity)) { throw 1009; }
// vector 10: ES5.1 15.8.2
v = Math.abs(true); if (!(v === 1)) { throw 1010; }
// vector 11: ES5.1 15.8.2
v = Math.abs(false); if (!(v === 0 && 1/v === Infinity)) { throw 1011; }
// vector 12: ES5.1 15.8.2
v = Math.abs(0); if (!(v === 0 && 1/v === Infinity)) { throw 1012; }
// vector 13: ES5.1 15.8.2
v = Math.abs(-0); if (!(v === 0 && 1/v === Infinity)) { throw 1013; }
// vector 14: ES5.1 15.8.2
v = Math.abs(0.5); if (!(v === 0.5)) { throw 1014; }
// vector 15: ES5.1 15.8.2
v = Math.abs(-0.5); if (!(v === 0.5)) { throw 1015; }
// vector 16: ES5.1 15.8.2
v = Math.abs(1); if (!(v === 1)) { throw 1016; }
// vector 17: ES5.1 15.8.2
v = Math.abs(-1); if (!(v === 1)) { throw 1017; }
// vector 18: ES5.1 15.8.2
v = Math.abs(2); if (!(v === 2)) { throw 1018; }
// vector 19: ES5.1 15.8.2
v = Math.abs(-2); if (!(v === 2)) { throw 1019; }
// vector 20: ES5.1 15.8.2
v = Math.abs(Infinity); if (!(v === Infinity)) { throw 1020; }
// vector 21: ES5.1 15.8.2
v = Math.abs(-Infinity); if (!(v === Infinity)) { throw 1021; }
// vector 22: ES5.1 15.8.2
v = Math.abs(NaN); if (!(v !== v)) { throw 1022; }
// vector 23: ES5.1 15.8.2
v = Math.abs(" 2 "); if (!(v === 2)) { throw 1023; }
// vector 24: ES5.1 15.8.2
v = Math.abs("bad"); if (!(v !== v)) { throw 1024; }
// vector 25: ES5.1 15.8.2
v = Math.abs(); if (!(v !== v)) { throw 1025; }
// vector 26: ES5.1 15.8.2
v = Math.acos(undefined); if (!(v !== v)) { throw 1026; }
// vector 27: ES5.1 15.8.2
v = Math.acos(null); if (!(v === 1.5707963267948966)) { throw 1027; }
// vector 28: ES5.1 15.8.2
v = Math.acos(true); if (!(v === 0 && 1/v === Infinity)) { throw 1028; }
// vector 29: ES5.1 15.8.2
v = Math.acos(false); if (!(v === 1.5707963267948966)) { throw 1029; }
true;
