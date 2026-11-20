//! Locked LuaJIT 2.1 Lua 5.2-compat syntax types.
//!
//! Spans are byte offsets into the original source, including a leading BOM
//! when one is present. Names and string values keep raw bytes so a non-UTF-8
//! identifier or `\xFF` literal is representable.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: u32::try_from(start).unwrap_or(u32::MAX),
            end: u32::try_from(end).unwrap_or(u32::MAX),
        }
    }

    pub fn cover(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub span: Span,
    pub bytes: Vec<u8>,
}

impl Name {
    pub fn is(&self, text: &str) -> bool {
        self.bytes == text.as_bytes()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub span: Span,
    pub has_bom: bool,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub span: Span,
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfArm {
    pub cond: Expr,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalName {
    pub name: Name,
    pub const_binding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stmt {
    Empty {
        span: Span,
    },
    Do {
        span: Span,
        body: Block,
    },
    While {
        span: Span,
        cond: Expr,
        body: Block,
    },
    Repeat {
        span: Span,
        body: Block,
        until: Expr,
    },
    If {
        span: Span,
        arms: Vec<IfArm>,
        else_body: Option<Block>,
    },
    NumericFor {
        span: Span,
        name: Name,
        start: Expr,
        limit: Expr,
        step: Option<Expr>,
        body: Block,
    },
    GenericFor {
        span: Span,
        names: Vec<Name>,
        iters: ExprList,
        body: Block,
    },
    Local {
        span: Span,
        names: Vec<LocalName>,
        values: ExprList,
    },
    LocalFunction {
        span: Span,
        name: Name,
        const_binding: bool,
        func: FuncBody,
    },
    Function {
        span: Span,
        name: FuncName,
        body: FuncBody,
    },
    Assign {
        span: Span,
        targets: Vec<AssignTarget>,
        values: ExprList,
    },
    CompoundAssign {
        span: Span,
        target: AssignTarget,
        op: BinOp,
        value: Expr,
    },
    Call {
        span: Span,
        call: Expr,
    },
    Label {
        span: Span,
        name: Name,
    },
    Goto {
        span: Span,
        name: Name,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    Return {
        span: Span,
        values: ExprList,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuncName {
    pub span: Span,
    pub segments: Vec<Name>,
    pub method: Option<Name>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuncBody {
    pub span: Span,
    pub params: Vec<Name>,
    pub vararg: bool,
    pub self_param: bool,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortBody {
    Expr(Box<Expr>),
    Block(Block),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExprList {
    pub span: Span,
    pub exprs: Vec<Expr>,
}

impl ExprList {
    pub fn empty(span: Span) -> Self {
        Self {
            span,
            exprs: Vec::new(),
        }
    }

    pub fn tail_multi(&self) -> bool {
        self.exprs.last().is_some_and(Expr::is_multi)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssignTarget {
    Name(Name),
    Index {
        span: Span,
        table: Box<Expr>,
        key: Box<Expr>,
        nav: bool,
    },
    Field {
        span: Span,
        table: Box<Expr>,
        name: Name,
        nav: bool,
    },
}

impl AssignTarget {
    pub fn span(&self) -> Span {
        match self {
            Self::Name(name) => name.span,
            Self::Index { span, .. } | Self::Field { span, .. } => *span,
        }
    }

    pub fn has_nav(&self) -> bool {
        match self {
            Self::Name(_) => false,
            Self::Index { nav, .. } | Self::Field { nav, .. } => *nav,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberKind {
    Integer,
    Float,
    HexInteger,
    HexFloat,
    Binary,
    Signed64,
    Unsigned64,
    Imaginary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Len,
    BNot,
    Bang,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Band,
    Bor,
    BXor,
    Shl,
    Shr,
    Sar,
    Concat,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Coal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Args {
    List(ExprList),
    String { span: Span, bytes: Vec<u8> },
    Table { span: Span, fields: Vec<Field> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Index { span: Span, key: Expr, value: Expr },
    Name { span: Span, name: Name, value: Expr },
    Item { span: Span, value: Expr },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Nil {
        span: Span,
    },
    Bool {
        span: Span,
        value: bool,
    },
    Number {
        span: Span,
        raw: String,
        kind: NumberKind,
    },
    String {
        span: Span,
        bytes: Vec<u8>,
    },
    Vararg {
        span: Span,
    },
    Name {
        span: Span,
        name: Name,
    },
    Paren {
        span: Span,
        inner: Box<Expr>,
    },
    Unary {
        span: Span,
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        span: Span,
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Ternary {
        span: Span,
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Call {
        span: Span,
        callee: Box<Expr>,
        args: Args,
        nav: bool,
    },
    MethodCall {
        span: Span,
        receiver: Box<Expr>,
        method: Name,
        args: Args,
        receiver_nav: bool,
        method_nav: bool,
    },
    Index {
        span: Span,
        table: Box<Expr>,
        key: Box<Expr>,
        nav: bool,
    },
    Field {
        span: Span,
        table: Box<Expr>,
        name: Name,
        nav: bool,
    },
    Function {
        span: Span,
        body: FuncBody,
    },
    ShortFunction {
        span: Span,
        params: Vec<Name>,
        vararg: bool,
        body: Box<ShortBody>,
    },
    Table {
        span: Span,
        fields: Vec<Field>,
    },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Self::Nil { span }
            | Self::Bool { span, .. }
            | Self::Number { span, .. }
            | Self::String { span, .. }
            | Self::Vararg { span }
            | Self::Name { span, .. }
            | Self::Paren { span, .. }
            | Self::Unary { span, .. }
            | Self::Binary { span, .. }
            | Self::Ternary { span, .. }
            | Self::Call { span, .. }
            | Self::MethodCall { span, .. }
            | Self::Index { span, .. }
            | Self::Field { span, .. }
            | Self::Function { span, .. }
            | Self::ShortFunction { span, .. }
            | Self::Table { span, .. } => *span,
        }
    }

    pub fn is_multi(&self) -> bool {
        matches!(
            self,
            Self::Call { .. } | Self::MethodCall { .. } | Self::Vararg { .. }
        )
    }
}
