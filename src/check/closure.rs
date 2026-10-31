//! Closures, function pointers and calls through them.
//!
//! A closure `|a| a + n` becomes:
//! * a unique type `Ty::Closure(id)`, a struct of its captured values
//!   (references to captured variables, or the values themselves for
//!   `move` closures), and
//! * a function `(env: &Closure, a) -> R` that reads captures through `env`
//!   (`&mut Closure` if the closure mutates its captures: an `FnMut`).
//!
//! Generic functions taking `F: Fn(A) -> R` are instantiated per closure
//! type, so calls are direct and inline like any other call. Closures
//! without captures, and named functions, are also `fn(A) -> R` values.

use std::collections::{BTreeSet, HashMap};

use super::expr::{x, FnCx, Scope, X};
use super::{ClosureDef, FnId, FnKey, FnOwner};
use crate::ast::{self, ExprKind as E, PatKind};
use crate::diag::{Result, Span};
use crate::types::Ty;
use crate::xir::{self, ExprKind as K, LocalId, Place, UseKind};

/// A captured variable, as seen from inside the closure body.
#[derive(Clone, Debug)]
pub struct Capture {
    pub name: String,
    pub by_ref: bool,
}

#[derive(Clone, Debug)]
pub struct CaptureCx {
    pub env: LocalId,
    pub caps: Vec<Capture>,
    /// For captures typed by a defaulted literal: that literal's key.
    pub keys: Vec<Option<Span>>,
}

/// Return type of a closure whose result type is still being inferred.
pub const UNKNOWN_RET: Ty = Ty::Param(u32::MAX);

