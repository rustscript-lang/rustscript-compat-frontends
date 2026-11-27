//! Scope149 primitive compound assignment and update. Length smokes are historical only.
#[path = "javascript_es5/object_observer.rs"]
mod object_observer;

struct Ex {
    kind: u8,
    nan: bool,
    bits: u64,
    units: &'static [u16],
}
struct Row {
    result: &'static str,
    stored: &'static str,
    result_ex: Ex,
    stored_ex: Ex,
}
const U0: &[u16] = &[];
const U1: &[u16] = &[50, 120];
const U2: &[u16] = &[120, 50];
const U3: &[u16] = &[120, 120];
const U4: &[u16] = &[120, 116, 114, 117, 101];
const U5: &[u16] = &[120, 110, 117, 108, 108];
const U6: &[u16] = &[120, 117, 110, 100, 101, 102, 105, 110, 101, 100];
const U7: &[u16] = &[116, 114, 117, 101, 120];
const U8: &[u16] = &[110, 117, 108, 108, 120];
const U9: &[u16] = &[117, 110, 100, 101, 102, 105, 110, 101, 100, 120];
const COMPOUND: &[Row] = &[
    Row {
        result: "var x=2;x+=2;",
        stored: "var x=2;x+=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=2;",
        stored: "function id(v){return v;}var x=id(2);x+=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x+=id(2);",
        stored: "function id(v){return v;}var x=2;x+=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=id(2);",
        stored: "function id(v){return v;}var x=id(2);x+=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x+='x';",
        stored: "var x=2;x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U1,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U1,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+='x';",
        stored: "function id(v){return v;}var x=id(2);x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U1,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U1,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x+=id('x');",
        stored: "function id(v){return v;}var x=2;x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U1,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U1,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=id('x');",
        stored: "function id(v){return v;}var x=id(2);x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U1,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U1,
        },
    },
    Row {
        result: "var x=2;x+=true;",
        stored: "var x=2;x+=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=true;",
        stored: "function id(v){return v;}var x=id(2);x+=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x+=id(true);",
        stored: "function id(v){return v;}var x=2;x+=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=id(true);",
        stored: "function id(v){return v;}var x=id(2);x+=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x+=null;",
        stored: "var x=2;x+=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=null;",
        stored: "function id(v){return v;}var x=id(2);x+=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x+=id(null);",
        stored: "function id(v){return v;}var x=2;x+=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=id(null);",
        stored: "function id(v){return v;}var x=id(2);x+=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x+=undefined;",
        stored: "var x=2;x+=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=undefined;",
        stored: "function id(v){return v;}var x=id(2);x+=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x+=id(undefined);",
        stored: "function id(v){return v;}var x=2;x+=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x+=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x+=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x+=2;",
        stored: "var x='x';x+=2;x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U2,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U2,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=2;",
        stored: "function id(v){return v;}var x=id('x');x+=2;x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U2,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U2,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x+=id(2);",
        stored: "function id(v){return v;}var x='x';x+=id(2);x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U2,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U2,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=id(2);",
        stored: "function id(v){return v;}var x=id('x');x+=id(2);x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U2,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U2,
        },
    },
    Row {
        result: "var x='x';x+='x';",
        stored: "var x='x';x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U3,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U3,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+='x';",
        stored: "function id(v){return v;}var x=id('x');x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U3,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U3,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x+=id('x');",
        stored: "function id(v){return v;}var x='x';x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U3,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U3,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=id('x');",
        stored: "function id(v){return v;}var x=id('x');x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U3,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U3,
        },
    },
    Row {
        result: "var x='x';x+=true;",
        stored: "var x='x';x+=true;x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U4,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U4,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=true;",
        stored: "function id(v){return v;}var x=id('x');x+=true;x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U4,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U4,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x+=id(true);",
        stored: "function id(v){return v;}var x='x';x+=id(true);x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U4,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U4,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=id(true);",
        stored: "function id(v){return v;}var x=id('x');x+=id(true);x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U4,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U4,
        },
    },
    Row {
        result: "var x='x';x+=null;",
        stored: "var x='x';x+=null;x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U5,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U5,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=null;",
        stored: "function id(v){return v;}var x=id('x');x+=null;x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U5,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U5,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x+=id(null);",
        stored: "function id(v){return v;}var x='x';x+=id(null);x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U5,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U5,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=id(null);",
        stored: "function id(v){return v;}var x=id('x');x+=id(null);x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U5,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U5,
        },
    },
    Row {
        result: "var x='x';x+=undefined;",
        stored: "var x='x';x+=undefined;x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U6,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U6,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=undefined;",
        stored: "function id(v){return v;}var x=id('x');x+=undefined;x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U6,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U6,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x+=id(undefined);",
        stored: "function id(v){return v;}var x='x';x+=id(undefined);x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U6,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U6,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x+=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x+=id(undefined);x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U6,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U6,
        },
    },
    Row {
        result: "var x=true;x+=2;",
        stored: "var x=true;x+=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=2;",
        stored: "function id(v){return v;}var x=id(true);x+=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x+=id(2);",
        stored: "function id(v){return v;}var x=true;x+=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=id(2);",
        stored: "function id(v){return v;}var x=id(true);x+=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x+='x';",
        stored: "var x=true;x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U7,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U7,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+='x';",
        stored: "function id(v){return v;}var x=id(true);x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U7,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U7,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x+=id('x');",
        stored: "function id(v){return v;}var x=true;x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U7,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U7,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=id('x');",
        stored: "function id(v){return v;}var x=id(true);x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U7,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U7,
        },
    },
    Row {
        result: "var x=true;x+=true;",
        stored: "var x=true;x+=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=true;",
        stored: "function id(v){return v;}var x=id(true);x+=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x+=id(true);",
        stored: "function id(v){return v;}var x=true;x+=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=id(true);",
        stored: "function id(v){return v;}var x=id(true);x+=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x+=null;",
        stored: "var x=true;x+=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=null;",
        stored: "function id(v){return v;}var x=id(true);x+=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x+=id(null);",
        stored: "function id(v){return v;}var x=true;x+=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=id(null);",
        stored: "function id(v){return v;}var x=id(true);x+=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x+=undefined;",
        stored: "var x=true;x+=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=undefined;",
        stored: "function id(v){return v;}var x=id(true);x+=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x+=id(undefined);",
        stored: "function id(v){return v;}var x=true;x+=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x+=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x+=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x+=2;",
        stored: "var x=null;x+=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=2;",
        stored: "function id(v){return v;}var x=id(null);x+=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x+=id(2);",
        stored: "function id(v){return v;}var x=null;x+=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=id(2);",
        stored: "function id(v){return v;}var x=id(null);x+=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x+='x';",
        stored: "var x=null;x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U8,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U8,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+='x';",
        stored: "function id(v){return v;}var x=id(null);x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U8,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U8,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x+=id('x');",
        stored: "function id(v){return v;}var x=null;x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U8,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U8,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=id('x');",
        stored: "function id(v){return v;}var x=id(null);x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U8,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U8,
        },
    },
    Row {
        result: "var x=null;x+=true;",
        stored: "var x=null;x+=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=true;",
        stored: "function id(v){return v;}var x=id(null);x+=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x+=id(true);",
        stored: "function id(v){return v;}var x=null;x+=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=id(true);",
        stored: "function id(v){return v;}var x=id(null);x+=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x+=null;",
        stored: "var x=null;x+=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=null;",
        stored: "function id(v){return v;}var x=id(null);x+=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x+=id(null);",
        stored: "function id(v){return v;}var x=null;x+=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=id(null);",
        stored: "function id(v){return v;}var x=id(null);x+=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x+=undefined;",
        stored: "var x=null;x+=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=undefined;",
        stored: "function id(v){return v;}var x=id(null);x+=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x+=id(undefined);",
        stored: "function id(v){return v;}var x=null;x+=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x+=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x+=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x+=2;",
        stored: "var x=undefined;x+=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=2;",
        stored: "function id(v){return v;}var x=id(undefined);x+=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x+=id(2);",
        stored: "function id(v){return v;}var x=undefined;x+=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x+=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x+='x';",
        stored: "var x=undefined;x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U9,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U9,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+='x';",
        stored: "function id(v){return v;}var x=id(undefined);x+='x';x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U9,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U9,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x+=id('x');",
        stored: "function id(v){return v;}var x=undefined;x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U9,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U9,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x+=id('x');x;",
        result_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U9,
        },
        stored_ex: Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: U9,
        },
    },
    Row {
        result: "var x=undefined;x+=true;",
        stored: "var x=undefined;x+=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=true;",
        stored: "function id(v){return v;}var x=id(undefined);x+=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x+=id(true);",
        stored: "function id(v){return v;}var x=undefined;x+=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x+=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x+=null;",
        stored: "var x=undefined;x+=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=null;",
        stored: "function id(v){return v;}var x=id(undefined);x+=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x+=id(null);",
        stored: "function id(v){return v;}var x=undefined;x+=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x+=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x+=undefined;",
        stored: "var x=undefined;x+=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x+=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x+=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x+=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x+=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x+=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x-=2;",
        stored: "var x=2;x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=2;",
        stored: "function id(v){return v;}var x=id(2);x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x-=id(2);",
        stored: "function id(v){return v;}var x=2;x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=id(2);",
        stored: "function id(v){return v;}var x=id(2);x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x-='x';",
        stored: "var x=2;x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-='x';",
        stored: "function id(v){return v;}var x=id(2);x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x-=id('x');",
        stored: "function id(v){return v;}var x=2;x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=id('x');",
        stored: "function id(v){return v;}var x=id(2);x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x-=true;",
        stored: "var x=2;x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=true;",
        stored: "function id(v){return v;}var x=id(2);x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x-=id(true);",
        stored: "function id(v){return v;}var x=2;x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=id(true);",
        stored: "function id(v){return v;}var x=id(2);x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x-=null;",
        stored: "var x=2;x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=null;",
        stored: "function id(v){return v;}var x=id(2);x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x-=id(null);",
        stored: "function id(v){return v;}var x=2;x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=id(null);",
        stored: "function id(v){return v;}var x=id(2);x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x-=undefined;",
        stored: "var x=2;x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=undefined;",
        stored: "function id(v){return v;}var x=id(2);x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x-=id(undefined);",
        stored: "function id(v){return v;}var x=2;x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x-=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x-=2;",
        stored: "var x='x';x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=2;",
        stored: "function id(v){return v;}var x=id('x');x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x-=id(2);",
        stored: "function id(v){return v;}var x='x';x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=id(2);",
        stored: "function id(v){return v;}var x=id('x');x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x-='x';",
        stored: "var x='x';x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-='x';",
        stored: "function id(v){return v;}var x=id('x');x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x-=id('x');",
        stored: "function id(v){return v;}var x='x';x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=id('x');",
        stored: "function id(v){return v;}var x=id('x');x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x-=true;",
        stored: "var x='x';x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=true;",
        stored: "function id(v){return v;}var x=id('x');x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x-=id(true);",
        stored: "function id(v){return v;}var x='x';x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=id(true);",
        stored: "function id(v){return v;}var x=id('x');x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x-=null;",
        stored: "var x='x';x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=null;",
        stored: "function id(v){return v;}var x=id('x');x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x-=id(null);",
        stored: "function id(v){return v;}var x='x';x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=id(null);",
        stored: "function id(v){return v;}var x=id('x');x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x-=undefined;",
        stored: "var x='x';x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=undefined;",
        stored: "function id(v){return v;}var x=id('x');x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x-=id(undefined);",
        stored: "function id(v){return v;}var x='x';x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x-=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x-=2;",
        stored: "var x=true;x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=2;",
        stored: "function id(v){return v;}var x=id(true);x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x-=id(2);",
        stored: "function id(v){return v;}var x=true;x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=id(2);",
        stored: "function id(v){return v;}var x=id(true);x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x-='x';",
        stored: "var x=true;x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-='x';",
        stored: "function id(v){return v;}var x=id(true);x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x-=id('x');",
        stored: "function id(v){return v;}var x=true;x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=id('x');",
        stored: "function id(v){return v;}var x=id(true);x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x-=true;",
        stored: "var x=true;x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=true;",
        stored: "function id(v){return v;}var x=id(true);x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x-=id(true);",
        stored: "function id(v){return v;}var x=true;x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=id(true);",
        stored: "function id(v){return v;}var x=id(true);x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x-=null;",
        stored: "var x=true;x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=null;",
        stored: "function id(v){return v;}var x=id(true);x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x-=id(null);",
        stored: "function id(v){return v;}var x=true;x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=id(null);",
        stored: "function id(v){return v;}var x=id(true);x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x-=undefined;",
        stored: "var x=true;x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=undefined;",
        stored: "function id(v){return v;}var x=id(true);x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x-=id(undefined);",
        stored: "function id(v){return v;}var x=true;x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x-=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x-=2;",
        stored: "var x=null;x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13835058055282163712,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13835058055282163712,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=2;",
        stored: "function id(v){return v;}var x=id(null);x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13835058055282163712,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13835058055282163712,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x-=id(2);",
        stored: "function id(v){return v;}var x=null;x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13835058055282163712,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13835058055282163712,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=id(2);",
        stored: "function id(v){return v;}var x=id(null);x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13835058055282163712,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13835058055282163712,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x-='x';",
        stored: "var x=null;x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-='x';",
        stored: "function id(v){return v;}var x=id(null);x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x-=id('x');",
        stored: "function id(v){return v;}var x=null;x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=id('x');",
        stored: "function id(v){return v;}var x=id(null);x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x-=true;",
        stored: "var x=null;x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=true;",
        stored: "function id(v){return v;}var x=id(null);x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x-=id(true);",
        stored: "function id(v){return v;}var x=null;x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=id(true);",
        stored: "function id(v){return v;}var x=id(null);x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x-=null;",
        stored: "var x=null;x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=null;",
        stored: "function id(v){return v;}var x=id(null);x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x-=id(null);",
        stored: "function id(v){return v;}var x=null;x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=id(null);",
        stored: "function id(v){return v;}var x=id(null);x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x-=undefined;",
        stored: "var x=null;x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=undefined;",
        stored: "function id(v){return v;}var x=id(null);x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x-=id(undefined);",
        stored: "function id(v){return v;}var x=null;x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x-=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x-=2;",
        stored: "var x=undefined;x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=2;",
        stored: "function id(v){return v;}var x=id(undefined);x-=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x-=id(2);",
        stored: "function id(v){return v;}var x=undefined;x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x-=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x-='x';",
        stored: "var x=undefined;x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-='x';",
        stored: "function id(v){return v;}var x=id(undefined);x-='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x-=id('x');",
        stored: "function id(v){return v;}var x=undefined;x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x-=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x-=true;",
        stored: "var x=undefined;x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=true;",
        stored: "function id(v){return v;}var x=id(undefined);x-=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x-=id(true);",
        stored: "function id(v){return v;}var x=undefined;x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x-=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x-=null;",
        stored: "var x=undefined;x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=null;",
        stored: "function id(v){return v;}var x=id(undefined);x-=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x-=id(null);",
        stored: "function id(v){return v;}var x=undefined;x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x-=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x-=undefined;",
        stored: "var x=undefined;x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x-=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x-=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x-=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x-=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x*=2;",
        stored: "var x=2;x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=2;",
        stored: "function id(v){return v;}var x=id(2);x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x*=id(2);",
        stored: "function id(v){return v;}var x=2;x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=id(2);",
        stored: "function id(v){return v;}var x=id(2);x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x*='x';",
        stored: "var x=2;x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*='x';",
        stored: "function id(v){return v;}var x=id(2);x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x*=id('x');",
        stored: "function id(v){return v;}var x=2;x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=id('x');",
        stored: "function id(v){return v;}var x=id(2);x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x*=true;",
        stored: "var x=2;x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=true;",
        stored: "function id(v){return v;}var x=id(2);x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x*=id(true);",
        stored: "function id(v){return v;}var x=2;x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=id(true);",
        stored: "function id(v){return v;}var x=id(2);x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x*=null;",
        stored: "var x=2;x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=null;",
        stored: "function id(v){return v;}var x=id(2);x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x*=id(null);",
        stored: "function id(v){return v;}var x=2;x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=id(null);",
        stored: "function id(v){return v;}var x=id(2);x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x*=undefined;",
        stored: "var x=2;x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=undefined;",
        stored: "function id(v){return v;}var x=id(2);x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x*=id(undefined);",
        stored: "function id(v){return v;}var x=2;x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x*=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x*=2;",
        stored: "var x='x';x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=2;",
        stored: "function id(v){return v;}var x=id('x');x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x*=id(2);",
        stored: "function id(v){return v;}var x='x';x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=id(2);",
        stored: "function id(v){return v;}var x=id('x');x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x*='x';",
        stored: "var x='x';x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*='x';",
        stored: "function id(v){return v;}var x=id('x');x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x*=id('x');",
        stored: "function id(v){return v;}var x='x';x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=id('x');",
        stored: "function id(v){return v;}var x=id('x');x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x*=true;",
        stored: "var x='x';x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=true;",
        stored: "function id(v){return v;}var x=id('x');x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x*=id(true);",
        stored: "function id(v){return v;}var x='x';x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=id(true);",
        stored: "function id(v){return v;}var x=id('x');x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x*=null;",
        stored: "var x='x';x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=null;",
        stored: "function id(v){return v;}var x=id('x');x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x*=id(null);",
        stored: "function id(v){return v;}var x='x';x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=id(null);",
        stored: "function id(v){return v;}var x=id('x');x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x*=undefined;",
        stored: "var x='x';x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=undefined;",
        stored: "function id(v){return v;}var x=id('x');x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x*=id(undefined);",
        stored: "function id(v){return v;}var x='x';x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x*=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x*=2;",
        stored: "var x=true;x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=2;",
        stored: "function id(v){return v;}var x=id(true);x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x*=id(2);",
        stored: "function id(v){return v;}var x=true;x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=id(2);",
        stored: "function id(v){return v;}var x=id(true);x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x*='x';",
        stored: "var x=true;x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*='x';",
        stored: "function id(v){return v;}var x=id(true);x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x*=id('x');",
        stored: "function id(v){return v;}var x=true;x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=id('x');",
        stored: "function id(v){return v;}var x=id(true);x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x*=true;",
        stored: "var x=true;x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=true;",
        stored: "function id(v){return v;}var x=id(true);x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x*=id(true);",
        stored: "function id(v){return v;}var x=true;x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=id(true);",
        stored: "function id(v){return v;}var x=id(true);x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x*=null;",
        stored: "var x=true;x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=null;",
        stored: "function id(v){return v;}var x=id(true);x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x*=id(null);",
        stored: "function id(v){return v;}var x=true;x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=id(null);",
        stored: "function id(v){return v;}var x=id(true);x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x*=undefined;",
        stored: "var x=true;x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=undefined;",
        stored: "function id(v){return v;}var x=id(true);x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x*=id(undefined);",
        stored: "function id(v){return v;}var x=true;x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x*=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x*=2;",
        stored: "var x=null;x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=2;",
        stored: "function id(v){return v;}var x=id(null);x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x*=id(2);",
        stored: "function id(v){return v;}var x=null;x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=id(2);",
        stored: "function id(v){return v;}var x=id(null);x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x*='x';",
        stored: "var x=null;x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*='x';",
        stored: "function id(v){return v;}var x=id(null);x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x*=id('x');",
        stored: "function id(v){return v;}var x=null;x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=id('x');",
        stored: "function id(v){return v;}var x=id(null);x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x*=true;",
        stored: "var x=null;x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=true;",
        stored: "function id(v){return v;}var x=id(null);x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x*=id(true);",
        stored: "function id(v){return v;}var x=null;x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=id(true);",
        stored: "function id(v){return v;}var x=id(null);x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x*=null;",
        stored: "var x=null;x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=null;",
        stored: "function id(v){return v;}var x=id(null);x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x*=id(null);",
        stored: "function id(v){return v;}var x=null;x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=id(null);",
        stored: "function id(v){return v;}var x=id(null);x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x*=undefined;",
        stored: "var x=null;x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=undefined;",
        stored: "function id(v){return v;}var x=id(null);x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x*=id(undefined);",
        stored: "function id(v){return v;}var x=null;x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x*=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x*=2;",
        stored: "var x=undefined;x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=2;",
        stored: "function id(v){return v;}var x=id(undefined);x*=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x*=id(2);",
        stored: "function id(v){return v;}var x=undefined;x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x*=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x*='x';",
        stored: "var x=undefined;x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*='x';",
        stored: "function id(v){return v;}var x=id(undefined);x*='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x*=id('x');",
        stored: "function id(v){return v;}var x=undefined;x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x*=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x*=true;",
        stored: "var x=undefined;x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=true;",
        stored: "function id(v){return v;}var x=id(undefined);x*=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x*=id(true);",
        stored: "function id(v){return v;}var x=undefined;x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x*=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x*=null;",
        stored: "var x=undefined;x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=null;",
        stored: "function id(v){return v;}var x=id(undefined);x*=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x*=id(null);",
        stored: "function id(v){return v;}var x=undefined;x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x*=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x*=undefined;",
        stored: "var x=undefined;x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x*=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x*=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x*=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x*=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x/=2;",
        stored: "var x=2;x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=2;",
        stored: "function id(v){return v;}var x=id(2);x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x/=id(2);",
        stored: "function id(v){return v;}var x=2;x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=id(2);",
        stored: "function id(v){return v;}var x=id(2);x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x/='x';",
        stored: "var x=2;x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/='x';",
        stored: "function id(v){return v;}var x=id(2);x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x/=id('x');",
        stored: "function id(v){return v;}var x=2;x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=id('x');",
        stored: "function id(v){return v;}var x=id(2);x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x/=true;",
        stored: "var x=2;x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=true;",
        stored: "function id(v){return v;}var x=id(2);x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x/=id(true);",
        stored: "function id(v){return v;}var x=2;x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=id(true);",
        stored: "function id(v){return v;}var x=id(2);x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x/=null;",
        stored: "var x=2;x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=null;",
        stored: "function id(v){return v;}var x=id(2);x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x/=id(null);",
        stored: "function id(v){return v;}var x=2;x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=id(null);",
        stored: "function id(v){return v;}var x=id(2);x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x/=undefined;",
        stored: "var x=2;x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=undefined;",
        stored: "function id(v){return v;}var x=id(2);x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x/=id(undefined);",
        stored: "function id(v){return v;}var x=2;x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x/=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x/=2;",
        stored: "var x='x';x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=2;",
        stored: "function id(v){return v;}var x=id('x');x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x/=id(2);",
        stored: "function id(v){return v;}var x='x';x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=id(2);",
        stored: "function id(v){return v;}var x=id('x');x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x/='x';",
        stored: "var x='x';x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/='x';",
        stored: "function id(v){return v;}var x=id('x');x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x/=id('x');",
        stored: "function id(v){return v;}var x='x';x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=id('x');",
        stored: "function id(v){return v;}var x=id('x');x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x/=true;",
        stored: "var x='x';x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=true;",
        stored: "function id(v){return v;}var x=id('x');x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x/=id(true);",
        stored: "function id(v){return v;}var x='x';x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=id(true);",
        stored: "function id(v){return v;}var x=id('x');x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x/=null;",
        stored: "var x='x';x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=null;",
        stored: "function id(v){return v;}var x=id('x');x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x/=id(null);",
        stored: "function id(v){return v;}var x='x';x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=id(null);",
        stored: "function id(v){return v;}var x=id('x');x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x/=undefined;",
        stored: "var x='x';x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=undefined;",
        stored: "function id(v){return v;}var x=id('x');x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x/=id(undefined);",
        stored: "function id(v){return v;}var x='x';x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x/=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x/=2;",
        stored: "var x=true;x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4602678819172646912,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4602678819172646912,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=2;",
        stored: "function id(v){return v;}var x=id(true);x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4602678819172646912,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4602678819172646912,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x/=id(2);",
        stored: "function id(v){return v;}var x=true;x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4602678819172646912,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4602678819172646912,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=id(2);",
        stored: "function id(v){return v;}var x=id(true);x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4602678819172646912,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4602678819172646912,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x/='x';",
        stored: "var x=true;x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/='x';",
        stored: "function id(v){return v;}var x=id(true);x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x/=id('x');",
        stored: "function id(v){return v;}var x=true;x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=id('x');",
        stored: "function id(v){return v;}var x=id(true);x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x/=true;",
        stored: "var x=true;x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=true;",
        stored: "function id(v){return v;}var x=id(true);x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x/=id(true);",
        stored: "function id(v){return v;}var x=true;x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=id(true);",
        stored: "function id(v){return v;}var x=id(true);x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x/=null;",
        stored: "var x=true;x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=null;",
        stored: "function id(v){return v;}var x=id(true);x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x/=id(null);",
        stored: "function id(v){return v;}var x=true;x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=id(null);",
        stored: "function id(v){return v;}var x=id(true);x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 9218868437227405312,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x/=undefined;",
        stored: "var x=true;x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=undefined;",
        stored: "function id(v){return v;}var x=id(true);x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x/=id(undefined);",
        stored: "function id(v){return v;}var x=true;x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x/=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x/=2;",
        stored: "var x=null;x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=2;",
        stored: "function id(v){return v;}var x=id(null);x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x/=id(2);",
        stored: "function id(v){return v;}var x=null;x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=id(2);",
        stored: "function id(v){return v;}var x=id(null);x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x/='x';",
        stored: "var x=null;x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/='x';",
        stored: "function id(v){return v;}var x=id(null);x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x/=id('x');",
        stored: "function id(v){return v;}var x=null;x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=id('x');",
        stored: "function id(v){return v;}var x=id(null);x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x/=true;",
        stored: "var x=null;x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=true;",
        stored: "function id(v){return v;}var x=id(null);x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x/=id(true);",
        stored: "function id(v){return v;}var x=null;x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=id(true);",
        stored: "function id(v){return v;}var x=id(null);x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x/=null;",
        stored: "var x=null;x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=null;",
        stored: "function id(v){return v;}var x=id(null);x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x/=id(null);",
        stored: "function id(v){return v;}var x=null;x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=id(null);",
        stored: "function id(v){return v;}var x=id(null);x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x/=undefined;",
        stored: "var x=null;x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=undefined;",
        stored: "function id(v){return v;}var x=id(null);x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x/=id(undefined);",
        stored: "function id(v){return v;}var x=null;x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x/=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x/=2;",
        stored: "var x=undefined;x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=2;",
        stored: "function id(v){return v;}var x=id(undefined);x/=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x/=id(2);",
        stored: "function id(v){return v;}var x=undefined;x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x/=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x/='x';",
        stored: "var x=undefined;x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/='x';",
        stored: "function id(v){return v;}var x=id(undefined);x/='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x/=id('x');",
        stored: "function id(v){return v;}var x=undefined;x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x/=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x/=true;",
        stored: "var x=undefined;x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=true;",
        stored: "function id(v){return v;}var x=id(undefined);x/=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x/=id(true);",
        stored: "function id(v){return v;}var x=undefined;x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x/=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x/=null;",
        stored: "var x=undefined;x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=null;",
        stored: "function id(v){return v;}var x=id(undefined);x/=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x/=id(null);",
        stored: "function id(v){return v;}var x=undefined;x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x/=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x/=undefined;",
        stored: "var x=undefined;x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x/=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x/=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x/=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x/=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x%=2;",
        stored: "var x=2;x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=2;",
        stored: "function id(v){return v;}var x=id(2);x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x%=id(2);",
        stored: "function id(v){return v;}var x=2;x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=id(2);",
        stored: "function id(v){return v;}var x=id(2);x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x%='x';",
        stored: "var x=2;x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%='x';",
        stored: "function id(v){return v;}var x=id(2);x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x%=id('x');",
        stored: "function id(v){return v;}var x=2;x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=id('x');",
        stored: "function id(v){return v;}var x=id(2);x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x%=true;",
        stored: "var x=2;x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=true;",
        stored: "function id(v){return v;}var x=id(2);x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x%=id(true);",
        stored: "function id(v){return v;}var x=2;x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=id(true);",
        stored: "function id(v){return v;}var x=id(2);x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x%=null;",
        stored: "var x=2;x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=null;",
        stored: "function id(v){return v;}var x=id(2);x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x%=id(null);",
        stored: "function id(v){return v;}var x=2;x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=id(null);",
        stored: "function id(v){return v;}var x=id(2);x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x%=undefined;",
        stored: "var x=2;x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=undefined;",
        stored: "function id(v){return v;}var x=id(2);x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x%=id(undefined);",
        stored: "function id(v){return v;}var x=2;x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x%=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x%=2;",
        stored: "var x='x';x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=2;",
        stored: "function id(v){return v;}var x=id('x');x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x%=id(2);",
        stored: "function id(v){return v;}var x='x';x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=id(2);",
        stored: "function id(v){return v;}var x=id('x');x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x%='x';",
        stored: "var x='x';x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%='x';",
        stored: "function id(v){return v;}var x=id('x');x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x%=id('x');",
        stored: "function id(v){return v;}var x='x';x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=id('x');",
        stored: "function id(v){return v;}var x=id('x');x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x%=true;",
        stored: "var x='x';x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=true;",
        stored: "function id(v){return v;}var x=id('x');x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x%=id(true);",
        stored: "function id(v){return v;}var x='x';x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=id(true);",
        stored: "function id(v){return v;}var x=id('x');x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x%=null;",
        stored: "var x='x';x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=null;",
        stored: "function id(v){return v;}var x=id('x');x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x%=id(null);",
        stored: "function id(v){return v;}var x='x';x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=id(null);",
        stored: "function id(v){return v;}var x=id('x');x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x%=undefined;",
        stored: "var x='x';x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=undefined;",
        stored: "function id(v){return v;}var x=id('x');x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x%=id(undefined);",
        stored: "function id(v){return v;}var x='x';x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x%=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x%=2;",
        stored: "var x=true;x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=2;",
        stored: "function id(v){return v;}var x=id(true);x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x%=id(2);",
        stored: "function id(v){return v;}var x=true;x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=id(2);",
        stored: "function id(v){return v;}var x=id(true);x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x%='x';",
        stored: "var x=true;x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%='x';",
        stored: "function id(v){return v;}var x=id(true);x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x%=id('x');",
        stored: "function id(v){return v;}var x=true;x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=id('x');",
        stored: "function id(v){return v;}var x=id(true);x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x%=true;",
        stored: "var x=true;x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=true;",
        stored: "function id(v){return v;}var x=id(true);x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x%=id(true);",
        stored: "function id(v){return v;}var x=true;x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=id(true);",
        stored: "function id(v){return v;}var x=id(true);x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x%=null;",
        stored: "var x=true;x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=null;",
        stored: "function id(v){return v;}var x=id(true);x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x%=id(null);",
        stored: "function id(v){return v;}var x=true;x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=id(null);",
        stored: "function id(v){return v;}var x=id(true);x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x%=undefined;",
        stored: "var x=true;x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=undefined;",
        stored: "function id(v){return v;}var x=id(true);x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x%=id(undefined);",
        stored: "function id(v){return v;}var x=true;x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x%=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x%=2;",
        stored: "var x=null;x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=2;",
        stored: "function id(v){return v;}var x=id(null);x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x%=id(2);",
        stored: "function id(v){return v;}var x=null;x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=id(2);",
        stored: "function id(v){return v;}var x=id(null);x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x%='x';",
        stored: "var x=null;x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%='x';",
        stored: "function id(v){return v;}var x=id(null);x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x%=id('x');",
        stored: "function id(v){return v;}var x=null;x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=id('x');",
        stored: "function id(v){return v;}var x=id(null);x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x%=true;",
        stored: "var x=null;x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=true;",
        stored: "function id(v){return v;}var x=id(null);x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x%=id(true);",
        stored: "function id(v){return v;}var x=null;x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=id(true);",
        stored: "function id(v){return v;}var x=id(null);x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x%=null;",
        stored: "var x=null;x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=null;",
        stored: "function id(v){return v;}var x=id(null);x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x%=id(null);",
        stored: "function id(v){return v;}var x=null;x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=id(null);",
        stored: "function id(v){return v;}var x=id(null);x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x%=undefined;",
        stored: "var x=null;x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=undefined;",
        stored: "function id(v){return v;}var x=id(null);x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x%=id(undefined);",
        stored: "function id(v){return v;}var x=null;x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x%=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x%=2;",
        stored: "var x=undefined;x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=2;",
        stored: "function id(v){return v;}var x=id(undefined);x%=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x%=id(2);",
        stored: "function id(v){return v;}var x=undefined;x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x%=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x%='x';",
        stored: "var x=undefined;x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%='x';",
        stored: "function id(v){return v;}var x=id(undefined);x%='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x%=id('x');",
        stored: "function id(v){return v;}var x=undefined;x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x%=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x%=true;",
        stored: "var x=undefined;x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=true;",
        stored: "function id(v){return v;}var x=id(undefined);x%=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x%=id(true);",
        stored: "function id(v){return v;}var x=undefined;x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x%=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x%=null;",
        stored: "var x=undefined;x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=null;",
        stored: "function id(v){return v;}var x=id(undefined);x%=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x%=id(null);",
        stored: "function id(v){return v;}var x=undefined;x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x%=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x%=undefined;",
        stored: "var x=undefined;x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x%=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x%=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x%=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x%=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x<<=2;",
        stored: "var x=2;x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4620693217682128896,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4620693217682128896,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=2;",
        stored: "function id(v){return v;}var x=id(2);x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4620693217682128896,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4620693217682128896,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x<<=id(2);",
        stored: "function id(v){return v;}var x=2;x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4620693217682128896,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4620693217682128896,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=id(2);",
        stored: "function id(v){return v;}var x=id(2);x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4620693217682128896,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4620693217682128896,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x<<='x';",
        stored: "var x=2;x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<='x';",
        stored: "function id(v){return v;}var x=id(2);x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x<<=id('x');",
        stored: "function id(v){return v;}var x=2;x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=id('x');",
        stored: "function id(v){return v;}var x=id(2);x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x<<=true;",
        stored: "var x=2;x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=true;",
        stored: "function id(v){return v;}var x=id(2);x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x<<=id(true);",
        stored: "function id(v){return v;}var x=2;x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=id(true);",
        stored: "function id(v){return v;}var x=id(2);x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x<<=null;",
        stored: "var x=2;x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=null;",
        stored: "function id(v){return v;}var x=id(2);x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x<<=id(null);",
        stored: "function id(v){return v;}var x=2;x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=id(null);",
        stored: "function id(v){return v;}var x=id(2);x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x<<=undefined;",
        stored: "var x=2;x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=undefined;",
        stored: "function id(v){return v;}var x=id(2);x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x<<=id(undefined);",
        stored: "function id(v){return v;}var x=2;x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x<<=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x<<=2;",
        stored: "var x='x';x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=2;",
        stored: "function id(v){return v;}var x=id('x');x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x<<=id(2);",
        stored: "function id(v){return v;}var x='x';x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=id(2);",
        stored: "function id(v){return v;}var x=id('x');x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x<<='x';",
        stored: "var x='x';x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<='x';",
        stored: "function id(v){return v;}var x=id('x');x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x<<=id('x');",
        stored: "function id(v){return v;}var x='x';x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=id('x');",
        stored: "function id(v){return v;}var x=id('x');x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x<<=true;",
        stored: "var x='x';x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=true;",
        stored: "function id(v){return v;}var x=id('x');x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x<<=id(true);",
        stored: "function id(v){return v;}var x='x';x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=id(true);",
        stored: "function id(v){return v;}var x=id('x');x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x<<=null;",
        stored: "var x='x';x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=null;",
        stored: "function id(v){return v;}var x=id('x');x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x<<=id(null);",
        stored: "function id(v){return v;}var x='x';x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=id(null);",
        stored: "function id(v){return v;}var x=id('x');x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x<<=undefined;",
        stored: "var x='x';x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=undefined;",
        stored: "function id(v){return v;}var x=id('x');x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x<<=id(undefined);",
        stored: "function id(v){return v;}var x='x';x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x<<=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x<<=2;",
        stored: "var x=true;x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=2;",
        stored: "function id(v){return v;}var x=id(true);x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x<<=id(2);",
        stored: "function id(v){return v;}var x=true;x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=id(2);",
        stored: "function id(v){return v;}var x=id(true);x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4616189618054758400,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x<<='x';",
        stored: "var x=true;x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<='x';",
        stored: "function id(v){return v;}var x=id(true);x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x<<=id('x');",
        stored: "function id(v){return v;}var x=true;x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=id('x');",
        stored: "function id(v){return v;}var x=id(true);x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x<<=true;",
        stored: "var x=true;x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=true;",
        stored: "function id(v){return v;}var x=id(true);x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x<<=id(true);",
        stored: "function id(v){return v;}var x=true;x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=id(true);",
        stored: "function id(v){return v;}var x=id(true);x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x<<=null;",
        stored: "var x=true;x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=null;",
        stored: "function id(v){return v;}var x=id(true);x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x<<=id(null);",
        stored: "function id(v){return v;}var x=true;x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=id(null);",
        stored: "function id(v){return v;}var x=id(true);x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x<<=undefined;",
        stored: "var x=true;x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=undefined;",
        stored: "function id(v){return v;}var x=id(true);x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x<<=id(undefined);",
        stored: "function id(v){return v;}var x=true;x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x<<=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x<<=2;",
        stored: "var x=null;x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=2;",
        stored: "function id(v){return v;}var x=id(null);x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x<<=id(2);",
        stored: "function id(v){return v;}var x=null;x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=id(2);",
        stored: "function id(v){return v;}var x=id(null);x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x<<='x';",
        stored: "var x=null;x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<='x';",
        stored: "function id(v){return v;}var x=id(null);x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x<<=id('x');",
        stored: "function id(v){return v;}var x=null;x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=id('x');",
        stored: "function id(v){return v;}var x=id(null);x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x<<=true;",
        stored: "var x=null;x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=true;",
        stored: "function id(v){return v;}var x=id(null);x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x<<=id(true);",
        stored: "function id(v){return v;}var x=null;x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=id(true);",
        stored: "function id(v){return v;}var x=id(null);x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x<<=null;",
        stored: "var x=null;x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=null;",
        stored: "function id(v){return v;}var x=id(null);x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x<<=id(null);",
        stored: "function id(v){return v;}var x=null;x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=id(null);",
        stored: "function id(v){return v;}var x=id(null);x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x<<=undefined;",
        stored: "var x=null;x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=undefined;",
        stored: "function id(v){return v;}var x=id(null);x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x<<=id(undefined);",
        stored: "function id(v){return v;}var x=null;x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x<<=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x<<=2;",
        stored: "var x=undefined;x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=2;",
        stored: "function id(v){return v;}var x=id(undefined);x<<=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x<<=id(2);",
        stored: "function id(v){return v;}var x=undefined;x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x<<=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x<<='x';",
        stored: "var x=undefined;x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<='x';",
        stored: "function id(v){return v;}var x=id(undefined);x<<='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x<<=id('x');",
        stored: "function id(v){return v;}var x=undefined;x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x<<=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x<<=true;",
        stored: "var x=undefined;x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=true;",
        stored: "function id(v){return v;}var x=id(undefined);x<<=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x<<=id(true);",
        stored: "function id(v){return v;}var x=undefined;x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x<<=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x<<=null;",
        stored: "var x=undefined;x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=null;",
        stored: "function id(v){return v;}var x=id(undefined);x<<=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x<<=id(null);",
        stored: "function id(v){return v;}var x=undefined;x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x<<=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x<<=undefined;",
        stored: "var x=undefined;x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x<<=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x<<=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x<<=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x<<=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>=2;",
        stored: "var x=2;x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=2;",
        stored: "function id(v){return v;}var x=id(2);x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>=id(2);",
        stored: "function id(v){return v;}var x=2;x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=id(2);",
        stored: "function id(v){return v;}var x=id(2);x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>='x';",
        stored: "var x=2;x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>='x';",
        stored: "function id(v){return v;}var x=id(2);x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>=id('x');",
        stored: "function id(v){return v;}var x=2;x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=id('x');",
        stored: "function id(v){return v;}var x=id(2);x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>=true;",
        stored: "var x=2;x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=true;",
        stored: "function id(v){return v;}var x=id(2);x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>=id(true);",
        stored: "function id(v){return v;}var x=2;x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=id(true);",
        stored: "function id(v){return v;}var x=id(2);x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>=null;",
        stored: "var x=2;x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=null;",
        stored: "function id(v){return v;}var x=id(2);x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>=id(null);",
        stored: "function id(v){return v;}var x=2;x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=id(null);",
        stored: "function id(v){return v;}var x=id(2);x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>=undefined;",
        stored: "var x=2;x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=undefined;",
        stored: "function id(v){return v;}var x=id(2);x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>=id(undefined);",
        stored: "function id(v){return v;}var x=2;x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>=2;",
        stored: "var x='x';x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=2;",
        stored: "function id(v){return v;}var x=id('x');x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>=id(2);",
        stored: "function id(v){return v;}var x='x';x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=id(2);",
        stored: "function id(v){return v;}var x=id('x');x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>='x';",
        stored: "var x='x';x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>='x';",
        stored: "function id(v){return v;}var x=id('x');x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>=id('x');",
        stored: "function id(v){return v;}var x='x';x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=id('x');",
        stored: "function id(v){return v;}var x=id('x');x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>=true;",
        stored: "var x='x';x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=true;",
        stored: "function id(v){return v;}var x=id('x');x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>=id(true);",
        stored: "function id(v){return v;}var x='x';x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=id(true);",
        stored: "function id(v){return v;}var x=id('x');x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>=null;",
        stored: "var x='x';x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=null;",
        stored: "function id(v){return v;}var x=id('x');x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>=id(null);",
        stored: "function id(v){return v;}var x='x';x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=id(null);",
        stored: "function id(v){return v;}var x=id('x');x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>=undefined;",
        stored: "var x='x';x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=undefined;",
        stored: "function id(v){return v;}var x=id('x');x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>=id(undefined);",
        stored: "function id(v){return v;}var x='x';x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>=2;",
        stored: "var x=true;x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=2;",
        stored: "function id(v){return v;}var x=id(true);x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>=id(2);",
        stored: "function id(v){return v;}var x=true;x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=id(2);",
        stored: "function id(v){return v;}var x=id(true);x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>='x';",
        stored: "var x=true;x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>='x';",
        stored: "function id(v){return v;}var x=id(true);x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>=id('x');",
        stored: "function id(v){return v;}var x=true;x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=id('x');",
        stored: "function id(v){return v;}var x=id(true);x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>=true;",
        stored: "var x=true;x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=true;",
        stored: "function id(v){return v;}var x=id(true);x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>=id(true);",
        stored: "function id(v){return v;}var x=true;x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=id(true);",
        stored: "function id(v){return v;}var x=id(true);x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>=null;",
        stored: "var x=true;x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=null;",
        stored: "function id(v){return v;}var x=id(true);x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>=id(null);",
        stored: "function id(v){return v;}var x=true;x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=id(null);",
        stored: "function id(v){return v;}var x=id(true);x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>=undefined;",
        stored: "var x=true;x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=undefined;",
        stored: "function id(v){return v;}var x=id(true);x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>=id(undefined);",
        stored: "function id(v){return v;}var x=true;x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>=2;",
        stored: "var x=null;x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=2;",
        stored: "function id(v){return v;}var x=id(null);x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>=id(2);",
        stored: "function id(v){return v;}var x=null;x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=id(2);",
        stored: "function id(v){return v;}var x=id(null);x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>='x';",
        stored: "var x=null;x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>='x';",
        stored: "function id(v){return v;}var x=id(null);x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>=id('x');",
        stored: "function id(v){return v;}var x=null;x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=id('x');",
        stored: "function id(v){return v;}var x=id(null);x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>=true;",
        stored: "var x=null;x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=true;",
        stored: "function id(v){return v;}var x=id(null);x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>=id(true);",
        stored: "function id(v){return v;}var x=null;x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=id(true);",
        stored: "function id(v){return v;}var x=id(null);x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>=null;",
        stored: "var x=null;x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=null;",
        stored: "function id(v){return v;}var x=id(null);x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>=id(null);",
        stored: "function id(v){return v;}var x=null;x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=id(null);",
        stored: "function id(v){return v;}var x=id(null);x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>=undefined;",
        stored: "var x=null;x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=undefined;",
        stored: "function id(v){return v;}var x=id(null);x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>=id(undefined);",
        stored: "function id(v){return v;}var x=null;x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>=2;",
        stored: "var x=undefined;x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=2;",
        stored: "function id(v){return v;}var x=id(undefined);x>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>=id(2);",
        stored: "function id(v){return v;}var x=undefined;x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>='x';",
        stored: "var x=undefined;x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>='x';",
        stored: "function id(v){return v;}var x=id(undefined);x>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>=id('x');",
        stored: "function id(v){return v;}var x=undefined;x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>=true;",
        stored: "var x=undefined;x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=true;",
        stored: "function id(v){return v;}var x=id(undefined);x>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>=id(true);",
        stored: "function id(v){return v;}var x=undefined;x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>=null;",
        stored: "var x=undefined;x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=null;",
        stored: "function id(v){return v;}var x=id(undefined);x>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>=id(null);",
        stored: "function id(v){return v;}var x=undefined;x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>=undefined;",
        stored: "var x=undefined;x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>>=2;",
        stored: "var x=2;x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=2;",
        stored: "function id(v){return v;}var x=id(2);x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>>=id(2);",
        stored: "function id(v){return v;}var x=2;x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=id(2);",
        stored: "function id(v){return v;}var x=id(2);x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>>='x';",
        stored: "var x=2;x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>='x';",
        stored: "function id(v){return v;}var x=id(2);x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>>=id('x');",
        stored: "function id(v){return v;}var x=2;x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=id('x');",
        stored: "function id(v){return v;}var x=id(2);x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>>=true;",
        stored: "var x=2;x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=true;",
        stored: "function id(v){return v;}var x=id(2);x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>>=id(true);",
        stored: "function id(v){return v;}var x=2;x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=id(true);",
        stored: "function id(v){return v;}var x=id(2);x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>>=null;",
        stored: "var x=2;x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=null;",
        stored: "function id(v){return v;}var x=id(2);x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>>=id(null);",
        stored: "function id(v){return v;}var x=2;x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=id(null);",
        stored: "function id(v){return v;}var x=id(2);x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x>>>=undefined;",
        stored: "var x=2;x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=undefined;",
        stored: "function id(v){return v;}var x=id(2);x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=2;x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>>=2;",
        stored: "var x='x';x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=2;",
        stored: "function id(v){return v;}var x=id('x');x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>>=id(2);",
        stored: "function id(v){return v;}var x='x';x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=id(2);",
        stored: "function id(v){return v;}var x=id('x');x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>>='x';",
        stored: "var x='x';x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>='x';",
        stored: "function id(v){return v;}var x=id('x');x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>>=id('x');",
        stored: "function id(v){return v;}var x='x';x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=id('x');",
        stored: "function id(v){return v;}var x=id('x');x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>>=true;",
        stored: "var x='x';x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=true;",
        stored: "function id(v){return v;}var x=id('x');x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>>=id(true);",
        stored: "function id(v){return v;}var x='x';x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=id(true);",
        stored: "function id(v){return v;}var x=id('x');x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>>=null;",
        stored: "var x='x';x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=null;",
        stored: "function id(v){return v;}var x=id('x');x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>>=id(null);",
        stored: "function id(v){return v;}var x='x';x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=id(null);",
        stored: "function id(v){return v;}var x=id('x');x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x>>>=undefined;",
        stored: "var x='x';x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=undefined;",
        stored: "function id(v){return v;}var x=id('x');x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x>>>=id(undefined);",
        stored: "function id(v){return v;}var x='x';x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>>=2;",
        stored: "var x=true;x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=2;",
        stored: "function id(v){return v;}var x=id(true);x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>>=id(2);",
        stored: "function id(v){return v;}var x=true;x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=id(2);",
        stored: "function id(v){return v;}var x=id(true);x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>>='x';",
        stored: "var x=true;x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>='x';",
        stored: "function id(v){return v;}var x=id(true);x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>>=id('x');",
        stored: "function id(v){return v;}var x=true;x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=id('x');",
        stored: "function id(v){return v;}var x=id(true);x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>>=true;",
        stored: "var x=true;x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=true;",
        stored: "function id(v){return v;}var x=id(true);x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>>=id(true);",
        stored: "function id(v){return v;}var x=true;x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=id(true);",
        stored: "function id(v){return v;}var x=id(true);x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>>=null;",
        stored: "var x=true;x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=null;",
        stored: "function id(v){return v;}var x=id(true);x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>>=id(null);",
        stored: "function id(v){return v;}var x=true;x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=id(null);",
        stored: "function id(v){return v;}var x=id(true);x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x>>>=undefined;",
        stored: "var x=true;x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=undefined;",
        stored: "function id(v){return v;}var x=id(true);x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=true;x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>>=2;",
        stored: "var x=null;x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=2;",
        stored: "function id(v){return v;}var x=id(null);x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>>=id(2);",
        stored: "function id(v){return v;}var x=null;x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=id(2);",
        stored: "function id(v){return v;}var x=id(null);x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>>='x';",
        stored: "var x=null;x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>='x';",
        stored: "function id(v){return v;}var x=id(null);x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>>=id('x');",
        stored: "function id(v){return v;}var x=null;x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=id('x');",
        stored: "function id(v){return v;}var x=id(null);x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>>=true;",
        stored: "var x=null;x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=true;",
        stored: "function id(v){return v;}var x=id(null);x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>>=id(true);",
        stored: "function id(v){return v;}var x=null;x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=id(true);",
        stored: "function id(v){return v;}var x=id(null);x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>>=null;",
        stored: "var x=null;x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=null;",
        stored: "function id(v){return v;}var x=id(null);x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>>=id(null);",
        stored: "function id(v){return v;}var x=null;x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=id(null);",
        stored: "function id(v){return v;}var x=id(null);x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x>>>=undefined;",
        stored: "var x=null;x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=undefined;",
        stored: "function id(v){return v;}var x=id(null);x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=null;x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>>=2;",
        stored: "var x=undefined;x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=2;",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>>=id(2);",
        stored: "function id(v){return v;}var x=undefined;x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>>='x';",
        stored: "var x=undefined;x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>='x';",
        stored: "function id(v){return v;}var x=id(undefined);x>>>='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>>=id('x');",
        stored: "function id(v){return v;}var x=undefined;x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>>=true;",
        stored: "var x=undefined;x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=true;",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>>=id(true);",
        stored: "function id(v){return v;}var x=undefined;x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>>=null;",
        stored: "var x=undefined;x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=null;",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>>=id(null);",
        stored: "function id(v){return v;}var x=undefined;x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x>>>=undefined;",
        stored: "var x=undefined;x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x>>>=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x>>>=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x&=2;",
        stored: "var x=2;x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=2;",
        stored: "function id(v){return v;}var x=id(2);x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x&=id(2);",
        stored: "function id(v){return v;}var x=2;x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=id(2);",
        stored: "function id(v){return v;}var x=id(2);x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x&='x';",
        stored: "var x=2;x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&='x';",
        stored: "function id(v){return v;}var x=id(2);x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x&=id('x');",
        stored: "function id(v){return v;}var x=2;x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=id('x');",
        stored: "function id(v){return v;}var x=id(2);x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x&=true;",
        stored: "var x=2;x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=true;",
        stored: "function id(v){return v;}var x=id(2);x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x&=id(true);",
        stored: "function id(v){return v;}var x=2;x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=id(true);",
        stored: "function id(v){return v;}var x=id(2);x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x&=null;",
        stored: "var x=2;x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=null;",
        stored: "function id(v){return v;}var x=id(2);x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x&=id(null);",
        stored: "function id(v){return v;}var x=2;x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=id(null);",
        stored: "function id(v){return v;}var x=id(2);x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x&=undefined;",
        stored: "var x=2;x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=undefined;",
        stored: "function id(v){return v;}var x=id(2);x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x&=id(undefined);",
        stored: "function id(v){return v;}var x=2;x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x&=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x&=2;",
        stored: "var x='x';x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=2;",
        stored: "function id(v){return v;}var x=id('x');x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x&=id(2);",
        stored: "function id(v){return v;}var x='x';x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=id(2);",
        stored: "function id(v){return v;}var x=id('x');x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x&='x';",
        stored: "var x='x';x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&='x';",
        stored: "function id(v){return v;}var x=id('x');x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x&=id('x');",
        stored: "function id(v){return v;}var x='x';x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=id('x');",
        stored: "function id(v){return v;}var x=id('x');x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x&=true;",
        stored: "var x='x';x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=true;",
        stored: "function id(v){return v;}var x=id('x');x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x&=id(true);",
        stored: "function id(v){return v;}var x='x';x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=id(true);",
        stored: "function id(v){return v;}var x=id('x');x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x&=null;",
        stored: "var x='x';x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=null;",
        stored: "function id(v){return v;}var x=id('x');x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x&=id(null);",
        stored: "function id(v){return v;}var x='x';x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=id(null);",
        stored: "function id(v){return v;}var x=id('x');x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x&=undefined;",
        stored: "var x='x';x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=undefined;",
        stored: "function id(v){return v;}var x=id('x');x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x&=id(undefined);",
        stored: "function id(v){return v;}var x='x';x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x&=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x&=2;",
        stored: "var x=true;x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=2;",
        stored: "function id(v){return v;}var x=id(true);x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x&=id(2);",
        stored: "function id(v){return v;}var x=true;x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=id(2);",
        stored: "function id(v){return v;}var x=id(true);x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x&='x';",
        stored: "var x=true;x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&='x';",
        stored: "function id(v){return v;}var x=id(true);x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x&=id('x');",
        stored: "function id(v){return v;}var x=true;x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=id('x');",
        stored: "function id(v){return v;}var x=id(true);x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x&=true;",
        stored: "var x=true;x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=true;",
        stored: "function id(v){return v;}var x=id(true);x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x&=id(true);",
        stored: "function id(v){return v;}var x=true;x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=id(true);",
        stored: "function id(v){return v;}var x=id(true);x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x&=null;",
        stored: "var x=true;x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=null;",
        stored: "function id(v){return v;}var x=id(true);x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x&=id(null);",
        stored: "function id(v){return v;}var x=true;x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=id(null);",
        stored: "function id(v){return v;}var x=id(true);x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x&=undefined;",
        stored: "var x=true;x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=undefined;",
        stored: "function id(v){return v;}var x=id(true);x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x&=id(undefined);",
        stored: "function id(v){return v;}var x=true;x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x&=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x&=2;",
        stored: "var x=null;x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=2;",
        stored: "function id(v){return v;}var x=id(null);x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x&=id(2);",
        stored: "function id(v){return v;}var x=null;x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=id(2);",
        stored: "function id(v){return v;}var x=id(null);x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x&='x';",
        stored: "var x=null;x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&='x';",
        stored: "function id(v){return v;}var x=id(null);x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x&=id('x');",
        stored: "function id(v){return v;}var x=null;x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=id('x');",
        stored: "function id(v){return v;}var x=id(null);x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x&=true;",
        stored: "var x=null;x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=true;",
        stored: "function id(v){return v;}var x=id(null);x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x&=id(true);",
        stored: "function id(v){return v;}var x=null;x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=id(true);",
        stored: "function id(v){return v;}var x=id(null);x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x&=null;",
        stored: "var x=null;x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=null;",
        stored: "function id(v){return v;}var x=id(null);x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x&=id(null);",
        stored: "function id(v){return v;}var x=null;x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=id(null);",
        stored: "function id(v){return v;}var x=id(null);x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x&=undefined;",
        stored: "var x=null;x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=undefined;",
        stored: "function id(v){return v;}var x=id(null);x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x&=id(undefined);",
        stored: "function id(v){return v;}var x=null;x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x&=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x&=2;",
        stored: "var x=undefined;x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=2;",
        stored: "function id(v){return v;}var x=id(undefined);x&=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x&=id(2);",
        stored: "function id(v){return v;}var x=undefined;x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x&=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x&='x';",
        stored: "var x=undefined;x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&='x';",
        stored: "function id(v){return v;}var x=id(undefined);x&='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x&=id('x');",
        stored: "function id(v){return v;}var x=undefined;x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x&=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x&=true;",
        stored: "var x=undefined;x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=true;",
        stored: "function id(v){return v;}var x=id(undefined);x&=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x&=id(true);",
        stored: "function id(v){return v;}var x=undefined;x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x&=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x&=null;",
        stored: "var x=undefined;x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=null;",
        stored: "function id(v){return v;}var x=id(undefined);x&=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x&=id(null);",
        stored: "function id(v){return v;}var x=undefined;x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x&=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x&=undefined;",
        stored: "var x=undefined;x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x&=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x&=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x&=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x&=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x|=2;",
        stored: "var x=2;x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=2;",
        stored: "function id(v){return v;}var x=id(2);x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x|=id(2);",
        stored: "function id(v){return v;}var x=2;x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=id(2);",
        stored: "function id(v){return v;}var x=id(2);x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x|='x';",
        stored: "var x=2;x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|='x';",
        stored: "function id(v){return v;}var x=id(2);x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x|=id('x');",
        stored: "function id(v){return v;}var x=2;x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=id('x');",
        stored: "function id(v){return v;}var x=id(2);x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x|=true;",
        stored: "var x=2;x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=true;",
        stored: "function id(v){return v;}var x=id(2);x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x|=id(true);",
        stored: "function id(v){return v;}var x=2;x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=id(true);",
        stored: "function id(v){return v;}var x=id(2);x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x|=null;",
        stored: "var x=2;x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=null;",
        stored: "function id(v){return v;}var x=id(2);x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x|=id(null);",
        stored: "function id(v){return v;}var x=2;x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=id(null);",
        stored: "function id(v){return v;}var x=id(2);x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x|=undefined;",
        stored: "var x=2;x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=undefined;",
        stored: "function id(v){return v;}var x=id(2);x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x|=id(undefined);",
        stored: "function id(v){return v;}var x=2;x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x|=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x|=2;",
        stored: "var x='x';x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=2;",
        stored: "function id(v){return v;}var x=id('x');x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x|=id(2);",
        stored: "function id(v){return v;}var x='x';x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=id(2);",
        stored: "function id(v){return v;}var x=id('x');x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x|='x';",
        stored: "var x='x';x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|='x';",
        stored: "function id(v){return v;}var x=id('x');x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x|=id('x');",
        stored: "function id(v){return v;}var x='x';x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=id('x');",
        stored: "function id(v){return v;}var x=id('x');x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x|=true;",
        stored: "var x='x';x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=true;",
        stored: "function id(v){return v;}var x=id('x');x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x|=id(true);",
        stored: "function id(v){return v;}var x='x';x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=id(true);",
        stored: "function id(v){return v;}var x=id('x');x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x|=null;",
        stored: "var x='x';x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=null;",
        stored: "function id(v){return v;}var x=id('x');x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x|=id(null);",
        stored: "function id(v){return v;}var x='x';x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=id(null);",
        stored: "function id(v){return v;}var x=id('x');x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x|=undefined;",
        stored: "var x='x';x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=undefined;",
        stored: "function id(v){return v;}var x=id('x');x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x|=id(undefined);",
        stored: "function id(v){return v;}var x='x';x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x|=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x|=2;",
        stored: "var x=true;x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=2;",
        stored: "function id(v){return v;}var x=id(true);x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x|=id(2);",
        stored: "function id(v){return v;}var x=true;x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=id(2);",
        stored: "function id(v){return v;}var x=id(true);x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x|='x';",
        stored: "var x=true;x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|='x';",
        stored: "function id(v){return v;}var x=id(true);x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x|=id('x');",
        stored: "function id(v){return v;}var x=true;x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=id('x');",
        stored: "function id(v){return v;}var x=id(true);x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x|=true;",
        stored: "var x=true;x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=true;",
        stored: "function id(v){return v;}var x=id(true);x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x|=id(true);",
        stored: "function id(v){return v;}var x=true;x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=id(true);",
        stored: "function id(v){return v;}var x=id(true);x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x|=null;",
        stored: "var x=true;x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=null;",
        stored: "function id(v){return v;}var x=id(true);x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x|=id(null);",
        stored: "function id(v){return v;}var x=true;x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=id(null);",
        stored: "function id(v){return v;}var x=id(true);x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x|=undefined;",
        stored: "var x=true;x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=undefined;",
        stored: "function id(v){return v;}var x=id(true);x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x|=id(undefined);",
        stored: "function id(v){return v;}var x=true;x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x|=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x|=2;",
        stored: "var x=null;x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=2;",
        stored: "function id(v){return v;}var x=id(null);x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x|=id(2);",
        stored: "function id(v){return v;}var x=null;x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=id(2);",
        stored: "function id(v){return v;}var x=id(null);x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x|='x';",
        stored: "var x=null;x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|='x';",
        stored: "function id(v){return v;}var x=id(null);x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x|=id('x');",
        stored: "function id(v){return v;}var x=null;x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=id('x');",
        stored: "function id(v){return v;}var x=id(null);x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x|=true;",
        stored: "var x=null;x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=true;",
        stored: "function id(v){return v;}var x=id(null);x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x|=id(true);",
        stored: "function id(v){return v;}var x=null;x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=id(true);",
        stored: "function id(v){return v;}var x=id(null);x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x|=null;",
        stored: "var x=null;x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=null;",
        stored: "function id(v){return v;}var x=id(null);x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x|=id(null);",
        stored: "function id(v){return v;}var x=null;x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=id(null);",
        stored: "function id(v){return v;}var x=id(null);x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x|=undefined;",
        stored: "var x=null;x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=undefined;",
        stored: "function id(v){return v;}var x=id(null);x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x|=id(undefined);",
        stored: "function id(v){return v;}var x=null;x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x|=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x|=2;",
        stored: "var x=undefined;x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=2;",
        stored: "function id(v){return v;}var x=id(undefined);x|=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x|=id(2);",
        stored: "function id(v){return v;}var x=undefined;x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x|=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x|='x';",
        stored: "var x=undefined;x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|='x';",
        stored: "function id(v){return v;}var x=id(undefined);x|='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x|=id('x');",
        stored: "function id(v){return v;}var x=undefined;x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x|=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x|=true;",
        stored: "var x=undefined;x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=true;",
        stored: "function id(v){return v;}var x=id(undefined);x|=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x|=id(true);",
        stored: "function id(v){return v;}var x=undefined;x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x|=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x|=null;",
        stored: "var x=undefined;x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=null;",
        stored: "function id(v){return v;}var x=id(undefined);x|=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x|=id(null);",
        stored: "function id(v){return v;}var x=undefined;x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x|=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x|=undefined;",
        stored: "var x=undefined;x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x|=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x|=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x|=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x|=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x^=2;",
        stored: "var x=2;x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=2;",
        stored: "function id(v){return v;}var x=id(2);x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x^=id(2);",
        stored: "function id(v){return v;}var x=2;x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=id(2);",
        stored: "function id(v){return v;}var x=id(2);x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x^='x';",
        stored: "var x=2;x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^='x';",
        stored: "function id(v){return v;}var x=id(2);x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x^=id('x');",
        stored: "function id(v){return v;}var x=2;x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=id('x');",
        stored: "function id(v){return v;}var x=id(2);x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x^=true;",
        stored: "var x=2;x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=true;",
        stored: "function id(v){return v;}var x=id(2);x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x^=id(true);",
        stored: "function id(v){return v;}var x=2;x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=id(true);",
        stored: "function id(v){return v;}var x=id(2);x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x^=null;",
        stored: "var x=2;x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=null;",
        stored: "function id(v){return v;}var x=id(2);x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x^=id(null);",
        stored: "function id(v){return v;}var x=2;x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=id(null);",
        stored: "function id(v){return v;}var x=id(2);x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x^=undefined;",
        stored: "var x=2;x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=undefined;",
        stored: "function id(v){return v;}var x=id(2);x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=2;x^=id(undefined);",
        stored: "function id(v){return v;}var x=2;x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(2);x^=id(undefined);",
        stored: "function id(v){return v;}var x=id(2);x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x^=2;",
        stored: "var x='x';x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=2;",
        stored: "function id(v){return v;}var x=id('x');x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x^=id(2);",
        stored: "function id(v){return v;}var x='x';x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=id(2);",
        stored: "function id(v){return v;}var x=id('x');x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x^='x';",
        stored: "var x='x';x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^='x';",
        stored: "function id(v){return v;}var x=id('x');x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x^=id('x');",
        stored: "function id(v){return v;}var x='x';x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=id('x');",
        stored: "function id(v){return v;}var x=id('x');x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x^=true;",
        stored: "var x='x';x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=true;",
        stored: "function id(v){return v;}var x=id('x');x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x^=id(true);",
        stored: "function id(v){return v;}var x='x';x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=id(true);",
        stored: "function id(v){return v;}var x=id('x');x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x^=null;",
        stored: "var x='x';x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=null;",
        stored: "function id(v){return v;}var x=id('x');x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x^=id(null);",
        stored: "function id(v){return v;}var x='x';x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=id(null);",
        stored: "function id(v){return v;}var x=id('x');x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x^=undefined;",
        stored: "var x='x';x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=undefined;",
        stored: "function id(v){return v;}var x=id('x');x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x='x';x^=id(undefined);",
        stored: "function id(v){return v;}var x='x';x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id('x');x^=id(undefined);",
        stored: "function id(v){return v;}var x=id('x');x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x^=2;",
        stored: "var x=true;x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=2;",
        stored: "function id(v){return v;}var x=id(true);x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x^=id(2);",
        stored: "function id(v){return v;}var x=true;x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=id(2);",
        stored: "function id(v){return v;}var x=id(true);x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x^='x';",
        stored: "var x=true;x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^='x';",
        stored: "function id(v){return v;}var x=id(true);x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x^=id('x');",
        stored: "function id(v){return v;}var x=true;x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=id('x');",
        stored: "function id(v){return v;}var x=id(true);x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x^=true;",
        stored: "var x=true;x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=true;",
        stored: "function id(v){return v;}var x=id(true);x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x^=id(true);",
        stored: "function id(v){return v;}var x=true;x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=id(true);",
        stored: "function id(v){return v;}var x=id(true);x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x^=null;",
        stored: "var x=true;x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=null;",
        stored: "function id(v){return v;}var x=id(true);x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x^=id(null);",
        stored: "function id(v){return v;}var x=true;x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=id(null);",
        stored: "function id(v){return v;}var x=id(true);x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x^=undefined;",
        stored: "var x=true;x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=undefined;",
        stored: "function id(v){return v;}var x=id(true);x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=true;x^=id(undefined);",
        stored: "function id(v){return v;}var x=true;x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(true);x^=id(undefined);",
        stored: "function id(v){return v;}var x=id(true);x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x^=2;",
        stored: "var x=null;x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=2;",
        stored: "function id(v){return v;}var x=id(null);x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x^=id(2);",
        stored: "function id(v){return v;}var x=null;x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=id(2);",
        stored: "function id(v){return v;}var x=id(null);x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x^='x';",
        stored: "var x=null;x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^='x';",
        stored: "function id(v){return v;}var x=id(null);x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x^=id('x');",
        stored: "function id(v){return v;}var x=null;x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=id('x');",
        stored: "function id(v){return v;}var x=id(null);x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x^=true;",
        stored: "var x=null;x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=true;",
        stored: "function id(v){return v;}var x=id(null);x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x^=id(true);",
        stored: "function id(v){return v;}var x=null;x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=id(true);",
        stored: "function id(v){return v;}var x=id(null);x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x^=null;",
        stored: "var x=null;x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=null;",
        stored: "function id(v){return v;}var x=id(null);x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x^=id(null);",
        stored: "function id(v){return v;}var x=null;x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=id(null);",
        stored: "function id(v){return v;}var x=id(null);x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x^=undefined;",
        stored: "var x=null;x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=undefined;",
        stored: "function id(v){return v;}var x=id(null);x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=null;x^=id(undefined);",
        stored: "function id(v){return v;}var x=null;x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(null);x^=id(undefined);",
        stored: "function id(v){return v;}var x=id(null);x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x^=2;",
        stored: "var x=undefined;x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=2;",
        stored: "function id(v){return v;}var x=id(undefined);x^=2;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x^=id(2);",
        stored: "function id(v){return v;}var x=undefined;x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=id(2);",
        stored: "function id(v){return v;}var x=id(undefined);x^=id(2);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x^='x';",
        stored: "var x=undefined;x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^='x';",
        stored: "function id(v){return v;}var x=id(undefined);x^='x';x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x^=id('x');",
        stored: "function id(v){return v;}var x=undefined;x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=id('x');",
        stored: "function id(v){return v;}var x=id(undefined);x^=id('x');x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x^=true;",
        stored: "var x=undefined;x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=true;",
        stored: "function id(v){return v;}var x=id(undefined);x^=true;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x^=id(true);",
        stored: "function id(v){return v;}var x=undefined;x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=id(true);",
        stored: "function id(v){return v;}var x=id(undefined);x^=id(true);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x^=null;",
        stored: "var x=undefined;x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=null;",
        stored: "function id(v){return v;}var x=id(undefined);x^=null;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x^=id(null);",
        stored: "function id(v){return v;}var x=undefined;x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=id(null);",
        stored: "function id(v){return v;}var x=id(undefined);x^=id(null);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x^=undefined;",
        stored: "var x=undefined;x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=undefined;",
        stored: "function id(v){return v;}var x=id(undefined);x^=undefined;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=undefined;x^=id(undefined);",
        stored: "function id(v){return v;}var x=undefined;x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function id(v){return v;}var x=id(undefined);x^=id(undefined);",
        stored: "function id(v){return v;}var x=id(undefined);x^=id(undefined);x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
];
const UPDATE: &[Row] = &[
    Row {
        result: "var x=2;++x;",
        stored: "var x=2;++x;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return ++a;}f(2);",
        stored: "function f(a){++a;return a;}f(2);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var o={a:2};++o.a;",
        stored: "var o={a:2};++o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=2;++isNaN;",
        stored: "function f(){return isNaN;}isNaN=2;++isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var x='x';++x;",
        stored: "var x='x';++x;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return ++a;}f('x');",
        stored: "function f(a){++a;return a;}f('x');",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:'x'};++o.a;",
        stored: "var o={a:'x'};++o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN='x';++isNaN;",
        stored: "function f(){return isNaN;}isNaN='x';++isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;++x;",
        stored: "var x=true;++x;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return ++a;}f(true);",
        stored: "function f(a){++a;return a;}f(true);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var o={a:true};++o.a;",
        stored: "var o={a:true};++o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=true;++isNaN;",
        stored: "function f(){return isNaN;}isNaN=true;++isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=null;++x;",
        stored: "var x=null;++x;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return ++a;}f(null);",
        stored: "function f(a){++a;return a;}f(null);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var o={a:null};++o.a;",
        stored: "var o={a:null};++o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=null;++isNaN;",
        stored: "function f(){return isNaN;}isNaN=null;++isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;++x;",
        stored: "var x=undefined;++x;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return ++a;}f(undefined);",
        stored: "function f(a){++a;return a;}f(undefined);",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:undefined};++o.a;",
        stored: "var o={a:undefined};++o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=undefined;++isNaN;",
        stored: "function f(){return isNaN;}isNaN=undefined;++isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;--x;",
        stored: "var x=2;--x;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return --a;}f(2);",
        stored: "function f(a){--a;return a;}f(2);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var o={a:2};--o.a;",
        stored: "var o={a:2};--o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=2;--isNaN;",
        stored: "function f(){return isNaN;}isNaN=2;--isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x='x';--x;",
        stored: "var x='x';--x;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return --a;}f('x');",
        stored: "function f(a){--a;return a;}f('x');",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:'x'};--o.a;",
        stored: "var o={a:'x'};--o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN='x';--isNaN;",
        stored: "function f(){return isNaN;}isNaN='x';--isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;--x;",
        stored: "var x=true;--x;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return --a;}f(true);",
        stored: "function f(a){--a;return a;}f(true);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:true};--o.a;",
        stored: "var o={a:true};--o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=true;--isNaN;",
        stored: "function f(){return isNaN;}isNaN=true;--isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;--x;",
        stored: "var x=null;--x;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return --a;}f(null);",
        stored: "function f(a){--a;return a;}f(null);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "var o={a:null};--o.a;",
        stored: "var o={a:null};--o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=null;--isNaN;",
        stored: "function f(){return isNaN;}isNaN=null;--isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;--x;",
        stored: "var x=undefined;--x;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return --a;}f(undefined);",
        stored: "function f(a){--a;return a;}f(undefined);",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:undefined};--o.a;",
        stored: "var o={a:undefined};--o.a;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=undefined;--isNaN;",
        stored: "function f(){return isNaN;}isNaN=undefined;--isNaN;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x++;",
        stored: "var x=2;x++;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a++;}f(2);",
        stored: "function f(a){a++;return a;}f(2);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var o={a:2};o.a++;",
        stored: "var o={a:2};o.a++;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=2;isNaN++;",
        stored: "function f(){return isNaN;}isNaN=2;isNaN++;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4613937818241073152,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x++;",
        stored: "var x='x';x++;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a++;}f('x');",
        stored: "function f(a){a++;return a;}f('x');",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:'x'};o.a++;",
        stored: "var o={a:'x'};o.a++;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN='x';isNaN++;",
        stored: "function f(){return isNaN;}isNaN='x';isNaN++;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x++;",
        stored: "var x=true;x++;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a++;}f(true);",
        stored: "function f(a){a++;return a;}f(true);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var o={a:true};o.a++;",
        stored: "var o={a:true};o.a++;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=true;isNaN++;",
        stored: "function f(){return isNaN;}isNaN=true;isNaN++;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x++;",
        stored: "var x=null;x++;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a++;}f(null);",
        stored: "function f(a){a++;return a;}f(null);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var o={a:null};o.a++;",
        stored: "var o={a:null};o.a++;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=null;isNaN++;",
        stored: "function f(){return isNaN;}isNaN=null;isNaN++;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x++;",
        stored: "var x=undefined;x++;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a++;}f(undefined);",
        stored: "function f(a){a++;return a;}f(undefined);",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:undefined};o.a++;",
        stored: "var o={a:undefined};o.a++;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=undefined;isNaN++;",
        stored: "function f(){return isNaN;}isNaN=undefined;isNaN++;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=2;x--;",
        stored: "var x=2;x--;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a--;}f(2);",
        stored: "function f(a){a--;return a;}f(2);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var o={a:2};o.a--;",
        stored: "var o={a:2};o.a--;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=2;isNaN--;",
        stored: "function f(){return isNaN;}isNaN=2;isNaN--;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4611686018427387904,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
    },
    Row {
        result: "var x='x';x--;",
        stored: "var x='x';x--;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a--;}f('x');",
        stored: "function f(a){a--;return a;}f('x');",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:'x'};o.a--;",
        stored: "var o={a:'x'};o.a--;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN='x';isNaN--;",
        stored: "function f(){return isNaN;}isNaN='x';isNaN--;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=true;x--;",
        stored: "var x=true;x--;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a--;}f(true);",
        stored: "function f(a){a--;return a;}f(true);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:true};o.a--;",
        stored: "var o={a:true};o.a--;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=true;isNaN--;",
        stored: "function f(){return isNaN;}isNaN=true;isNaN--;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 4607182418800017408,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var x=null;x--;",
        stored: "var x=null;x--;x;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a--;}f(null);",
        stored: "function f(a){a--;return a;}f(null);",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "var o={a:null};o.a--;",
        stored: "var o={a:null};o.a--;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=null;isNaN--;",
        stored: "function f(){return isNaN;}isNaN=null;isNaN--;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: false,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: false,
            bits: 13830554455654793216,
            units: U0,
        },
    },
    Row {
        result: "var x=undefined;x--;",
        stored: "var x=undefined;x--;x;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(a){return a--;}f(undefined);",
        stored: "function f(a){a--;return a;}f(undefined);",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "var o={a:undefined};o.a--;",
        stored: "var o={a:undefined};o.a--;o.a;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
    Row {
        result: "function f(){return isNaN;}isNaN=undefined;isNaN--;",
        stored: "function f(){return isNaN;}isNaN=undefined;isNaN--;isNaN;",
        result_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
        stored_ex: Ex {
            kind: 0,
            nan: true,
            bits: 0,
            units: U0,
        },
    },
];
fn reset(machine: &mut vm::Vm) {
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.reset_for_reuse().unwrap();
    if machine.scope_reset_pending() {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            machine.poll_reset_for_reuse(&mut cx),
            std::task::Poll::Ready(Ok(()))
        ));
    }
    assert!(!machine.scope_reset_pending());
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(!machine.capture_certification_stats().unwrap().valid);
    assert!(pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine).is_none());
}

