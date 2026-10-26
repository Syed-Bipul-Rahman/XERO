//! Match exhaustiveness checking (pattern usefulness, after Maranget).

use super::Checker;
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Ty;
use crate::xir::{Arm, Pat};

#[derive(Clone, Debug, PartialEq)]
enum Ctor {
    Variant(u32),
    Bool(bool),
    Single,
    Int(i128, i128),
}

fn strip(p: &Pat) -> &Pat {
    match p {
        Pat::Deref(inner) => strip(inner),
        _ => p,
    }
}

fn ctor_of(p: &Pat) -> Option<Ctor> {
    match strip(p) {
        Pat::Wild | Pat::Bind(..) => None,
        Pat::Variant(i, _) => Some(Ctor::Variant(*i)),
        Pat::Bool(b) => Some(Ctor::Bool(*b)),
        Pat::Tuple(_) => Some(Ctor::Single),
        Pat::Int(v) => Some(Ctor::Int(*v, *v)),
        Pat::Range(a, b) => Some(Ctor::Int(*a, *b)),
        Pat::Or(_) | Pat::Deref(_) => unreachable!(),
    }
}

fn base_ty(t: &Ty) -> &Ty {
    match t {
        Ty::Ref(inner, _) => base_ty(inner),
        _ => t,
    }
}

/// Sub-field types for constructor `c` of type `ty`.
fn sub_tys(ck: &Checker, ty: &Ty, c: &Ctor) -> Vec<Ty> {
    match (base_ty(ty), c) {
        (Ty::Adt(id, args), Ctor::Variant(i)) => {
            let a = &ck.adts[*id as usize];
            if a.is_enum() {
                a.variants[*i as usize].iter().map(|f| f.subst(args)).collect()
            } else {
                a.fields.iter().map(|f| f.subst(args)).collect()
            }
        }
        (Ty::Tuple(ts), Ctor::Single) => ts.clone(),
        _ => Vec::new(),
    }
}

/// All constructors of a type if its signature is finite.
fn all_ctors(ck: &Checker, ty: &Ty) -> Option<Vec<Ctor>> {
    match base_ty(ty) {
        Ty::Bool => Some(vec![Ctor::Bool(false), Ctor::Bool(true)]),
        Ty::Adt(id, _) => {
            let a = &ck.adts[*id as usize];
            if a.is_enum() {
                Some((0..a.variants.len() as u32).map(Ctor::Variant).collect())
            } else {
                Some(vec![Ctor::Variant(0)])
            }
        }
        Ty::Tuple(_) => Some(vec![Ctor::Single]),
        _ => None,
    }
}

fn covers(row_ctor: &Ctor, c: &Ctor) -> bool {
    match (row_ctor, c) {
        (Ctor::Int(a, b), Ctor::Int(x, y)) => a <= x && y <= b,
        _ => row_ctor == c,
    }
}

fn expand_or(rows: Vec<Vec<Pat>>) -> Vec<Vec<Pat>> {
    let mut out = Vec::new();
    for row in rows {
        if let Some(first) = row.first() {
            if let Pat::Or(alts) = strip(first) {
                for a in alts {
                    let mut r = vec![a.clone()];
                    r.extend_from_slice(&row[1..]);
                    out.extend(expand_or(vec![r]));
                }
                continue;
            }
        }
        out.push(row);
    }
    out
}

fn specialize(rows: &[Vec<Pat>], c: &Ctor, arity: usize) -> Vec<Vec<Pat>> {
    let mut out = Vec::new();
    for row in rows {
        let head = strip(&row[0]);
        match ctor_of(head) {
            None => {
                let mut r = vec![Pat::Wild; arity];
                r.extend_from_slice(&row[1..]);
                out.push(r);
            }
            Some(rc) if covers(&rc, c) => {
                let mut r = match head {
                    Pat::Variant(_, ps) | Pat::Tuple(ps) => ps.clone(),
                    _ => Vec::new(),
                };
                r.extend_from_slice(&row[1..]);
                out.push(r);
            }
            _ => {}
        }
    }
    out
}

