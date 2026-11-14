//! Pattern lowering with default binding modes.

use super::expr::{FnCx, PathRes};
use super::{AdtKind, Def};
use crate::ast::{self, ExprKind as E, PatKind, UnOp, Vis};
use crate::diag::Result;
use crate::types::{AdtId, Ty};
use crate::xir::{BindMode, LocalId, Pat};

impl FnCx<'_> {
    /// Lower pattern `p` matched against a value of type `ty`.
    /// New bindings are appended to `binds` (the caller declares them).
    pub fn pattern(
        &mut self,
        p: &ast::Pattern,
        ty: &Ty,
        mode: BindMode,
        binds: &mut Vec<(String, LocalId)>,
    ) -> Result<Pat> {
        // Default binding mode: matching a non-reference pattern against `&T`
        // dereferences and switches bindings to by-reference.
        let structural = match &p.kind {
            PatKind::Wild => false,
            PatKind::Bind(id, _) => self.bare_variant(id, ty).is_some(),
            _ => true,
        };
        if structural {
            if let Ty::Ref(inner, m) = ty {
                let new_mode = match mode {
                    BindMode::Ref(outer) => BindMode::Ref(outer && *m),
                    BindMode::Value => BindMode::Ref(*m),
                };
                let inner = (**inner).clone();
                return Ok(Pat::Deref(Box::new(
                    self.pattern(p, &inner, new_mode, binds)?,
                )));
            }
        }
        match &p.kind {
            PatKind::Wild => Ok(Pat::Wild),
            PatKind::Bind(id, mutable) => {
                if let Some((aid, idx)) = self.bare_variant(id, ty) {
                    return self.variant_pat(aid, idx, None, ty, mode, p.span, binds);
                }
                if binds.iter().any(|(n, _)| *n == id.name) {
                    return self.err(
                        id.span,
                        format!(
                            "identifier `{}` is bound more than once in the same pattern",
                            id.name
                        ),
                    );
                }
                let bty = match mode {
                    BindMode::Value => ty.clone(),
                    BindMode::Ref(m) => Ty::reference(ty.clone(), m),
                };
                // a later alternative of `a | b` reuses the first one's variable
                if let Some(reuse) = &self.or_reuse {
                    if let Some((_, lid)) = reuse.iter().find(|(n, _)| *n == id.name) {
                        let lid = *lid;
                        let have = self.locals[lid as usize].ty.clone();
                        if have != bty {
                            return self.err(id.span, format!("`{}` has type `{}` here but `{}` in the first alternative of the `|` pattern", id.name, self.ts(&bty), self.ts(&have)));
                        }
                        binds.push((id.name.clone(), lid));
                        return Ok(Pat::Bind(lid, mode));
                    }
                }
                let lid = self.new_local(&id.name, bty, *mutable, id.span, false);
                binds.push((id.name.clone(), lid));
                Ok(Pat::Bind(lid, mode))
            }
            PatKind::Lit(e) => {
                let (v, is_bool) = self.lit_value(e, ty)?;
                Ok(if is_bool {
                    Pat::Bool(v != 0)
                } else {
                    Pat::Int(v)
                })
            }
            PatKind::Range(lo, hi, inclusive) => {
                if !ty.is_int() {
                    return self.err(
                        p.span,
                        format!(
                            "range patterns require an integer type, found `{}`",
                            self.ts(ty)
                        ),
                    );
                }
                let (lo_v, _) = self.lit_value(lo, ty)?;
                let (mut hi_v, _) = self.lit_value(hi, ty)?;
                if !inclusive {
                    hi_v -= 1;
                }
                if lo_v > hi_v {
                    return self.err(p.span, "range pattern is empty");
                }
                Ok(Pat::Range(lo_v, hi_v))
            }
            PatKind::Tuple(ps) => {
                let Ty::Tuple(ts) = ty else {
                    return self.err(
                        p.span,
                        format!(
                            "tuple pattern cannot match a value of type `{}`",
                            self.ts(ty)
                        ),
                    );
                };
                if ts.len() != ps.len() {
                    return self.err(
                        p.span,
                        format!(
                            "tuple pattern has {} element(s), but the value has {}",
                            ps.len(),
                            ts.len()
                        ),
                    );
                }
                let ts = ts.clone();
                let mut out = Vec::new();
                for (sp, st) in ps.iter().zip(&ts) {
                    out.push(self.pattern(sp, st, mode, binds)?);
                }
                Ok(Pat::Tuple(out))
            }
            PatKind::Variant(path, args) => {
                let (aid, idx) = self.resolve_variant_path(path, ty, p.span)?;
                self.variant_pat(aid, idx, args.as_deref(), ty, mode, p.span, binds)
            }
            PatKind::Struct(path, fields, rest) => {
                let (aid, _) = self.resolve_variant_path(path, ty, p.span)?;
                let a = &self.ck.adts[aid as usize];
                let sname = a.name.clone();
                if a.is_enum() {
                    return self.err(
                        p.span,
                        format!("`{sname}` is an enum; match its variants: `{sname}.Variant(...)`"),
                    );
                }
                let Ty::Adt(tid, args) = ty else {
                    return self.err(
                        p.span,
                        format!(
                            "struct pattern `{sname}` cannot match a value of type `{}`",
                            self.ts(ty)
                        ),
                    );
                };
                if *tid != aid {
                    return self.err(
                        p.span,
                        format!(
                            "struct pattern `{sname}` cannot match a value of type `{}`",
                            self.ts(ty)
                        ),
                    );
                }
                let ftys = self.ck.struct_fields(aid, args);
                let AdtKind::Struct(decls) = self.ck.adts[aid as usize].kind.clone() else {
                    unreachable!()
                };
                let amod = self.ck.adts[aid as usize].module;
                let mut pats = vec![Pat::Wild; ftys.len()];
                let mut seen = vec![false; ftys.len()];
                for (name, sub) in fields {
                    let Some(i) = decls.iter().position(|d| d.name.name == name.name) else {
                        return self.err(
                            name.span,
                            format!("struct `{sname}` has no field `{}`", name.name),
                        );
                    };
                    if seen[i] {
                        return self.err(
                            name.span,
                            format!("field `{}` appears twice in the pattern", name.name),
                        );
                    }
                    if decls[i].vis != Vis::Pub
                        && amod != self.module
                        && !self.ck.modules[self.module as usize].is_std
                    {
                        return self.err(
                            name.span,
                            format!("field `{}` of `{sname}` is private", name.name),
                        );
                    }
                    seen[i] = true;
                    pats[i] = self.pattern(sub, &ftys[i], mode, binds)?;
                }
                if !rest {
                    let missing: Vec<String> = decls
                        .iter()
                        .zip(&seen)
                        .filter(|(_, s)| !**s)
                        .map(|(d, _)| format!("`{}`", d.name.name))
                        .collect();
                    if !missing.is_empty() {
                        return self.err(p.span, format!("pattern does not mention field(s) {}; list them or end the pattern with `..`", missing.join(", ")));
                    }
                }
                Ok(Pat::Variant(0, pats))
            }
            PatKind::Or(alts) => {
                // every alternative binds the same names, to the same variables
                let mut out = Vec::new();
                let mut first: Vec<(String, LocalId)> = Vec::new();
                let saved = self.or_reuse.take();
                for (i, a) in alts.iter().enumerate() {
                    let mut inner = Vec::new();
                    if i > 0 {
                        self.or_reuse = Some(first.clone());
                    }
                    let r = self.pattern(a, ty, mode, &mut inner);
                    self.or_reuse = None;
                    let r = match r {
                        Ok(r) => r,
                        Err(e) => {
                            self.or_reuse = saved;
                            return Err(e);
                        }
                    };
                    out.push(r);
                    if i == 0 {
                        first = inner;
                    } else {
                        let mut a_names: Vec<&String> = inner.iter().map(|(n, _)| n).collect();
                        let mut f_names: Vec<&String> = first.iter().map(|(n, _)| n).collect();
                        a_names.sort();
                        f_names.sort();
                        if a_names != f_names {
                            self.or_reuse = saved;
                            let list = |v: &[&String]| {
                                v.iter()
                                    .map(|n| format!("`{n}`"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            };
                            return self.err(a.span, format!("every alternative of a `|` pattern must bind the same names: the first binds {}, this one binds {}", if f_names.is_empty() { "nothing".into() } else { list(&f_names) }, if a_names.is_empty() { "nothing".into() } else { list(&a_names) }));
                        }
                    }
                }
                self.or_reuse = saved;
                binds.extend(first);
                Ok(Pat::Or(out))
            }
        }
    }

    /// A bare name in a pattern that names a unit variant of `ty` (or an
    /// imported/prelude variant like `None`).
    fn bare_variant(&mut self, id: &ast::Ident, ty: &Ty) -> Option<(AdtId, u32)> {
        let target = match ty.deref_all() {
            Ty::Adt(aid, _) => Some(*aid),
            _ => None,
        };
        if let Some(aid) = target {
            if let Some(idx) = self.ck.adts[aid as usize].variant_index(&id.name) {
                if self.ck.adts[aid as usize].variants[idx as usize].is_empty() {
                    return Some((aid, idx));
                }
            }
        }
        if let Some(Def::Variant(aid, idx)) = self.ck.lookup(self.module, &id.name) {
            return Some((aid, idx));
        }
        None
    }

    fn resolve_variant_path(
        &mut self,
        path: &[ast::Ident],
        ty: &Ty,
        span: crate::diag::Span,
    ) -> Result<(AdtId, u32)> {
        let target = match ty {
            Ty::Adt(aid, _) => Some(*aid),
            _ => None,
        };
        if path.len() == 1 {
            let name = &path[0];
            if let Some(aid) = target {
                if let Some(idx) = self.ck.adts[aid as usize].variant_index(&name.name) {
                    return Ok((aid, idx));
                }
                if self.ck.adts[aid as usize].name == name.name
                    && !self.ck.adts[aid as usize].is_enum()
                {
                    return Ok((aid, 0));
                }
            }
            return match self.ck.lookup(self.module, &name.name) {
                Some(Def::Variant(aid, idx)) => Ok((aid, idx)),
                Some(Def::Adt(aid)) if !self.ck.adts[aid as usize].is_enum() => Ok((aid, 0)),
                _ => self.err(
                    name.span,
                    format!(
                        "no variant named `{}` for type `{}`",
                        name.name,
                        self.ts(ty)
                    ),
                ),
            };
        }
        // Enum.Variant / module.Enum.Variant / module.Variant
        let mut e = ast::Expr {
            kind: E::Name(path[0].clone(), Vec::new()),
            span: path[0].span,
        };
        if path[0].name == "Self" {
            e.kind = E::SelfType(Vec::new());
        }
        for seg in &path[1..path.len() - 1] {
            let sp = e.span.to(seg.span);
            e = ast::Expr {
                kind: E::Field(Box::new(e), seg.clone(), Vec::new()),
                span: sp,
            };
        }
        let last = path.last().unwrap();
        match self.path(&e)? {
            Some(PathRes::Type(Ty::Adt(aid, _))) | Some(PathRes::GenericAdt(aid)) => {
                match self.ck.adts[aid as usize].variant_index(&last.name) {
                    Some(idx) => Ok((aid, idx)),
                    None => self.err(
                        last.span,
                        format!(
                            "enum `{}` has no variant `{}`",
                            self.ck.adts[aid as usize].name, last.name
                        ),
                    ),
                }
            }
            Some(PathRes::Module(m)) => {
                match self.ck.lookup_in(self.module, m, &last.name, last.span)? {
                    Def::Variant(aid, idx) => Ok((aid, idx)),
                    Def::Adt(aid) if !self.ck.adts[aid as usize].is_enum() => Ok((aid, 0)),
                    _ => self.err(last.span, format!("`{}` is not an enum variant", last.name)),
                }
            }
            _ => self.err(span, "expected an enum variant path"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn variant_pat(
        &mut self,
        aid: AdtId,
        idx: u32,
        args: Option<&[ast::Pattern]>,
        ty: &Ty,
        mode: BindMode,
        span: crate::diag::Span,
        binds: &mut Vec<(String, LocalId)>,
    ) -> Result<Pat> {
        let Ty::Adt(tid, targs) = ty else {
            return self.err(
                span,
                format!(
                    "this pattern matches `{}`, but the value has type `{}`",
                    self.ck.adts[aid as usize].name,
                    self.ts(ty)
                ),
            );
        };
        if *tid != aid {
            return self.err(
                span,
                format!(
                    "this pattern matches `{}`, but the value has type `{}`",
                    self.ck.adts[aid as usize].name,
                    self.ts(ty)
                ),
            );
        }
        let a = &self.ck.adts[aid as usize];
        let fields: Vec<Ty> = if a.is_enum() {
            a.variants[idx as usize]
                .iter()
                .map(|f| f.subst(targs))
                .collect()
        } else {
            a.fields.iter().map(|f| f.subst(targs)).collect()
        };
        if !a.is_enum() && a.module != self.module {
            if let super::AdtKind::Struct(fs) = &a.kind {
                if fs.iter().any(|f| f.vis != ast::Vis::Pub) {
                    return self.err(
                        span,
                        format!("cannot destructure `{}`: it has private fields", a.name),
                    );
                }
            }
        }
        let name = if a.is_enum() {
            a.variant_name(idx).to_string()
        } else {
            a.name.clone()
        };
        let args = args.unwrap_or(&[]);
        if args.len() != fields.len() {
            return self.err(
                span,
                format!(
                    "`{name}` has {} field(s), but the pattern has {}",
                    fields.len(),
                    args.len()
                ),
            );
        }
        let mut out = Vec::new();
        for (sp, ft) in args.iter().zip(&fields) {
            out.push(self.pattern(sp, ft, mode, binds)?);
        }
        Ok(Pat::Variant(idx, out))
    }

    /// Value of a literal pattern; returns (value, is_bool).
    fn lit_value(&mut self, e: &ast::Expr, ty: &Ty) -> Result<(i128, bool)> {
        match (&e.kind, ty) {
            (E::Bool(b), Ty::Bool) => Ok((*b as i128, true)),
            (E::Int(..) | E::Char(_) | E::Unary(UnOp::Neg, _), Ty::Int(_)) => {
                let v = self.expr_as(e, ty)?;
                match v.kind {
                    crate::xir::ExprKind::Int(n) => Ok((n, false)),
                    _ => self.err(e.span, "expected an integer literal"),
                }
            }
            (E::Str(_), _) => self.err(
                e.span,
                "string literal patterns are not supported yet; compare with `==`",
            ),
            (E::Float(..), _) => {
                self.err(e.span, "floating-point literal patterns are not allowed")
            }
            _ => self.err(
                e.span,
                format!("literal pattern does not match type `{}`", self.ts(ty)),
            ),
        }
    }
}
