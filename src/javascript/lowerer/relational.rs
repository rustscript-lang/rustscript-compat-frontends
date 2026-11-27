//! Sealed primitive relations with checked runtime pair dispatch.
use super::super::prelude::object::{
    Emit, add, and, assign, branch, call, declare, eq, int, lt, not, or, sub, tag_is, var,
};
use super::*;

pub(super) const HELPER: &str = "__compat_es5_string_compare";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::javascript) enum Relation {
    Lt,
    Gt,
    Le,
    Ge,
}
impl Relation {
    pub(in crate::javascript) fn from_op(op: ast::BinaryOp) -> Option<Self> {
        match op {
            ast::BinaryOp::Lt => Some(Self::Lt),
            ast::BinaryOp::Gt => Some(Self::Gt),
            ast::BinaryOp::LtEq => Some(Self::Le),
            ast::BinaryOp::GtEq => Some(Self::Ge),
            _ => None,
        }
    }
    fn conversion_operator(self) -> primitive_number::Operator {
        use primitive_number::Operator;
        match self {
            Self::Lt => Operator::RelationalLt,
            Self::Gt => Operator::RelationalGt,
            Self::Le => Operator::RelationalLe,
            Self::Ge => Operator::RelationalGe,
        }
    }
    // Arguments are already-bound unboxed values. Equality preserves unordered
    // NaN and signed-zero semantics; complementing reversed less would not.
    fn predicate(self, left: Expr, right: Expr) -> Expr {
        match self {
            Self::Lt => lt(left, right),
            Self::Gt => lt(right, left),
            Self::Le => or(lt(left.clone(), right.clone()), eq(left, right)),
            Self::Ge => or(lt(right.clone(), left.clone()), eq(left, right)),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::javascript) enum Mode {
    Numeric,
    StringString,
    Primitive(primitive_number::Mode, primitive_number::Mode),
}
impl Mode {
    // A validation-only loop entry can be more precise than its sealed
    // back-edge mode. This never changes or creates an emission proof.
    fn covers_entry(self, entry: Self) -> bool {
        use primitive_number::Mode::{Dynamic, Number, String};
        let operands = |mode| match mode {
            Self::Numeric => (Number, Number),
            Self::StringString => (String, String),
            Self::Primitive(l, r) => (l, r),
        };
        let (l, r) = operands(self);
        let (el, er) = operands(entry);
        (l == Dynamic || l == el) && (r == Dynamic || r == er)
    }
    fn can_compare_strings(self) -> bool {
        use primitive_number::Mode::{Dynamic, String};
        matches!(
            self,
            Self::StringString | Self::Primitive(String | Dynamic, String | Dynamic)
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Site {
    relation: Relation,
    pub(super) mode: Mode,
}

fn register_site(
    sites: &mut HashMap<(u32, u32), Site>,
    sealed: bool,
    span: (u32, u32),
    site: Site,
) -> Result<(), &'static str> {
    // Even during collection a backedge may not change a previously selected
    // operand mode. An empty sealed registry is just as closed as a full one.
    if sites.get(&span).is_some_and(|old| *old != site)
        || (sealed && sites.get(&span) != Some(&site))
    {
        return Err(
            "Relational operator/mode site was not proved before the runtime index window sealed",
        );
    }
    if !sealed {
        sites.insert(span, site);
    }
    Ok(())
}

impl Lowerer<'_> {
    pub(in crate::javascript) fn record_primitive_relational_pair(
        &self,
        binary: &ast::BinExpr,
        left: Kind,
        right: Kind,
    ) -> Result<Mode, ParseError> {
        use primitive_number::{Mode as Operand, Role};
        // Check the complete pair before publishing either conversion demand.
        let (Some(l), Some(r)) = (Operand::from_kind(left), Operand::from_kind(right)) else {
            return Err(self.error(
                binary.span,
                "Primitive relational numeric conversion excludes nonprimitive operands",
            ));
        };
        let op = Relation::from_op(binary.op)
            .ok_or_else(|| self.error(binary.span, "Unknown relational operator"))?
            .conversion_operator();
        if !self.relational_collection_is_probe() {
            self.record_primitive_number_site(binary.span, op, Role::Left, left)?;
            self.record_primitive_number_site(binary.span, op, Role::Right, right)?;
        }
        Ok(Mode::Primitive(l, r))
    }
    pub(in crate::javascript) fn record_relational_site(
        &self,
        binary: &ast::BinExpr,
        mode: Mode,
    ) -> Result<(), ParseError> {
        let relation = Relation::from_op(binary.op)
            .ok_or_else(|| self.error(binary.span, "Unknown relational operator"))?;
        if self.relational_collection_is_probe() {
            if self.relational_sealed.get() {
                let sites = self.relational_sites.borrow();
                let approved = sites.get(&(binary.span.lo.0, binary.span.hi.0));
                if !approved
                    .is_some_and(|site| site.relation == relation && site.mode.covers_entry(mode))
                {
                    return Err(self.error(
                        binary.span,
                        "Relational entry validation is not covered by its sealed pair",
                    ));
                }
            }
            return Ok(());
        }
        register_site(
            &mut self.relational_sites.borrow_mut(),
            self.relational_sealed.get(),
            (binary.span.lo.0, binary.span.hi.0),
            Site { relation, mode },
        )
        .map_err(|message| self.error(binary.span, message))
    }
    pub(super) fn has_string_relational_sites(&self) -> bool {
        self.relational_sites
            .borrow()
            .values()
            .any(|site| site.mode.can_compare_strings())
    }
    pub(super) fn append_string_compare_helper(
        &mut self,
        ir: &mut FrontendIr,
        carrier_schema: &TypeSchema,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if !self.has_string_relational_sites() {
            return Ok(());
        }
        if !self.relational_sealed.get() {
            return Err(self.error(span, "Relational sites are not sealed"));
        }
        self.next_slot = self.next_slot.max(
            u16::try_from(ir.locals)
                .map_err(|_| self.error(span, "Relational helper local space exhausted"))?,
        );
        let index = heap_emitters::fresh_index(self, ir, span)?;
        let c = self.slot(span)?;
        let left = self.slot(span)?;
        let right = self.slot(span)?;
        let mut e = Emit::new(self, span, c)?;
        let line = e.line;
        let lb = e.integer()?;
        let rb = e.integer()?;
        let ll = e.integer()?;
        let rl = e.integer()?;
        let extent = e.integer()?;
        let cursor = e.integer()?;
        let lu = e.integer()?;
        let ru = e.integer()?;
        let out = e.integer()?;
        let header = layout::RAW_HEADER_CELLS + layout::STRING_HEADER_CELLS;
        let mut body = Vec::new();
        // Check BOTH complete headers before identity, prefix or unit addresses.
        // checked_base owns slot, live generation and occupied raw extent checks.
        for (boxed, base, length) in [(left, lb, ll), (right, rb, rl)] {
            body.extend([
                assert_stmt(tag_is(var(boxed), value::STRING), line),
                assign(base, e.base(var(boxed)), line),
                assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), line),
                assign(
                    extent,
                    e.decoded(sub(var(base), int(layout::RAW_HEADER_CELLS))),
                    line,
                ),
                assert_stmt(not(lt(var(extent), int(header))), line),
                assign(length, e.decoded(add(var(base), int(2))), line),
                assert_stmt(not(lt(var(length), int(0))), line),
                assert_stmt(not(lt(sub(var(extent), int(header)), var(length))), line),
            ]);
        }
        let mut visit = Vec::new();
        for (base, unit) in [(lb, lu), (rb, ru)] {
            visit.extend([
                assign(
                    unit,
                    e.decoded(add(
                        add(var(base), int(layout::STRING_HEADER_CELLS)),
                        var(cursor),
                    )),
                    line,
                ),
                assert_stmt(not(lt(var(unit), int(0))), line),
                assert_stmt(not(lt(int(65535), var(unit))), line),
            ]);
        }
        visit.push(branch(
            lt(var(lu), var(ru)),
            vec![assign(out, int(-1), line)],
            vec![branch(
                lt(var(ru), var(lu)),
                vec![assign(out, int(1), line)],
                vec![],
                line,
            )],
            line,
        ));
        visit.push(assign(cursor, add(var(cursor), int(1)), line));
        body.push(Stmt::While {
            condition: and(
                eq(var(out), int(0)),
                and(lt(var(cursor), var(ll)), lt(var(cursor), var(rl))),
            ),
            body: visit,
            line,
        });
        body.push(branch(
            eq(var(out), int(0)),
            vec![branch(
                lt(var(ll), var(rl)),
                vec![assign(out, int(-1), line)],
                vec![branch(
                    lt(var(rl), var(ll)),
                    vec![assign(out, int(1), line)],
                    vec![],
                    line,
                )],
                line,
            )],
            vec![],
            line,
        ));
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            HELPER,
            index,
            vec![c, left, right],
            vec![carrier_schema.clone(), value::schema(), value::schema()],
            TypeSchema::Int,
            body,
            var(out),
            line,
        );
        self.helper_string_compare = Some(index);
        // Later private kernels may reuse prerequisites with an early return.
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        Ok(())
    }

