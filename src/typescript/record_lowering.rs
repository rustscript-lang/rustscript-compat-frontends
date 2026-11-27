//! Private homogeneous numeric-record carrier lowering.
//!
//! A row whose resolved body is a non-empty object of *required numeric*
//! fields has no guest-level write channel: an ordinary record is a plain
//! map value, so `active[key()] = value` could only rewrite the receiver
//! slot and leave every alias on a stale copy. This module mints one
//! private nominal per canonical row whose physical value is a record of
//! four real guest closures over one shared `map<unknown>` cell:
//!
//! ```text
//! struct __CompatNumRec_<Row>(__Row) {
//!   read:      (key: string) => number;
//!   read_any:  (key: string) => unknown;
//!   write:     (key: string, value: number) => number;
//!   snapshot:  () => map<unknown>;
//! }
//! ```
//!
//! The cell is the *physical* row, never a `Map<number>` forgery: the
//! constructed row map is stored as-is, so legal extra properties survive
//! and a dynamic read keeps its own value domain. All four closures
//! capture the same root cell slot (`capture_copies`), so a `Set` written
//! back into the captured cell is observed by every alias, and rebinding
//! a binding replaces only that binding's carrier. There is no numeric
//! registry and no slot-keyed state anywhere in this lowering.
//!
//! Only existing Core constructs are used: `TypeSchema::Named` nominals,
//! `Map`/`Callable` schemas, `Expr::Closure` with `capture_copies`,
//! `MapNew`/`Get`/`Set` builtins and `Expr::LocalCall`. No new value
//! variant, schema, opcode, ABI or runtime descriptor is introduced, and
//! a carrier never claims to *be* the logical row: the logical row is
//! carried only as the nominal's single type argument, for hint
//! projection and identity mismatch.

use std::collections::{BTreeMap, HashMap, HashSet};

use swc_common::{Span, Spanned};
use swc_ecma_ast as ast;
use vm::compiler::ir::{
    AssignmentKind, ClosureExpr, GuestTypeProvenance, ParsedLexicalScope, StructDecl,
    StructDeclOrigin, StructDeclSite,
};
use vm::{Expr, ParseError, Stmt, TypeSchema};

use super::parser::{Lowerer, byte_span, line_at, unsupported};

/// Hygienic nominal prefix for one row's private carrier record.
pub(crate) const PREFIX: &str = "__CompatNumRec_";
/// The carrier's single unused type parameter (the logical row is passed
/// as the nominal's argument, not bound in the body).
pub(crate) const ROW_PARAM: &str = "__Row";

/// Binding name used for the captured cell while a closure context is
/// pushed. The binding is restored before the closure is assembled, so it
/// never leaks into the enclosing lexical environment.
const CAPTURE_NAME: &str = "__compat_numrec_cell";

/// One qualifying logical row and the private carrier minted for it.
#[derive(Clone, Debug)]
pub(crate) struct RowCarrier {
    /// The logical row schema (`Named(canonical)`), preserved as the
    /// carrier nominal's argument.
    pub(crate) logical: TypeSchema,
    /// Required numeric fields of the logical row, in name order.
    pub(crate) fields: BTreeMap<String, TypeSchema>,
    /// Private nominal of the physical carrier record.
    pub(crate) nominal: String,
}

fn callable(params: Vec<TypeSchema>, result: TypeSchema) -> TypeSchema {
    TypeSchema::Callable {
        params,
        result: Box::new(result),
    }
}

impl RowCarrier {
    /// The physical carrier schema handed to slots, parameters, returns
    /// and guest call arguments.
    pub(crate) fn schema(&self) -> TypeSchema {
        TypeSchema::Named(self.nominal.clone(), vec![self.logical.clone()])
    }

    pub(crate) fn read_schema(&self) -> TypeSchema {
        callable(vec![TypeSchema::String], TypeSchema::Number)
    }

    pub(crate) fn read_any_schema(&self) -> TypeSchema {
        callable(vec![TypeSchema::String], TypeSchema::Number)
    }

    pub(crate) fn write_schema(&self) -> TypeSchema {
        callable(
            vec![TypeSchema::String, TypeSchema::Number],
            TypeSchema::Number,
        )
    }

