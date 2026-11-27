//! Bounded, test-only access to the three emitted managed-store containers.
//! This deliberately does not walk arbitrary control flow or function bodies.
use vm::{AssignmentKind, Expr, FrontendIr, Stmt, TypeSchema};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListPath {
    Top,
    Guard,
    BlockGuard,
}

#[derive(Clone, Debug)]
pub(super) struct Site {
    pub top: usize,
    pub path: ListPath,
    pub assign: usize,
    pub binding: u16,
    pub pending: Option<u16>,
    pub assigned: Expr,
    pub refresh: Stmt,
}

impl Site {
    pub fn list<'a>(&self, ir: &'a FrontendIr) -> &'a [Stmt] {
        match self.path {
            ListPath::Top => &ir.stmts,
            ListPath::Guard => match &ir.stmts[self.top] {
                Stmt::IfElse { then_branch, .. } => then_branch,
                _ => unreachable!("resolved store guard"),
            },
            ListPath::BlockGuard => match &ir.stmts[self.top] {
                Stmt::Let {
                    expr: Expr::Block { stmts, .. },
                    ..
                } => match &stmts[0] {
                    Stmt::IfElse { then_branch, .. } => then_branch,
                    _ => unreachable!("resolved block guard"),
                },
                _ => unreachable!("resolved store block"),
            },
        }
    }

    pub fn list_mut<'a>(&self, ir: &'a mut FrontendIr) -> &'a mut Vec<Stmt> {
        match self.path {
            ListPath::Top => &mut ir.stmts,
            ListPath::Guard => match &mut ir.stmts[self.top] {
                Stmt::IfElse { then_branch, .. } => then_branch,
                _ => unreachable!("resolved store guard"),
            },
            ListPath::BlockGuard => match &mut ir.stmts[self.top] {
                Stmt::Let {
                    expr: Expr::Block { stmts, .. },
                    ..
                } => match &mut stmts[0] {
                    Stmt::IfElse { then_branch, .. } => then_branch,
                    _ => unreachable!("resolved block guard"),
                },
                _ => unreachable!("resolved store block"),
            },
        }
    }

    /// The plain container includes its refresh as the next top-level stmt.
    /// A guard/block container already includes both inside its anchor.
    pub fn after_anchor(&self) -> usize {
        self.top + if self.path == ListPath::Top { 2 } else { 1 }
    }
}

pub(super) fn root_entry(stmt: &Stmt, root: u16, binding: u16) -> Option<i64> {
    match stmt {
        Stmt::Let {
            expr: Expr::Call(target, types, args, None, None),
            ..
        } if *target == root
            && types.is_empty()
            && args.len() == 4
            && matches!(args[3], Expr::Var(slot) if slot == binding) =>
        {
            match args[2] {
                Expr::Int(entry) => Some(entry),
                _ => None,
            }
        }
        _ => None,
    }
}

fn pair(list: &[Stmt], at: usize, binding: u16, root: u16) -> Option<(Expr, Stmt)> {
    let Stmt::Assign {
        kind: AssignmentKind::Set,
        index,
        expr,
        ..
    } = list.get(at)?
    else {
        return None;
    };
    if *index != binding || root_entry(list.get(at + 1)?, root, binding).is_none() {
        return None;
    }
    Some((expr.clone(), list[at + 1].clone()))
}

pub(super) fn sites(ir: &FrontendIr, binding: u16, root: u16) -> Result<Vec<Site>, &'static str> {
    // The script's actual declaration, never a literal guard or a slot constant.
    // More than one candidate is ambiguous: do not infer identity from one store.
    let normals: Vec<_> = ir
        .stmts
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Let {
                index,
                declared_schema: Some(TypeSchema::Bool),
                expr: Expr::Bool(true),
                ..
            } => Some(*index),
            _ => None,
        })
        .collect();
    let normal = match normals.as_slice() {
        [] => None,
        [slot] => Some(*slot),
        _ => return Err("ambiguous pending-normal declaration"),
    };
    let mut out = Vec::new();
    for (top, stmt) in ir.stmts.iter().enumerate() {
        if let Some((assigned, refresh)) = pair(&ir.stmts, top, binding, root) {
            out.push(Site {
                top,
                path: ListPath::Top,
                assign: top,
                binding,
                pending: None,
                assigned,
                refresh,
            });
            continue;
        }
        let (path, guard) = match stmt {
            Stmt::IfElse { .. } => (ListPath::Guard, stmt),
            Stmt::Let {
                expr: Expr::Block { stmts, expr },
                ..
            } if stmts.len() == 1 && matches!(**expr, Expr::Var(slot) if slot == binding) => {
                (ListPath::BlockGuard, &stmts[0])
            }
            _ => continue,
        };
        let Stmt::IfElse {
            condition: Expr::Var(slot),
            then_branch,
            else_branch,
            ..
        } = guard
        else {
            continue;
        };
        if Some(*slot) != normal || !else_branch.is_empty() || then_branch.len() != 2 {
            continue;
        }
        if let Some((assigned, refresh)) = pair(then_branch, 0, binding, root) {
            out.push(Site {
                top,
                path,
                assign: 0,
                binding,
                pending: normal,
                assigned,
                refresh,
            });
        }
    }
    Ok(out)
}