fn probe_vm(source: &str, observer: &'static dyn vm::SourcePlugin, id: &'static str) -> vm::Vm {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(id)),
        vm::CompileSourceFileOptions::new()
            .with_host_api_catalog(catalog.clone())
            .with_source_plugin(observer),
    )
    .unwrap_or_else(|error| panic!("source={source:?} guest_invocations=0 {error:?}"));
    let mut machine = issued.into_vm().unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .unwrap();
    let mut registry = vm::HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
    }
    for name in [
        "math::floor",
        "math::abs",
        "math::acos",
        "math::asin",
        "math::atan",
        "math::atan2",
        "math::ceil",
        "math::cos",
        "math::exp",
        "math::ln",
        "math::sin",
        "math::tan",
        "math::trunc",
        "math::is_infinite",
        "math::copysign",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
        "bytes::from_utf8",
    ] {
        registry.allow_builtin(name).unwrap();
    }
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(10_000_000);
    machine
}

fn node_value(source: &str) -> serde_json::Value {
    let script = format!(
        "const v=(0,eval)({});if(typeof v==='number'){{const b=Buffer.alloc(8);b.writeDoubleBE(v);console.log(JSON.stringify({{kind:'number',nan:Number.isNaN(v),bits:b.toString('hex')}}));}}else if(typeof v==='string'){{console.log(JSON.stringify({{kind:'string',units:Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))}}));}}else{{console.log(JSON.stringify({{kind:typeof v}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "{source:?} {output:?}"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn expect_matches_node(source: &str, expected: &Ex) {
    let actual = node_value(source);
    if expected.kind == 0 {
        assert_eq!(actual["kind"], "number", "{source}");
        if expected.nan {
            assert_eq!(actual["nan"], true, "{source}");
        } else {
            assert_eq!(actual["nan"], false, "{source}");
            assert_eq!(
                actual["bits"],
                format!("{:016x}", expected.bits),
                "{source}"
            );
        }
    } else {
        assert_eq!(actual["kind"], "string", "{source}");
        assert_eq!(
            actual["units"],
            serde_json::json!(expected.units),
            "{source}"
        );
    }
}

fn run_number(source: &str, expected: &Ex) {
    let source = source.to_owned();
    let nan = expected.nan;
    let bits = expected.bits;
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!("ASSIGN149_COMPILE source={source:?} {error:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            machine.set_fuel(10_000_000);
            let status = machine.run();
            assert_eq!(status.unwrap(), vm::VmStatus::Halted, "{source}");
            let vm::Value::Map(boxed) = machine.stack().last().unwrap() else {
                panic!("Number box {source}")
            };
            assert_eq!(
                boxed.get(&vm::Value::string("tag")),
                Some(&vm::Value::Int(3)),
                "{source}"
            );
            let vm::Value::Float(actual) = boxed.get(&vm::Value::string("number")).unwrap() else {
                panic!("Float {source}")
            };
            assert!(
                if nan {
                    actual.is_nan()
                } else {
                    actual.to_bits() == bits
                },
                "{source}: actual={actual:?} bits={bits:016x}"
            );
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            assert!(machine.capture_certification_stats().unwrap().valid);
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

fn run_string(source: &str, units: &[u16]) {
    let source = source.to_owned();
    let units = units.to_vec();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let observer = Box::leak(Box::new(object_observer::Observer { units: Some(units.len()), ..Default::default() }));
        let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
        assert!(!machine.jit_config().enabled);
        assert_eq!((machine.aot_exec_count(), machine.jit_native_exec_count()), (0, 0));
        let status = machine.run();
        assert_eq!(status.unwrap(), vm::VmStatus::Halted, "{source}");
        let stack = machine.stack();
        let at = stack.iter().position(|v| v == &vm::Value::string("P7_OBSERVED")).unwrap();
        let vm::Value::Map(boxed) = &stack[at + 1] else { panic!("string box {source}") };
        assert_eq!(boxed.get(&vm::Value::string("tag")), Some(&vm::Value::Int(4)), "{source}");
        assert!(matches!(boxed.get(&vm::Value::string("generation")), Some(vm::Value::Int(n)) if *n > 0));
        assert!(matches!(boxed.get(&vm::Value::string("ref_slot")), Some(vm::Value::Int(n)) if *n > 0));
        assert_eq!(stack[at + 2], vm::Value::Float(units.len() as f64), "{source}");
        for (i, unit) in units.iter().enumerate() {
            assert_eq!(stack[at + 3 + i], vm::Value::Float(f64::from(*unit)), "{source} unit {i}");
        }
        assert_eq!((machine.aot_exec_count(), machine.jit_native_exec_count()), (0, 0));
        reset(&mut machine);
    }).unwrap().join().unwrap();
}

fn run_ex(source: &str, expected: &Ex, node: bool) {
    if node {
        expect_matches_node(source, expected);
    }
    if expected.kind == 0 {
        run_number(source, expected);
    } else {
        run_string(source, expected.units);
    }
}

fn run_row(row: &Row, node: bool) {
    run_ex(row.result, &row.result_ex, node);
    run_ex(row.stored, &row.stored_ex, node);
}

fn shard(table: &'static [Row], which: &str) {
    let spec = std::env::var("ASSIGN149_SHARD")
        .unwrap_or_else(|_| panic!("ASSIGN149_SHARD required for {which}"));
    let (kind, start) = spec.split_once(':').unwrap();
    assert_eq!(kind, which);
    let start: usize = start.parse().unwrap();
    assert!(start.is_multiple_of(8));
    let end = (start + 8).min(table.len());
    assert!(start < table.len());
    for (offset, row) in table[start..end].iter().enumerate() {
        eprintln!(
            "ASSIGN149_ROW {{\"family\":\"{which}\",\"index\":{},\"result\":{},\"stored\":{}}}",
            start + offset,
            serde_json::to_string(row.result).unwrap(),
            serde_json::to_string(row.stored).unwrap()
        );
        run_row(row, true);
    }
}

fn parse(source: &str) -> Result<vm::FrontendIr, vm::ParseError> {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    vm::SourcePlugin::parse_source(
        &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
        source,
        &vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        },
    )
}