    pub(crate) fn snapshot_schema(&self) -> TypeSchema {
        callable(Vec::new(), TypeSchema::Map(Box::new(TypeSchema::Unknown)))
    }

    /// Declared body of the private carrier record.
    pub(crate) fn body_schema(&self) -> TypeSchema {
        let mut fields = HashMap::new();
        fields.insert("read".to_owned(), self.read_schema());
        fields.insert("read_any".to_owned(), self.read_any_schema());
        fields.insert("write".to_owned(), self.write_schema());
        fields.insert("snapshot".to_owned(), self.snapshot_schema());
        TypeSchema::Object(fields)
    }
}

/// True when `name` is a private numeric-record carrier nominal.
pub(crate) fn is_carrier_name(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// True when `schema` is exactly a private numeric-record carrier: a
/// carrier nominal with its one logical-row argument. Arbitrary `Named`
/// schemas, `Map`, resources and object types never match.
pub(crate) fn is_carrier_schema(schema: &TypeSchema) -> bool {
    matches!(
        schema,
        TypeSchema::Named(name, arguments) if is_carrier_name(name) && arguments.len() == 1
    )
}

/// The logical row a carrier schema was minted from.
pub(crate) fn logical_of(schema: &TypeSchema) -> Option<&TypeSchema> {
    match schema {
        TypeSchema::Named(name, arguments) if is_carrier_name(name) && arguments.len() == 1 => {
            arguments.first()
        }
        _ => None,
    }
}

/// Qualify one schema as a homogeneous required-numeric row.
///
/// The schema must resolve (through `struct_schemas`,
/// `named_type_declarations`, alias schemas and `named_types`) to a
/// non-empty `Object` whose every field is numeric and required. Optional,
/// mixed, nested, array, callable, resource and generic rows never
/// qualify, so their lowering is untouched.
pub(crate) fn qualify(lowerer: &Lowerer<'_>, schema: &TypeSchema) -> Option<RowCarrier> {
    let (canonical, body) = resolve_row(lowerer, schema, 0)?;
    let TypeSchema::Object(fields) = body else {
        return None;
    };
    if fields.is_empty() || !fields.values().all(Lowerer::is_numeric_schema) {
        return None;
    }
    Some(RowCarrier {
        logical: TypeSchema::Named(canonical.clone(), Vec::new()),
        fields: fields.into_iter().collect(),
        nominal: format!("{PREFIX}{canonical}"),
    })
}

/// Resolve a named row to the canonical declaration name and its body.
/// Only the outermost spelling is normalized, so `Row` and an alias
/// `type Rows = Row` mint the same private nominal.
fn resolve_row(
    lowerer: &Lowerer<'_>,
    schema: &TypeSchema,
    depth: usize,
) -> Option<(String, TypeSchema)> {
    if depth > 32 {
        return None;
    }
    let TypeSchema::Named(name, arguments) = schema else {
        return None;
    };
    if is_carrier_name(name) {
        // A carrier is never a logical row: qualifying one would wrap the
        // physical record in a second carrier.
        return None;
    }
    let declaration = lowerer
        .struct_schemas
        .get(name)
        .or_else(|| lowerer.named_type_declarations.get(name));
    if let Some(declaration) = declaration {
        // Generic rows stay on their existing path.
        if !declaration.type_params.is_empty() || !arguments.is_empty() {
            return None;
        }
        return match &declaration.body_schema {
            TypeSchema::Object(_) => Some((name.clone(), declaration.body_schema.clone())),
            body => resolve_row(lowerer, body, depth + 1),
        };
    }
    if let Some((parameters, body)) = lowerer.type_alias_schemas.get(name) {
        if !parameters.is_empty() || !arguments.is_empty() {
            return None;
        }
        return match body {
            TypeSchema::Object(_) => Some((name.clone(), body.clone())),
            body => resolve_row(lowerer, body, depth + 1),
        };
    }
    let expanded = lowerer.named_types.get(name)?;
    if expanded == schema {
        return None;
    }
    resolve_row(lowerer, expanded, depth + 1)
}

impl<'a> Lowerer<'a> {
    /// Register the private carrier record for `row` once and return its
    /// schema. Re-registration is a no-op; a user declaration that already
    /// owns the hygienic name fails with the ordinary duplicate diagnostic.
    pub(crate) fn ensure_record_carrier(
        &mut self,
        row: &RowCarrier,
        span: Span,
    ) -> Result<TypeSchema, ParseError> {
        if self.record_carriers.contains_key(&row.nominal) {
            return Ok(row.schema());
        }
        let reserved = &row.nominal;
        if self.struct_schemas.contains_key(reserved)
            || self.named_types.contains_key(reserved)
            || self.named_type_declarations.contains_key(reserved)
            || self.function_indices.contains_key(reserved)
            || self.imported_functions.contains_key(reserved)
            || self.host_function_indices.contains_key(reserved)
            || self.locals.contains_key(reserved)
        {
            return Err(ParseError::at_span(
                byte_span(span, self.base, self.source_id),
                format!("duplicate TypeScript type '{reserved}'"),
            ));
        }
        let declaration = StructDecl {
            name: row.nominal.clone(),
            type_params: vec![ROW_PARAM.to_owned()],
            body_schema: row.body_schema(),
            origin: StructDeclOrigin::Guest,
        };
        self.named_type_declarations
            .insert(row.nominal.clone(), declaration.clone());
        self.struct_schemas.insert(row.nominal.clone(), declaration);
        let declaration_span = byte_span(span, self.base, self.source_id);
        self.type_provenance.push(GuestTypeProvenance {
            canonical_name: row.nominal.clone(),
            source_name: match &row.logical {
                TypeSchema::Named(name, _) => name.clone(),
                _ => row.nominal.clone(),
            },
            source_id: self.source_id,
            declaration_span: Some(declaration_span),
        });
        let site_id = self.alloc_node_id();
        self.struct_decl_sites.push(StructDeclSite {
            id: site_id,
            ident_span: declaration_span,
            decl_span: declaration_span,
            name: row.nominal.clone(),
            scope_id: 0,
        });
        self.record_carriers.insert(row.nominal.clone(), row.clone());
        Ok(row.schema())
    }

    /// The row a schema is the carrier of, reconstructed from the nominal
    /// when this unit has not registered it yet (predeclared signatures).
    pub(crate) fn record_carrier_of_schema(&self, schema: &TypeSchema) -> Option<RowCarrier> {
        let TypeSchema::Named(nominal, _) = schema else {
            return None;
        };
        if let Some(row) = self.record_carriers.get(nominal) {
            return Some(row.clone());
        }
        let row = qualify(self, logical_of(schema)?)?;
        (row.nominal == *nominal).then_some(row)
    }

    /// The carrier schema for an annotation or slot schema: the physical
    /// carrier when the schema qualifies as a row, otherwise the schema
    /// itself. Registration happens here, before any value is built.
    pub(crate) fn record_carrier_schema(
        &mut self,
        schema: TypeSchema,
        span: Span,
    ) -> Result<TypeSchema, ParseError> {
        match qualify(self, &schema) {
            Some(row) => self.ensure_record_carrier(&row, span),
            None => Ok(schema),
        }
    }

    /// Stage an already-evaluated expression into a hidden local appended to
    /// `stmts`, returning its slot. Callers stage in source order, so each
    /// subexpression runs exactly once and before the next one.
    fn record_stage(
        &mut self,
        stmts: &mut Vec<Stmt>,
        value: Expr,
        declared: Option<TypeSchema>,
        span: Span,
        line: u32,
    ) -> Result<u16, ParseError> {
        let slot = self.alloc_hidden_local(span)?;
        stmts.push(Stmt::Let {
            index: slot,
            declared_schema: declared,
            expr: value,
            line,
        });
        Ok(slot)
    }

    /// Build one carrier closure over the shared cell slot and return its
    /// declared schema plus the closure expression.
    fn record_closure(
        &mut self,
        cell: u16,
        param_schemas: &[TypeSchema],
        result_schema: TypeSchema,
        span: Span,
        build: &dyn Fn(&[u16], u16) -> Expr,
    ) -> Result<(TypeSchema, Expr), ParseError> {
        let closure_scope = self.next_scope_id;
        self.next_scope_id = self
            .next_scope_id
            .checked_add(1)
            .ok_or_else(|| unsupported(span, self.base, self.source_id, "lexical scope count"))?;
        self.closure_captures
            .push(super::parser::ClosureCaptureContext {
                local_slots: HashSet::new(),
                captures: HashMap::new(),
                capture_copies: Vec::new(),
            });
        let mut param_slots = Vec::with_capacity(param_schemas.len());
        for _ in param_schemas {
            param_slots.push(self.alloc_hidden_local(span)?);
        }
        // `capture_slot` resolves the cell through every enclosing capture
        // context, so a carrier built inside a closure captures the correct
        // remapped cell. It also publishes a binding for the name while a
        // context is pushed; restore the previous binding exactly.
        let previous_binding = self.locals.get(CAPTURE_NAME).copied();
        let captured_cell = self.capture_slot(cell, CAPTURE_NAME, span)?;
        match previous_binding {
            Some(slot) => {
                self.locals.insert(CAPTURE_NAME.to_owned(), slot);
            }
            None => {
                self.locals.remove(CAPTURE_NAME);
            }
        }
        let context = self
            .closure_captures
            .pop()
            .expect("the current record closure context was pushed");
        self.function_scopes.push(ParsedLexicalScope {
            id: closure_scope,
            parent: Some(self.scope_id),
            range: byte_span(span, self.base, self.source_id),
            declarations: Vec::new(),
            functions: Vec::new(),
        });
        self.next_scope_id = self.next_scope_id.max(closure_scope + 1);
        let body = build(&param_slots, captured_cell);
        let declared = TypeSchema::Callable {
            params: param_schemas.to_vec(),
            result: Box::new(result_schema),
        };
        Ok((
            declared,
            Expr::Closure(ClosureExpr {
                param_slots,
                capture_copies: context.capture_copies,
                body: Box::new(body),
            }),
        ))
    }

    /// `Get(container, name)` with the name passed through a hidden string
    /// local. Core rewrites a *literal-key* read of a non-copyable field
    /// into a moving `MoveField`, which reaps the key and would lose the
    /// closure for the next read. A dynamic key keeps the plain copying
    /// `Get`, so a carrier record is never consumed by a read.
    fn record_field_get(
        &mut self,
        stmts: &mut Vec<Stmt>,
        container: u16,
        name: &str,
        declared: TypeSchema,
        span: Span,
        line: u32,
    ) -> Result<u16, ParseError> {
        let key_slot = self.record_stage(
            stmts,
            Expr::String(name.to_owned()),
            Some(TypeSchema::String),
            span,
            line,
        )?;
        let get = Self::builtin_call(
            vm::BuiltinFunction::Get,
            vec![Expr::Var(container), Expr::Var(key_slot)],
        );
        self.record_stage(stmts, get, Some(declared), span, line)
    }

    /// The cell read used by `read` and `read_any`: a borrow of the captured
    /// cell (never a move of it) indexed by the closure's key parameter.
    fn record_cell_read(captured_cell: u16, key_slot: u16) -> Expr {
        Self::builtin_call(
            vm::BuiltinFunction::Get,
            vec![
                Expr::Borrow(Box::new(Expr::Var(captured_cell))),
                Expr::Var(key_slot),
            ],
        )
    }

    /// Wrap an already-lowered row value in the private carrier: one shared
    /// `map<unknown>` cell plus the four guest closures over it.
    pub(crate) fn carrier_from_value(
        &mut self,
        value: Expr,
        row: &RowCarrier,
        span: Span,
    ) -> Result<Expr, ParseError> {
        let carrier = self.ensure_record_carrier(row, span)?;
        debug_assert_eq!(carrier, row.schema());
        let line = line_at(self.source, byte_span(span, self.base, self.source_id).lo);
        let mut stmts = Vec::new();
        // The source row map is staged under its logical schema, then taken
        // as the shared cell. The cell is `map<unknown>`: the physical row
        // keeps every property it actually has, so nothing is forged.
        let source_slot = self.record_stage(
            &mut stmts,
            value,
            Some(row.logical.clone()),
            span,
            line,
        )?;
        let cell = self.record_stage(
            &mut stmts,
            Expr::Var(source_slot),
            Some(TypeSchema::Map(Box::new(TypeSchema::Unknown))),
            span,
            line,
        )?;
        let (read_declared, read) = self.record_closure(
            cell,
            &[TypeSchema::String],
            TypeSchema::Number,
            span,
            &|params: &[u16], captured_cell: u16| Self::record_cell_read(captured_cell, params[0]),
        )?;
        let read_slot = self.record_stage(&mut stmts, read, Some(read_declared), span, line)?;
        let (read_any_declared, read_any) = self.record_closure(
            cell,
            &[TypeSchema::String],
            TypeSchema::Number,
            span,
            &|params: &[u16], captured_cell: u16| Self::record_cell_read(captured_cell, params[0]),
        )?;
        let read_any_slot = self.record_stage(
            &mut stmts,
            read_any,
            Some(read_any_declared),
            span,
            line,
        )?;
        // Write durability is `cell = Set(cell, key, value)` back into the
        // captured cell: the write is shared, and the written value is the
        // statement result with no slot re-read anywhere.
        let (write_declared, write) = self.record_closure(
            cell,
            &[TypeSchema::String, TypeSchema::Number],
            TypeSchema::Number,
            span,
            &|params: &[u16], captured_cell: u16| {
                let updated = Self::builtin_call(
                    vm::BuiltinFunction::Set,
                    vec![
                        Expr::Var(captured_cell),
                        Expr::Var(params[0]),
                        Expr::Var(params[1]),
                    ],
                );
                Expr::Block {
                    stmts: vec![Stmt::Assign {
                        kind: AssignmentKind::Set,
                        index: captured_cell,
                        expr: updated,
                        line,
                    }],
                    expr: Box::new(Expr::Var(params[1])),
                }
            },
        )?;
        let write_slot = self.record_stage(&mut stmts, write, Some(write_declared), span, line)?;
        let (snapshot_declared, snapshot) = self.record_closure(
            cell,
            &[],
            TypeSchema::Map(Box::new(TypeSchema::Unknown)),
            span,
            &|_: &[u16], captured_cell: u16| Expr::Borrow(Box::new(Expr::Var(captured_cell))),
        )?;
        let snapshot_slot = self.record_stage(
            &mut stmts,
            snapshot,
            Some(snapshot_declared),
            span,
            line,
        )?;
        let mut carrier_value = Self::builtin_call(vm::BuiltinFunction::MapNew, Vec::new());
        for (name, slot) in [
            ("read", read_slot),
            ("read_any", read_any_slot),
            ("write", write_slot),
            ("snapshot", snapshot_slot),
        ] {
            carrier_value = Self::builtin_call(
                vm::BuiltinFunction::Set,
                vec![
                    carrier_value,
                    Expr::String(name.to_owned()),
                    Expr::Var(slot),
                ],
            );
        }
        Ok(Expr::Block {
            stmts,
            expr: Box::new(carrier_value),
        })
    }

    /// Read one field through the carrier's `read` closure (`typed`) or
    /// `read_any` closure (dynamic key, no result proof). The base is
    /// captured first, so a key with side effects cannot change which
    /// carrier is read.
    pub(crate) fn carrier_read(
        &mut self,
        base: Expr,
        key: Expr,
        typed: bool,
        row: &RowCarrier,
        span: Span,
    ) -> Result<Expr, ParseError> {
        let line = line_at(self.source, byte_span(span, self.base, self.source_id).lo);
        let mut stmts = Vec::new();
        let receiver = self.record_stage(&mut stmts, base, Some(row.schema()), span, line)?;
        let key_slot = self.record_stage(&mut stmts, key, Some(TypeSchema::String), span, line)?;
        let (field, declared) = if typed {
            ("read", row.read_schema())
        } else {
            ("read_any", row.read_any_schema())
        };
        let callee = self.record_field_get(&mut stmts, receiver, field, declared, span, line)?;
        let result = self.record_stage(
            &mut stmts,
            Expr::LocalCall(callee, Vec::new(), vec![Expr::Var(key_slot)], None),
            typed.then_some(TypeSchema::Number),
            span,
            line,
        )?;
        Ok(Expr::Block {
            stmts,
            expr: Box::new(Expr::Var(result)),
        })
    }

    /// Statement computed store: base, then key, then the right-hand side,
    /// then the carrier's `write` closure. No source slot is re-read and no
    /// slot is reassigned, so every alias observes the write.
    pub(crate) fn carrier_write(
        &mut self,
        base: Expr,
        key: Expr,
        value: Expr,
        row: &RowCarrier,
        span: Span,
    ) -> Result<Expr, ParseError> {
        let line = line_at(self.source, byte_span(span, self.base, self.source_id).lo);
        let mut stmts = Vec::new();
        let receiver = self.record_stage(&mut stmts, base, Some(row.schema()), span, line)?;
        let key_slot = self.record_stage(&mut stmts, key, Some(TypeSchema::String), span, line)?;
        let value_slot =
            self.record_stage(&mut stmts, value, Some(TypeSchema::Number), span, line)?;
        let callee = self.record_field_get(
            &mut stmts,
            receiver,
            "write",
            row.write_schema(),
            span,
            line,
        )?;
        let written = self.record_stage(
            &mut stmts,
            Expr::LocalCall(
                callee,
                Vec::new(),
                vec![Expr::Var(key_slot), Expr::Var(value_slot)],
                None,
            ),
            Some(TypeSchema::Number),
            span,
            line,
        )?;
        Ok(Expr::Block {
            stmts,
            expr: Box::new(Expr::Var(written)),
        })
    }

    /// Host-egress adapter: the current physical state as a value snapshot
    /// re-declared under the logical row. Not wired yet — a carrier that
    /// would reach a host call or an export is refused precisely instead
    /// of being passed while claiming the logical map shape.
    #[allow(dead_code)]
    pub(crate) fn carrier_snapshot(
        &mut self,
        base: Expr,
        logical: &TypeSchema,
        row: &RowCarrier,
        span: Span,
    ) -> Result<Expr, ParseError> {
        let line = line_at(self.source, byte_span(span, self.base, self.source_id).lo);
        let mut stmts = Vec::new();
        let receiver = self.record_stage(&mut stmts, base, Some(row.schema()), span, line)?;
        let callee = self.record_field_get(
            &mut stmts,
            receiver,
            "snapshot",
            row.snapshot_schema(),
            span,
            line,
        )?;
        let snapshot = self.record_stage(
            &mut stmts,
            Expr::LocalCall(callee, Vec::new(), Vec::new(), None),
            Some(logical.clone()),
            span,
            line,
        )?;
        Ok(Expr::Block {
            stmts,
            expr: Box::new(Expr::Var(snapshot)),
        })
    }

    /// Lower a returned value against the declared return schema. A
    /// carrier-returning function passes an already-carrier value through
    /// untouched and converts a row literal; any other expression claiming
    /// the row is refused precisely rather than returned as a fresh map
    /// under a carrier schema.
    pub(crate) fn record_return_value(
        &mut self,
        value: &ast::Expr,
        return_schema: Option<&TypeSchema>,
    ) -> Result<Expr, ParseError> {
        let row = return_schema.and_then(|schema| self.record_carrier_of_schema(schema));
        let Some(row) = row else {
            // The declared schema is not the carrier: an exported signature
            // (and any other logical row) keeps the guest-visible shape, so
            // a tail return of an internal carrier would be published under
            // a declaration that cannot honour it. Snapshotting or
            // converting here would invent a host bridge, so refuse
            // precisely instead.
            if self
                .expression_schema(value)
                .as_ref()
                .is_some_and(is_carrier_schema)
            {
                return Err(unsupported(
                    value.span(),
                    self.base,
                    self.source_id,
                    "record carrier return value",
                ));
            }
            return self.lower_expr(value.clone());
        };
        let already = self
            .expression_schema(value)
            .as_ref()
            .is_some_and(is_carrier_schema);
        let literal = self.record_row_literal(value, &row);
        let lowered = self.lower_expr(value.clone())?;
        if already {
            return Ok(lowered);
        }
        if literal {
            return self.carrier_from_value(lowered, &row, value.span());
        }
        Err(unsupported(
            value.span(),
            self.base,
            self.source_id,
            "record carrier return value",
        ))
    }

    /// True when an object literal statically provides every required field
    /// of `row` with a numeric value. Extra properties are permitted (the
    /// physical cell keeps them); spreads, computed keys, methods and
    /// non-numeric values never qualify, so no carrier is built over a
    /// shape that would read back as `null`.
    pub(crate) fn record_row_literal(&self, expression: &ast::Expr, row: &RowCarrier) -> bool {
        let mut expression = expression;
        while let ast::Expr::Paren(paren) = expression {
            expression = &paren.expr;
        }
        let ast::Expr::Object(object) = expression else {
            return false;
        };
        let mut provided = HashSet::new();
        for property in &object.props {
            let ast::PropOrSpread::Prop(property) = property else {
                return false;
            };
            let (name, numeric) = match property.as_ref() {
                ast::Prop::KeyValue(property) => {
                    let name = match &property.key {
                        ast::PropName::Ident(name) => name.sym.to_string(),
                        ast::PropName::Str(name) => name.value.to_string_lossy().into_owned(),
                        _ => return false,
                    };
                    let numeric = self
                        .expression_schema(&property.value)
                        .as_ref()
                        .is_some_and(Lowerer::is_numeric_schema);
                    (name, numeric)
                }
                ast::Prop::Shorthand(ident) => {
                    let name = ident.sym.to_string();
                    let numeric = self
                        .local_types
                        .get(&name)
                        .is_some_and(Lowerer::is_numeric_schema);
                    (name, numeric)
                }
                _ => return false,
            };
            if !row.fields.contains_key(&name) {
                // A legal extra property is preserved by the physical cell.
                continue;
            }
            if !numeric {
                return false;
            }
            provided.insert(name);
        }
        row.fields.keys().all(|name| provided.contains(name))
    }

    /// Span of the first `return` (recursing through blocks, `if` branches
    /// and loops) whose value is statically a carrier, if any. Used at
    /// host/export boundaries that keep the logical row: a carrier return
    /// cannot be published there, and the early-return transform must not
    /// stage one behind a logical declaration.
    pub(crate) fn carrier_return_span(&self, stmts: &[ast::Stmt]) -> Option<Span> {
        for stmt in stmts {
            let found = match stmt {
                ast::Stmt::Return(ret) => ret.arg.as_deref().and_then(|value| {
                    self.expression_schema(value)
                        .as_ref()
                        .is_some_and(is_carrier_schema)
                        .then_some(ret.span)
                }),
                ast::Stmt::Block(block) => self.carrier_return_span(&block.stmts),
                ast::Stmt::If(if_stmt) => self
                    .carrier_return_span(std::slice::from_ref(if_stmt.cons.as_ref()))
                    .or_else(|| {
                        if_stmt.alt.as_ref().and_then(|alt| {
                            self.carrier_return_span(std::slice::from_ref(alt.as_ref()))
                        })
                    }),
                ast::Stmt::While(while_stmt) => {
                    self.carrier_return_span(std::slice::from_ref(while_stmt.body.as_ref()))
                }
                ast::Stmt::For(for_stmt) => {
                    self.carrier_return_span(std::slice::from_ref(for_stmt.body.as_ref()))
                }
                _ => None,
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// True when the callable expression carries the record through its
    /// result: either its declared result annotation is (or mints) a
    /// carrier, or one of its returned values already is one. Such a
    /// callable only crosses a guest boundary whose formal is itself a
    /// guest carrier callable.
    fn callable_result_is_carrier(&self, expression: &ast::Expr) -> bool {
        let mut expression = expression;
        while let ast::Expr::Paren(paren) = expression {
            expression = &paren.expr;
        }
        match expression {
            ast::Expr::Arrow(arrow) => {
                self.annotation_is_carrier(arrow.return_type.as_deref())
                    || match arrow.body.as_ref() {
                        ast::ArrowFunctionBody::Expr(body) => self
                            .expression_schema(body)
                            .as_ref()
                            .is_some_and(is_carrier_schema),
                        ast::ArrowFunctionBody::FunctionBody(block) => {
                            self.carrier_return_span(&block.stmts).is_some()
                        }
                    }
            }
            ast::Expr::Fn(function) => {
                self.annotation_is_carrier(function.function.return_type.as_deref())
                    || function
                        .function
                        .body
                        .as_ref()
                        .is_some_and(|block| self.carrier_return_span(&block.stmts).is_some())
            }
            // A binding or a module/imported function is not itself a
            // carrier, but its declared callable schema may carry the
            // record through its result (`const cb = make; host(cb)`).
            // Read it through `expression_schema`, which already resolves
            // locals, so an annotated local is refused exactly like the
            // closure written inline would be.
            ast::Expr::Ident(ident) => {
                let name = ident.sym.as_ref();
                let result = match self.expression_schema(expression) {
                    Some(TypeSchema::Callable { result, .. }) => Some(*result),
                    // Ordinary TypeScript is not ref mode, so
                    // `expression_schema` refuses a plain identifier before
                    // consulting the module function table. A module
                    // function name still carries the record through its
                    // declared result, unless a local binding shadows it.
                    None if !self.locals.contains_key(name) => {
                        self.module_function_result_schema(name)
                    }
                    _ => None,
                };
                result.is_some_and(|result| {
                    is_carrier_schema(&result) || qualify(self, &result).is_some()
                })
            }
            _ => matches!(
                self.expression_schema(expression),
                Some(TypeSchema::Callable { ref result, .. })
                    if is_carrier_schema(result) || qualify(self, result).is_some()
            ),
        }
    }

    /// Declared result schema of a module function by name, readable outside
    /// reference mode where `expression_schema` refuses identifiers. Uses
    /// the lowered declaration once its body is present, else the
    /// predeclared annotation facts; generic functions stay unresolved.
    fn module_function_result_schema(&self, name: &str) -> Option<TypeSchema> {
        let (index, _) = self.function_indices.get(name)?;
        if let Some(declaration) = self.functions.iter().find(|decl| decl.index == *index) {
            if !declaration.type_params.is_empty() {
                return None;
            }
            return declaration.return_schema.clone();
        }
        self.predeclared_signatures
            .get(name)
            .and_then(|(_, result)| result.clone())
    }

    /// True when a return-type annotation is the carrier or a logical row
    /// that would be minted as one. Annotations that fail to lower never
    /// count: their own rejection stays where it is.
    fn annotation_is_carrier(&self, annotation: Option<&ast::TsTypeAnn>) -> bool {
        let Some(annotation) = annotation else {
            return false;
        };
        let Ok(schema) = self.lower_type(&annotation.type_ann) else {
            return false;
        };
        let schema = self.ref_map_number_array(schema, annotation.span);
        is_carrier_schema(&schema) || qualify(self, &schema).is_some()
    }

    /// True when the declared formal is a guest callable whose result is
    /// the carrier, the only shape allowed to receive a carrier-returning
    /// callable across a boundary.
    fn guest_carrier_callable(&self, expected: Option<&TypeSchema>) -> bool {
        let Some(TypeSchema::Callable { result, .. }) = expected else {
            return false;
        };
        is_carrier_schema(result) || qualify(self, result).is_some()
    }

    /// Decide whether one guest call argument must be converted to the
    /// callee's carrier formal.
    ///
    /// * the formal is the carrier and the argument is a row literal →
    ///   convert (`Some(row)`);
    /// * the formal is the carrier and the argument is already a carrier →
    ///   pass through: a guest boundary never copies;
    /// * a carrier argument against any other formal (host call, imported
    ///   function, exported signature, plain callable) is refused precisely
    ///   instead of being passed under a shape the callee cannot honour.
    pub(crate) fn record_argument_carrier(
        &self,
        argument: &ast::Expr,
        expected: Option<&TypeSchema>,
        span: swc_common::Span,
    ) -> Result<Option<RowCarrier>, ParseError> {
        let formal = expected.and_then(|schema| self.record_carrier_of_schema(schema));
        let actual = self.expression_schema(argument);
        if actual
            .as_ref()
            .is_some_and(super::record_lowering::is_carrier_schema)
        {
            return match formal {
                Some(_) => Ok(None),
                None => Err(unsupported(
                    span,
                    self.base,
                    self.source_id,
                    "record carrier argument beyond a guest row boundary",
                )),
            };
        }
        // A callable argument may carry the record through its result even
        // though the closure value itself is not a carrier. Unless the
        // declared formal is itself a guest callable that returns the
        // carrier, the boundary would receive a physical carrier under a
        // logical result declaration; there is no logical callable bridge,
        // so refuse precisely.
        if self.callable_result_is_carrier(argument) && !self.guest_carrier_callable(expected) {
            return Err(unsupported(
                span,
                self.base,
                self.source_id,
                "record carrier argument beyond a guest row boundary",
            ));
        }
        let Some(row) = formal else {
            return Ok(None);
        };
        if self.record_row_literal(argument, &row) {
            Ok(Some(row))
        } else {
            Err(unsupported(
                span,
                self.base,
                self.source_id,
                "record carrier argument",
            ))
        }
    }
}
