// Public Object.keys result-length consumer on the normal default file route.
// The last expression is the boxed Boolean completion read by the entry test.
var k=Object.keys({a:1,b:2});
var n=0;
if(!(k.length===2))n=1;
if(n===0&&!(k[0]==="a"&&k[1]==="b"))n=2;
var e=Object.keys({});
if(n===0&&!(e.length===0))n=3;
var a=[7,8];
if(n===0&&!(Object.keys(a).length===2))n=4;
var c=0;
for(var x of k){c=c+1;}
if(n===0&&c!==2)n=5;
n===0;