    pub(super) fn lower_relational(
        &mut self,
        binary: &ast::BinExpr,
    ) -> Result<Lowered, ParseError> {
        let span = binary.span;
        if !self.relational_sealed.get() {
            return Err(self.error(span, "Relational sites are not sealed"));
        }
        let site = self
            .relational_sites
            .borrow()
            .get(&(span.lo.0, span.hi.0))
            .copied()
            .ok_or_else(|| self.error(span, "Relational site was not proved"))?;
        self.record_relational_site(binary, site.mode)?;
        let line = self.line(span);
        let left = self.expr(&binary.left)?;
        let right = self.expr(&binary.right)?;
        // A loop's sealed conservative mode owns evaluation roots as well as
        // conversion. Its first-entry Lowered kind cannot erase a later String.
        let (left_kind, right_kind) = match site.mode {
            Mode::Primitive(l, r) => (l.kind(), r.kind()),
            _ => (left.kind, right.kind),
        };
        let mut owned = left.owned_roots;
        owned.extend(right.owned_roots);
        // The original left box is bound/rooted before any right allocation.
        let (l, mut stmts) = self.bind_evaluation(left.expr, left_kind, binary.left.span())?;
        let out = if self.pending_normal.is_some() {
            let (out, init) = self.bind_temp_bare(value::boolean(Expr::Bool(false)), span)?;
            stmts.push(init);
            Some(out)
        } else {
            None
        };
        let (r, mut rhs) = self.bind_evaluation(right.expr, right_kind, binary.right.span())?;
        owned.extend([l, r].into_iter().filter(|slot| self.has_root(*slot)));
        let mut compare = Vec::new();
        let flag = match site.mode {
            Mode::Numeric => {
                let (a, as_) = self.bind_typed(
                    self.numeric_projection(l, left.kind),
                    TypeSchema::Number,
                    span,
                )?;
                let (b, bs) = self.bind_typed(
                    self.numeric_projection(r, right.kind),
                    TypeSchema::Number,
                    span,
                )?;
                compare.extend([as_, bs]);
                site.relation.predicate(var(a), var(b))
            }
            Mode::StringString => {
                let helper = self
                    .helper_string_compare
                    .ok_or_else(|| self.error(span, "String comparator was not selected"))?;
                let c = self.active_carrier(span)?;
                let (cmp, statement) = self.bind_typed(
                    call(helper, vec![var(c), var(l), var(r)]),
                    TypeSchema::Int,
                    span,
                )?;
                compare.push(statement);
                site.relation.predicate(var(cmp), int(0))
            }
            Mode::Primitive(expected_left, expected_right) => {
                use primitive_number::{Mode as Operand, Role};
                if (expected_left != Operand::Dynamic
                    && Operand::from_kind(left.kind) != Some(expected_left))
                    || (expected_right != Operand::Dynamic
                        && Operand::from_kind(right.kind) != Some(expected_right))
                {
                    return Err(self.error(span, "Primitive relational pair mode changed"));
                }
                let op = site.relation.conversion_operator();
                let (a, pa) = self.bind_typed(
                    self.primitive_number_projection(l, left_kind, span, op, Role::Left)?,
                    TypeSchema::Float,
                    span,
                )?;
                let (b, pb) = self.bind_typed(
                    self.primitive_number_projection(r, right_kind, span, op, Role::Right)?,
                    TypeSchema::Float,
                    span,
                )?;
                // Source-order projections execute only in the numeric arm and
                // only after both GetValues completed normally. Reversing the
                // final predicate never reverses conversion or guard effects.
                let numeric = Expr::Block {
                    stmts: vec![pa, pb],
                    expr: Box::new(site.relation.predicate(var(a), var(b))),
                };
                if site.mode.can_compare_strings() {
                    let helper = self
                        .helper_string_compare
                        .ok_or_else(|| self.error(span, "String comparator was not selected"))?;
                    let c = self.active_carrier(span)?;
                    let (cmp, statement) = self.bind_typed(
                        call(helper, vec![var(c), var(l), var(r)]),
                        TypeSchema::Int,
                        span,
                    )?;
                    super::super::prelude::object::choose(
                        and(tag_is(var(l), value::STRING), tag_is(var(r), value::STRING)),
                        Expr::Block {
                            stmts: vec![statement],
                            expr: Box::new(site.relation.predicate(var(cmp), int(0))),
                        },
                        numeric,
                    )
                } else {
                    numeric
                }
            }
        };
        let result = if let Some(normal) = self.pending_normal {
            let out = out.expect("pending relational result has a dominating placeholder");
            compare.push(assign(out, value::boolean(flag), line));
            // Separate joins are necessary: a right throw must skip even the
            // first tag/projection/helper call, while preserving pending payload.
            rhs.push(branch(var(normal), compare, vec![], line));
            stmts.push(branch(var(normal), rhs, vec![], line));
            var(out)
        } else {
            stmts.append(&mut rhs);
            stmts.append(&mut compare);
            value::boolean(flag)
        };
        let owned_roots =
            Self::compose_owned(owned.into_iter().filter(|slot| self.has_root(*slot)));
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(result),
            },
            kind: Kind::Boolean,
            array_length: None,
            owned_roots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lower_for(source: &str) -> Lowerer<'_> {
        let source_id = 0;
        let (script, base) = crate::javascript::parser::parse_script(source, source_id).unwrap();
        let mut lower = Lowerer {
            globals: crate::javascript::global_binding::Plan::default(),
            global_helpers: None,
            global_env: None,
            source,
            source_id,
            base,
            next_slot: 0,
            bindings: HashMap::new(),
            array_lengths: HashMap::new(),
            array_length_ids: HashMap::new(),
            array_names: std::collections::HashSet::new(),
            source_bindings: Vec::new(),
            heap: None,
            intrinsics: None,
            objects: None,
            string_length_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
            string_length_sealed: std::cell::Cell::new(false),
            helper_string_length: None,
            string_concat_sites: std::cell::RefCell::new(std::collections::HashMap::new()),
            string_concat_sealed: std::cell::Cell::new(false),
            helper_string_concat: None,
            helper_fixed_scalar_string: None,
            primitive_number: primitive_number::State::default(),
            primitive_addition: primitive_addition::State::default(),
            mutation: primitive_assignment::State::default(),
            abstract_equality: abstract_equality::State::default(),
            relational_sites: std::cell::RefCell::new(HashMap::new()),
            relational_sealed: std::cell::Cell::new(false),
            helper_string_compare: None,
            number_key_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
            number_key_sealed: std::cell::Cell::new(false),
            number_string_entries: None,
            primitive_key: super::primitive_key::State::default(),
            init_cells: 0,
            math_used: std::cell::Cell::new(false),
            numeric_globals: std::cell::Cell::new(0),
            uri_used: std::cell::Cell::new(0),
            json_used: std::cell::Cell::new(false),
            number_namespace_used: std::cell::Cell::new(false),
                number_to_string_used: std::cell::Cell::new(false),
                function_name_used: std::cell::Cell::new(false),
                function_to_string_used: std::cell::Cell::new(false),
function_prototype_used: std::cell::Cell::new(false),
            primitive_constructor_read: std::cell::Cell::new(false),
construct_used: std::cell::Cell::new(false),
helper_construct: None,
            helper_construct_check: None,
                string_replace_all_used: std::cell::Cell::new(false),
                string_search_used: std::cell::Cell::new(false),
                string_match_used: std::cell::Cell::new(false),
            string_namespace_used: std::cell::Cell::new(false),
            array_namespace_used: std::cell::Cell::new(false),
            array_constructor_used: std::cell::Cell::new(false),
            array_buffer_used: std::cell::Cell::new(false),
            typed_array_elements: std::cell::Cell::new(0),
            boolean_used: std::cell::Cell::new(false),
            range_error_used: std::cell::Cell::new(false),
            optional_errors_used: std::cell::Cell::new(0),
            utf16_bridge: None,
            uri_call: None,
            init_nodes: 0,
            string_eq: None,
            enclosing_strict: false,
            function_this: None,
            function_args: None,
            arguments_array_from_values: None,
            arguments_string_key: None,
            arguments_sync: None,
            arguments_object: None,
            carrier_override: None,
            frame_override: None,
            root_scope: None,
            helper_decode: None,
            helper_ref_get: None,
            helper_dispatch: None,
            helper_int_identity: None,
            function_code_ids: HashMap::new(),
            collected_functions: Vec::new(),
            graph: scope::build(&script, source, source_id, base).unwrap(),
            active_scope: scope::ScopeId(0),
            iteration: iteration::IterationState::default(),
            next_target: 1,
            catch_overlay: std::cell::Cell::new(None),
            needs_ordinary_realm: std::cell::Cell::new(false),
            pending_normal: None,
            pending_completion: None,
            pending_payload: None,
            uncaught_host: None,
            binding_slots: HashMap::new(),
            binding_kinds: HashMap::new(),
            type_names: None,
            utf16_inits: utf16_init::Utf16InitPool::default(),
        };
        lower.active_scope = lower.graph.script;
        collect_source_functions(&script, &mut lower, false).unwrap();
        lower
    }
    #[test]
    fn primitive145_sealed_pair_role_operator_empty() {
        use primitive_number::{Mode as Operand, Operator, Role};
        let source = "true<null;";
        let (script, _) = crate::javascript::parser::parse_script(source, 0).unwrap();
        let ast::Stmt::Expr(e) = &script.body[0] else {
            panic!("expression")
        };
        let ast::Expr::Bin(binary) = e.expr.as_ref() else {
            panic!("relation")
        };
        let mut lower = lower_for(source);
        lower.seal_primitive_number_sites();
        assert!(
            lower
                .primitive_number_projection(
                    0,
                    Kind::Boolean,
                    binary.span,
                    Operator::RelationalLt,
                    Role::Left
                )
                .is_err()
        );
        lower.primitive_number = primitive_number::State::default();
        let mode = lower
            .record_primitive_relational_pair(binary, Kind::Boolean, Kind::Null)
            .unwrap();
        assert_eq!(mode, Mode::Primitive(Operand::Boolean, Operand::Null));
        lower.record_relational_site(binary, mode).unwrap();
        lower.seal_primitive_number_sites();
        lower.relational_sealed.set(true);
        assert!(
            lower
                .primitive_number_projection(
                    0,
                    Kind::Boolean,
                    binary.span,
                    Operator::RelationalLt,
                    Role::Left
                )
                .is_ok()
        );
        assert!(
            lower
                .primitive_number_projection(
                    0,
                    Kind::Null,
                    binary.span,
                    Operator::RelationalLt,
                    Role::Right
                )
                .is_ok()
        );
        for (op, role, kind) in [
            (Operator::RelationalGt, Role::Left, Kind::Boolean),
            (Operator::RelationalLe, Role::Left, Kind::Boolean),
            (Operator::RelationalGe, Role::Left, Kind::Boolean),
            (Operator::Plus, Role::Unary, Kind::Boolean),
            (Operator::And, Role::Left, Kind::Boolean),
            (Operator::RelationalLt, Role::Right, Kind::Boolean),
            (Operator::RelationalLt, Role::Left, Kind::Null),
        ] {
            assert!(
                lower
                    .primitive_number_projection(0, kind, binary.span, op, role)
                    .is_err()
            );
        }
        let mut changed = binary.clone();
        changed.span.hi.0 += 1;
        lower
            .probe_relational_collection(|| {
                assert!(lower.relational_collection_is_probe());
                assert!(lower.record_relational_site(&changed, mode).is_err());
                assert!(
                    lower
                        .record_relational_site(
                            binary,
                            Mode::Primitive(Operand::Null, Operand::Boolean)
                        )
                        .is_err()
                );
                let before = lower.relational_sites.borrow().clone();
                lower.record_relational_site(binary, mode).unwrap();
                assert_eq!(*lower.relational_sites.borrow(), before);
                Ok(())
            })
            .unwrap();
        assert!(!lower.relational_collection_is_probe());
        assert!(
            lower
                .primitive_number_projection(
                    0,
                    Kind::Boolean,
                    changed.span,
                    Operator::RelationalLt,
                    Role::Left
                )
                .is_err()
        );
        assert!(lower.record_relational_site(&changed, mode).is_err());
        changed = binary.clone();
        changed.op = ast::BinaryOp::Gt;
        assert!(lower.record_relational_site(&changed, mode).is_err());
        assert!(
            lower
                .record_relational_site(binary, Mode::Primitive(Operand::Null, Operand::Boolean))
                .is_err()
        );
        assert!(
            lower
                .record_primitive_relational_pair(binary, Kind::Boolean, Kind::Number)
                .is_err()
        );
        assert!(!lower.has_string_relational_sites());
        assert!(!lower.has_primitive_number_sites());
        for mode in [
            Mode::Numeric,
            Mode::Primitive(Operand::Dynamic, Operand::Number),
            Mode::Primitive(Operand::Boolean, Operand::String),
        ] {
            assert!(!mode.can_compare_strings());
        }
        for mode in [
            Mode::StringString,
            Mode::Primitive(Operand::Dynamic, Operand::String),
            Mode::Primitive(Operand::Dynamic, Operand::Dynamic),
        ] {
            assert!(mode.can_compare_strings());
        }
    }
    #[test]
    fn primitive145_activation_scope_restore() {
        for (source, fails, strings) in [
            (
                "function f(undefined){return undefined<2;}var o={'\\141':1};",
                false,
                1,
            ),
            (
                "function f(){'use strict';function g(a){return a<'2';}return true<2;}var o={'\\141':1};",
                false,
                2,
            ),
            (
                "function f(){'use strict';'a'<'b';return {}<2;}var o={'\\141':1};",
                true,
                1,
            ),
        ] {
            let mut lower = lower_for(source);
            let parent = lower.active_scope;
            assert!(!lower.enclosing_strict);
            let result = lower.collect_primitive_number_function_sites();
            assert_eq!(result.is_err(), fails);
            assert_eq!(lower.active_scope, parent);
            assert!(!lower.enclosing_strict);
            assert_eq!(
                usize::from(lower.has_string_relational_sites())
                    + usize::from(lower.has_primitive_number_sites()),
                strings
            );
            let (script, _) = crate::javascript::parser::parse_script(source, 0).unwrap();
            let ast::Stmt::Decl(ast::Decl::Var(v)) = &script.body[1] else {
                panic!("var")
            };
            let ast::Expr::Object(o) = v.decls[0].init.as_deref().unwrap() else {
                panic!("object")
            };
            let ast::PropOrSpread::Prop(p) = &o.props[0] else {
                panic!("prop")
            };
            let ast::Prop::KeyValue(k) = p.as_ref() else {
                panic!("key")
            };
            assert_eq!(
                lower.object_key_units(&k.key, k.key.span()).unwrap().0,
                vec![97]
            );
        }
    }
    #[test]
    fn primitive145_loop_probe_restores_without_authorization() {
        let source = "var x=1;var n=0;while(x<2){n=n+1;x='x';}n===1;";
        let (script, _) = crate::javascript::parser::parse_script(source, 0).unwrap();
        let lower = lower_for(source);
        assert!(!lower.relational_collection_is_probe());
        let error: Result<(), ParseError> = lower.probe_relational_collection(|| {
            assert!(lower.relational_collection_is_probe());
            lower.probe_relational_collection(|| Ok(()))?;
            assert!(lower.relational_collection_is_probe());
            Err(lower.error(script.span, "probe-control"))
        });
        assert!(error.is_err());
        assert!(!lower.relational_collection_is_probe());
        assert!(lower.relational_sites.borrow().is_empty());
        reference::script_needs_ordinary_realm(&lower, &script.body).unwrap();
        let sites = lower.relational_sites.borrow();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            sites.values().next().unwrap().mode,
            Mode::Primitive(
                primitive_number::Mode::Dynamic,
                primitive_number::Mode::Number
            )
        );
        assert!(lower.has_primitive_number_sites());
        drop(sites);
        let before = lower.relational_sites.borrow().clone();
        lower.relational_sealed.set(true);
        lower.seal_primitive_number_sites();
        reference::script_needs_ordinary_realm(&lower, &script.body).unwrap();
        assert_eq!(*lower.relational_sites.borrow(), before);
    }
    #[test]
    fn relational127_sealed_exact_span_operator_mode() {
        let mut sites = HashMap::new();
        let site = Site {
            relation: Relation::Lt,
            mode: Mode::Numeric,
        };
        register_site(&mut sites, false, (1, 4), site).unwrap();
        register_site(&mut sites, true, (1, 4), site).unwrap();
        let before = sites.clone();
        for (span, changed) in [
            ((2, 4), site),
            ((1, 5), site),
            (
                (1, 4),
                Site {
                    relation: Relation::Gt,
                    ..site
                },
            ),
            (
                (1, 4),
                Site {
                    mode: Mode::StringString,
                    ..site
                },
            ),
        ] {
            assert!(register_site(&mut sites, true, span, changed).is_err());
            assert_eq!(sites, before);
        }
        assert!(register_site(&mut HashMap::new(), true, (1, 4), site).is_err());
        assert!(
            register_site(
                &mut sites,
                false,
                (1, 4),
                Site {
                    mode: Mode::StringString,
                    ..site
                }
            )
            .is_err()
        );
    }
}
