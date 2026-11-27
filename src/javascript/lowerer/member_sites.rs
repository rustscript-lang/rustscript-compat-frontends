//! Shared activation-local feature fact collection before runtime indices.
use super::*;

impl Lowerer<'_> {
    /// Facts only: no activation, instantiation or IR slots are constructed.
    pub(super) fn collect_function_feature_sites(&mut self) -> Result<(), ParseError> {
        for collected in self.collected_functions.clone() {
            let Some(body) = &collected.function.body else {
                continue;
            };
            if !reference::body_has_possible_feature_site(&body.stmts) {
                continue;
            }
            let scope = *self
                .graph
                .function_scopes
                .get(&(collected.function.span.lo.0, collected.function.span.hi.0))
                .ok_or_else(|| {
                    self.error(collected.span, "Feature-site function scope is missing")
                })?;
            if self.graph.scope(scope).strict != collected.strict {
                return Err(self.error(
                    collected.span,
                    "Feature-site function strictness disagrees with its scope",
                ));
            }
            let parent_scope = self.active_scope;
            let parent_strict = self.enclosing_strict;
            self.active_scope = scope;
            self.enclosing_strict = collected.strict;
            // This facts-only pass runs before activation slots exist, but the
            // `arguments` gates key off the invocation vector's presence.
            // Install a placeholder for the body walk; validation never emits
            // or reads the slot value.
            let saved_args = self.function_args.replace(0);
            let result = reference::validate_function_feature_body(self, &body.stmts);
            self.function_args = saved_args;
            self.active_scope = parent_scope;
            self.enclosing_strict = parent_strict;
            result?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_length115_scope_restore_success_and_error() {
        use string_concat::Mode;

        use relational::Mode as RelationalMode;
        for (source, fails, length_count, concat_count, expected_modes, relational_modes) in [
            (
                "function f(){'use strict';return 'ab'.length;}var o={'\\141':1};",
                false,
                1,
                0,
                &[] as &[Mode],
                &[] as &[RelationalMode],
            ),
            (
                "function f(){'use strict';'ab'.length;return 'ab'.other;}var o={'\\141':1};",
                true,
                1,
                0,
                &[],
                &[],
            ),
            (
                "function f(){'use strict';return 'A'+'B';}var o={'\\141':1};",
                false,
                0,
                1,
                &[Mode::StringString],
                &[],
            ),
            (
                "function f(){'use strict';'A'+'B';return 'a'+1;}var o={'\\141':1};",
                false,
                0,
                2,
                &[Mode::StringString, Mode::StringNumber],
                &[],
            ),
            (
                "function f(){'use strict';'A'+'B';return 'a'+true;}var o={'\\141':1};",
                false,
                0,
                2,
                &[Mode::StringString, Mode::StringBoolean],
                &[],
            ),
            (
                "function f(){'use strict';'A'+'B';return 'a'+({});}var o={'\\141':1};",
                true,
                0,
                1,
                &[Mode::StringString],
                &[],
            ),
            (
                "function f(){return 1<2;}var o={'\\141':1};",
                false,
                0,
                0,
                &[],
                &[RelationalMode::Numeric],
            ),
            (
                "function f(){'use strict';return 'a'<='b';}var o={'\\141':1};",
                false,
                0,
                0,
                &[],
                &[RelationalMode::StringString],
            ),
            (
                "function f(){'a'<'b';return true<2;}var o={'\\141':1};",
                false,
                0,
                0,
                &[],
                &[
                    RelationalMode::StringString,
                    RelationalMode::Primitive(
                        primitive_number::Mode::Boolean,
                        primitive_number::Mode::Number,
                    ),
                ],
            ),
            (
                "function f(){'use strict';function g(){return 1>=2;}return 'b'>'a';}var o={'\\141':1};",
                false,
                0,
                0,
                &[],
                &[RelationalMode::StringString, RelationalMode::Numeric],
            ),
            (
                "function f(){try{throw 1;}catch(e){var n=1;return n<2;}}var o={'\\141':1};",
                false,
                0,
                0,
                &[],
                &[RelationalMode::Numeric],
            ),
        ] {
            let source_id = 0;
            let (script, base) =
                crate::javascript::parser::parse_script(source, source_id).unwrap();
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
            let parent = lower.active_scope;
            collect_source_functions(&script, &mut lower, false).unwrap();
            let result = lower.collect_function_feature_sites();
            assert_eq!(result.is_err(), fails, "{result:?}");
            assert_eq!(lower.active_scope, parent);
            assert!(!lower.enclosing_strict);
            let ast::Stmt::Decl(ast::Decl::Var(decl)) = &script.body[1] else {
                panic!("var")
            };
            let ast::Expr::Object(object) = decl.decls[0].init.as_deref().unwrap() else {
                panic!("object")
            };
            let ast::PropOrSpread::Prop(prop) = &object.props[0] else {
                panic!("prop")
            };
            let ast::Prop::KeyValue(kv) = prop.as_ref() else {
                panic!("key value")
            };
            assert_eq!(
                lower.object_key_units(&kv.key, kv.key.span()).unwrap().0,
                vec![97]
            );
            assert_eq!(lower.string_length_sites.borrow().len(), length_count);
            assert_eq!(lower.string_concat_sites.borrow().len(), concat_count);
            let relational_sites = lower.relational_sites.borrow();
            assert_eq!(relational_sites.len(), relational_modes.len(), "{source}");
            for expected in relational_modes {
                assert_eq!(
                    relational_sites
                        .values()
                        .filter(|site| site.mode == *expected)
                        .count(),
                    relational_modes
                        .iter()
                        .filter(|mode| *mode == expected)
                        .count(),
                    "{source}: {expected:?}"
                );
            }
            let sites = lower.string_concat_sites.borrow();
            assert_eq!(sites.len(), expected_modes.len());
            for expected in expected_modes {
                assert_eq!(
                    sites.values().filter(|mode| *mode == expected).count(),
                    expected_modes
                        .iter()
                        .filter(|mode| *mode == expected)
                        .count(),
                    "{source}: expected mode {expected:?}"
                );
            }
            eprintln!(
                "LENGTH115_SCOPE failure={fails} actual_scope_restored=true actual_strict_restored=true sloppy_octal_decoded=true"
            );
        }
    }
}