pub(super) fn unique(sites: &[Site]) -> Option<&Site> {
    match sites {
        [site] => Some(site),
        _ => None,
    }
}

pub(super) fn first(sites: &[Site]) -> Option<&Site> {
    sites.first()
}

pub(super) fn null_store(sites: &[Site]) -> Option<&Site> {
    let mut matching = sites.iter().filter(|site| canonical_null(&site.assigned));
    let site = matching.next()?;
    if matching.next().is_some() {
        None
    } else {
        Some(site)
    }
}

pub(super) fn canonical_null(expr: &Expr) -> bool {
    let mut node = expr;
    for field in ["generation", "ref_slot", "flag", "number", "tag"] {
        let Expr::Call(target, types, args, None, None) = node else {
            return false;
        };
        if *target != vm::BuiltinFunction::Set.call_index()
            || !types.is_empty()
            || args.len() != 3
            || !matches!(&args[1], Expr::String(key) if key == field)
        {
            return false;
        }
        let correct = match (field, &args[2]) {
            ("generation" | "ref_slot", Expr::Int(0))
            | ("flag", Expr::Bool(false))
            | ("tag", Expr::Int(1)) => true,
            ("number", Expr::Float(number)) => number.to_bits() == 0.0f64.to_bits(),
            _ => false,
        };
        if !correct {
            return false;
        }
        node = &args[0];
    }
    matches!(node, Expr::Call(target, types, args, None, None) if *target == vm::BuiltinFunction::MapNew.call_index() && types.is_empty() && args.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm::SourcePlugin;

    fn parse(source: &str) -> FrontendIr {
        let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
        pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN
            .parse_source(
                source,
                &vm::SourceFrontendContext {
                    source_id: 0,
                    original_path: None,
                    phase: vm::compiler::ParsePhase::Standalone,
                    host_api_catalog: Some(&catalog),
                    imports: &[],
                },
            )
            .expect("real control source")
    }

    fn fixture() -> (FrontendIr, u16, u16) {
        let ir = parse("var x={child:{n:7}};var y=x;x=null;var n=y.child.n;");
        let binding = ir
            .local_bindings
            .iter()
            .find(|(name, _)| name == "x")
            .unwrap()
            .1;
        let root = ir
            .functions
            .iter()
            .find(|f| f.name == "__compat_es5_root_write")
            .unwrap()
            .index;
        (ir, binding, root)
    }

    #[test]
    fn bounded_container_controls() {
        let (mut ir, binding, root) = fixture();
        let found = sites(&ir, binding, root).unwrap();
        assert_eq!(found.len(), 2);
        let site = null_store(&found).unwrap().clone();
        assert_eq!(site.path, ListPath::BlockGuard);
        assert_eq!(site.assign, 0);
        assert_eq!(site.binding, binding);
        assert_eq!(
            format!("{:?}", site.list(&ir)[1]),
            format!("{:?}", site.refresh)
        );
        let original = ir.stmts.clone();
        let normal = site.pending.unwrap();
        let guard = match &original[site.top] {
            Stmt::Let {
                expr: Expr::Block { stmts, .. },
                ..
            } => stmts[0].clone(),
            _ => unreachable!(),
        };
        let declaration = original
            .iter()
            .find(|s| matches!(s, Stmt::Let {index,..} if *index==normal))
            .unwrap()
            .clone();
        let mut wrong = guard.clone();
        if let Stmt::IfElse { condition, .. } = &mut wrong {
            *condition = Expr::Var(normal + 1);
        }
        ir.stmts = vec![declaration.clone(), wrong];
        assert!(sites(&ir, binding, root).unwrap().is_empty(), "wrong slot");
        let mut nonempty = guard.clone();
        if let Stmt::IfElse { else_branch, .. } = &mut nonempty {
            else_branch.push(Stmt::Expr {
                expr: Expr::Int(0),
                line: 1,
            });
        }
        ir.stmts = vec![declaration.clone(), nonempty];
        assert!(
            sites(&ir, binding, root).unwrap().is_empty(),
            "nonempty else"
        );
        let mut literal = guard.clone();
        if let Stmt::IfElse { condition, .. } = &mut literal {
            *condition = Expr::Bool(true);
        }
        ir.stmts = vec![declaration.clone(), literal];
        assert!(
            sites(&ir, binding, root).unwrap().is_empty(),
            "arbitrary if"
        );
        let mut nested = guard.clone();
        if let Stmt::IfElse { then_branch, .. } = &mut nested {
            *then_branch = vec![guard.clone()];
        }
        ir.stmts = vec![declaration.clone(), nested];
        assert!(
            sites(&ir, binding, root).unwrap().is_empty(),
            "no enhanced guards"
        );
        ir.stmts = vec![
            declaration.clone(),
            guard.clone(),
            Stmt::Let {
                index: normal + 1,
                declared_schema: Some(TypeSchema::Bool),
                expr: Expr::Bool(true),
                line: 1,
            },
        ];
        assert!(sites(&ir, binding, root).is_err(), "ambiguous declaration");
        ir.stmts = vec![declaration, guard.clone(), guard];
        let duplicate = sites(&ir, binding, root).unwrap();
        assert!(unique(&duplicate).is_none());
        assert!(null_store(&duplicate).is_none());
        // Actual hidden try/function sources: their stores cannot be exposed
        // by this observer's strictly script-list-only selection.
        for source in [
            "try{var x={};x=null;}catch(e){}",
            "function f(){\"use strict\";var x={};x=null;}var y={};",
        ] {
            let hidden = parse(source);
            let slot = hidden
                .local_bindings
                .iter()
                .find(|(name, _)| name == "x")
                .map(|(_, s)| *s);
            if let Some(slot) = slot {
                assert!(sites(&hidden, slot, root).unwrap().is_empty());
            } else {
                assert!(
                    hidden
                        .function_impls
                        .values()
                        .any(|f| !f.body_stmts.is_empty())
                );
            }
        }
        // The mutation accessor resolves the inner list, not the top index.
        ir.stmts = original;
        let len = site.list(&ir).len();
        site.list_mut(&mut ir).push(Stmt::Expr {
            expr: Expr::Int(0),
            line: 1,
        });
        assert_eq!(site.list(&ir).len(), len + 1);
        eprintln!(
            "BOUNDED_STORE_CONTROLS wrong-slot else arbitrary-if nested ambiguous-declaration duplicate-store hidden-try function inner-list"
        );
    }

    #[test]
    fn selection_policy_controls() {
        let (ir, binding, root) = fixture();
        let found = sites(&ir, binding, root).unwrap();
        let initial = first(&found).unwrap();
        let null = null_store(&found).unwrap();
        assert!(initial.top < null.top);
        assert!(!canonical_null(&initial.assigned));
        assert_eq!(first(&found).unwrap().top, initial.top);
        let mut negative_zero = null.assigned.clone();
        // Copy only the bounded Set spine: reject -0 and every changed field.
        for field in ["generation", "ref_slot", "flag", "number", "tag"] {
            let mut altered = null.assigned.clone();
            let mut node = &mut altered;
            loop {
                let Expr::Call(_, _, args, _, _) = node else {
                    panic!("canonical spine");
                };
                if matches!(&args[1],Expr::String(key) if key==field) {
                    args[2] = Expr::Int(99);
                    break;
                }
                node = &mut args[0];
            }
            assert!(!canonical_null(&altered));
        }
        let mut node = &mut negative_zero;
        loop {
            let Expr::Call(_, _, args, _, _) = node else {
                unreachable!()
            };
            if matches!(&args[1],Expr::String(key) if key=="number") {
                args[2] = Expr::Float(-0.0);
                break;
            }
            node = &mut args[0];
        }
        assert!(!canonical_null(&negative_zero));
        let plain = parse("var x={n:1};x=null;");
        let x = plain
            .local_bindings
            .iter()
            .find(|(name, _)| name == "x")
            .unwrap()
            .1;
        let root = plain
            .functions
            .iter()
            .find(|f| f.name == "__compat_es5_root_write")
            .unwrap()
            .index;
        let found = sites(&plain, x, root).unwrap();
        let initial = first(&found).unwrap();
        assert_eq!(initial.path, ListPath::Top);
        assert!(!canonical_null(&initial.assigned));
        assert!(root_entry(&plain.stmts[initial.top + 1], root, x).is_some());
        // The plain source's later assignment expression has a different
        // Block shape. Reclaim needs only its first top-level Assign; do not
        // broaden this walker to admit a fourth container for that overwrite.
        eprintln!(
            "STORE_POLICY_CONTROLS alias-not-initializer reclaim-not-null full-null-fields positive-zero plain-address01"
        );
    }
}