impl FnCx<'_> {
    // ------------------------------------------------------- captures

    /// Place of captured variable `name` inside a closure body.
    pub fn capture_place(&self, name: &str, span: Span) -> Option<(Place, Ty)> {
        let cx = self.captures.as_ref()?;
        let i = cx.caps.iter().position(|c| c.name == name)?;
        let env_ty = self.locals[cx.env as usize].ty.clone();
        let field = match &env_ty {
            // Fn / FnMut: the environment is behind `&env` / `&mut env`
            Ty::Ref(..) => {
                let env = x(
                    K::Use(Place::Local(cx.env), UseKind::Copy),
                    env_ty.clone(),
                    span,
                );
                Place::Field(Box::new(Place::Deref(Box::new(env))), i as u32)
            }
            // FnOnce: the closure owns its environment
            _ => Place::Field(Box::new(Place::Local(cx.env)), i as u32),
        };
        let cl = match &env_ty {
            Ty::Ref(cl, _) => (**cl).clone(),
            t => t.clone(),
        };
        let ft = self.ck.fields_of(&cl)?[i].clone();
        if cx.caps[i].by_ref {
            let Ty::Ref(inner, _) = &ft else { return None };
            let inner = (**inner).clone();
            let r = x(K::Use(field, UseKind::Copy), ft, span);
            Some((Place::Deref(Box::new(r)), inner))
        } else {
            Some((field, ft))
        }
    }

    /// The defaulted-literal key of a captured variable read through `p`.
    pub fn capture_key(&self, p: &Place) -> Option<Span> {
        let cx = self.captures.as_ref()?;
        let field = match p {
            Place::Deref(e) => match &e.kind {
                K::Use(f @ Place::Field(..), _) => f,
                _ => return None,
            },
            f @ Place::Field(..) => f,
            _ => return None,
        };
        let Place::Field(b, i) = field else {
            return None;
        };
        let direct = matches!(&**b, Place::Local(l) if *l == cx.env);
        let through_ref = matches!(&**b, Place::Deref(e) if matches!(&e.kind, K::Use(Place::Local(l), _) if *l == cx.env));
        if !direct && !through_ref {
            return None;
        }
        cx.keys.get(*i as usize).copied().flatten()
    }

    pub fn is_capture(&self, name: &str) -> bool {
        self.captures
            .as_ref()
            .is_some_and(|c| c.caps.iter().any(|c| c.name == name))
    }

    /// Record that `p` is used mutably; if it is (part of) a captured
    /// variable, the closure must capture it by `&mut` / be `FnMut`.
    pub fn note_mut(&mut self, p: &Place) {
        let Some(cx) = &self.captures else { return };
        let env = cx.env;
        if let Some(i) = capture_field_of(p, env) {
            self.mut_captures.insert(i);
        }
    }

    /// Variables of the enclosing function used in `e`, in first-use order.
    fn free_names(&self, e: &ast::Expr, bound: &mut Vec<String>, out: &mut Vec<String>) {
        let outer = |cx: &Self, n: &str| cx.lookup_local(n).is_some() || cx.is_capture(n);
        let add = |n: &str, bound: &Vec<String>, out: &mut Vec<String>, cx: &Self| {
            if !bound.iter().any(|b| b == n) && outer(cx, n) && !out.iter().any(|o| o == n) {
                out.push(n.to_string());
            }
        };
        match &e.kind {
            E::Name(id, targs) if targs.is_empty() => add(&id.name, bound, out, self),
            E::SelfValue => add("self", bound, out, self),
            E::Block(b) | E::Unsafe(b) | E::Loop(b) => self.free_block(b, bound, out),
            E::Closure(c) => {
                let n = bound.len();
                bound.extend(c.params.iter().map(|p| p.name.name.clone()));
                self.free_names(&c.body, bound, out);
                bound.truncate(n);
            }
            E::Match(s, arms) => {
                self.free_names(s, bound, out);
                for a in arms {
                    let n = bound.len();
                    pat_names(&a.pat, bound);
                    if let Some(g) = &a.guard {
                        self.free_names(g, bound, out);
                    }
                    self.free_names(&a.body, bound, out);
                    bound.truncate(n);
                }
            }
            E::For(pat, iter, body) => {
                self.free_names(iter, bound, out);
                let n = bound.len();
                pat_names(pat, bound);
                self.free_block(body, bound, out);
                bound.truncate(n);
            }
            E::If(cond, then, els) => {
                self.free_cond_block(cond, then, bound, out);
                if let Some(els) = els {
                    self.free_names(els, bound, out);
                }
            }
            E::While(cond, body) => self.free_cond_block(cond, body, bound, out),
            E::Catch(inner, binding, handler) => {
                self.free_names(inner, bound, out);
                let n = bound.len();
                if let Some(b) = binding {
                    bound.push(b.name.clone());
                }
                self.free_block(handler, bound, out);
                bound.truncate(n);
            }
            _ => ast_children(e, &mut |c| self.free_names(c, bound, out)),
        }
    }

    /// `if`/`while` heads may bind names (`if x is Some(v) { v }`).
    fn free_cond_block(
        &self,
        cond: &ast::Expr,
        body: &ast::Block,
        bound: &mut Vec<String>,
        out: &mut Vec<String>,
    ) {
        let n = bound.len();
        if let E::Is(s, p) = &cond.kind {
            self.free_names(s, bound, out);
            pat_names(p, bound);
        } else {
            self.free_names(cond, bound, out);
        }
        self.free_block(body, bound, out);
        bound.truncate(n);
    }

    fn free_block(&self, b: &ast::Block, bound: &mut Vec<String>, out: &mut Vec<String>) {
        let n = bound.len();
        for s in &b.stmts {
            match s {
                ast::Stmt::Let { pat, init, .. } => {
                    self.free_names(init, bound, out);
                    pat_names(pat, bound);
                }
                ast::Stmt::Expr(e, _) => self.free_names(e, bound, out),
            }
        }
        bound.truncate(n);
    }

    // ------------------------------------------------------- closures

    pub fn closure_expr(
        &mut self,
        c: &ast::ClosureExpr,
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let hint = match expected {
            Some(Ty::FnPtr(ps, r)) if ps.len() == c.params.len() => {
                Some((ps.clone(), (**r).clone()))
            }
            _ => None,
        };
        let mut ptys = Vec::new();
        for (i, p) in c.params.iter().enumerate() {
            let t = match &p.ty {
                Some(t) => self.ck.resolve_type(t, self.module, &self.env)?,
                None => match &hint {
                    Some((ps, _)) if !ps[i].has_params() => ps[i].clone(),
                    _ => {
                        return self.err(
                            p.name.span,
                            format!("cannot infer the type of closure parameter `{}`; annotate it: `|{}: i32| ...`", p.name.name, p.name.name),
                        )
                    }
                },
            };
            self.check_sized(&t, p.name.span)?;
            ptys.push(t);
        }
        let declared_ret = match &c.ret {
            Some(t) => Some(self.ck.resolve_type(t, self.module, &self.env)?),
            None => match &hint {
                Some((_, r)) if !r.has_params() => Some(r.clone()),
                _ => None,
            },
        };

        // captured variables and where they live in the enclosing function
        let mut bound: Vec<String> = c.params.iter().map(|p| p.name.name.clone()).collect();
        let mut names = Vec::new();
        self.free_names(&c.body, &mut bound, &mut names);
        let mut caps: Vec<(String, Place, Ty)> = Vec::new();
        let mut keys: Vec<Option<Span>> = Vec::new();
        for n in &names {
            let e = if n == "self" {
                ast::Expr {
                    kind: E::SelfValue,
                    span,
                }
            } else {
                ast::Expr {
                    kind: E::Name(
                        ast::Ident {
                            name: n.clone(),
                            span,
                        },
                        Vec::new(),
                    ),
                    span,
                }
            };
            if let Some((p, t)) = self.place(&e)? {
                keys.push(match &p {
                    Place::Local(l) => self.defaulted.get(l).copied(),
                    other => self.capture_key(other),
                });
                caps.push((n.clone(), p, t));
            }
        }

        // the same closure keeps its id when the function is re-checked
        let key = (self.fn_name.clone(), span);
        let cid = match self.ck.closure_ids.get(&key) {
            Some(&id) => id,
            None => {
                let id = self.ck.closures.len() as u32;
                self.ck.closures.push(ClosureDef {
                    fields: Vec::new(),
                    params: Vec::new(),
                    ret: Ty::unit(),
                    fn_name: String::new(),
                    kind: 0,
                });
                self.ck.closure_ids.insert(key, id);
                id
            }
        };
        let fn_name = format!("{}.closure#{}", self.fn_name, cid);

        // check the body, upgrading captures to `&mut` where it mutates them
        let mut mut_set: BTreeSet<u32> = BTreeSet::new();
        let mut once = false;
        let (body, locals, params, pending) = loop {
            let fields: Vec<Ty> = caps
                .iter()
                .enumerate()
                .map(|(i, (_, _, t))| {
                    if c.is_move {
                        t.clone()
                    } else {
                        Ty::reference(t.clone(), mut_set.contains(&(i as u32)))
                    }
                })
                .collect();
            let kind = if once {
                2
            } else if mut_set.is_empty() {
                0
            } else {
                1
            };
            self.ck.closures[cid as usize] = ClosureDef {
                fields: fields.clone(),
                params: ptys.clone(),
                ret: declared_ret.clone().unwrap_or(Ty::unit()),
                fn_name: fn_name.clone(),
                kind,
            };
            let capture_list: Vec<Capture> = caps
                .iter()
                .map(|(n, _, _)| Capture {
                    name: n.clone(),
                    by_ref: !c.is_move,
                })
                .collect();
            let (body, locals, params, used_mut, pending) = self.closure_body(
                cid,
                &fn_name,
                c,
                &ptys,
                declared_ret.clone(),
                capture_list,
                keys.clone(),
                kind,
                span,
            )?;
            // a `move` closure that moves a capture out owns its environment
            let mut moved = BTreeSet::new();
            if let Some(&env) = params.first().filter(|_| !caps.is_empty()) {
                moved_captures(&body, env, self.ck, &mut moved);
            }
            let now_once = c.is_move && !moved.is_empty();
            if used_mut.is_subset(&mut_set) && now_once == once {
                break (body, locals, params, pending);
            }
            mut_set.extend(used_mut);
            once = now_once || once;
        };
        let ret = declared_ret.unwrap_or_else(|| {
            if body.ty.is_never() {
                Ty::Never
            } else {
                body.ty.clone()
            }
        });
        self.ck.closures[cid as usize].ret = ret.clone();
        self.pending_funcs.extend(pending);
        self.pending_funcs.push(xir::Func {
            name: fn_name,
            params,
            ret,
            locals,
            body,
            export: false,
            span,
        });

        // the closure value: its captured references or values
        let mut vals = Vec::new();
        for (i, (_, p, t)) in caps.into_iter().enumerate() {
            if c.is_move {
                vals.push(self.use_place(p, t, span));
            } else {
                let m = mut_set.contains(&(i as u32));
                if m {
                    self.note_mut(&p);
                }
                vals.push(x(K::Borrow(p, m), Ty::reference(t, m), span));
            }
        }
        Ok(x(K::Struct(vals), Ty::Closure(cid), span))
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn closure_body(
        &mut self,
        cid: u32,
        fn_name: &str,
        c: &ast::ClosureExpr,
        ptys: &[Ty],
        ret: Option<Ty>,
        caps: Vec<Capture>,
        keys: Vec<Option<Span>>,
        kind: u8,
        span: Span,
    ) -> Result<(
        X,
        Vec<xir::Local>,
        Vec<LocalId>,
        BTreeSet<u32>,
        Vec<xir::Func>,
    )> {
        let has_env = !caps.is_empty();
        let mut inner = FnCx {
            ck: &mut *self.ck,
            module: self.module,
            env: self.env.clone(),
            locals: Vec::new(),
            scopes: vec![Scope::default()],
            ret_ty: ret.clone().unwrap_or(UNKNOWN_RET),
            loops: Vec::new(),
            next_loop: 0,
            unsafe_depth: 0,
            overrides: self.overrides.clone(),
            defaulted: HashMap::new(),
            pending: None,
            fn_name: fn_name.to_string(),
            captures: None,
            mut_captures: BTreeSet::new(),
            pending_funcs: Vec::new(),
            arg_hints: Vec::new(),
        };
        let mut params = Vec::new();
        if has_env {
            let env_ty = match kind {
                0 => Ty::reference(Ty::Closure(cid), false),
                1 => Ty::reference(Ty::Closure(cid), true),
                _ => Ty::Closure(cid),
            };
            let env = inner.new_local("env", env_ty, kind == 2, span, true);
            params.push(env);
            inner.captures = Some(CaptureCx { env, caps, keys });
        }
        for (p, t) in c.params.iter().zip(ptys) {
            let id = inner.new_local(&p.name.name, t.clone(), p.mutable, p.name.span, false);
            if p.name.name != "_" {
                inner.declare(&p.name.name, id);
            }
            params.push(id);
        }
        let body = inner.expr(&c.body, ret.as_ref());
        let body = body.and_then(|b| match &ret {
            Some(r) => inner.coerce(b, r, c.body.span),
            None => Ok(b),
        });
        // a literal of the enclosing function that needs another type
        let pending = inner.pending.take();
        let (locals, mut_captures, funcs) = (inner.locals, inner.mut_captures, inner.pending_funcs);
        if self.pending.is_none() {
            self.pending = pending;
        }
        Ok((body?, locals, params, mut_captures, funcs))
    }

    // ------------------------------------------------------- calls

    /// Call a value: a closure or a function pointer.
    pub fn call_place(&mut self, p: Place, t: Ty, args: &[ast::Expr], span: Span) -> Result<X> {
        let (p, t) = self.autoderef(p, t, span);
        match t.clone() {
            Ty::Closure(cid) => {
                let def = self.ck.closures[cid as usize].clone();
                if args.len() != def.params.len() {
                    return self.err(
                        span,
                        format!(
                            "this closure takes {} argument(s), but {} were given",
                            def.params.len(),
                            args.len()
                        ),
                    );
                }
                let mut xs = Vec::new();
                if !def.fields.is_empty() {
                    match def.kind {
                        // FnOnce: the call consumes the closure
                        2 => xs.push(self.use_place(p, t, span)),
                        k => {
                            let m = k == 1;
                            if m {
                                self.note_mut(&p);
                            }
                            xs.push(x(K::Borrow(p, m), Ty::reference(t, m), span));
                        }
                    }
                }
                for (a, pt) in args.iter().zip(&def.params) {
                    xs.push(self.expr_as(a, pt)?);
                }
                Ok(x(K::Call(def.fn_name, xs), def.ret, span))
            }
            Ty::FnPtr(ps, r) => {
                if args.len() != ps.len() {
                    return self.err(
                        span,
                        format!(
                            "this function takes {} argument(s), but {} were given",
                            ps.len(),
                            args.len()
                        ),
                    );
                }
                let f = x(K::Use(p, UseKind::Copy), t, span);
                let mut xs = Vec::new();
                for (a, pt) in args.iter().zip(&ps) {
                    xs.push(self.expr_as(a, pt)?);
                }
                Ok(x(K::CallPtr(Box::new(f), xs), *r, span))
            }
            other => self.err(
                span,
                format!("`{}` is not a function or closure", self.ts(&other)),
            ),
        }
    }

    /// `f(args)` where `f` is not a named function.
    pub fn call_value(&mut self, callee: &ast::Expr, args: &[ast::Expr], span: Span) -> Result<X> {
        let (p, t) = self.place_or_temp(callee)?;
        self.call_place(p, t, args, span)
    }

    /// A named function used as a value: `apply(double, 3)`.
    pub fn fn_value(&mut self, fid: FnId, targs: Vec<Ty>, span: Span) -> Result<X> {
        let f = &self.ck.fns[fid as usize];
        let fname = f.decl.name.name.clone();
        if f.decl.is_unsafe || f.decl.body.is_none() {
            return self.err(
                span,
                format!("unsafe or foreign function `{fname}` cannot be used as a value"),
            );
        }
        if f.owner != FnOwner::Free {
            return self.err(
                span,
                format!("method `{fname}` cannot be used as a value yet; wrap it in a closure"),
            );
        }
        let n = f.decl.generics.params.len();
        if n != targs.len() {
            return self.err(span, format!("`{fname}` is generic; give its type arguments to use it as a value, e.g. `{fname}<i32>`"));
        }
        let generics = f.decl.generics.params.clone();
        let module = f.module;
        let (params, ret, _, _) = self.signature(fid, None)?;
        self.ck.check_bounds(&generics, &targs, module, span)?;
        let ps: Vec<Ty> = params.iter().map(|t| t.subst(&targs)).collect();
        let r = ret.subst(&targs);
        let name = self.ck.instance(FnKey {
            fid,
            targs,
            self_ty: None,
        });
        Ok(x(K::FnRef(name), Ty::FnPtr(ps, Box::new(r)), span))
    }

    /// A closure without captures can be used where a `fn(...)` is expected.
    pub fn closure_to_fn_ptr(&self, v: &X, expected: &Ty) -> Option<X> {
        let (Ty::Closure(cid), Ty::FnPtr(ps, r)) = (&v.ty, expected) else {
            return None;
        };
        let def = &self.ck.closures[*cid as usize];
        if def.fields.is_empty() && def.params == *ps && def.ret == **r {
            return Some(x(K::FnRef(def.fn_name.clone()), expected.clone(), v.span));
        }
        None
    }

    /// `fn(...)` hints for parameters bounded by `Fn(...)`, so a closure
    /// argument can infer its parameter types.
    pub fn fn_bound_hints(&mut self, fid: FnId, params: &[Ty], n_impl: usize) -> Vec<Option<Ty>> {
        let f = &self.ck.fns[fid as usize];
        let generics = f.decl.generics.params.clone();
        let module = f.module;
        let owner = f.owner;
        let mut env = super::Env::default();
        if let FnOwner::Impl(iid) = owner {
            let im = &self.ck.impls[iid as usize];
            for (i, g) in im.generics.iter().enumerate() {
                env.names.insert(g.name.name.clone(), Ty::Param(i as u32));
            }
            env.self_ty = Some(im.self_ty.clone());
        }
        for (j, g) in generics.iter().enumerate() {
            env.names
                .insert(g.name.name.clone(), Ty::Param((n_impl + j) as u32));
        }
        params
            .iter()
            .map(|p| {
                let Ty::Param(k) = p else { return None };
                let j = (*k as usize).checked_sub(n_impl)?;
                let g = generics.get(j)?;
                g.bounds.iter().find_map(|b| {
                    let ast::TypeKind::FnTrait(_, ps, r) = &b.kind else {
                        return None;
                    };
                    let ps: Vec<Ty> = ps
                        .iter()
                        .map(|t| self.ck.resolve_type(t, module, &env).ok())
                        .collect::<Option<_>>()?;
                    let r = match r {
                        Some(r) => self.ck.resolve_type(r, module, &env).ok()?,
                        None => Ty::unit(),
                    };
                    Some(Ty::FnPtr(ps, Box::new(r)))
                })
            })
            .collect()
    }

    /// `obj.field(args)` where `field` holds a closure or function pointer.
    pub fn call_field(
        &mut self,
        base: &ast::Expr,
        name: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<Option<X>> {
        let fe = ast::Expr {
            kind: E::Field(Box::new(base.clone()), name.clone(), Vec::new()),
            span: base.span.to(name.span),
        };
        let Ok(Some((p, t))) = self.place(&fe) else {
            return Ok(None);
        };
        if self.ck.callable_sig(t.deref_all()).is_none() {
            return Ok(None);
        }
        self.call_place(p, t, args, span).map(Some)
    }
}

/// The capture (environment field) a place inside a closure body belongs to.
fn capture_field_of(p: &Place, env: LocalId) -> Option<u32> {
    match p {
        Place::Field(b, i) => {
            if matches!(&**b, Place::Local(l) if *l == env) {
                return Some(*i);
            }
            if let Place::Deref(e) = &**b {
                if matches!(&e.kind, K::Use(Place::Local(l), _) if *l == env) {
                    return Some(*i);
                }
            }
            capture_field_of(b, env)
        }
        Place::Index(b, _) | Place::Slice(b, ..) => capture_field_of(b, env),
        Place::Deref(e) => match &e.kind {
            K::Use(inner, _) => capture_field_of(inner, env),
            _ => None,
        },
        _ => None,
    }
}

/// Captures a closure body moves out of (making the closure `FnOnce`).
fn moved_captures(e: &X, env: LocalId, ck: &super::Checker, out: &mut BTreeSet<u32>) {
    let moving_pat = |p: &xir::Pat| pat_moves(p, ck);
    match &e.kind {
        K::Use(p, UseKind::Move) | K::Drop(p) => {
            if let Some(i) = capture_field_of(p, env) {
                out.insert(i);
            }
        }
        K::Match(p, arms) if arms.iter().any(|a| moving_pat(&a.pat)) => {
            if let Some(i) = capture_field_of(p, env) {
                out.insert(i);
            }
        }
        _ => {}
    }
    super::xir_children(e, &mut |c| moved_captures(c, env, ck, out));
}

fn pat_moves(p: &xir::Pat, ck: &super::Checker) -> bool {
    match p {
        xir::Pat::Bind(_, xir::BindMode::Value) => true,
        xir::Pat::Tuple(ps) | xir::Pat::Variant(_, ps) | xir::Pat::Or(ps) => {
            ps.iter().any(|q| pat_moves(q, ck))
        }
        _ => false,
    }
}

/// Names bound by a pattern.
fn pat_names(p: &ast::Pattern, out: &mut Vec<String>) {
    match &p.kind {
        PatKind::Bind(id, _) => out.push(id.name.clone()),
        PatKind::Tuple(ps) | PatKind::Or(ps) => ps.iter().for_each(|p| pat_names(p, out)),
        PatKind::Variant(_, Some(ps)) => ps.iter().for_each(|p| pat_names(p, out)),
        _ => {}
    }
}

/// Visit the direct sub-expressions of an AST expression.
pub fn ast_children(e: &ast::Expr, f: &mut dyn FnMut(&ast::Expr)) {
    let block = |b: &ast::Block, f: &mut dyn FnMut(&ast::Expr)| {
        for s in &b.stmts {
            match s {
                ast::Stmt::Let { init, .. } => f(init),
                ast::Stmt::Expr(e, _) => f(e),
            }
        }
    };
    match &e.kind {
        E::Int(..)
        | E::Float(..)
        | E::Str(_)
        | E::Char(_)
        | E::Bool(_)
        | E::Unit
        | E::Name(..)
        | E::SelfValue
        | E::SelfType(_)
        | E::Continue => {}
        E::Field(b, _, _) | E::TupleField(b, _, _) => f(b),
        E::Call(c, args) => {
            f(c);
            args.iter().for_each(|a| f(a));
        }
        E::Index(a, b)
        | E::Binary(_, a, b)
        | E::Assign(a, b)
        | E::CompoundAssign(_, a, b)
        | E::ArrayRepeat(a, b) => {
            f(a);
            f(b);
        }
        E::Unary(_, a)
        | E::Ref(_, a)
        | E::Deref(a)
        | E::Move(a)
        | E::Copy(a)
        | E::Drop(a)
        | E::Cast(a, _)
        | E::Try(a)
        | E::Throw(a) => f(a),
        E::Is(a, _) => f(a),
        E::StructLit(p, fields) => {
            f(p);
            fields.iter().for_each(|(_, v)| f(v));
        }
        E::Tuple(xs) | E::Array(xs) => xs.iter().for_each(|a| f(a)),
        E::Range(lo, hi, _) => {
            if let Some(lo) = lo {
                f(lo);
            }
            if let Some(hi) = hi {
                f(hi);
            }
        }
        E::Block(b) | E::Unsafe(b) | E::Loop(b) => block(b, f),
        E::If(c, t, e) => {
            f(c);
            block(t, f);
            if let Some(e) = e {
                f(e);
            }
        }
        E::While(c, b) => {
            f(c);
            block(b, f);
        }
        E::For(_, it, b) => {
            f(it);
            block(b, f);
        }
        E::Break(v) | E::Return(v) => {
            if let Some(v) = v {
                f(v);
            }
        }
        E::Match(s, arms) => {
            f(s);
            for a in arms {
                if let Some(g) = &a.guard {
                    f(g);
                }
                f(&a.body);
            }
        }
        E::Catch(a, _, b) => {
            f(a);
            block(b, f);
        }
        E::Closure(c) => f(&c.body),
    }
}