fn helper_count(source: &str, name: &str) -> usize {
    let ir = parse(source).unwrap_or_else(|error| panic!("{source}: {error:?}"));
    ir.functions.iter().filter(|f| f.name == name).count()
}

#[test]
fn assignment149_public_compound_local() {
    run_number(
        "var x=1;x+=2;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
    run_number(
        "var x=1;x+=2;x;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
    expect_matches_node(
        "var x=1;x+=2;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
    expect_matches_node(
        "var x=1;x+=2;x;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
}

#[test]
fn assignment149_public_prefix_update() {
    run_number(
        "var x=1;++x;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 2.0f64.to_bits(),
            units: &[],
        },
    );
    run_number(
        "var x=1;++x;x;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 2.0f64.to_bits(),
            units: &[],
        },
    );
    expect_matches_node(
        "var x=1;++x;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 2.0f64.to_bits(),
            units: &[],
        },
    );
}

#[test]
fn assignment149_public_property_place() {
    run_number(
        "var o={a:1};o.a+=2;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
    run_number(
        "var o={a:1};o.a+=2;o.a;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
    expect_matches_node(
        "var o={a:1};o.a+=2;o.a;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
}

#[test]
fn assignment149_string_units() {
    let rows = [
        ("var x=\"a\";x+=\"b\";", &[97u16, 98][..]),
        ("var x=\"a\";x+=\"b\";x;", &[97, 98][..]),
        ("var x=1;x+=\"2\";x;", &[49, 50][..]),
        ("var x=\"1\";x+=2;x;", &[49, 50][..]),
        ("var x=true;x+=\"!\";x;", &[116, 114, 117, 101, 33][..]),
        ("var x=null;x+=\"x\";x;", &[110, 117, 108, 108, 120][..]),
        (
            "function id(v){return v;}var y=id(\"a\");y+=id(\"b\");y;",
            &[97, 98][..],
        ),
        ("var o={a:\"a\"};o.a+=\"b\";o.a;", &[97, 98][..]),
    ];
    for (source, units) in rows {
        let expected = Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units,
        };
        expect_matches_node(source, &expected);
        run_string(source, units);
    }
}

#[test]
fn assignment149_matrix_compound() {
    assert_eq!(COMPOUND.len(), 1100);
    shard(COMPOUND, "c");
}

#[test]
fn assignment149_matrix_update() {
    assert_eq!(UPDATE.len(), 80);
    shard(UPDATE, "u");
}

#[test]
fn assignment149_ieee_shift_float() {
    let rows = [
        ("var x=0;x+=-0;x;", 0.0f64),
        ("var x=-0;x++;", -0.0),
        ("var x=-0;x++;x;", 1.0),
        ("var x=1;x/=2;x;", 0.5),
        ("var x=8;x>>=1;x;", 4.0),
        ("var x=-1;x>>>=1;x;", 2147483647.0),
        ("var x=1;x<<=31;x;", -2147483648.0),
        ("var x=NaN;++x;x;", f64::NAN),
    ];
    assert!(rows.len() <= 8);
    for (source, expected) in rows {
        let expected: f64 = expected;
        let ex = Ex {
            kind: 0,
            nan: expected.is_nan(),
            bits: expected.to_bits(),
            units: &[],
        };
        expect_matches_node(source, &ex);
        run_number(source, &ex);
    }
}

#[test]
fn assignment149_order_rebind_throw() {
    let rows = [
        (
            "var n=0;var o={get a(){throw 'G';}};try{o.a+=(n=9,2);}catch(e){if(e!=='G')throw 'bad';}n;",
            0.0,
        ),
        (
            "var o={a:1};var p={a:9};var r=o;r.a+=(r=p,2);o.a*10+p.a;",
            39.0,
        ),
        ("var x=1;var r=(x+=(x=10,2));r*100+x;", 303.0),
        (
            "var n=0;var o={get a(){return 2;}};var r=(o.a+=(n=3,4));if(r!==6)throw 'bad';n;",
            3.0,
        ),
        (
            "var n=0;var o={get a(){return 1;},set a(v){throw 'P';}};try{o.a+=(n=7,3);}catch(e){if(e!=='P')throw 'bad';}n;",
            7.0,
        ),
        (
            "var o={a:1};var p={get b(){throw 'R';}};try{o.a+=p.b;}catch(e){if(e!=='R')throw 'bad';}o.a;",
            1.0,
        ),
        (
            "function id(v){return v;}var n=0;var x=id(1);var r=(x+=(n=7,2));r*100+x*10+n;",
            337.0,
        ),
        (
            "function id(v){return v;}var o={get b(){throw 'R';}};var x=id({});try{x+=o.b;}catch(e){if(e!=='R')throw e;}1;",
            1.0,
        ),
    ];
    for (source, expected) in rows {
        let expected: f64 = expected;
        let ex = Ex {
            kind: 0,
            nan: false,
            bits: expected.to_bits(),
            units: &[],
        };
        expect_matches_node(source, &ex);
        run_number(source, &ex);
    }
}

#[test]
fn assignment149_put_strict_and_fatal() {
    run_number(
        "var o={};Object.defineProperty(o,'x',{value:1,writable:false});var r=(o.x+=2);r;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
    run_number(
        "var o={};Object.defineProperty(o,'x',{value:1,writable:false});o.x+=2;o.x;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 1.0f64.to_bits(),
            units: &[],
        },
    );
    expect_matches_node(
        "var o={};Object.defineProperty(o,'x',{value:1,writable:false});var r=(o.x+=2);r;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 3.0f64.to_bits(),
            units: &[],
        },
    );
    expect_matches_node(
        "var o={};Object.defineProperty(o,'x',{value:1,writable:false});o.x+=2;o.x;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 1.0f64.to_bits(),
            units: &[],
        },
    );
    run_number(
        "'use strict';var o={};Object.defineProperty(o,'x',{value:1,writable:false});var n=0;try{o.x+=2;}catch(e){n=1;}if(o.x!==1)throw 'store';n;",
        &Ex {
            kind: 0,
            nan: false,
            bits: 1.0f64.to_bits(),
            units: &[],
        },
    );
    for source in [
        "function id(v){return v;}var x=id({});x+=1;",
        "function id(v){return v;}var x=1;x+=id({});",
        "function id(v){return v;}var x=id([]);x*=2;",
        "function id(v){return v;}var x=id(function(){});x++;",
    ] {
        let source = source.to_owned();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut machine =
                    pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                        .unwrap();
                machine.set_jit_config(vm::JitConfig {
                    enabled: false,
                    ..*machine.jit_config()
                });
                assert!(!machine.jit_config().enabled);
                assert_eq!(
                    (machine.aot_exec_count(), machine.jit_native_exec_count()),
                    (0, 0)
                );
                machine.set_fuel(10_000_000);
                let status = machine.run();
                assert!(
                    matches!(&status, Err(vm::VmError::HostError(e)) if e == "assertion failed"),
                    "{source} {status:?}"
                );
                assert_eq!(
                    (machine.aot_exec_count(), machine.jit_native_exec_count()),
                    (0, 0)
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[test]
fn assignment149_finally_loop_return_owners() {
    let rows = [
        (
            "function f(){var x=1;try{return x++;}finally{x=x+10;}}f();",
            1.0,
        ),
        (
            "function f(){var x=1;try{return ++x;}finally{x=x+10;}}f();",
            2.0,
        ),
        ("var x=0;while(x<2){x++;continue;}x;", 2.0),
        (
            "function f(){var x=1;for(var i=0;i<2;i++){x+=i;}return x;}f();",
            2.0,
        ),
        ("try{throw 1;}catch(e){e+=2;e;}", 3.0),
        ("function f(a){a+=2;return a;}f(1);", 3.0),
        ("function f(){return isNaN;}isNaN=2;++isNaN;isNaN;", 3.0),
        ("var a=[1];a[0]+=2;a[0];", 3.0),
    ];
    for (source, expected) in rows {
        let expected: f64 = expected;
        let ex = Ex {
            kind: 0,
            nan: expected.is_nan(),
            bits: expected.to_bits(),
            units: &[],
        };
        expect_matches_node(source, &ex);
        run_number(source, &ex);
    }
}

#[test]
fn assignment149_demand_refusal_unicode3_float() {
    assert!(parse("var x=1;x&&=2;").is_err());
    assert!(parse("var x={};x+=1;").is_err());
    assert!(parse("var a=[1];a[0]+='x';").is_err());
    assert!(parse("var x=1;x+=2;").is_ok());
    assert_eq!(
        helper_count("var x=1;x+=2;", "__compat_es5_string_concat"),
        0
    );
    assert_eq!(
        helper_count("var x=1;x+=2;", "__compat_es5_number_string_managed"),
        0
    );
    assert_eq!(
        helper_count("var x='a';x+='b';", "__compat_es5_string_concat"),
        1
    );
    assert_eq!(
        helper_count("var x=1;x+='a';", "__compat_es5_number_string_managed"),
        1
    );
    assert_eq!(
        helper_count(
            "var x=true;x+='!';",
            "__compat_es5_fixed_scalar_string_managed"
        ),
        1
    );
    assert_eq!(
        helper_count(
            "function f(a){return ++a;}f(1);",
            "__compat_es5_numeric_scan"
        ),
        1
    );
    assert_eq!(
        helper_count("var x=1;x++;", "__compat_es5_string_concat"),
        0
    );
    let es5 = [
        ("var x='\\u200b';x*=1;x;", 0.0),
        ("var x='\\u200b';++x;x;", 1.0),
        ("var x='\\u200b2';x*=1;x;", 2.0),
    ];
    for (source, expected) in es5 {
        let expected: f64 = expected;
        let node = node_value(source);
        assert_eq!(node["kind"], "number", "{source}");
        assert_eq!(
            node["nan"], true,
            "modern Node must differ: {source} {node}"
        );
        run_number(
            source,
            &Ex {
                kind: 0,
                nan: false,
                bits: expected.to_bits(),
                units: &[],
            },
        );
        eprintln!("ASSIGN149_UNICODE3 source={source:?} es51={expected} node=NaN");
    }
    run_string(
        "var x='A\\0\\ud800';x+='B\\0\\udc00';x;",
        &[65, 0, 0xd800, 66, 0, 0xdc00],
    );
    expect_matches_node(
        "var x='A\\0\\ud800';x+='B\\0\\udc00';x;",
        &Ex {
            kind: 1,
            nan: false,
            bits: 0,
            units: &[65, 0, 0xd800, 66, 0, 0xdc00],
        },
    );
}
