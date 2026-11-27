//! Drop-and-collect observer for `Object.keys` output identity.
//! Injects only production helpers: the real root store drops the binding, the
//! real collector frees the node, and the real checked-base gate must then
//! refuse the stale slot/generation pair.
use vm::{BuiltinFunction as B, Expr as E, Stmt as S, TypeSchema as T};

pub struct KeysObserver;

fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing helper {name}"))
        .index
}

fn field(index: u16, name: &str) -> E {
    E::Call(
        B::Get.call_index(),
        Vec::new(),
        vec![E::Borrow(Box::new(E::Var(index))), E::String(name.into())],
        None,
        None,
    )
}

fn call(index: u16, args: Vec<E>) -> E {
    E::Call(index, Vec::new(), args, None, None)
}

fn number(expr: E) -> E {
    E::Div(Box::new(expr), Box::new(E::Float(1.0)))
}

fn fresh(ir: &mut vm::FrontendIr) -> u16 {
    let index = u16::try_from(ir.locals).expect("observer slot");
    ir.locals += 1;
    index
}

/// Path of branch arms from the program root to the last store of `binding`.
fn last_store_path(stmts: &[S], binding: u16) -> Option<Vec<(usize, u8)>> {
    for (at, stmt) in stmts.iter().enumerate().rev() {
        match stmt {
            S::Assign { index, .. } if *index == binding => return Some(Vec::new()),
            S::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                if let Some(mut path) = last_store_path(else_branch, binding) {
                    path.insert(0, (at, 1));
                    return Some(path);
                }
                if let Some(mut path) = last_store_path(then_branch, binding) {
                    path.insert(0, (at, 0));
                    return Some(path);
                }
            }
            S::While { body, .. } | S::For { body, .. } => {
                if let Some(mut path) = last_store_path(body, binding) {
                    path.insert(0, (at, 2));
                    return Some(path);
                }
            }
            _ => {}
        }
    }
    None
}

// The observer inserts statements into a nested arm, so the returned handle
// must stay a growable `Vec` for `splice`; a slice cannot express that.
#[allow(clippy::ptr_arg)]
fn branch_mut(stmts: &mut Vec<S>, at: usize, arm: u8) -> &mut Vec<S> {
    match (&mut stmts[at], arm) {
        (S::IfElse { then_branch, .. }, 0) => then_branch,
        (S::IfElse { else_branch, .. }, 1) => else_branch,
        (S::While { body, .. } | S::For { body, .. }, 2) => body,
        _ => panic!("observer path mismatch"),
    }
}

impl vm::SourcePlugin for KeysObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("javascript-es5-object-keys-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            context,
        )?;
        let checked = helper(&ir, "__compat_es5_checked_base");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let carrier = ir
            .stmts
            .iter()
            .find_map(|stmt| match stmt {
                S::Let {
                    index,
                    expr: E::Call(target, _, _, _, _),
                    ..
                } if *target == helper(&ir, "__compat_ref_make") => Some(*index),
                _ => None,
            })
            .expect("real carrier");
        let binding = ir
            .local_bindings
            .iter()
            .find(|(name, _)| name == "k")
            .map(|(_, slot)| *slot)
            .expect("real k binding");
        let path = last_store_path(&ir.stmts, binding).expect("real releasing store for k");
        let (last_at, last_arm) = *path.last().expect("non-empty path");
        let slot = fresh(&mut ir);
        let generation = fresh(&mut ir);
        let capture = vec![
            S::Let {
                index: slot,
                declared_schema: Some(T::Int),
                expr: field(binding, "ref_slot"),
                line: 1,
            },
            S::Let {
                index: generation,
                declared_schema: Some(T::Int),
                expr: field(binding, "generation"),
                line: 1,
            },
        ];
        let mut cursor: &mut Vec<S> = &mut ir.stmts;
        for (at, arm) in &path[..path.len() - 1] {
            cursor = branch_mut(cursor, *at, *arm);
        }
        // The releasing store itself is the next statement in this arm.
        if last_arm != 0 {
            panic!("observer expects the releasing store in a then arm");
        }
        cursor.splice(last_at..last_at, capture);
        let _ = cursor;
        // After the real store releases k, one real collection must free the
        // output; the real production gate then refuses the stale identity.
        ir.stmts.push(S::Expr {
            expr: call(collect, vec![E::Var(carrier)]),
            line: 1,
        });
        let probe = fresh(&mut ir);
        ir.stmts.push(S::Let {
            index: probe,
            declared_schema: Some(T::Int),
            expr: call(
                checked,
                vec![E::Var(carrier), E::Var(slot), E::Var(generation)],
            ),
            line: 1,
        });
        ir.stmts.push(S::Expr {
            expr: number(E::Var(probe)),
            line: 1,
        });
        eprintln!(
            "KEYS_OBSERVER path={path:?} store_at={last_at} slot={slot} generation={generation}"
        );
        Ok(ir)
    }
}