fn useful(ck: &Checker, rows: Vec<Vec<Pat>>, q: Vec<Pat>, tys: &[Ty]) -> bool {
    if q.is_empty() {
        return rows.is_empty();
    }
    let rows = expand_or(rows);
    if let Pat::Or(alts) = strip(&q[0]) {
        return alts.iter().any(|a| {
            let mut nq = vec![a.clone()];
            nq.extend_from_slice(&q[1..]);
            useful(ck, rows.clone(), nq, tys)
        });
    }
    let ty = &tys[0];
    let rest_tys = &tys[1..];
    let recurse = |c: &Ctor, q_sub: Vec<Pat>| -> bool {
        let sub = sub_tys(ck, ty, c);
        let arity = sub.len();
        let srows = specialize(&rows, c, arity);
        let mut nq = q_sub;
        nq.resize(arity, Pat::Wild);
        nq.extend_from_slice(&q[1..]);
        let mut ntys = sub;
        ntys.extend_from_slice(rest_tys);
        useful(ck, srows, nq, &ntys)
    };
    match ctor_of(&q[0]) {
        Some(c) => {
            let sub = match strip(&q[0]) {
                Pat::Variant(_, ps) | Pat::Tuple(ps) => ps.clone(),
                _ => Vec::new(),
            };
            recurse(&c, sub)
        }
        None => {
            let present: Vec<Ctor> = rows.iter().filter_map(|r| ctor_of(&r[0])).collect();
            if let Some(all) = all_ctors(ck, ty) {
                if all.iter().all(|c| present.iter().any(|p| covers(p, c))) {
                    return all.iter().any(|c| recurse(c, Vec::new()));
                }
            }
            // default matrix
            let def: Vec<Vec<Pat>> = rows.iter().filter(|r| ctor_of(&r[0]).is_none()).map(|r| r[1..].to_vec()).collect();
            useful(ck, def, q[1..].to_vec(), rest_tys)
        }
    }
}

pub fn check(ck: &Checker, ty: &Ty, arms: &[Arm], span: Span) -> Result<()> {
    let rows: Vec<Vec<Pat>> = arms.iter().filter(|a| a.guard.is_none()).map(|a| vec![a.pat.clone()]).collect();
    if !useful(ck, rows.clone(), vec![Pat::Wild], std::slice::from_ref(ty)) {
        return Ok(());
    }
    let mut missing = Vec::new();
    if let Some(all) = all_ctors(ck, ty) {
        for c in &all {
            let arity = sub_tys(ck, ty, c).len();
            let q = match c {
                Ctor::Variant(i) => Pat::Variant(*i, vec![Pat::Wild; arity]),
                Ctor::Bool(b) => Pat::Bool(*b),
                Ctor::Single => Pat::Tuple(vec![Pat::Wild; arity]),
                Ctor::Int(..) => continue,
            };
            if useful(ck, rows.clone(), vec![q], std::slice::from_ref(ty)) {
                missing.push(match (c, base_ty(ty)) {
                    (Ctor::Variant(i), Ty::Adt(id, _)) => {
                        let a = &ck.adts[*id as usize];
                        let n = if a.is_enum() { a.variant_name(*i).to_string() } else { a.name.clone() };
                        if arity > 0 { format!("`{n}(..)`") } else { format!("`{n}`") }
                    }
                    (Ctor::Bool(b), _) => format!("`{b}`"),
                    _ => "`_`".to_string(),
                });
            }
        }
    }
    let msg = if missing.is_empty() || missing.len() == all_ctors(ck, ty).map(|a| a.len()).unwrap_or(0) && missing.len() > 4 {
        "non-exhaustive patterns: add a `_ => ...` arm to handle the remaining values".to_string()
    } else {
        format!("non-exhaustive patterns: {} not covered", missing.join(", "))
    };
    Err(Diagnostic::new(span, msg))
}
