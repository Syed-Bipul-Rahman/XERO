//! Type checking and lowering of function bodies.

use std::collections::HashMap;

use super::{Checker, Def, Env, FnId, FnKey, FnOwner, ImplId, IntrinsicFn, ModId};
use crate::ast::{self, BinOp, ExprKind as E, Ident, SelfKind, UnOp, Vis};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{match_template, AdtId, FloatTy, IntTy, Ty};
use crate::xir::{self, BindMode, ExprKind as K, LocalId, Pat, Place, UseKind};

pub type X = xir::Expr;

pub struct LoopCx {
    pub id: u32,
    pub break_ty: Option<Ty>,
    pub allows_value: bool,
}

#[derive(Default)]
pub struct Scope {
    pub names: Vec<(String, LocalId)>,
    pub locals: Vec<LocalId>,
}

pub struct FnCx<'a> {
    pub ck: &'a mut Checker,
    pub module: ModId,
    pub env: Env,
    pub locals: Vec<xir::Local>,
    pub scopes: Vec<Scope>,
    pub ret_ty: Ty,
    pub loops: Vec<LoopCx>,
    pub next_loop: u32,
    pub unsafe_depth: u32,
    /// Literal-typing retries: types chosen for literals that defaulted to
    /// `i32`/`f64` in an earlier attempt, keyed by the literal's span.
    pub overrides: HashMap<Span, Ty>,
    /// Locals whose type came from a defaulted literal, with that literal's key.
    pub defaulted: HashMap<LocalId, Span>,
    /// First defaulted literal found to need a different type.
    pub pending: Option<(Span, Ty)>,
    /// Symbol of the function being checked (names its closures).
    pub fn_name: String,
    /// Inside a closure body: how captured variables are reached.
    pub captures: Option<super::closure::CaptureCx>,
    /// Captured variables (by field index) the closure body mutates.
    pub mut_captures: std::collections::BTreeSet<u32>,
    /// Closure functions created while checking; emitted with this function.
    pub pending_funcs: Vec<xir::Func>,
    /// Expected `fn(...)` shapes for arguments bounded by `Fn(...)`.
    pub arg_hints: Vec<Option<Ty>>,
}

/// What a path-like expression (`a`, `a.b`, `Type.member`) refers to.
pub enum PathRes {
    Module(ModId),
    /// A type; `None` args means "generic, infer the arguments".
    Type(Ty),
    GenericAdt(AdtId),
    Def(Def, Vec<Ty>, Span),
    /// `Type.member`
    Member(Ty, Option<AdtId>, Ident, Vec<Ty>),
}

/// Receiver of a method call.
enum Recv {
    Place(Place, Ty),
    Value(X),
}

pub fn x(kind: K, ty: Ty, span: Span) -> X {
    X { kind, ty, span }
}

/// Check a function. Untyped literals default to `i32`/`f64`; when such a
/// literal's variable later meets a different numeric type, the function is
/// re-checked with that type for the literal (bounded number of attempts).
pub fn check_fn(
    ck: &mut Checker,
    key: &FnKey,
    name: String,
) -> Result<(xir::Func, Vec<xir::Func>)> {
    let mut overrides = HashMap::new();
    for _ in 0..32 {
        match check_fn_once(ck, key, name.clone(), &overrides) {
            (Ok(f), _) => return Ok(f),
            (Err(d), Some((k, t))) if !overrides.contains_key(&k) => {
                let _ = d;
                overrides.insert(k, t);
            }
            (Err(d), _) => return Err(d),
        }
    }
    check_fn_once(ck, key, name, &overrides).0
}

fn check_fn_once(
    ck: &mut Checker,
    key: &FnKey,
    name: String,
    overrides: &HashMap<Span, Ty>,
) -> (Result<(xir::Func, Vec<xir::Func>)>, Option<(Span, Ty)>) {
    let mut pending = None;
    let r = check_fn_inner(ck, key, name, overrides, &mut pending);
    (r, pending)
}

fn check_fn_inner(
    ck: &mut Checker,
    key: &FnKey,
    name: String,
    overrides: &HashMap<Span, Ty>,
    pending_out: &mut Option<(Span, Ty)>,
) -> Result<(xir::Func, Vec<xir::Func>)> {
    let fdef = &ck.fns[key.fid as usize];
    let decl = fdef.decl.clone();
    let module = fdef.module;
    let owner = fdef.owner;
    let mut env = Env::default();
    let mut fn_targs = &key.targs[..];
    match owner {
        FnOwner::Free => {}
        FnOwner::Impl(iid) => {
            let im = &ck.impls[iid as usize];
            let n = im.generics.len();
            for (g, t) in im.generics.iter().zip(&key.targs[..n]) {
                env.names.insert(g.name.name.clone(), t.clone());
            }
            env.self_ty = Some(im.self_ty.subst(&key.targs[..n]));
            fn_targs = &key.targs[n..];
        }
        FnOwner::Trait(_) => env.self_ty = key.self_ty.clone(),
    }
    for (g, t) in decl.generics.params.iter().zip(fn_targs) {
        env.names.insert(g.name.name.clone(), t.clone());
    }
    let ret_ty = match &decl.ret {
        Some(t) => ck.resolve_type(t, module, &env)?,
        None => Ty::unit(),
    };
    let mut cx = FnCx {
        ck,
        module,
        env,
        locals: Vec::new(),
        scopes: vec![Scope::default()],
        ret_ty: ret_ty.clone(),
        loops: Vec::new(),
        next_loop: 0,
        unsafe_depth: 0,
        overrides: overrides.clone(),
        defaulted: HashMap::new(),
        pending: None,
        fn_name: name.clone(),
        captures: None,
        mut_captures: std::collections::BTreeSet::new(),
        pending_funcs: Vec::new(),
        arg_hints: Vec::new(),
    };
    if decl.is_unsafe {
        cx.unsafe_depth = 1;
    }
    let mut params = Vec::new();
    if let Some((kind, mutable, span)) = decl.self_param {
        let st = cx.env.self_ty.clone().unwrap();
        let ty = match kind {
            SelfKind::Value => st,
            SelfKind::Ref => Ty::reference(st, false),
            SelfKind::RefMut => Ty::reference(st, true),
        };
        let id = cx.new_local("self", ty, mutable, span, false);
        cx.declare("self", id);
        params.push(id);
    }
    for p in &decl.params {
        let ty = cx.ck.resolve_type(&p.ty, module, &cx.env)?;
        cx.check_sized(&ty, p.ty.span)?;
        let id = cx.new_local(&p.name.name, ty, p.mutable, p.name.span, false);
        cx.declare(&p.name.name, id);
        params.push(id);
    }
    if decl.is_export {
        for &p in &params {
            let t = cx.locals[p as usize].ty.clone();
            if !matches!(t, Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Ptr(_)) {
                return Err(Diagnostic::new(decl.span, "`export fn` parameters must be C-compatible (integers, floats, bool, raw pointers)"));
            }
        }
        if !matches!(ret_ty, Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Ptr(_)) && !ret_ty.is_unit()
        {
            return Err(Diagnostic::new(
                decl.span,
                "`export fn` must return a C-compatible type or nothing",
            ));
        }
    }
    let body_ast = decl.body.as_ref().unwrap();
    let body = cx.block(body_ast, Some(&ret_ty));
    let body = body.and_then(|b| cx.coerce(b, &ret_ty, body_ast.span));
    *pending_out = cx.pending.take();
    let body = body?;
    let closures = std::mem::take(&mut cx.pending_funcs);
    Ok((
        xir::Func {
            name,
            params,
            ret: ret_ty,
            locals: cx.locals,
            body,
            export: decl.is_export,
            span: decl.span,
        },
        closures,
    ))
}

impl<'a> FnCx<'a> {
    // ------------------------------------------------------------- helpers

    pub fn err<T>(&self, span: Span, msg: impl Into<String>) -> Result<T> {
        Err(Diagnostic::new(span, msg))
    }

    pub fn ts(&self, t: &Ty) -> String {
        self.ck.ty_str(t)
    }

    pub fn new_local(
        &mut self,
        name: &str,
        ty: Ty,
        mutable: bool,
        span: Span,
        synthetic: bool,
    ) -> LocalId {
        self.locals.push(xir::Local {
            name: name.to_string(),
            ty,
            mutable,
            span,
            synthetic,
        });
        (self.locals.len() - 1) as LocalId
    }

    pub fn declare(&mut self, name: &str, id: LocalId) {
        let s = self.scopes.last_mut().unwrap();
        s.names.push((name.to_string(), id));
        s.locals.push(id);
    }

    /// A compiler temporary owned by the innermost block.
    pub fn synthetic(&mut self, name: &str, ty: Ty, span: Span) -> LocalId {
        let id = self.new_local(name, ty, true, span, true);
        self.scopes.last_mut().unwrap().locals.push(id);
        id
    }

    pub fn lookup_local(&self, name: &str) -> Option<LocalId> {
        for s in self.scopes.iter().rev() {
            if let Some((_, id)) = s.names.iter().rev().find(|(n, _)| n == name) {
                return Some(*id);
            }
        }
        None
    }

    pub fn check_sized(&self, ty: &Ty, span: Span) -> Result<()> {
        if ty.is_unsized() {
            return self.err(
                span,
                format!(
                    "`{}` has no statically known size; use a reference like `&{}`",
                    self.ts(ty),
                    self.ts(ty)
                ),
            );
        }
        Ok(())
    }

    pub fn in_unsafe(&self) -> bool {
        self.unsafe_depth > 0
    }

    fn require_unsafe(&self, span: Span, what: &str) -> Result<()> {
        if !self.in_unsafe() {
            return self.err(
                span,
                format!("{what} is unsafe and requires an `unsafe {{ ... }}` block"),
            );
        }
        Ok(())
    }

    pub fn use_place(&self, p: Place, ty: Ty, span: Span) -> X {
        let kind = if self.ck.is_copy(&ty) {
            UseKind::Copy
        } else {
            UseKind::Move
        };
        x(K::Use(p, kind), ty, span)
    }

    /// Materialize an rvalue into a synthetic local and return a place for it.
    pub fn materialize(&mut self, v: X) -> Place {
        let span = v.span;
        let ty = v.ty.clone();
        let tmp = self.synthetic("tmp", ty.clone(), span);
        let block = xir::Block {
            stmts: vec![xir::Stmt::Let(tmp, v)],
            tail: Some(Box::new(x(
                K::Borrow(Place::Local(tmp), true),
                Ty::reference(ty.clone(), true),
                span,
            ))),
            scope: Vec::new(),
        };
        Place::Deref(Box::new(x(K::Block(block), Ty::reference(ty, true), span)))
    }

    /// Borrow an rvalue: the temporary lives until the end of the enclosing block.
    fn borrow_value(&mut self, v: X, mutable: bool) -> X {
        let span = v.span;
        let ty = v.ty.clone();
        let tmp = self.synthetic("tmp", ty.clone(), span);
        let block = xir::Block {
            stmts: vec![xir::Stmt::Let(tmp, v)],
            tail: Some(Box::new(x(
                K::Borrow(Place::Local(tmp), mutable),
                Ty::reference(ty.clone(), mutable),
                span,
            ))),
            scope: Vec::new(),
        };
        x(K::Block(block), Ty::reference(ty, mutable), span)
    }

    fn deref_place(&self, p: Place, ty: &Ty, span: Span) -> (Place, Ty) {
        match ty {
            Ty::Ref(inner, _) => (
                Place::Deref(Box::new(x(K::Use(p, UseKind::Copy), ty.clone(), span))),
                (**inner).clone(),
            ),
            _ => (p, ty.clone()),
        }
    }

    /// Follow references until reaching a non-reference type.
    pub fn autoderef(&self, mut p: Place, mut ty: Ty, span: Span) -> (Place, Ty) {
        while let Ty::Ref(..) = ty {
            let (np, nt) = self.deref_place(p, &ty, span);
            p = np;
            ty = nt;
        }
        (p, ty)
    }

    // ------------------------------------------------- literal type inference

    /// If `v`'s type comes from a defaulted literal, that literal's key.
    fn defaulted_expr(&self, v: &X) -> Option<Span> {
        match &v.kind {
            K::Use(Place::Local(l), _) => self.defaulted.get(l).copied(),
            K::Use(p @ Place::Index(..), _) => self.place_default_key(p),
            K::Use(p, _) if self.captures.is_some() => self.capture_key(p),
            K::Binary(op, a, b) if !op.is_comparison() && !matches!(op, BinOp::And | BinOp::Or) => {
                self.defaulted_expr(a).or_else(|| self.defaulted_expr(b))
            }
            K::Unary(UnOp::Neg, a) => self.defaulted_expr(a),
            _ => None,
        }
    }

    /// The literal key of a variable (or an element of an array variable,
    /// or a captured variable) typed by a defaulted literal.
    fn place_default_key(&self, p: &Place) -> Option<Span> {
        let root = match p {
            Place::Index(b, _) => &**b,
            p => p,
        };
        match root {
            Place::Local(l) => self.defaulted.get(l).copied(),
            other if self.captures.is_some() => self.capture_key(other),
            _ => None,
        }
    }

    /// Remember that the literal `key` should have had type `want`.
    fn want_literal_type(&mut self, key: Span, have: &Ty, want: &Ty) {
        let numeric = |t: &Ty| t.is_int() || t.is_float();
        if self.pending.is_none()
            && numeric(have)
            && numeric(want)
            && have != want
            && !self.overrides.contains_key(&key)
        {
            self.pending = Some((key, want.clone()));
        }
    }

    // ------------------------------------------------------------ coercion

    pub fn coerce(&mut self, v: X, expected: &Ty, span: Span) -> Result<X> {
        if v.ty != *expected {
            if let Some(k) = self.defaulted_expr(&v) {
                let have = v.ty.clone();
                self.want_literal_type(k, &have, expected);
            }
        }
        if let Some(fp) = self.closure_to_fn_ptr(&v, expected) {
            return Ok(fp);
        }
        if v.ty == *expected || v.ty.is_never() {
            if v.ty.is_never() && !expected.is_never() {
                let mut v = v;
                v.ty = expected.clone();
                return Ok(v);
            }
            return Ok(v);
        }
        match (&v.ty, expected) {
            // &[T; N] -> &[T]
            (Ty::Ref(a, m1), Ty::Ref(b, m2)) if (*m1 || !*m2) => {
                if let (Ty::Array(e1, _), Ty::Slice(e2)) = (&**a, &**b) {
                    if e1 == e2 {
                        let vs = v.span;
                        let ty = expected.clone();
                        let v = if *m1 && !*m2 { self.weaken(v) } else { v };
                        return Ok(x(K::ArrayToSlice(Box::new(v)), ty, vs));
                    }
                }
                // &mut T -> &T
                if a == b && *m1 && !*m2 {
                    return Ok(self.weaken(v));
                }
            }
            _ => {}
        }
        if expected.is_unit() {
            // value in statement context: evaluate and discard
            let vs = v.span;
            let block = xir::Block {
                stmts: vec![xir::Stmt::Expr(v)],
                tail: None,
                scope: Vec::new(),
            };
            return Ok(x(K::Block(block), Ty::unit(), vs));
        }
        if let (Ty::Closure(_), Ty::FnPtr(..)) = (&v.ty, expected) {
            return self.err(span, format!("this closure captures variables, so it cannot be used as the plain function pointer `{}`; take a generic `F: Fn(...)` instead", self.ts(expected)));
        }
        self.err(
            span,
            format!(
                "mismatched types: expected `{}`, found `{}`",
                self.ts(expected),
                self.ts(&v.ty)
            ),
        )
    }

    /// `&mut T` -> `&T` (a shared reborrow).
    fn weaken(&mut self, v: X) -> X {
        let span = v.span;
        let Ty::Ref(inner, _) = v.ty.clone() else {
            return v;
        };
        let p = Place::Deref(Box::new(v));
        x(K::Borrow(p, false), Ty::Ref(inner, false), span)
    }

    /// `expr_as` for a value stored into `place`: if the place is a variable
    /// typed by a defaulted literal, a differently typed value retypes it.
    fn expr_as_into(&mut self, place: &Place, e: &ast::Expr, expected: &Ty) -> Result<X> {
        // `arr[i] = v` where `arr` came from a literal array: retype its elements
        {
            if let Some(k) = self.place_default_key(place) {
                let v = self.expr(e, Some(expected))?;
                if v.ty != *expected && !v.ty.is_never() {
                    let found = v.ty.clone();
                    self.want_literal_type(k, expected, &found);
                }
                return self.coerce(v, expected, e.span);
            }
        }
        self.expr_as(e, expected)
    }

    pub fn expr_as(&mut self, e: &ast::Expr, expected: &Ty) -> Result<X> {
        let v = self.expr(e, Some(expected))?;
        self.coerce(v, expected, e.span)
    }

    // -------------------------------------------------------------- blocks

    pub fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    pub fn pop_scope(&mut self) -> Vec<LocalId> {
        self.scopes.pop().unwrap().locals
    }

    pub fn block(&mut self, b: &ast::Block, expected: Option<&Ty>) -> Result<X> {
        self.push_scope();
        let r = self.block_inner(b, expected);
        let scope = self.pop_scope();
        let (stmts, tail) = r?;
        let ty = match &tail {
            Some(t) => t.ty.clone(),
            None => {
                if stmts.iter().any(|s| {
                    matches!(s, xir::Stmt::Expr(e) if e.ty.is_never())
                        || matches!(s, xir::Stmt::Let(_, e) if e.ty.is_never())
                }) {
                    Ty::Never
                } else {
                    Ty::unit()
                }
            }
        };
        Ok(x(
            K::Block(xir::Block {
                stmts,
                tail: tail.map(Box::new),
                scope,
            }),
            ty,
            b.span,
        ))
    }

    fn block_inner(
        &mut self,
        b: &ast::Block,
        expected: Option<&Ty>,
    ) -> Result<(Vec<xir::Stmt>, Option<X>)> {
        let mut stmts = Vec::new();
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            let last = i + 1 == n;
            match s {
                ast::Stmt::Let {
                    pat,
                    ty,
                    init,
                    span,
                } => {
                    let declared = match ty {
                        Some(t) => {
                            let t = self.ck.resolve_type(t, self.module, &self.env)?;
                            Some(t)
                        }
                        None => None,
                    };
                    // an un-annotated literal may get its type from a later use
                    let literal_key = if declared.is_none() && untyped_literal(init) {
                        Some(init.span)
                    } else {
                        None
                    };
                    // `[0; 4]` / `[1, 2]`: the element literals can be retyped too
                    let array_key = if declared.is_none() && literal_array(init) {
                        Some(init.span)
                    } else {
                        None
                    };
                    let hinted = literal_key.and_then(|k| self.overrides.get(&k).cloned());
                    let elem_hint = array_key.and_then(|k| self.overrides.get(&k).cloned());
                    let v = match (&declared, &hinted, &elem_hint) {
                        (Some(t), _, _) | (None, Some(t), _) => {
                            let t = t.clone();
                            let v = self.expr(init, Some(&t))?;
                            self.coerce(v, &t, init.span)?
                        }
                        (None, None, Some(e)) => {
                            self.expr(init, Some(&Ty::Array(Box::new(e.clone()), 0)))?
                        }
                        (None, None, None) => self.expr(init, None)?,
                    };
                    let literal_key = literal_key.or(array_key);
                    let default_key = match literal_key {
                        Some(k) => Some(k),
                        None if declared.is_none() => self.defaulted_expr(&v),
                        None => None,
                    };
                    let vty = v.ty.clone();
                    if vty.is_never() {
                        stmts.push(xir::Stmt::Expr(v));
                        continue;
                    }
                    self.check_sized(&vty, *span)?;
                    match &pat.kind {
                        ast::PatKind::Bind(id, mutable) => {
                            let lid = self.new_local(&id.name, vty, *mutable, id.span, false);
                            stmts.push(xir::Stmt::Let(lid, v));
                            self.declare(&id.name, lid);
                            if let Some(k) = default_key {
                                self.defaulted.insert(lid, k);
                            }
                        }
                        ast::PatKind::Wild => {
                            let lid = self.synthetic("_", vty, pat.span);
                            stmts.push(xir::Stmt::Let(lid, v));
                        }
                        _ => {
                            let mut binds = Vec::new();
                            let p = self.pattern(pat, &vty, BindMode::Value, &mut binds)?;
                            stmts.push(xir::Stmt::LetPat(p, v));
                            for (name, id) in binds {
                                self.declare(&name, id);
                            }
                        }
                    }
                }
                ast::Stmt::Expr(e, semi) => {
                    if last && !semi {
                        let v = self.expr(e, expected)?;
                        return Ok((stmts, Some(v)));
                    }
                    let stmt_ctx = matches!(
                        e.kind,
                        E::If(..)
                            | E::Match(..)
                            | E::Block(_)
                            | E::Unsafe(_)
                            | E::Loop(_)
                            | E::While(..)
                            | E::For(..)
                    );
                    let v = if stmt_ctx {
                        self.expr(e, Some(&Ty::unit()))?
                    } else {
                        self.expr(e, None)?
                    };
                    let v = if stmt_ctx && !v.ty.is_unit() && !v.ty.is_never() {
                        self.coerce(v, &Ty::unit(), e.span)?
                    } else {
                        v
                    };
                    stmts.push(xir::Stmt::Expr(v));
                }
            }
        }
        Ok((stmts, None))
    }

    // --------------------------------------------------------- expressions

    pub fn expr(&mut self, e: &ast::Expr, expected: Option<&Ty>) -> Result<X> {
        let span = e.span;
        match &e.kind {
            E::Int(v, suffix) => self.int_lit(*v as i128, suffix.as_deref(), expected, span),
            E::Float(v, suffix) => {
                let ty = match (suffix.as_deref(), expected) {
                    (Some("f32"), _) => Ty::Float(FloatTy::F32),
                    (Some("f64"), _) => Ty::Float(FloatTy::F64),
                    (_, Some(Ty::Float(f))) => Ty::Float(*f),
                    _ => Ty::Float(FloatTy::F64),
                };
                Ok(x(K::Float(*v), ty, span))
            }
            E::Char(c) => {
                let ty = match expected {
                    Some(Ty::Int(it)) => Ty::Int(*it),
                    _ => Ty::Int(IntTy::U8),
                };
                Ok(x(K::Int(*c as i128), ty, span))
            }
            E::Str(s) => Ok(x(K::Str(s.clone()), Ty::reference(Ty::Str, false), span)),
            E::Bool(b) => Ok(x(K::Bool(*b), Ty::Bool, span)),
            E::Unit => Ok(x(K::Unit, Ty::unit(), span)),
            E::Name(..)
            | E::SelfValue
            | E::SelfType(_)
            | E::Field(..)
            | E::TupleField(..)
            | E::Index(..)
            | E::Deref(_) => {
                if let Some((p, ty)) = self.place(e)? {
                    // implicit reborrow of `&mut T` places where a `&mut` is expected
                    if let (Ty::Ref(inner, true), Some(Ty::Ref(_, true))) = (&ty, expected) {
                        if matches!(p, Place::Local(_)) {
                            let inner = (**inner).clone();
                            let d = Place::Deref(Box::new(x(
                                K::Use(p, UseKind::Copy),
                                ty.clone(),
                                span,
                            )));
                            return Ok(x(K::Borrow(d, true), Ty::reference(inner, true), span));
                        }
                    }
                    self.check_sized(&ty, span)?;
                    return Ok(self.use_place(p, ty, span));
                }
                self.path_value(e, expected)
            }
            E::Call(callee, args) => self.call(callee, args, expected, span),
            E::Unary(op, inner) => self.unary(*op, inner, expected, span),
            E::Binary(op, a, b) => self.binary(*op, a, b, expected, span),
            E::Assign(lhs, rhs) => {
                let (p, ty) = self.place_required(lhs, "assign to")?;
                self.note_mut(&p);
                let v = self.expr_as_into(&p, rhs, &ty)?;
                Ok(x(K::Assign(p, Box::new(v)), Ty::unit(), span))
            }
            E::CompoundAssign(op, lhs, rhs) => {
                let (p, ty) = self.place_required(lhs, "assign to")?;
                self.note_mut(&p);
                let rty = if matches!(op, BinOp::Shl | BinOp::Shr) {
                    let v = self.expr(rhs, None)?;
                    if !v.ty.is_int() || !ty.is_int() {
                        return self.err(
                            span,
                            format!(
                                "cannot apply `{}=` to `{}` and `{}`",
                                op.symbol(),
                                self.ts(&ty),
                                self.ts(&v.ty)
                            ),
                        );
                    }
                    return Ok(x(K::CompoundAssign(*op, p, Box::new(v)), Ty::unit(), span));
                } else {
                    ty.clone()
                };
                if self.overloadable(*op, &ty) {
                    // `a += b` on a user type is `a = a + b`
                    if !self.ck.is_copy(&ty) {
                        return self.err(span, format!("`{}=` on the move-only type `{}` is not supported yet; write `x = x {} y`", op.symbol(), self.ts(&ty), op.symbol()));
                    }
                    let cur = x(K::Use(p.clone(), UseKind::Copy), ty.clone(), lhs.span);
                    let v = self.overloaded_arith(*op, cur, rhs, span)?;
                    return Ok(x(K::Assign(p, Box::new(v)), Ty::unit(), span));
                }
                let ok = match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                        ty.is_numeric()
                            || (matches!(ty, Ty::Ptr(_)) && matches!(op, BinOp::Add | BinOp::Sub))
                    }
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => ty.is_int() || ty == Ty::Bool,
                    _ => false,
                };
                if !ok {
                    return self.err(
                        span,
                        format!("cannot apply `{}=` to `{}`", op.symbol(), self.ts(&ty)),
                    );
                }
                let v = if matches!(ty, Ty::Ptr(_)) {
                    let v = self.expr(rhs, Some(&Ty::Int(IntTy::Usize)))?;
                    if !v.ty.is_int() {
                        return self.err(rhs.span, "pointer offset must be an integer");
                    }
                    v
                } else {
                    self.expr_as_into(&p, rhs, &rty)?
                };
                Ok(x(K::CompoundAssign(*op, p, Box::new(v)), Ty::unit(), span))
            }
            E::Ref(m, inner) => {
                let exp_inner = match expected {
                    Some(Ty::Ref(t, _)) => Some((**t).clone()),
                    _ => None,
                };
                if let Some((p, ty)) = self.place(inner)? {
                    if *m {
                        self.note_mut(&p);
                    }
                    return Ok(x(K::Borrow(p, *m), Ty::reference(ty, *m), span));
                }
                let mut v = self.expr(inner, exp_inner.as_ref())?;
                if let Some(t) = &exp_inner {
                    if !t.is_unsized() {
                        v = self.coerce(v, t, inner.span)?;
                    }
                }
                Ok(self.borrow_value(v, *m))
            }
            E::Move(inner) => {
                let Some((p, ty)) = self.place(inner)? else {
                    return self.err(span, "`move` applies to a variable or field");
                };
                Ok(x(
                    K::Use(
                        p,
                        if self.ck.is_copy(&ty) {
                            UseKind::Copy
                        } else {
                            UseKind::Move
                        },
                    ),
                    ty,
                    span,
                ))
            }
            E::Copy(inner) => {
                let v = self.expr(inner, expected)?;
                if !self.ck.is_copy(&v.ty) {
                    return self.err(
                        span,
                        format!(
                            "`copy` requires a copyable type, but `{}` is move-only",
                            self.ts(&v.ty)
                        ),
                    );
                }
                Ok(v)
            }
            E::Drop(inner) => {
                let Some((p, ty)) = self.place(inner)? else {
                    return self.err(span, "`drop` applies to a variable");
                };
                if !matches!(p, Place::Local(_)) {
                    return self.err(span, "`drop` applies to a whole local variable");
                }
                let _ = ty;
                Ok(x(K::Drop(p), Ty::unit(), span))
            }
            E::Cast(inner, ty) => {
                let target = self.ck.resolve_type(ty, self.module, &self.env)?;
                self.cast(inner, target, span)
            }
            E::Is(scrut, pat) => {
                let (p, ty) = self.place_or_temp(scrut)?;
                let mut binds = Vec::new();
                let xp = self.pattern(pat, &ty, BindMode::Value, &mut binds)?;
                if !binds.is_empty() {
                    return self.err(pat.span, "bindings in `is` patterns are only allowed directly in an `if` or `while` condition");
                }
                Ok(x(K::Is(p, xp), Ty::Bool, span))
            }
            E::StructLit(path, fields) => self.struct_lit(path, fields, expected, span),
            E::Tuple(items) => {
                let exp: Vec<Option<Ty>> = match expected {
                    Some(Ty::Tuple(ts)) if ts.len() == items.len() => {
                        ts.iter().cloned().map(Some).collect()
                    }
                    _ => vec![None; items.len()],
                };
                let mut out = Vec::new();
                for (it, et) in items.iter().zip(exp) {
                    let v = match et {
                        Some(t) => self.expr_as(it, &t)?,
                        None => self.expr(it, None)?,
                    };
                    self.check_sized(&v.ty, it.span)?;
                    out.push(v);
                }
                let ty = Ty::Tuple(out.iter().map(|v| v.ty.clone()).collect());
                Ok(x(K::Tuple(out), ty, span))
            }
            E::Array(items) => {
                let mut et = match expected {
                    Some(Ty::Array(t, _)) => Some((**t).clone()),
                    _ => None,
                };
                let mut out = Vec::new();
                for it in items {
                    let v = match &et {
                        Some(t) => self.expr_as(it, &t.clone())?,
                        None => {
                            let v = self.expr(it, None)?;
                            et = Some(v.ty.clone());
                            v
                        }
                    };
                    out.push(v);
                }
                let Some(et) = et else {
                    return self.err(
                        span,
                        "cannot infer the element type of an empty array; add a type annotation",
                    );
                };
                let n = out.len() as u64;
                Ok(x(K::Array(out), Ty::Array(Box::new(et), n), span))
            }
            E::ArrayRepeat(v, n) => {
                let et = match expected {
                    Some(Ty::Array(t, _)) => Some((**t).clone()),
                    _ => None,
                };
                let v = match &et {
                    Some(t) => self.expr_as(v, t)?,
                    None => self.expr(v, None)?,
                };
                let count = self.ck.const_int(n, self.module)?;
                if count < 0 {
                    return self.err(n.span, "array length must be non-negative");
                }
                if count > 1 && !self.ck.is_copy(&v.ty) {
                    return self.err(
                        span,
                        format!(
                            "`[value; N]` requires a copyable element type, `{}` is move-only",
                            self.ts(&v.ty)
                        ),
                    );
                }
                let ty = Ty::Array(Box::new(v.ty.clone()), count as u64);
                Ok(x(K::ArrayRepeat(Box::new(v), count as u64), ty, span))
            }
            E::Range(..) => self.err(span, "ranges can only be used in `for` loops"),
            E::Block(b) => self.block(b, expected),
            E::Unsafe(b) => {
                self.unsafe_depth += 1;
                let r = self.block(b, expected);
                self.unsafe_depth -= 1;
                r
            }
            E::If(cond, then, els) => self.if_expr(cond, then, els.as_deref(), expected, span),
            E::While(cond, body) => self.while_loop(cond, body, span),
            E::Loop(body) => {
                let id = self.next_loop;
                self.next_loop += 1;
                self.loops.push(LoopCx {
                    id,
                    break_ty: None,
                    allows_value: true,
                });
                let b = self.block(body, Some(&Ty::unit()));
                let cx = self.loops.pop().unwrap();
                let b = b?;
                let b = self.coerce(b, &Ty::unit(), body.span)?;
                let ty = cx.break_ty.unwrap_or(Ty::Never);
                Ok(x(K::Loop(Box::new(b), id), ty, span))
            }
            E::For(pat, iter, body) => self.for_loop(pat, iter, body, span),
            E::Break(v) => {
                let Some(lp) = self.loops.last() else {
                    return self.err(span, "`break` outside of a loop");
                };
                let id = lp.id;
                let allows = lp.allows_value;
                let prev = lp.break_ty.clone();
                let val = match v {
                    Some(v) => {
                        if !allows {
                            return self
                                .err(span, "`break` with a value is only allowed in `loop`");
                        }
                        let val = match &prev {
                            Some(t) => self.expr_as(v, t)?,
                            None => self.expr(v, None)?,
                        };
                        Some(val)
                    }
                    None => {
                        if let Some(t) = &prev {
                            if !t.is_unit() {
                                return self.err(
                                    span,
                                    format!(
                                        "this loop produces `{}`; `break` needs a value",
                                        self.ts(t)
                                    ),
                                );
                            }
                        }
                        None
                    }
                };
                let bty = val.as_ref().map(|v| v.ty.clone()).unwrap_or(Ty::unit());
                let lp = self.loops.last_mut().unwrap();
                if lp.break_ty.is_none() || lp.break_ty.as_ref().is_some_and(|t| t.is_never()) {
                    lp.break_ty = Some(bty);
                }
                Ok(x(K::Break(id, val.map(Box::new)), Ty::Never, span))
            }
            E::Continue => {
                let Some(lp) = self.loops.last() else {
                    return self.err(span, "`continue` outside of a loop");
                };
                Ok(x(K::Continue(lp.id), Ty::Never, span))
            }
            E::Return(v) => {
                let rt = self.ret_ty.clone();
                if rt == super::closure::UNKNOWN_RET {
                    return self.err(span, "to use `return` in a closure, give it a return type: `|x: i32| -> i32 { ... }`");
                }
                let val = match v {
                    Some(v) => Some(Box::new(self.expr_as(v, &rt)?)),
                    None => {
                        if !rt.is_unit() {
                            return self.err(
                                span,
                                format!(
                                    "this function returns `{}`; `return` needs a value",
                                    self.ts(&rt)
                                ),
                            );
                        }
                        None
                    }
                };
                Ok(x(K::Return(val), Ty::Never, span))
            }
            E::Match(scrut, arms) => self.match_expr(scrut, arms, expected, span),
            E::Try(inner) => self.try_expr(inner, span),
            E::Throw(inner) => {
                let rt = self.ret_ty.clone();
                let Some((rid, args)) = self.result_parts(&rt) else {
                    return self.err(span, format!("`throw` requires the function to return `Result<T, E>`, but it returns `{}`", self.ts(&rt)));
                };
                let err_ty = args[1].clone();
                let v = self.expr_as(inner, &err_ty)?;
                let idx = self.ck.adts[rid as usize].variant_index("Err").unwrap();
                let val = x(K::Variant(idx, vec![v]), rt, span);
                Ok(x(K::Return(Some(Box::new(val))), Ty::Never, span))
            }
            E::Closure(c) => self.closure_expr(c, expected, span),
            E::Spawn(is_move, body) => {
                // `spawn { body }` is `std.thread.start(|| body)`
                let fid = self.ck.find_module("std.thread").and_then(|m| {
                    match self.ck.modules[m as usize]
                        .scope
                        .get("start")
                        .map(|b| b.def)
                    {
                        Some(Def::Fn(f)) => Some(f),
                        _ => None,
                    }
                });
                let Some(fid) = fid else {
                    return self.err(
                        span,
                        "`spawn` needs the standard library module `std.thread`",
                    );
                };
                let closure = ast::Expr {
                    kind: E::Closure(Box::new(ast::ClosureExpr {
                        params: Vec::new(),
                        ret: None,
                        body: ast::Expr {
                            kind: E::Block(body.clone()),
                            span: body.span,
                        },
                        is_move: *is_move,
                    })),
                    span,
                };
                self.fn_call(
                    fid,
                    Vec::new(),
                    None,
                    std::slice::from_ref(&closure),
                    expected,
                    span,
                    span,
                )
            }
            E::Catch(inner, binding, handler) => {
                self.catch_expr(inner, binding.as_ref(), handler, expected, span)
            }
        }
    }

    fn int_lit(
        &mut self,
        v: i128,
        suffix: Option<&str>,
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let ty = match (suffix, expected) {
            (Some(s), _) if s == "f32" || s == "f64" => {
                let f = if s == "f32" {
                    FloatTy::F32
                } else {
                    FloatTy::F64
                };
                return Ok(x(K::Float(v as f64), Ty::Float(f), span));
            }
            (Some(s), _) => Ty::Int(IntTy::from_name(s).unwrap()),
            (None, Some(Ty::Int(it))) => Ty::Int(*it),
            _ => Ty::Int(IntTy::I32),
        };
        let Ty::Int(it) = ty else { unreachable!() };
        let (lo, hi) = it.range(self.ck.target.ptr_bits);
        if v < lo || v > hi {
            return self.err(
                span,
                format!(
                    "integer literal {v} is out of range for `{}` ({lo}..={hi})",
                    it.name()
                ),
            );
        }
        Ok(x(K::Int(v), ty, span))
    }

    // -------------------------------------------------------------- places

    /// Lower `e` as a place expression, if it is one.
    pub fn place(&mut self, e: &ast::Expr) -> Result<Option<(Place, Ty)>> {
        let span = e.span;
        match &e.kind {
            E::Name(id, args) if args.is_empty() => {
                if let Some(l) = self.lookup_local(&id.name) {
                    return Ok(Some((Place::Local(l), self.locals[l as usize].ty.clone())));
                }
                if let Some(c) = self.capture_place(&id.name, span) {
                    return Ok(Some(c));
                }
                if let Some(Def::Static(sid)) = self.ck.lookup(self.module, &id.name) {
                    return self.static_place(sid, span).map(Some);
                }
                Ok(None)
            }
            E::SelfValue => match self.lookup_local("self") {
                Some(l) => Ok(Some((Place::Local(l), self.locals[l as usize].ty.clone()))),
                None => match self.capture_place("self", span) {
                    Some(c) => Ok(Some(c)),
                    None => self.err(span, "`self` is only available in methods"),
                },
            },
            E::Field(base, name, targs) => {
                if self.path(base)?.is_some() {
                    // `module.STATIC`
                    if let Some(PathRes::Module(m)) = self.path(base)? {
                        if let Def::Static(sid) =
                            self.ck.lookup_in(self.module, m, &name.name, name.span)?
                        {
                            return self.static_place(sid, span).map(Some);
                        }
                    }
                    return Ok(None);
                }
                if !targs.is_empty() {
                    return Ok(None);
                }
                let (bp, bty) = self.base_place(base, span)?;
                match &bty {
                    Ty::Adt(aid, args) if !self.ck.adts[*aid as usize].is_enum() => {
                        let Some(idx) = self.ck.adts[*aid as usize].field_index(&name.name) else {
                            if self.method_exists(&bty, &name.name) {
                                return self.err(
                                    name.span,
                                    format!(
                                        "`{}` is a method; call it with `{}()`",
                                        name.name, name.name
                                    ),
                                );
                            }
                            let an = self.ck.adts[*aid as usize].name.clone();
                            return self.err(
                                name.span,
                                format!("struct `{an}` has no field `{}`", name.name),
                            );
                        };
                        let a = &self.ck.adts[*aid as usize];
                        if let super::AdtKind::Struct(fs) = &a.kind {
                            if fs[idx].vis != Vis::Pub && a.module != self.module {
                                return self.err(
                                    name.span,
                                    format!(
                                        "field `{}` of struct `{}` is private",
                                        name.name, a.name
                                    ),
                                );
                            }
                        }
                        let fty = a.fields[idx].subst(args);
                        Ok(Some((Place::Field(Box::new(bp), idx as u32), fty)))
                    }
                    _ => {
                        if self.method_exists(&bty, &name.name) || name.name == "len" {
                            return self.err(
                                name.span,
                                format!(
                                    "`{}` is a method; call it with `{}()`",
                                    name.name, name.name
                                ),
                            );
                        }
                        self.err(
                            name.span,
                            format!("type `{}` has no field `{}`", self.ts(&bty), name.name),
                        )
                    }
                }
            }
            E::TupleField(base, idx, isp) => {
                let (bp, bty) = self.base_place(base, span)?;
                match &bty {
                    Ty::Tuple(ts) if (*idx as usize) < ts.len() => {
                        let t = ts[*idx as usize].clone();
                        Ok(Some((Place::Field(Box::new(bp), *idx), t)))
                    }
                    _ => self.err(
                        *isp,
                        format!("type `{}` has no field `{}`", self.ts(&bty), idx),
                    ),
                }
            }
            E::Index(base, idx) => {
                if let E::Range(lo, hi, inclusive) = &idx.kind {
                    let (bp, bty) = self.base_place(base, span)?;
                    let sliced = match &bty {
                        Ty::Array(t, _) | Ty::Slice(t) => Ty::Slice(t.clone()),
                        Ty::Str => Ty::Str,
                        _ => {
                            return self
                                .err(span, format!("type `{}` cannot be sliced", self.ts(&bty)))
                        }
                    };
                    let usize_t = Ty::Int(IntTy::Usize);
                    let lo = match lo {
                        Some(e) => Some(Box::new(self.expr_as(e, &usize_t)?)),
                        None => None,
                    };
                    let hi = match hi {
                        Some(e) => {
                            let h = self.expr_as(e, &usize_t)?;
                            if *inclusive {
                                // `a..=b` is `a..b + 1`
                                let hs = h.span;
                                let one = x(K::Int(1), usize_t.clone(), hs);
                                Some(Box::new(x(
                                    K::Binary(BinOp::Add, Box::new(h), Box::new(one)),
                                    usize_t,
                                    hs,
                                )))
                            } else {
                                Some(Box::new(h))
                            }
                        }
                        None if *inclusive => {
                            return self.err(idx.span, "an inclusive range `..=` needs an end")
                        }
                        None => None,
                    };
                    return Ok(Some((Place::Slice(Box::new(bp), lo, hi), sliced)));
                }
                let (bp, bty) = self.base_place(base, span)?;
                let elem = match &bty {
                    Ty::Array(t, _) | Ty::Slice(t) => (**t).clone(),
                    Ty::Ptr(_) => {
                        return self.err(
                            span,
                            "raw pointers cannot be indexed; use `*(p + i)` in unsafe code",
                        )
                    }
                    _ => {
                        return self
                            .err(span, format!("type `{}` cannot be indexed", self.ts(&bty)))
                    }
                };
                let i = self.expr_as(idx, &Ty::Int(IntTy::Usize))?;
                Ok(Some((Place::Index(Box::new(bp), Box::new(i)), elem)))
            }
            E::Deref(inner) => {
                // reading a reference in order to dereference it never moves it
                let v = match self.place(inner)? {
                    Some((p, t @ (Ty::Ref(..) | Ty::Ptr(_)))) => {
                        x(K::Use(p, UseKind::Copy), t, inner.span)
                    }
                    Some((p, t)) => self.use_place(p, t, inner.span),
                    None => self.expr(inner, None)?,
                };
                match v.ty.clone() {
                    Ty::Ref(t, _) => Ok(Some((Place::Deref(Box::new(v)), *t))),
                    Ty::Ptr(t) => {
                        self.require_unsafe(span, "dereferencing a raw pointer")?;
                        Ok(Some((Place::Deref(Box::new(v)), *t)))
                    }
                    t => self.err(
                        span,
                        format!("type `{}` cannot be dereferenced", self.ts(&t)),
                    ),
                }
            }
            _ => Ok(None),
        }
    }

    fn static_place(&mut self, sid: u32, span: Span) -> Result<(Place, Ty)> {
        let ty = self.ck.static_type(sid)?;
        let s = &self.ck.statics[sid as usize];
        let mutable = s.mutable;
        let sym = s.symbol.clone();
        if mutable {
            self.require_unsafe(span, "accessing a `static mut` (a potential data race)")?;
        }
        self.ck.use_static(sid);
        Ok((Place::Static(sym, mutable), ty))
    }

    /// The value being matched: a place, or a temporary held in a hidden
    /// local (so patterns may move out of it: `while ch.recv() is Some(v)`).
    fn scrutinee(&mut self, e: &ast::Expr) -> Result<(Option<xir::Stmt>, Place, Ty)> {
        if let Some((p, t)) = self.place(e)? {
            return Ok((None, p, t));
        }
        let v = self.expr(e, None)?;
        let t = v.ty.clone();
        let tmp = self.synthetic("scrutinee", t.clone(), e.span);
        Ok((Some(xir::Stmt::Let(tmp, v)), Place::Local(tmp), t))
    }

    fn with_pre(&self, pre: Option<xir::Stmt>, m: X) -> X {
        match pre {
            None => m,
            Some(s) => {
                let (ty, span) = (m.ty.clone(), m.span);
                x(
                    K::Block(xir::Block {
                        stmts: vec![s],
                        tail: Some(Box::new(m)),
                        scope: Vec::new(),
                    }),
                    ty,
                    span,
                )
            }
        }
    }

    pub fn place_or_temp(&mut self, e: &ast::Expr) -> Result<(Place, Ty)> {
        if let Some(p) = self.place(e)? {
            return Ok(p);
        }
        let v = self.expr(e, None)?;
        let ty = v.ty.clone();
        Ok((self.materialize(v), ty))
    }

    /// Place for the base of a field/index access, dereferencing references.
    /// A reference-typed rvalue is dereferenced directly (no temporary).
    fn base_place(&mut self, base: &ast::Expr, span: Span) -> Result<(Place, Ty)> {
        if let Some((p, t)) = self.place(base)? {
            return Ok(self.autoderef(p, t, span));
        }
        let v = self.expr(base, None)?;
        if let Ty::Ref(inner, _) = v.ty.clone() {
            let p = Place::Deref(Box::new(v));
            return Ok(self.autoderef(p, *inner, span));
        }
        let t = v.ty.clone();
        Ok((self.materialize(v), t))
    }

    fn place_required(&mut self, e: &ast::Expr, what: &str) -> Result<(Place, Ty)> {
        match self.place(e)? {
            Some(p) => Ok(p),
            None => self.err(e.span, format!("cannot {what} this expression; expected a variable, field, index or dereference")),
        }
    }

    // --------------------------------------------------------------- paths

    /// Resolve a path-like expression to a module/type/item, or `None` if it
    /// is an ordinary value expression (e.g. a local variable).
    pub fn path(&mut self, e: &ast::Expr) -> Result<Option<PathRes>> {
        match &e.kind {
            E::Name(id, targs) => {
                if targs.is_empty()
                    && (self.lookup_local(&id.name).is_some() || self.is_capture(&id.name))
                {
                    return Ok(None);
                }
                if let Some(t) = self.env.names.get(&id.name) {
                    return Ok(Some(PathRes::Type(t.clone())));
                }
                let def = self.ck.lookup(self.module, &id.name);
                let def = match def {
                    Some(d) => d,
                    None => {
                        if let Some(p) = super::prim_type(&id.name) {
                            return Ok(Some(PathRes::Type(p)));
                        }
                        return Ok(None);
                    }
                };
                self.def_to_path(def, targs, id.span).map(Some)
            }
            E::SelfType(targs) => {
                let Some(st) = self.env.self_ty.clone() else {
                    return self.err(
                        e.span,
                        "`Self` is only available inside `impl` and `trait` blocks",
                    );
                };
                if !targs.is_empty() {
                    return self.err(e.span, "`Self` takes no generic arguments");
                }
                Ok(Some(PathRes::Type(st)))
            }
            E::Field(base, name, targs) => {
                let Some(b) = self.path(base)? else {
                    return Ok(None);
                };
                let targs: Vec<Ty> = targs
                    .iter()
                    .map(|t| self.ck.resolve_type(t, self.module, &self.env))
                    .collect::<Result<_>>()?;
                match b {
                    PathRes::Module(m) => {
                        let def = self.ck.lookup_in(self.module, m, &name.name, name.span)?;
                        let targs_ast = match &e.kind {
                            E::Field(_, _, t) => t.clone(),
                            _ => unreachable!(),
                        };
                        self.def_to_path(def, &targs_ast, name.span).map(Some)
                    }
                    PathRes::Type(t) => Ok(Some(PathRes::Member(t, None, name.clone(), targs))),
                    PathRes::GenericAdt(id) => Ok(Some(PathRes::Member(
                        Ty::unit(),
                        Some(id),
                        name.clone(),
                        targs,
                    ))),
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    fn def_to_path(&mut self, def: Def, targs: &[ast::TypeExpr], span: Span) -> Result<PathRes> {
        let targs: Vec<Ty> = targs
            .iter()
            .map(|t| self.ck.resolve_type(t, self.module, &self.env))
            .collect::<Result<_>>()?;
        Ok(match def {
            Def::Module(m) => PathRes::Module(m),
            Def::Adt(id) => {
                let n = self.ck.adts[id as usize].generics.len();
                if targs.is_empty() && n > 0 {
                    PathRes::GenericAdt(id)
                } else if targs.len() != n {
                    return self.err(
                        span,
                        format!(
                            "type `{}` expects {} generic argument(s), found {}",
                            self.ck.adts[id as usize].name,
                            n,
                            targs.len()
                        ),
                    );
                } else {
                    PathRes::Type(Ty::Adt(id, targs))
                }
            }
            Def::Alias(_) => {
                return self.err(
                    span,
                    "type aliases cannot be used as values; use the aliased type's name",
                );
            }
            d => PathRes::Def(d, targs, span),
        })
    }

    /// A path used as a value (not called): consts, unit variants, unit structs.
    fn path_value(&mut self, e: &ast::Expr, expected: Option<&Ty>) -> Result<X> {
        let span = e.span;
        let Some(res) = self.path(e)? else {
            if let E::Name(id, _) = &e.kind {
                return self.err(
                    span,
                    format!("cannot find value `{}` in this scope", id.name),
                );
            }
            return self.err(span, "expected a value");
        };
        match res {
            PathRes::Def(Def::Const(cid), _, _) => {
                let (ty, v) = self.ck.const_item(cid, span)?;
                self.const_expr(&v, &ty, span)
            }
            PathRes::Def(Def::Variant(aid, idx), targs, _) => {
                self.variant_ctor(aid, idx, targs, &[], expected, span, false)
            }
            PathRes::Member(ty, gen, name, targs) => {
                if let Some((aid, idx, eargs)) = self.member_variant(&ty, gen, &name) {
                    let targs = if eargs.is_empty() { targs } else { eargs };
                    return self.variant_ctor(aid, idx, targs, &[], expected, span, false);
                }
                self.err(
                    name.span,
                    format!(
                        "`{}` is not a value; associated functions must be called",
                        name.name
                    ),
                )
            }
            PathRes::Type(Ty::Adt(aid, args))
                if !self.ck.adts[aid as usize].is_enum()
                    && self.ck.adts[aid as usize].fields.is_empty() =>
            {
                Ok(x(K::Struct(Vec::new()), Ty::Adt(aid, args), span))
            }
            PathRes::Def(Def::Fn(fid), targs, _) => self.fn_value(fid, targs, span),
            PathRes::Module(_) => self.err(span, "a module is not a value"),
            _ => self.err(span, "expected a value, found a type"),
        }
    }

    fn const_expr(&mut self, v: &super::ConstValue, ty: &Ty, span: Span) -> Result<X> {
        use super::ConstValue as C;
        Ok(match (v, ty) {
            (C::Int(n), Ty::Int(_)) => x(K::Int(*n), ty.clone(), span),
            (C::Int(n), Ty::Float(_)) => x(K::Float(*n as f64), ty.clone(), span),
            (C::Float(f), _) => x(K::Float(*f), ty.clone(), span),
            (C::Bool(b), _) => x(K::Bool(*b), ty.clone(), span),
            (C::Str(s), _) => x(K::Str(s.clone()), ty.clone(), span),
            (C::Array(items), Ty::Array(et, _)) => {
                let items = items
                    .iter()
                    .map(|i| self.const_expr(i, et, span))
                    .collect::<Result<_>>()?;
                x(K::Array(items), ty.clone(), span)
            }
            (C::Repeat(item, n), Ty::Array(et, _)) => {
                let item = self.const_expr(item, et, span)?;
                x(K::ArrayRepeat(Box::new(item), *n), ty.clone(), span)
            }
            (C::Tuple(items), Ty::Tuple(ts)) => {
                let items = items
                    .iter()
                    .zip(ts)
                    .map(|(i, t)| self.const_expr(i, t, span))
                    .collect::<Result<_>>()?;
                x(K::Tuple(items), ty.clone(), span)
            }
            _ => return self.err(span, "invalid constant"),
        })
    }

    fn member_variant(
        &self,
        ty: &Ty,
        gen: Option<AdtId>,
        name: &Ident,
    ) -> Option<(AdtId, u32, Vec<Ty>)> {
        let (aid, args) = match (ty, gen) {
            (_, Some(id)) => (id, Vec::new()),
            (Ty::Adt(id, args), None) => (*id, args.clone()),
            _ => return None,
        };
        let idx = self.ck.adts[aid as usize].variant_index(&name.name)?;
        Some((aid, idx, args))
    }

    // ------------------------------------------------------- constructors

    #[allow(clippy::too_many_arguments)]
    fn variant_ctor(
        &mut self,
        aid: AdtId,
        idx: u32,
        targs: Vec<Ty>,
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
        called: bool,
    ) -> Result<X> {
        let a = &self.ck.adts[aid as usize];
        let n = a.generics.len();
        let fields = a.variants[idx as usize].clone();
        let vname = a.variant_name(idx).to_string();
        if !called && !fields.is_empty() {
            return self.err(
                span,
                format!(
                    "variant `{vname}` has {} field(s); construct it with `{vname}(...)`",
                    fields.len()
                ),
            );
        }
        if called && fields.is_empty() {
            return self.err(
                span,
                format!("variant `{vname}` has no fields; write it without parentheses"),
            );
        }
        if args.len() != fields.len() {
            return self.err(
                span,
                format!(
                    "variant `{vname}` expects {} field(s), found {}",
                    fields.len(),
                    args.len()
                ),
            );
        }
        let ret = Ty::Adt(aid, (0..n as u32).map(Ty::Param).collect());
        let pre: Vec<Option<Ty>> = if targs.is_empty() {
            vec![None; n]
        } else {
            targs.into_iter().map(Some).collect()
        };
        let (xs, slots) =
            self.infer_args(n, pre, &fields, &ret, Vec::new(), args, expected, span)?;
        let ty = Ty::Adt(aid, slots);
        Ok(x(K::Variant(idx, xs), ty, span))
    }

    fn struct_ctor(
        &mut self,
        aid: AdtId,
        targs: Vec<Ty>,
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let a = &self.ck.adts[aid as usize];
        let n = a.generics.len();
        let fields = a.fields.clone();
        let name = a.name.clone();
        if a.module != self.module {
            if let super::AdtKind::Struct(fs) = &a.kind {
                if let Some(f) = fs.iter().find(|f| f.vis != Vis::Pub) {
                    return self.err(
                        span,
                        format!(
                            "cannot construct `{name}` here: field `{}` is private",
                            f.name.name
                        ),
                    );
                }
            }
        }
        if args.len() != fields.len() {
            return self.err(
                span,
                format!(
                    "struct `{name}` has {} field(s), but {} value(s) were given",
                    fields.len(),
                    args.len()
                ),
            );
        }
        let ret = Ty::Adt(aid, (0..n as u32).map(Ty::Param).collect());
        let pre: Vec<Option<Ty>> = if targs.is_empty() {
            vec![None; n]
        } else {
            targs.into_iter().map(Some).collect()
        };
        let (xs, slots) =
            self.infer_args(n, pre, &fields, &ret, Vec::new(), args, expected, span)?;
        Ok(x(K::Struct(xs), Ty::Adt(aid, slots), span))
    }

    fn struct_lit(
        &mut self,
        path: &ast::Expr,
        fields: &[(Ident, ast::Expr)],
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let (aid, targs) = match self.path(path)? {
            Some(PathRes::Type(Ty::Adt(id, args))) => (id, args),
            Some(PathRes::GenericAdt(id)) => (id, Vec::new()),
            _ => return self.err(path.span, "expected a struct name"),
        };
        let a = &self.ck.adts[aid as usize];
        let super::AdtKind::Struct(decls) = &a.kind else {
            return self.err(
                path.span,
                format!(
                    "`{}` is an enum; construct a variant like `{}.Variant(...)`",
                    a.name, a.name
                ),
            );
        };
        let mut ordered: Vec<Option<&ast::Expr>> = vec![None; decls.len()];
        for (name, v) in fields {
            let Some(i) = decls.iter().position(|d| d.name.name == name.name) else {
                return self.err(
                    name.span,
                    format!("struct `{}` has no field `{}`", a.name, name.name),
                );
            };
            if ordered[i].is_some() {
                return self.err(
                    name.span,
                    format!("field `{}` specified more than once", name.name),
                );
            }
            ordered[i] = Some(v);
        }
        let missing: Vec<String> = decls
            .iter()
            .zip(&ordered)
            .filter(|(_, o)| o.is_none())
            .map(|(d, _)| format!("`{}`", d.name.name))
            .collect();
        if !missing.is_empty() {
            return self.err(
                span,
                format!(
                    "missing field(s) {} in initializer of `{}`",
                    missing.join(", "),
                    a.name
                ),
            );
        }
        let args: Vec<ast::Expr> = ordered.into_iter().map(|o| o.unwrap().clone()).collect();
        self.struct_ctor(aid, targs, &args, expected, span)
    }

    /// Generic argument inference for calls and constructors.
    ///
    /// `param_tpls`/`ret_tpl` use `Param(i)` for the `n` inference slots.
    /// `pre` are already-lowered leading arguments (method receivers).
    #[allow(clippy::too_many_arguments)]
    fn infer_args(
        &mut self,
        n: usize,
        mut slots: Vec<Option<Ty>>,
        param_tpls: &[Ty],
        ret_tpl: &Ty,
        pre: Vec<X>,
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<(Vec<X>, Vec<Ty>)> {
        slots.resize(n, None);
        let subst_known = |slots: &[Option<Ty>], t: &Ty| -> Option<Ty> {
            if !t.has_params() {
                return Some(t.clone());
            }
            let full: Option<Vec<Ty>> = slots.iter().cloned().collect();
            let s = t.subst(&full.unwrap_or_else(|| {
                slots
                    .iter()
                    .enumerate()
                    .map(|(i, s)| s.clone().unwrap_or(Ty::Param(i as u32)))
                    .collect()
            }));
            if s.has_params() {
                None
            } else {
                Some(s)
            }
        };
        if let Some(exp) = expected {
            if ret_tpl.has_params() {
                let mut trial = slots.clone();
                if match_template(ret_tpl, exp, &mut trial) {
                    slots = trial;
                }
            }
        }
        let mut out = Vec::new();
        for (i, v) in pre.into_iter().enumerate() {
            let tpl = &param_tpls[i];
            let mut trial = slots.clone();
            if match_template(tpl, &v.ty, &mut trial) {
                slots = trial;
            }
            out.push(v);
        }
        let offset = out.len();
        let hints = std::mem::take(&mut self.arg_hints);
        for (i, a) in args.iter().enumerate() {
            let tpl = &param_tpls[offset + i];
            let mut exp = subst_known(&slots, tpl);
            if exp.is_none() {
                // a closure for an `F: Fn(A) -> R` parameter learns `A` from the bound
                if let Some(Some(h)) = hints.get(offset + i) {
                    let partial: Vec<Ty> = slots
                        .iter()
                        .enumerate()
                        .map(|(j, s)| s.clone().unwrap_or(Ty::Param(j as u32)))
                        .collect();
                    exp = Some(h.subst(&partial));
                }
            }
            let v = self.expr(a, exp.as_ref())?;
            // the closure's actual signature fills in the bound's unknowns
            // (`U` in `map<U, F: Fn(&T) -> U>`)
            if let (Some(Some(h)), Some((ps, r, _))) =
                (hints.get(offset + i), self.ck.callable_sig(&v.ty))
            {
                let mut trial = slots.clone();
                if match_template(h, &Ty::FnPtr(ps, Box::new(r)), &mut trial) {
                    slots = trial;
                }
            }
            let exp = if matches!(exp, Some(Ty::FnPtr(..)))
                && !matches!(v.ty, Ty::FnPtr(..))
                && tpl.has_params()
            {
                None
            } else {
                exp
            };
            if exp.is_none() {
                let mut trial = slots.clone();
                if match_template(tpl, &v.ty, &mut trial) {
                    slots = trial;
                } else if let Ty::Ref(inner, m) = &v.ty {
                    // &[T; N] argument for a &[T] parameter
                    if let Ty::Array(e, _) = &**inner {
                        let as_slice = Ty::Ref(Box::new(Ty::Slice(e.clone())), *m);
                        let mut trial = slots.clone();
                        if match_template(tpl, &as_slice, &mut trial) {
                            slots = trial;
                        }
                    }
                }
            }
            out.push(v);
        }
        let mut final_slots = Vec::new();
        for (i, s) in slots.iter().enumerate() {
            match s {
                Some(t) => final_slots.push(t.clone()),
                None => return self.err(span, format!("cannot infer generic argument #{} here; specify it explicitly (e.g. `name<T>(...)`) or annotate the expected type", i + 1)),
            }
        }
        let mut coerced = Vec::new();
        for (i, v) in out.into_iter().enumerate() {
            let pt = param_tpls[i].subst(&final_slots);
            let vs = v.span;
            coerced.push(self.coerce(v, &pt, vs)?);
        }
        Ok((coerced, final_slots))
    }

    // --------------------------------------------------------------- calls

    fn call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        if let E::Field(base, name, targs) = &callee.kind {
            if self.path(base)?.is_none() {
                return self.method_call(base, name, targs, args, expected, span);
            }
        }
        let Some(res) = self.path(callee)? else {
            if let E::Name(id, _) = &callee.kind {
                if self.lookup_local(&id.name).is_some() || self.is_capture(&id.name) {
                    return self.call_value(callee, args, span);
                }
                return self.err(
                    callee.span,
                    format!("cannot find function `{}` in this scope", id.name),
                );
            }
            return self.call_value(callee, args, span);
        };
        match res {
            PathRes::Def(Def::Fn(fid), targs, sp) => {
                self.fn_call(fid, targs, None, args, expected, span, sp)
            }
            PathRes::Def(Def::Variant(aid, idx), targs, _) => {
                self.variant_ctor(aid, idx, targs, args, expected, span, true)
            }
            PathRes::Def(Def::Intrinsic(i), targs, _) => {
                self.intrinsic(i, targs, args, expected, span)
            }
            PathRes::Type(Ty::Adt(aid, targs)) if !self.ck.adts[aid as usize].is_enum() => {
                self.struct_ctor(aid, targs, args, expected, span)
            }
            PathRes::GenericAdt(aid) if !self.ck.adts[aid as usize].is_enum() => {
                self.struct_ctor(aid, Vec::new(), args, expected, span)
            }
            PathRes::Member(ty, gen, name, targs) => {
                if let Some((aid, idx, eargs)) = self.member_variant(&ty, gen, &name) {
                    if !targs.is_empty() {
                        return self.err(
                            name.span,
                            "generic arguments go on the enum type: `Enum<T>.Variant(...)`",
                        );
                    }
                    return self.variant_ctor(aid, idx, eargs, args, expected, span, true);
                }
                self.assoc_call(ty, gen, &name, targs, args, expected, span)
            }
            PathRes::Def(Def::Const(_) | Def::Static(_), _, _) => {
                self.err(callee.span, "this value is not callable")
            }
            _ => self.err(callee.span, "this is not a function"),
        }
    }

    /// Signature of a function as templates: impl generics are `Param(0..n_impl)`,
    /// the function's own generics follow. Includes `self` as the first parameter.
    pub fn signature(
        &mut self,
        fid: FnId,
        self_ty: Option<Ty>,
    ) -> Result<(Vec<Ty>, Ty, usize, usize)> {
        let f = &self.ck.fns[fid as usize];
        let decl = f.decl.clone();
        let module = f.module;
        let mut env = Env::default();
        let mut n_impl = 0;
        match f.owner {
            FnOwner::Impl(iid) => {
                let im = &self.ck.impls[iid as usize];
                n_impl = im.generics.len();
                for (i, g) in im.generics.iter().enumerate() {
                    env.names.insert(g.name.name.clone(), Ty::Param(i as u32));
                }
                env.self_ty = Some(im.self_ty.clone());
            }
            FnOwner::Trait(_) => env.self_ty = self_ty,
            FnOwner::Free => {}
        }
        for (j, g) in decl.generics.params.iter().enumerate() {
            env.names
                .insert(g.name.name.clone(), Ty::Param((n_impl + j) as u32));
        }
        let mut params = Vec::new();
        if let Some((kind, _, _)) = decl.self_param {
            let st = env.self_ty.clone().unwrap();
            params.push(match kind {
                SelfKind::Value => st,
                SelfKind::Ref => Ty::reference(st, false),
                SelfKind::RefMut => Ty::reference(st, true),
            });
        }
        for p in &decl.params {
            params.push(self.ck.resolve_type(&p.ty, module, &env)?);
        }
        let ret = match &decl.ret {
            Some(t) => self.ck.resolve_type(t, module, &env)?,
            None => Ty::unit(),
        };
        Ok((params, ret, n_impl, decl.generics.params.len()))
    }

    #[allow(clippy::too_many_arguments)]
    fn fn_call(
        &mut self,
        fid: FnId,
        targs: Vec<Ty>,
        impl_args: Option<Vec<Ty>>,
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
        name_span: Span,
    ) -> Result<X> {
        self.invoke(
            fid,
            targs,
            impl_args,
            None,
            Vec::new(),
            args,
            expected,
            span,
            name_span,
        )
    }

    /// Call function `fid` with `pre` (already lowered receiver) + `args`.
    #[allow(clippy::too_many_arguments)]
    fn invoke(
        &mut self,
        fid: FnId,
        targs: Vec<Ty>,
        impl_args: Option<Vec<Ty>>,
        trait_self: Option<Ty>,
        pre: Vec<X>,
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
        name_span: Span,
    ) -> Result<X> {
        let f = &self.ck.fns[fid as usize];
        let fname = f.decl.name.name.clone();
        let is_unsafe = f.decl.is_unsafe;
        let is_extern = f.decl.body.is_none();
        let variadic = f.decl.variadic;
        let fmodule = f.module;
        if f.vis != Vis::Pub && fmodule != self.module {
            return self.err(name_span, format!("function `{fname}` is private"));
        }
        let fn_generics = f.decl.generics.params.clone();
        if is_unsafe {
            self.require_unsafe(span, &format!("calling unsafe function `{fname}`"))?;
        }
        let (params, ret, n_impl, n_fn) = self.signature(fid, trait_self.clone())?;
        let n_args = pre.len() + args.len();
        if (!variadic && n_args != params.len()) || (variadic && n_args < params.len()) {
            return self.err(
                span,
                format!(
                    "function `{fname}` takes {} argument(s), but {} were given",
                    params.len(),
                    n_args
                ),
            );
        }
        if !targs.is_empty() && targs.len() != n_fn {
            return self.err(
                name_span,
                format!(
                    "function `{fname}` expects {n_fn} generic argument(s), found {}",
                    targs.len()
                ),
            );
        }
        let mut slots: Vec<Option<Ty>> = match impl_args {
            Some(ia) => ia.into_iter().map(Some).collect(),
            None => vec![None; n_impl],
        };
        if targs.is_empty() {
            slots.extend(std::iter::repeat_n(None, n_fn));
        } else {
            slots.extend(targs.into_iter().map(Some));
        }
        let (fixed_args, extra) = if variadic {
            args.split_at(params.len() - pre.len())
        } else {
            (args, &[][..])
        };
        self.arg_hints = self.fn_bound_hints(fid, &params, n_impl);
        let (mut xs, slots) = self.infer_args(
            n_impl + n_fn,
            slots,
            &params,
            &ret,
            pre,
            fixed_args,
            expected,
            span,
        )?;
        for a in extra {
            let v = self.expr(a, None)?;
            let v = match &v.ty {
                Ty::Int(it) if it.bits(self.ck.target.ptr_bits) < 32 => {
                    let t = if it.signed() {
                        Ty::Int(IntTy::I32)
                    } else {
                        Ty::Int(IntTy::U32)
                    };
                    let vs = v.span;
                    x(K::Cast(Box::new(v), t.clone()), t, vs)
                }
                Ty::Float(FloatTy::F32) => {
                    let vs = v.span;
                    x(
                        K::Cast(Box::new(v), Ty::Float(FloatTy::F64)),
                        Ty::Float(FloatTy::F64),
                        vs,
                    )
                }
                Ty::Int(_) | Ty::Float(_) | Ty::Ptr(_) | Ty::Bool => v,
                t => {
                    return self.err(
                        a.span,
                        format!(
                            "type `{}` cannot be passed as a C variadic argument",
                            self.ts(t)
                        ),
                    )
                }
            };
            xs.push(v);
        }
        let ret = ret.subst(&slots);
        if is_extern {
            let sym = self.ck.declare_extern(fid)?;
            return Ok(x(K::Call(sym, xs), ret, span));
        }
        // bounds on the function's own generic parameters
        let mut base = Env::default();
        if let FnOwner::Impl(iid) = self.ck.fns[fid as usize].owner {
            let im = &self.ck.impls[iid as usize];
            for (g, t) in im.generics.iter().zip(&slots[..n_impl]) {
                base.names.insert(g.name.name.clone(), t.clone());
            }
            base.self_ty = Some(im.self_ty.subst(&slots[..n_impl]));
        }
        self.ck
            .check_bounds_in(&fn_generics, &slots[n_impl..], fmodule, span, base)?;
        let self_ty = match self.ck.fns[fid as usize].owner {
            FnOwner::Trait(_) => trait_self,
            _ => None,
        };
        let name = self.ck.instance(FnKey {
            fid,
            targs: slots,
            self_ty,
        });
        Ok(x(K::Call(name, xs), ret, span))
    }

    /// `Type.function(...)`
    #[allow(clippy::too_many_arguments)]
    fn assoc_call(
        &mut self,
        ty: Ty,
        gen: Option<AdtId>,
        name: &Ident,
        targs: Vec<Ty>,
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        // inherent impls first
        for iid in 0..self.ck.impls.len() {
            let im = &self.ck.impls[iid];
            if im.trait_id.is_some() {
                continue;
            }
            let Some(&fid) = im.methods.get(&name.name) else {
                continue;
            };
            let impl_args = match gen {
                Some(aid) => {
                    if !matches!(&im.self_ty, Ty::Adt(id, _) if *id == aid) {
                        continue;
                    }
                    None
                }
                None => match self.ck.match_impl(iid as ImplId, &ty) {
                    Some(a) => Some(a),
                    None => continue,
                },
            };
            if let Some(ia) = &impl_args {
                let generics = self.ck.impls[iid].generics.clone();
                let module = self.ck.impls[iid].module;
                self.ck.check_bounds(&generics, ia, module, span)?;
            }
            return self.invoke(
                fid,
                targs,
                impl_args,
                None,
                Vec::new(),
                args,
                expected,
                span,
                name.span,
            );
        }
        // trait methods called as `Type.method(receiver, ...)`
        if gen.is_none() {
            if let Some((fid, impl_args, trait_self)) = self.find_trait_method(&ty, &name.name) {
                return self.invoke(
                    fid,
                    targs,
                    impl_args,
                    trait_self,
                    Vec::new(),
                    args,
                    expected,
                    span,
                    name.span,
                );
            }
        }
        let tname = match gen {
            Some(id) => self.ck.adts[id as usize].name.clone(),
            None => self.ts(&ty),
        };
        self.err(
            name.span,
            format!(
                "no function or variant named `{}` found for type `{tname}`",
                name.name
            ),
        )
    }

    /// Search trait impls (and trait default methods) for `name` on `ty`.
    fn find_trait_method(
        &mut self,
        ty: &Ty,
        name: &str,
    ) -> Option<(FnId, Option<Vec<Ty>>, Option<Ty>)> {
        for iid in 0..self.ck.impls.len() {
            let Some(tid) = self.ck.impls[iid].trait_id else {
                continue;
            };
            let declares = self.ck.traits[tid as usize]
                .decl
                .methods
                .iter()
                .any(|m| m.name.name == name);
            if !declares {
                continue;
            }
            let Some(args) = self.ck.match_impl(iid as ImplId, ty) else {
                continue;
            };
            if let Some(&fid) = self.ck.impls[iid].methods.get(name) {
                return Some((fid, Some(args), None));
            }
            if let Some(&fid) = self.ck.traits[tid as usize].defaults.get(name) {
                return Some((fid, None, Some(ty.clone())));
            }
        }
        None
    }

    fn find_method(&mut self, ty: &Ty, name: &str) -> Option<(FnId, Option<Vec<Ty>>, Option<Ty>)> {
        for iid in 0..self.ck.impls.len() {
            if self.ck.impls[iid].trait_id.is_some() {
                continue;
            }
            let Some(&fid) = self.ck.impls[iid].methods.get(name) else {
                continue;
            };
            if let Some(args) = self.ck.match_impl(iid as ImplId, ty) {
                return Some((fid, Some(args), None));
            }
        }
        self.find_trait_method(ty, name)
    }

    fn method_exists(&mut self, ty: &Ty, name: &str) -> bool {
        self.find_method(ty, name).is_some()
    }

    fn method_call(
        &mut self,
        base: &ast::Expr,
        name: &Ident,
        targs_ast: &[ast::TypeExpr],
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let targs: Vec<Ty> = targs_ast
            .iter()
            .map(|t| self.ck.resolve_type(t, self.module, &self.env))
            .collect::<Result<_>>()?;
        let recv = match self.place(base)? {
            Some((p, t)) => Recv::Place(p, t),
            None => Recv::Value(self.expr(base, None)?),
        };
        let recv_ty = match &recv {
            Recv::Place(_, t) => t.clone(),
            Recv::Value(v) => v.ty.clone(),
        };
        // builtin `.len()` on arrays, slices and strings
        if name.name == "len"
            && args.is_empty()
            && matches!(recv_ty.deref_all(), Ty::Array(..) | Ty::Slice(_) | Ty::Str)
        {
            let (p, t) = self.recv_place(recv, span);
            let (p, t) = self.autoderef(p, t, span);
            if let Ty::Array(_, n) = t {
                return Ok(x(K::Int(n as i128), Ty::Int(IntTy::Usize), span));
            }
            let r = x(K::Borrow(p, false), Ty::reference(t, false), span);
            return Ok(x(
                K::Intrinsic(xir::Intrinsic::Len, vec![r]),
                Ty::Int(IntTy::Usize),
                span,
            ));
        }
        // autoderef search
        let mut cand = recv_ty.clone();
        let mut depth = 0;
        let found = loop {
            if let Some(m) = self.find_method(&cand, &name.name) {
                break Some(m);
            }
            match &cand {
                Ty::Ref(inner, _) => {
                    cand = (**inner).clone();
                    depth += 1;
                }
                _ => break None,
            }
        };
        let Some((fid, impl_args, trait_self)) = found else {
            // `self.on_click(...)`: a field holding a closure or `fn`
            if let Some(v) = self.call_field(base, name, args, span)? {
                return Ok(v);
            }
            if let Ty::Adt(aid, _) = recv_ty.deref_all() {
                if self.ck.adts[*aid as usize]
                    .field_index(&name.name)
                    .is_some()
                {
                    return self.err(
                        name.span,
                        format!("`{}` is a field, not a method", name.name),
                    );
                }
            }
            return self.err(
                name.span,
                format!(
                    "no method named `{}` found for type `{}`",
                    name.name,
                    self.ts(&recv_ty)
                ),
            );
        };
        let decl = &self.ck.fns[fid as usize].decl;
        let Some((kind, _, _)) = decl.self_param else {
            let tn = self.ts(&cand);
            return self.err(
                name.span,
                format!(
                    "`{}` is an associated function, not a method; call it as `{tn}.{}(...)`",
                    name.name, name.name
                ),
            );
        };
        // build the receiver argument
        let recv_x = match kind {
            SelfKind::Value => {
                if depth == 0 {
                    match recv {
                        Recv::Place(p, t) => self.use_place(p, t, base.span),
                        Recv::Value(v) => v,
                    }
                } else {
                    let (mut p, mut t, d) = self.recv_place_deref(recv, depth, span);
                    for _ in 0..d {
                        let (np, nt) = self.deref_place(p, &t, span);
                        p = np;
                        t = nt;
                    }
                    if !self.ck.is_copy(&t) {
                        return self.err(
                            base.span,
                            format!(
                                "cannot move out of a borrowed `{}` to call `{}(self)`",
                                self.ts(&t),
                                name.name
                            ),
                        );
                    }
                    x(K::Use(p, UseKind::Copy), t, base.span)
                }
            }
            SelfKind::Ref | SelfKind::RefMut => {
                let m = kind == SelfKind::RefMut;
                let (mut p, mut t, d) = self.recv_place_deref(recv, depth, span);
                for _ in 0..d {
                    let (np, nt) = self.deref_place(p, &t, span);
                    p = np;
                    t = nt;
                }
                if m {
                    self.note_mut(&p);
                }
                x(K::Borrow(p, m), Ty::reference(t, m), base.span)
            }
        };
        if let Some(ia) = &impl_args {
            if let FnOwner::Impl(iid) = self.ck.fns[fid as usize].owner {
                let generics = self.ck.impls[iid as usize].generics.clone();
                let module = self.ck.impls[iid as usize].module;
                self.ck.check_bounds(&generics, ia, module, span)?;
            }
        }
        self.invoke(
            fid,
            targs,
            impl_args,
            trait_self,
            vec![recv_x],
            args,
            expected,
            span,
            name.span,
        )
    }

    /// Receiver place with `depth` pending dereferences; a reference rvalue
    /// is dereferenced in place instead of being stored in a temporary.
    fn recv_place_deref(&mut self, recv: Recv, depth: usize, span: Span) -> (Place, Ty, usize) {
        match recv {
            Recv::Value(v) if depth > 0 => {
                if let Ty::Ref(inner, _) = v.ty.clone() {
                    return (Place::Deref(Box::new(v)), *inner, depth - 1);
                }
                let (p, t) = self.recv_place(Recv::Value(v), span);
                (p, t, depth)
            }
            r => {
                let (p, t) = self.recv_place(r, span);
                (p, t, depth)
            }
        }
    }

    fn recv_place(&mut self, recv: Recv, _span: Span) -> (Place, Ty) {
        match recv {
            Recv::Place(p, t) => (p, t),
            Recv::Value(v) => {
                let t = v.ty.clone();
                (self.materialize(v), t)
            }
        }
    }

    fn intrinsic(
        &mut self,
        i: IntrinsicFn,
        targs: Vec<Ty>,
        args: &[ast::Expr],
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let usize_t = Ty::Int(IntTy::Usize);
        let want = |cx: &Self, n: usize| -> Result<()> {
            if args.len() != n {
                return cx.err(
                    span,
                    format!(
                        "this intrinsic takes {n} argument(s), but {} were given",
                        args.len()
                    ),
                );
            }
            Ok(())
        };
        match i {
            IntrinsicFn::SizeOf | IntrinsicFn::AlignOf => {
                want(self, 0)?;
                let [t] = &targs[..] else {
                    return self.err(span, "expected one generic argument, e.g. `size_of<T>()`");
                };
                self.check_sized(t, span)?;
                let k = if i == IntrinsicFn::SizeOf {
                    xir::Intrinsic::SizeOf(t.clone())
                } else {
                    xir::Intrinsic::AlignOf(t.clone())
                };
                Ok(x(K::Intrinsic(k, Vec::new()), usize_t, span))
            }
            IntrinsicFn::Trap => {
                want(self, 0)?;
                Ok(x(
                    K::Intrinsic(xir::Intrinsic::Trap, Vec::new()),
                    Ty::Never,
                    span,
                ))
            }
            IntrinsicFn::Asm => {
                want(self, 1)?;
                self.require_unsafe(span, "inline assembly")?;
                let E::Str(s) = &args[0].kind else {
                    return self.err(args[0].span, "`asm` expects a string literal");
                };
                let s = String::from_utf8(s.clone())
                    .map_err(|_| Diagnostic::new(args[0].span, "assembly must be valid UTF-8"))?;
                Ok(x(
                    K::Intrinsic(xir::Intrinsic::Asm(s), Vec::new()),
                    Ty::unit(),
                    span,
                ))
            }
            IntrinsicFn::PtrRead | IntrinsicFn::DropInPlace | IntrinsicFn::VolatileLoad => {
                want(self, 1)?;
                let what = if i == IntrinsicFn::DropInPlace {
                    "dropping through a raw pointer"
                } else {
                    "reading through a raw pointer"
                };
                self.require_unsafe(span, what)?;
                let exp = match (targs.first(), expected) {
                    (Some(t), _) => Some(Ty::Ptr(Box::new(t.clone()))),
                    (None, Some(t)) if i != IntrinsicFn::DropInPlace => {
                        Some(Ty::Ptr(Box::new(t.clone())))
                    }
                    _ => None,
                };
                let p = match &exp {
                    Some(t) => self.expr_as(&args[0], t)?,
                    None => self.expr(&args[0], None)?,
                };
                let Ty::Ptr(elem) = p.ty.clone() else {
                    return self.err(args[0].span, "expected a raw pointer");
                };
                self.check_sized(&elem, span)?;
                if i == IntrinsicFn::VolatileLoad {
                    if !self.ck.is_copy(&elem) {
                        return self.err(span, "`volatile_load` requires a copyable type");
                    }
                    Ok(x(
                        K::Intrinsic(xir::Intrinsic::VolatileLoad, vec![p]),
                        *elem,
                        span,
                    ))
                } else if i == IntrinsicFn::PtrRead {
                    Ok(x(
                        K::Intrinsic(xir::Intrinsic::PtrRead, vec![p]),
                        *elem,
                        span,
                    ))
                } else {
                    self.ck.ensure_drop(&elem);
                    Ok(x(
                        K::Intrinsic(xir::Intrinsic::DropInPlace(*elem), vec![p]),
                        Ty::unit(),
                        span,
                    ))
                }
            }
            IntrinsicFn::Syscall => {
                if args.is_empty() || args.len() > 7 {
                    return self.err(
                        span,
                        "`syscall` takes a system call number and up to 6 arguments",
                    );
                }
                self.require_unsafe(span, "a system call")?;
                let mut xs = Vec::new();
                for a in args {
                    let v = self.expr(a, Some(&usize_t))?;
                    if !(v.ty.is_int() || matches!(v.ty, Ty::Ptr(_))) {
                        return self.err(
                            a.span,
                            format!(
                                "system call arguments must be integers or raw pointers; found `{}`",
                                self.ts(&v.ty)
                            ),
                        );
                    }
                    xs.push(v);
                }
                Ok(x(
                    K::Intrinsic(xir::Intrinsic::Syscall, xs),
                    Ty::Int(IntTy::Isize),
                    span,
                ))
            }
            IntrinsicFn::OsArgc | IntrinsicFn::OsArgv => {
                want(self, 0)?;
                let (k, t) = if i == IntrinsicFn::OsArgc {
                    (xir::Intrinsic::OsArgc, usize_t)
                } else {
                    (
                        xir::Intrinsic::OsArgv,
                        Ty::Ptr(Box::new(Ty::Ptr(Box::new(Ty::Int(IntTy::U8))))),
                    )
                };
                Ok(x(K::Intrinsic(k, Vec::new()), t, span))
            }
            IntrinsicFn::Fence => {
                want(self, 0)?;
                Ok(x(
                    K::Intrinsic(xir::Intrinsic::Fence, Vec::new()),
                    Ty::unit(),
                    span,
                ))
            }
            IntrinsicFn::AtomicLoad
            | IntrinsicFn::AtomicStore
            | IntrinsicFn::AtomicSwap
            | IntrinsicFn::AtomicAdd
            | IntrinsicFn::AtomicSub
            | IntrinsicFn::AtomicCas => {
                let n = match i {
                    IntrinsicFn::AtomicLoad => 1,
                    IntrinsicFn::AtomicCas => 3,
                    _ => 2,
                };
                want(self, n)?;
                self.require_unsafe(span, "atomic access through a raw pointer")?;
                let exp = targs.first().map(|t| Ty::Ptr(Box::new(t.clone())));
                let p = match &exp {
                    Some(t) => self.expr_as(&args[0], t)?,
                    None => self.expr(&args[0], None)?,
                };
                let Ty::Ptr(elem) = p.ty.clone() else {
                    return self.err(args[0].span, "expected a raw pointer");
                };
                let arith = matches!(i, IntrinsicFn::AtomicAdd | IntrinsicFn::AtomicSub);
                if !(elem.is_int() || (!arith && matches!(*elem, Ty::Ptr(_)))) {
                    return self.err(
                        span,
                        format!(
                            "atomics work on integers{}; found `{}`",
                            if arith { "" } else { " and raw pointers" },
                            self.ts(&elem)
                        ),
                    );
                }
                let mut xs = vec![p];
                for a in &args[1..] {
                    xs.push(self.expr_as(a, &elem)?);
                }
                let (k, ty) = match i {
                    IntrinsicFn::AtomicLoad => (xir::Intrinsic::AtomicLoad, *elem),
                    IntrinsicFn::AtomicStore => (xir::Intrinsic::AtomicStore, Ty::unit()),
                    IntrinsicFn::AtomicSwap => (xir::Intrinsic::AtomicRmw("xchg"), *elem),
                    IntrinsicFn::AtomicAdd => (xir::Intrinsic::AtomicRmw("add"), *elem),
                    IntrinsicFn::AtomicSub => (xir::Intrinsic::AtomicRmw("sub"), *elem),
                    _ => (xir::Intrinsic::AtomicCas, *elem),
                };
                Ok(x(K::Intrinsic(k, xs), ty, span))
            }
            IntrinsicFn::PtrWrite | IntrinsicFn::VolatileStore => {
                want(self, 2)?;
                self.require_unsafe(span, "writing through a raw pointer")?;
                let exp = targs.first().map(|t| Ty::Ptr(Box::new(t.clone())));
                let p = match &exp {
                    Some(t) => self.expr_as(&args[0], t)?,
                    None => self.expr(&args[0], None)?,
                };
                let Ty::Ptr(elem) = p.ty.clone() else {
                    return self.err(args[0].span, "expected a raw pointer");
                };
                let v = self.expr_as(&args[1], &elem)?;
                let k = if i == IntrinsicFn::VolatileStore {
                    xir::Intrinsic::VolatileStore
                } else {
                    xir::Intrinsic::PtrWrite
                };
                Ok(x(K::Intrinsic(k, vec![p, v]), Ty::unit(), span))
            }
            IntrinsicFn::Forget => {
                want(self, 1)?;
                // leaking a value skips its destructor; scoped threads rely on
                // their handles being joined on drop, so this must be explicit
                self.require_unsafe(span, "`forget` (skipping a destructor)")?;
                let v = self.expr(&args[0], targs.first())?;
                Ok(x(
                    K::Intrinsic(xir::Intrinsic::Forget, vec![v]),
                    Ty::unit(),
                    span,
                ))
            }
            IntrinsicFn::SliceFromRaw | IntrinsicFn::SliceFromRawMut | IntrinsicFn::StrFromRaw => {
                want(self, 2)?;
                self.require_unsafe(span, "building a slice from a raw pointer")?;
                let mutable = i == IntrinsicFn::SliceFromRawMut;
                let exp_ptr = match (i, expected, targs.first()) {
                    (IntrinsicFn::StrFromRaw, _, _) => Some(Ty::Ptr(Box::new(Ty::Int(IntTy::U8)))),
                    (_, _, Some(t)) => Some(Ty::Ptr(Box::new(t.clone()))),
                    (_, Some(Ty::Ref(inner, _)), _) => match &**inner {
                        Ty::Slice(t) => Some(Ty::Ptr(t.clone())),
                        _ => None,
                    },
                    _ => None,
                };
                let p = match &exp_ptr {
                    Some(t) => self.expr_as(&args[0], t)?,
                    None => self.expr(&args[0], None)?,
                };
                let Ty::Ptr(elem) = p.ty.clone() else {
                    return self.err(args[0].span, "expected a raw pointer");
                };
                let len = self.expr_as(&args[1], &usize_t)?;
                let ty = if i == IntrinsicFn::StrFromRaw {
                    Ty::reference(Ty::Str, false)
                } else {
                    Ty::reference(Ty::Slice(elem), mutable)
                };
                Ok(x(
                    K::Intrinsic(xir::Intrinsic::MakeFat, vec![p, len]),
                    ty,
                    span,
                ))
            }
        }
    }

    // ----------------------------------------------------------- operators

    fn unary(
        &mut self,
        op: UnOp,
        inner: &ast::Expr,
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        if op == UnOp::Neg {
            if let E::Int(v, suffix) = &inner.kind {
                let lit = self.int_lit_signed(-(*v as i128), suffix.as_deref(), expected, span)?;
                return Ok(lit);
            }
        }
        let v = self.expr(inner, expected)?;
        if op == UnOp::Neg && self.overloadable(BinOp::Sub, &v.ty) {
            let t = v.ty.clone();
            return self.call_operator("Neg", "neg", "-", &t, vec![v], span);
        }
        let ok = match op {
            UnOp::Neg => matches!(v.ty, Ty::Float(_)) || matches!(v.ty, Ty::Int(it) if it.signed()),
            UnOp::Not => v.ty == Ty::Bool || v.ty.is_int(),
        };
        if !ok {
            let sym = if op == UnOp::Neg { "-" } else { "!" };
            return self.err(
                span,
                format!("cannot apply unary `{sym}` to type `{}`", self.ts(&v.ty)),
            );
        }
        let ty = v.ty.clone();
        Ok(x(K::Unary(op, Box::new(v)), ty, span))
    }

    fn int_lit_signed(
        &mut self,
        v: i128,
        suffix: Option<&str>,
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        if let Some(Ty::Float(f)) = expected {
            if suffix.is_none() {
                return self.err(
                    span,
                    format!(
                        "expected `{}`, found an integer literal; write `{}.0`",
                        if *f == FloatTy::F32 { "f32" } else { "f64" },
                        v
                    ),
                );
            }
        }
        if let Some(s) = suffix {
            if s == "f32" || s == "f64" {
                let f = if s == "f32" {
                    FloatTy::F32
                } else {
                    FloatTy::F64
                };
                return Ok(x(K::Float(v as f64), Ty::Float(f), span));
            }
        }
        let lit = self.int_lit(v, suffix, expected, span)?;
        if let Ty::Int(it) = lit.ty {
            if !it.signed() {
                return self.err(span, format!("cannot negate unsigned type `{}`", it.name()));
            }
        }
        Ok(lit)
    }

    fn binary(
        &mut self,
        op: BinOp,
        a: &ast::Expr,
        b: &ast::Expr,
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let l = self.expr_as(a, &Ty::Bool)?;
            let r = self.expr_as(b, &Ty::Bool)?;
            return Ok(x(K::Binary(op, Box::new(l), Box::new(r)), Ty::Bool, span));
        }
        let arith_expected = if op.is_comparison() {
            None
        } else {
            expected.filter(|t| t.is_numeric())
        };
        let (l, r) = if matches!(op, BinOp::Shl | BinOp::Shr) {
            let l = self.expr(a, arith_expected)?;
            let r = self.expr(b, None)?;
            (l, r)
        } else if untyped_literal(a) && !untyped_literal(b) {
            let r = self.expr(b, arith_expected)?;
            let rt = r.ty.clone();
            let l = if matches!(rt, Ty::Ptr(_)) {
                self.expr(a, None)?
            } else {
                self.expr_as(a, &rt)?
            };
            (l, r)
        } else {
            // user types dispatch to the prelude's operator traits
            let l = match self.place(a)? {
                Some((p, t)) if op.is_comparison() && self.overloadable(op, &t) => {
                    let lref = x(K::Borrow(p, false), Ty::reference(t, false), a.span);
                    return self.overloaded_compare(op, lref, b, span);
                }
                Some((p, t)) => {
                    self.check_sized(&t, a.span)?;
                    self.use_place(p, t, a.span)
                }
                None => self.expr(a, arith_expected)?,
            };
            if self.overloadable(op, &l.ty) {
                if op.is_comparison() {
                    let lref = self.borrow_value(l, false);
                    return self.overloaded_compare(op, lref, b, span);
                }
                return self.overloaded_arith(op, l, b, span);
            }
            let lt = l.ty.clone();
            let r = match &lt {
                Ty::Ptr(_) if matches!(op, BinOp::Add | BinOp::Sub) => {
                    self.expr(b, Some(&Ty::Int(IntTy::Usize)))?
                }
                _ => match self.defaulted_expr(&l) {
                    // `i < v.len()` with `i` from a literal: `i` becomes `usize`
                    Some(k) => {
                        let r = self.expr(b, Some(&lt))?;
                        if r.ty != lt && !r.ty.is_never() {
                            let found = r.ty.clone();
                            self.want_literal_type(k, &lt, &found);
                        }
                        self.coerce(r, &lt, b.span)?
                    }
                    None => self.expr_as(b, &lt)?,
                },
            };
            (l, r)
        };
        let (lt, rt) = (l.ty.clone(), r.ty.clone());
        if lt.is_never() || rt.is_never() {
            let ty = if op.is_comparison() {
                Ty::Bool
            } else if lt.is_never() {
                rt.clone()
            } else {
                lt.clone()
            };
            return Ok(x(K::Binary(op, Box::new(l), Box::new(r)), ty, span));
        }
        let bad = |cx: &Self| {
            cx.err::<X>(
                span,
                format!(
                    "cannot apply `{}` to `{}` and `{}`",
                    op.symbol(),
                    cx.ts(&lt),
                    cx.ts(&rt)
                ),
            )
        };
        let ty = match op {
            BinOp::Add | BinOp::Sub if matches!(lt, Ty::Ptr(_)) => {
                if !rt.is_int() {
                    return bad(self);
                }
                lt.clone()
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                if !lt.is_numeric() || lt != rt {
                    return bad(self);
                }
                lt.clone()
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                if !(lt.is_int() || lt == Ty::Bool) || lt != rt {
                    return bad(self);
                }
                lt.clone()
            }
            BinOp::Shl | BinOp::Shr => {
                if !lt.is_int() || !rt.is_int() {
                    return bad(self);
                }
                lt.clone()
            }
            BinOp::Eq | BinOp::Ne => {
                if lt != rt {
                    return bad(self);
                }
                if is_str_ref(&lt) {
                    return self.str_eq(op, l, r, span);
                }
                if !(lt.is_numeric()
                    || lt == Ty::Bool
                    || matches!(lt, Ty::Ptr(_))
                    || self.is_fieldless_enum(&lt))
                {
                    return self.err(span, format!("`{}` cannot be compared with `{}`; compare fields or implement an `eq` method", self.ts(&lt), op.symbol()));
                }
                Ty::Bool
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if lt != rt || !(lt.is_numeric() || matches!(lt, Ty::Ptr(_))) {
                    return bad(self);
                }
                Ty::Bool
            }
            BinOp::And | BinOp::Or => unreachable!(),
        };
        Ok(x(K::Binary(op, Box::new(l), Box::new(r)), ty, span))
    }

    /// Does `op` on values of type `t` go through an operator trait?
    fn overloadable(&self, op: BinOp, t: &Ty) -> bool {
        match t {
            Ty::Adt(..) => !(self.is_fieldless_enum(t) && matches!(op, BinOp::Eq | BinOp::Ne)),
            Ty::Tuple(ts) => !ts.is_empty(),
            Ty::Array(..) => true,
            _ => false,
        }
    }

    /// Call operator-trait method `method` of prelude trait `trait_name` on `self_ty`.
    fn call_operator(
        &mut self,
        trait_name: &str,
        method: &str,
        symbol: &str,
        self_ty: &Ty,
        args: Vec<X>,
        span: Span,
    ) -> Result<X> {
        let tid = self.ck.prelude.and_then(|p| {
            match self.ck.modules[p as usize]
                .scope
                .get(trait_name)
                .map(|b| b.def)
            {
                Some(Def::Trait(t)) => Some(t),
                _ => None,
            }
        });
        let Some(tid) = tid else {
            return self.err(
                span,
                format!("operator `{symbol}` needs the prelude trait `{trait_name}`"),
            );
        };
        let Some((iid, iargs)) = self.ck.find_trait_impl(self_ty, tid) else {
            let tn = self.ts(self_ty);
            let sig = match trait_name {
                "Eq" => format!("fn eq(&self, other: &{tn}) -> bool"),
                "Ord" => format!("fn cmp(&self, other: &{tn}) -> Ordering"),
                "Neg" => format!("fn neg(self) -> {tn}"),
                _ => format!("fn {method}(self, other: {tn}) -> {tn}"),
            };
            return self.err(span, format!("operator `{symbol}` is not defined for `{tn}`; implement `{trait_name}` for it:  impl {trait_name} for {tn} {{ {sig} {{ ... }} }}"));
        };
        if let Some(&fid) = self.ck.impls[iid as usize].methods.get(method) {
            return self.invoke(
                fid,
                Vec::new(),
                Some(iargs),
                None,
                args,
                &[],
                None,
                span,
                span,
            );
        }
        match self.ck.traits[tid as usize].defaults.get(method).copied() {
            Some(fid) => self.invoke(
                fid,
                Vec::new(),
                None,
                Some(self_ty.clone()),
                args,
                &[],
                None,
                span,
                span,
            ),
            None => self.err(
                span,
                format!("trait `{trait_name}` has no method `{method}`"),
            ),
        }
    }

    /// `a == b`, `a < b`, ... on user types: `Eq.eq(&a, &b)`, `Ord.lt(&a, &b)`, ...
    fn overloaded_compare(&mut self, op: BinOp, lref: X, b: &ast::Expr, span: Span) -> Result<X> {
        let Ty::Ref(t, _) = lref.ty.clone() else {
            unreachable!()
        };
        let t = *t;
        let rref = match self.place(b)? {
            Some((p, rt)) => x(K::Borrow(p, false), Ty::reference(rt, false), b.span),
            None => {
                let v = self.expr(b, Some(&t))?;
                self.borrow_value(v, false)
            }
        };
        let rref = self.coerce(rref, &Ty::reference(t.clone(), false), b.span)?;
        let (tr, m) = match op {
            BinOp::Eq => ("Eq", "eq"),
            BinOp::Ne => ("Eq", "ne"),
            BinOp::Lt => ("Ord", "lt"),
            BinOp::Le => ("Ord", "le"),
            BinOp::Gt => ("Ord", "gt"),
            _ => ("Ord", "ge"),
        };
        self.call_operator(tr, m, op.symbol(), &t, vec![lref, rref], span)
    }

    /// `a + b`, ... on user types: `Add.add(a, b)`, ... (operands are moved).
    fn overloaded_arith(&mut self, op: BinOp, l: X, b: &ast::Expr, span: Span) -> Result<X> {
        let t = l.ty.clone();
        let r = self.expr_as(b, &t)?;
        let (tr, m) = match op {
            BinOp::Add => ("Add", "add"),
            BinOp::Sub => ("Sub", "sub"),
            BinOp::Mul => ("Mul", "mul"),
            BinOp::Div => ("Div", "div"),
            BinOp::Rem => ("Rem", "rem"),
            _ => {
                return self.err(
                    span,
                    format!("operator `{}` cannot be overloaded", op.symbol()),
                )
            }
        };
        self.call_operator(tr, m, op.symbol(), &t, vec![l, r], span)
    }

    fn is_fieldless_enum(&self, t: &Ty) -> bool {
        match t {
            Ty::Adt(id, _) => {
                let a = &self.ck.adts[*id as usize];
                a.is_enum() && a.variants.iter().all(|v| v.is_empty())
            }
            _ => false,
        }
    }

    /// `&str == &str` calls the prelude's `str_eq`.
    fn str_eq(&mut self, op: BinOp, l: X, r: X, span: Span) -> Result<X> {
        let Some(p) = self.ck.prelude else {
            return self.err(span, "string comparison requires the standard prelude");
        };
        let Some(Def::Fn(fid)) = self.ck.modules[p as usize]
            .scope
            .get("str_eq")
            .map(|b| b.def)
        else {
            return self.err(span, "string comparison requires `str_eq` in the prelude");
        };
        let name = self.ck.instance(FnKey {
            fid,
            targs: Vec::new(),
            self_ty: None,
        });
        let call = x(K::Call(name, vec![l, r]), Ty::Bool, span);
        if op == BinOp::Ne {
            return Ok(x(K::Unary(UnOp::Not, Box::new(call)), Ty::Bool, span));
        }
        Ok(call)
    }

    fn cast(&mut self, inner: &ast::Expr, target: Ty, span: Span) -> Result<X> {
        let exp = match &target {
            Ty::Int(_) | Ty::Float(_) if untyped_literal(inner) => Some(target.clone()),
            Ty::Ptr(_) if untyped_literal(inner) => Some(Ty::Int(IntTy::Usize)),
            _ => None,
        };
        let v = self.expr(inner, exp.as_ref())?;
        let from = v.ty.clone();
        let ok = match (&from, &target) {
            _ if from == target => true,
            (Ty::Int(_) | Ty::Float(_), Ty::Int(_) | Ty::Float(_)) => true,
            (Ty::Bool, Ty::Int(_)) => true,
            (Ty::Int(IntTy::U8), Ty::Bool) => false,
            (Ty::Ptr(_), Ty::Ptr(_)) => true,
            (Ty::Ptr(_), Ty::Int(IntTy::Usize | IntTy::Isize | IntTy::U64 | IntTy::I64)) => true,
            (Ty::Int(IntTy::Usize | IntTy::Isize | IntTy::U64 | IntTy::I64), Ty::Ptr(_)) => true,
            (Ty::Ref(a, _), Ty::Ptr(b)) => {
                let a = &**a;
                if a == &**b
                    || matches!(a, Ty::Array(e, _) | Ty::Slice(e) if **e == **b)
                    || (*a == Ty::Str && **b == Ty::Int(IntTy::U8))
                {
                    if from.is_fat_ref() {
                        return Ok(x(
                            K::Intrinsic(xir::Intrinsic::DataPtr, vec![v]),
                            target,
                            span,
                        ));
                    }
                    true
                } else {
                    false
                }
            }
            (Ty::Adt(..), Ty::Int(_)) if self.is_fieldless_enum(&from) => true,
            _ => false,
        };
        if !ok {
            return self.err(
                span,
                format!("cannot cast `{}` as `{}`", self.ts(&from), self.ts(&target)),
            );
        }
        Ok(x(K::Cast(Box::new(v), target.clone()), target, span))
    }

    // ------------------------------------------------------- control flow

    fn if_expr(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        els: Option<&ast::Expr>,
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        // `if x is Pattern { ... }` with bindings lowers to a match
        if let E::Is(scrut, pat) = &cond.kind {
            if pattern_binds(pat) {
                return self.if_is(scrut, pat, then, els, expected, span);
            }
        }
        let c = self.expr_as(cond, &Ty::Bool)?;
        match els {
            None => {
                let t = self.block(then, Some(&Ty::unit()))?;
                let t = self.coerce(t, &Ty::unit(), then.span)?;
                Ok(x(K::If(Box::new(c), Box::new(t), None), Ty::unit(), span))
            }
            Some(e) if expected.is_none() && block_is_untyped_literal(then) => {
                // `if c { 4 } else { n * 2 }`: let the typed branch decide
                let ev = self.expr(e, None)?;
                let ty = ev.ty.clone();
                let t = if ty.is_never() {
                    self.block(then, None)?
                } else {
                    self.block(then, Some(&ty))?
                };
                let ty = if ty.is_never() { t.ty.clone() } else { ty };
                let t = self.coerce(t, &ty, then.span)?;
                let ev = self.coerce(ev, &ty, e.span)?;
                Ok(x(
                    K::If(Box::new(c), Box::new(t), Some(Box::new(ev))),
                    ty,
                    span,
                ))
            }
            Some(e) => {
                let t = self.block(then, expected)?;
                let target = match expected {
                    Some(t) => Some(t.clone()),
                    None if t.ty.is_never() => None,
                    None => Some(t.ty.clone()),
                };
                let ev = self.expr(e, target.as_ref())?;
                let ty = match &target {
                    Some(t) => t.clone(),
                    None => ev.ty.clone(),
                };
                let t = self.coerce(t, &ty, then.span)?;
                let ev = self.coerce(ev, &ty, e.span)?;
                Ok(x(
                    K::If(Box::new(c), Box::new(t), Some(Box::new(ev))),
                    ty,
                    span,
                ))
            }
        }
    }

    fn if_is(
        &mut self,
        scrut: &ast::Expr,
        pat: &ast::Pattern,
        then: &ast::Block,
        els: Option<&ast::Expr>,
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let (pre, p, sty) = self.scrutinee(scrut)?;
        let ty_target = if els.is_none() {
            Some(Ty::unit())
        } else {
            expected.cloned()
        };
        self.push_scope();
        let mut binds = Vec::new();
        let xp = self.pattern(pat, &sty, BindMode::Value, &mut binds);
        let xp = match xp {
            Ok(p) => p,
            Err(e) => {
                self.pop_scope();
                return Err(e);
            }
        };
        for (n, id) in &binds {
            self.declare(n, *id);
        }
        let body = self.block(then, ty_target.as_ref());
        let scope = self.pop_scope();
        let body = body?;
        let ty = match (&ty_target, els) {
            (Some(t), _) => t.clone(),
            (None, _) if body.ty.is_never() => Ty::Never,
            (None, _) => body.ty.clone(),
        };
        let other = match els {
            Some(e) => {
                let target = if ty.is_never() {
                    None
                } else {
                    Some(ty.clone())
                };
                self.expr(e, target.as_ref())?
            }
            None => x(K::Unit, Ty::unit(), span),
        };
        let ty = if ty.is_never() { other.ty.clone() } else { ty };
        let body = self.coerce(body, &ty, then.span)?;
        let other = self.coerce(other, &ty, span)?;
        let bspan = body.span;
        let body = x(
            K::Block(xir::Block {
                stmts: Vec::new(),
                tail: Some(Box::new(body)),
                scope,
            }),
            ty.clone(),
            bspan,
        );
        let arms = vec![
            xir::Arm {
                pat: xp,
                guard: None,
                body,
                span: pat.span,
            },
            xir::Arm {
                pat: Pat::Wild,
                guard: None,
                body: other,
                span,
            },
        ];
        self.check_match_moves(&p, &arms, span)?;
        Ok(self.with_pre(pre, x(K::Match(p, arms), ty, span)))
    }

    fn while_loop(&mut self, cond: &ast::Expr, body: &ast::Block, span: Span) -> Result<X> {
        let id = self.next_loop;
        self.next_loop += 1;
        self.loops.push(LoopCx {
            id,
            break_ty: Some(Ty::unit()),
            allows_value: false,
        });
        self.push_scope();
        let r = (|| -> Result<X> {
            if let E::Is(scrut, pat) = &cond.kind {
                if pattern_binds(pat) {
                    // loop { match scrut { pat => body, _ => break } }
                    let (pre, p, sty) = self.scrutinee(scrut)?;
                    self.push_scope();
                    let mut binds = Vec::new();
                    let xp = self.pattern(pat, &sty, BindMode::Value, &mut binds);
                    let xp = match xp {
                        Ok(p) => p,
                        Err(e) => {
                            self.pop_scope();
                            return Err(e);
                        }
                    };
                    for (n, lid) in &binds {
                        self.declare(n, *lid);
                    }
                    let b = self.block(body, Some(&Ty::unit()));
                    let scope = self.pop_scope();
                    let b = self.coerce(b?, &Ty::unit(), body.span)?;
                    let bspan = b.span;
                    let b = x(
                        K::Block(xir::Block {
                            stmts: Vec::new(),
                            tail: Some(Box::new(b)),
                            scope,
                        }),
                        Ty::unit(),
                        bspan,
                    );
                    let arms = vec![
                        xir::Arm {
                            pat: xp,
                            guard: None,
                            body: b,
                            span: pat.span,
                        },
                        xir::Arm {
                            pat: Pat::Wild,
                            guard: None,
                            body: x(K::Break(id, None), Ty::Never, span),
                            span,
                        },
                    ];
                    self.check_match_moves(&p, &arms, span)?;
                    return Ok(self.with_pre(pre, x(K::Match(p, arms), Ty::unit(), span)));
                }
            }
            let c = self.expr_as(cond, &Ty::Bool)?;
            let not_c = x(K::Unary(UnOp::Not, Box::new(c)), Ty::Bool, cond.span);
            let brk = x(
                K::If(
                    Box::new(not_c),
                    Box::new(x(K::Break(id, None), Ty::Never, span)),
                    None,
                ),
                Ty::unit(),
                cond.span,
            );
            let b = self.block(body, Some(&Ty::unit()))?;
            let b = self.coerce(b, &Ty::unit(), body.span)?;
            let bspan = b.span;
            Ok(x(
                K::Block(xir::Block {
                    stmts: vec![xir::Stmt::Expr(brk)],
                    tail: Some(Box::new(b)),
                    scope: Vec::new(),
                }),
                Ty::unit(),
                bspan,
            ))
        })();
        let scope = self.pop_scope();
        self.loops.pop();
        let inner = r?;
        let ispan = inner.span;
        let body = x(
            K::Block(xir::Block {
                stmts: Vec::new(),
                tail: Some(Box::new(inner)),
                scope,
            }),
            Ty::unit(),
            ispan,
        );
        Ok(x(K::Loop(Box::new(body), id), Ty::unit(), span))
    }

    fn for_loop(
        &mut self,
        pat: &ast::Pattern,
        iter: &ast::Expr,
        body: &ast::Block,
        span: Span,
    ) -> Result<X> {
        let id = self.next_loop;
        self.next_loop += 1;
        let usize_t = Ty::Int(IntTy::Usize);
        self.push_scope(); // holds the iteration state
        let r = (|| -> Result<(Vec<xir::Stmt>, X)> {
            let mut pre = Vec::new();
            // Produces (condition, element value, increment) over local state.
            let (cond, elem, step): (X, X, X);
            // the loop variable of `for i in 0..10` is typed by its literals
            let mut range_key: Option<Span> = None;
            if let E::Range(lo, hi, inclusive) = &iter.kind {
                let (Some(lo), Some(hi)) = (lo, hi) else {
                    return self.err(
                        iter.span,
                        "`for` loops need a bounded range `a..b` or `a..=b`",
                    );
                };
                let both_literals = untyped_literal(lo) && untyped_literal(hi);
                let hinted = if both_literals {
                    self.overrides.get(&iter.span).cloned()
                } else {
                    None
                };
                let (l, h) = if let Some(t) = hinted {
                    (self.expr_as(lo, &t)?, self.expr_as(hi, &t)?)
                } else if untyped_literal(lo) && !untyped_literal(hi) {
                    let h = self.expr(hi, None)?;
                    let ht = h.ty.clone();
                    (self.expr_as(lo, &ht)?, h)
                } else {
                    let l = self.expr(lo, None)?;
                    let lt = l.ty.clone();
                    (l, self.expr_as(hi, &lt)?)
                };
                range_key = if both_literals {
                    Some(iter.span)
                } else {
                    self.defaulted_expr(&l).or_else(|| self.defaulted_expr(&h))
                };
                let ity = l.ty.clone();
                if !ity.is_int() {
                    return self.err(
                        iter.span,
                        format!("range bounds must be integers, found `{}`", self.ts(&ity)),
                    );
                }
                let cur = self.synthetic("i", ity.clone(), iter.span);
                let end = self.synthetic("end", ity.clone(), iter.span);
                pre.push(xir::Stmt::Let(cur, l));
                pre.push(xir::Stmt::Let(end, h));
                let usec = |id| {
                    x(
                        K::Use(Place::Local(id), UseKind::Copy),
                        ity.clone(),
                        iter.span,
                    )
                };
                if *inclusive {
                    let done = self.synthetic("done", Ty::Bool, iter.span);
                    pre.push(xir::Stmt::Let(
                        done,
                        x(
                            K::Binary(BinOp::Gt, Box::new(usec(cur)), Box::new(usec(end))),
                            Ty::Bool,
                            iter.span,
                        ),
                    ));
                    cond = x(
                        K::Unary(
                            UnOp::Not,
                            Box::new(x(
                                K::Use(Place::Local(done), UseKind::Copy),
                                Ty::Bool,
                                iter.span,
                            )),
                        ),
                        Ty::Bool,
                        iter.span,
                    );
                    elem = usec(cur);
                    // if cur == end { done = true } else { cur += 1 }
                    let at_end = x(
                        K::Binary(BinOp::Eq, Box::new(usec(cur)), Box::new(usec(end))),
                        Ty::Bool,
                        iter.span,
                    );
                    let set_done = x(
                        K::Assign(
                            Place::Local(done),
                            Box::new(x(K::Bool(true), Ty::Bool, iter.span)),
                        ),
                        Ty::unit(),
                        iter.span,
                    );
                    let inc = x(
                        K::CompoundAssign(
                            BinOp::Add,
                            Place::Local(cur),
                            Box::new(x(K::Int(1), ity.clone(), iter.span)),
                        ),
                        Ty::unit(),
                        iter.span,
                    );
                    step = x(
                        K::If(Box::new(at_end), Box::new(set_done), Some(Box::new(inc))),
                        Ty::unit(),
                        iter.span,
                    );
                } else {
                    cond = x(
                        K::Binary(BinOp::Lt, Box::new(usec(cur)), Box::new(usec(end))),
                        Ty::Bool,
                        iter.span,
                    );
                    elem = usec(cur);
                    step = x(
                        K::CompoundAssign(
                            BinOp::Add,
                            Place::Local(cur),
                            Box::new(x(K::Int(1), ity.clone(), iter.span)),
                        ),
                        Ty::unit(),
                        iter.span,
                    );
                }
            } else {
                // arrays and slices; `for x in [1, 2, 3]` types its literals by later use
                let literal_array = matches!(&iter.kind, E::Array(items) if !items.is_empty() && items.iter().all(untyped_literal));
                let v = match (literal_array, self.overrides.get(&iter.span).cloned()) {
                    (true, Some(t)) => {
                        let E::Array(items) = &iter.kind else {
                            unreachable!()
                        };
                        let at = Ty::Array(Box::new(t), items.len() as u64);
                        self.expr_as(iter, &at)?
                    }
                    _ => self.expr(iter, None)?,
                };
                if literal_array {
                    range_key = Some(iter.span);
                }
                let (seq, by_ref, mutable, elem_ty) = match v.ty.clone() {
                    Ty::Ref(inner, m) => match *inner {
                        Ty::Array(e, _) => {
                            let st = Ty::reference(Ty::Slice(e.clone()), m);
                            let vs = v.span;
                            (x(K::ArrayToSlice(Box::new(v)), st, vs), true, m, *e)
                        }
                        Ty::Slice(e) => (v, true, m, *e),
                        t => {
                            return self
                                .err(iter.span, format!("cannot iterate over `&{}`", self.ts(&t)))
                        }
                    },
                    Ty::Array(e, _) => {
                        if !self.ck.is_copy(&e) {
                            return self.err(iter.span, "iterating an array by value requires copyable elements; iterate over `&array` instead");
                        }
                        let arr = self.synthetic("arr", v.ty.clone(), iter.span);
                        let vt = v.ty.clone();
                        pre.push(xir::Stmt::Let(arr, v));
                        let st = Ty::reference(Ty::Slice(e.clone()), false);
                        let r = x(
                            K::Borrow(Place::Local(arr), false),
                            Ty::reference(vt, false),
                            iter.span,
                        );
                        (
                            x(K::ArrayToSlice(Box::new(r)), st, iter.span),
                            false,
                            false,
                            *e,
                        )
                    }
                    t => {
                        return self.err(
                            iter.span,
                            format!(
                                "cannot iterate over `{}`; use a range `a..b`, an array or a slice",
                                self.ts(&t)
                            ),
                        )
                    }
                };
                let sty = seq.ty.clone();
                let s = self.synthetic("seq", sty.clone(), iter.span);
                pre.push(xir::Stmt::Let(s, seq));
                let k = self.synthetic("k", usize_t.clone(), iter.span);
                pre.push(xir::Stmt::Let(k, x(K::Int(0), usize_t.clone(), iter.span)));
                let n = self.synthetic("n", usize_t.clone(), iter.span);
                let use_s = x(
                    K::Use(Place::Local(s), UseKind::Copy),
                    sty.clone(),
                    iter.span,
                );
                // a `&mut [T]` local is read (not moved) to compute its length
                let shared = if mutable {
                    x(
                        K::Borrow(Place::Deref(Box::new(use_s.clone())), false),
                        Ty::reference(
                            (*match &sty {
                                Ty::Ref(i, _) => i.clone(),
                                _ => unreachable!(),
                            })
                            .clone(),
                            false,
                        ),
                        iter.span,
                    )
                } else {
                    use_s.clone()
                };
                pre.push(xir::Stmt::Let(
                    n,
                    x(
                        K::Intrinsic(xir::Intrinsic::Len, vec![shared]),
                        usize_t.clone(),
                        iter.span,
                    ),
                ));
                let usek = x(
                    K::Use(Place::Local(k), UseKind::Copy),
                    usize_t.clone(),
                    iter.span,
                );
                cond = x(
                    K::Binary(
                        BinOp::Lt,
                        Box::new(usek.clone()),
                        Box::new(x(
                            K::Use(Place::Local(n), UseKind::Copy),
                            usize_t.clone(),
                            iter.span,
                        )),
                    ),
                    Ty::Bool,
                    iter.span,
                );
                let place = Place::Index(Box::new(Place::Deref(Box::new(use_s))), Box::new(usek));
                elem = if by_ref {
                    x(
                        K::Borrow(place, mutable),
                        Ty::reference(elem_ty.clone(), mutable),
                        iter.span,
                    )
                } else {
                    x(K::Use(place, UseKind::Copy), elem_ty.clone(), iter.span)
                };
                step = x(
                    K::CompoundAssign(
                        BinOp::Add,
                        Place::Local(k),
                        Box::new(x(K::Int(1), usize_t.clone(), iter.span)),
                    ),
                    Ty::unit(),
                    iter.span,
                );
            }
            // loop body
            self.loops.push(LoopCx {
                id,
                break_ty: Some(Ty::unit()),
                allows_value: false,
            });
            self.push_scope();
            let inner = (|| -> Result<Vec<xir::Stmt>> {
                let mut stmts = Vec::new();
                let not_c = x(K::Unary(UnOp::Not, Box::new(cond)), Ty::Bool, iter.span);
                stmts.push(xir::Stmt::Expr(x(
                    K::If(
                        Box::new(not_c),
                        Box::new(x(K::Break(id, None), Ty::Never, span)),
                        None,
                    ),
                    Ty::unit(),
                    iter.span,
                )));
                let ety = elem.ty.clone();
                match &pat.kind {
                    ast::PatKind::Bind(name, m) => {
                        let lid = self.new_local(&name.name, ety, *m, name.span, false);
                        stmts.push(xir::Stmt::Let(lid, elem));
                        stmts.push(xir::Stmt::Expr(step));
                        self.declare(&name.name, lid);
                        if let Some(k) = range_key {
                            self.defaulted.insert(lid, k);
                        }
                    }
                    ast::PatKind::Wild => {
                        stmts.push(xir::Stmt::Expr(elem));
                        stmts.push(xir::Stmt::Expr(step));
                    }
                    _ => {
                        let mut binds = Vec::new();
                        let p = self.pattern(pat, &ety, BindMode::Value, &mut binds)?;
                        stmts.push(xir::Stmt::LetPat(p, elem));
                        stmts.push(xir::Stmt::Expr(step));
                        for (n, lid) in binds {
                            self.declare(&n, lid);
                        }
                    }
                }
                let b = self.block(body, Some(&Ty::unit()))?;
                let b = self.coerce(b, &Ty::unit(), body.span)?;
                stmts.push(xir::Stmt::Expr(b));
                Ok(stmts)
            })();
            let scope = self.pop_scope();
            self.loops.pop();
            let stmts = inner?;
            let lb = x(
                K::Block(xir::Block {
                    stmts,
                    tail: None,
                    scope,
                }),
                Ty::unit(),
                body.span,
            );
            Ok((pre, x(K::Loop(Box::new(lb), id), Ty::unit(), span)))
        })();
        let scope = self.pop_scope();
        let (pre, lp) = r?;
        Ok(x(
            K::Block(xir::Block {
                stmts: pre,
                tail: Some(Box::new(lp)),
                scope,
            }),
            Ty::unit(),
            span,
        ))
    }

    fn match_expr(
        &mut self,
        scrut: &ast::Expr,
        arms: &[ast::Arm],
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let (pre, p, sty) = self.scrutinee(scrut)?;
        if arms.is_empty() && !matches!(&sty, Ty::Never) {
            return self.err(span, "`match` needs at least one arm");
        }
        let mut target = expected.cloned();
        // lower arms with typed bodies first so literal arms adopt their type
        let mut order: Vec<usize> = (0..arms.len()).collect();
        if target.is_none() {
            order.sort_by_key(|&i| untyped_literal(&arms[i].body));
        }
        let mut out_slots: Vec<Option<xir::Arm>> = vec![None; arms.len()];
        for &ai in &order {
            let arm = &arms[ai];
            self.push_scope();
            let r = (|| -> Result<xir::Arm> {
                let mut binds = Vec::new();
                let xp = self.pattern(&arm.pat, &sty, BindMode::Value, &mut binds)?;
                for (n, id) in &binds {
                    self.declare(n, *id);
                }
                let guard = match &arm.guard {
                    Some(g) => Some(self.expr_as(g, &Ty::Bool)?),
                    None => None,
                };
                let body = self.expr(&arm.body, target.as_ref())?;
                if target.is_none() && !body.ty.is_never() {
                    target = Some(body.ty.clone());
                }
                Ok(xir::Arm {
                    pat: xp,
                    guard,
                    body,
                    span: arm.span,
                })
            })();
            let scope = self.pop_scope();
            let mut a = r?;
            if let Some(t) = &target {
                a.body = self.coerce(a.body, t, arm.body.span)?;
            }
            let bspan = a.body.span;
            let bty = a.body.ty.clone();
            a.body = x(
                K::Block(xir::Block {
                    stmts: Vec::new(),
                    tail: Some(Box::new(a.body)),
                    scope,
                }),
                bty,
                bspan,
            );
            out_slots[ai] = Some(a);
        }
        let mut out: Vec<xir::Arm> = out_slots.into_iter().map(|a| a.unwrap()).collect();
        if let Some(t) = &target {
            for a in out.iter_mut() {
                if a.body.ty != *t && !a.body.ty.is_never() {
                    let b = std::mem::replace(&mut a.body, x(K::Unit, Ty::unit(), span));
                    let bs = b.span;
                    a.body = self.coerce(b, t, bs)?;
                }
            }
        }
        let ty = target.unwrap_or(Ty::Never);
        for a in out.iter_mut() {
            if a.body.ty.is_never() && !ty.is_never() {
                a.body.ty = ty.clone();
            }
        }
        super::exhaust::check(self.ck, &sty, &out, span)?;
        self.check_match_moves(&p, &out, span)?;
        Ok(self.with_pre(pre, x(K::Match(p, out), ty, span)))
    }

    /// Moving bindings require the scrutinee to be an owned local.
    pub fn check_match_moves(&self, p: &Place, arms: &[xir::Arm], span: Span) -> Result<()> {
        for a in arms {
            if self.pat_moves(&a.pat) {
                if a.guard.is_some() {
                    return self.err(a.span, "cannot move values out in a pattern with an `if` guard; match on a reference instead");
                }
                if !matches!(p, Place::Local(_)) {
                    return self.err(span, "cannot move out of this place in a pattern; match on a reference (`match &value`) or move the value into a variable first");
                }
            }
        }
        Ok(())
    }

    pub fn pat_moves(&self, p: &Pat) -> bool {
        match p {
            Pat::Bind(id, BindMode::Value) => !self.ck.is_copy(&self.locals[*id as usize].ty),
            Pat::Tuple(ps) | Pat::Variant(_, ps) | Pat::Or(ps) => {
                ps.iter().any(|p| self.pat_moves(p))
            }
            Pat::Deref(_) => false,
            _ => false,
        }
    }

    fn result_parts(&self, t: &Ty) -> Option<(AdtId, Vec<Ty>)> {
        let p = self.ck.prelude?;
        let Some(Def::Adt(rid)) = self.ck.modules[p as usize]
            .scope
            .get("Result")
            .map(|b| b.def)
        else {
            return None;
        };
        match t {
            Ty::Adt(id, args) if *id == rid => Some((rid, args.clone())),
            _ => None,
        }
    }

    fn option_parts(&self, t: &Ty) -> Option<(AdtId, Vec<Ty>)> {
        let p = self.ck.prelude?;
        let Some(Def::Adt(oid)) = self.ck.modules[p as usize]
            .scope
            .get("Option")
            .map(|b| b.def)
        else {
            return None;
        };
        match t {
            Ty::Adt(id, args) if *id == oid => Some((oid, args.clone())),
            _ => None,
        }
    }

    /// `try e`: unwrap `Ok`/`Some` or return the `Err`/`None` early.
    fn try_expr(&mut self, inner: &ast::Expr, span: Span) -> Result<X> {
        let v = self.expr(inner, None)?;
        let vty = v.ty.clone();
        let rt = self.ret_ty.clone();
        let tmp = self.synthetic("try", vty.clone(), span);
        if let Some((rid, args)) = self.result_parts(&vty) {
            let Some((_, rargs)) = self.result_parts(&rt) else {
                return self.err(span, format!("`try` on a `Result` requires the function to return `Result<_, {}>`, but it returns `{}`", self.ts(&args[1]), self.ts(&rt)));
            };
            if rargs[1] != args[1] {
                return self.err(span, format!("`try` error type mismatch: expression has error type `{}`, function returns error type `{}`", self.ts(&args[1]), self.ts(&rargs[1])));
            }
            let a = &self.ck.adts[rid as usize];
            let (ok, er) = (
                a.variant_index("Ok").unwrap(),
                a.variant_index("Err").unwrap(),
            );
            let val = self.new_local("ok", args[0].clone(), false, span, true);
            let err = self.new_local("err", args[1].clone(), false, span, true);
            let ret = x(
                K::Return(Some(Box::new(x(
                    K::Variant(
                        er,
                        vec![self.use_place(Place::Local(err), args[1].clone(), span)],
                    ),
                    rt,
                    span,
                )))),
                Ty::Never,
                span,
            );
            let arms = vec![
                xir::Arm {
                    pat: Pat::Variant(ok, vec![Pat::Bind(val, BindMode::Value)]),
                    guard: None,
                    body: self.wrap_scope(
                        self.use_place(Place::Local(val), args[0].clone(), span),
                        vec![val],
                    ),
                    span,
                },
                xir::Arm {
                    pat: Pat::Variant(er, vec![Pat::Bind(err, BindMode::Value)]),
                    guard: None,
                    body: self.wrap_scope(ret, vec![err]),
                    span,
                },
            ];
            let m = x(K::Match(Place::Local(tmp), arms), args[0].clone(), span);
            return Ok(x(
                K::Block(xir::Block {
                    stmts: vec![xir::Stmt::Let(tmp, v)],
                    tail: Some(Box::new(m)),
                    scope: Vec::new(),
                }),
                args[0].clone(),
                span,
            ));
        }
        if let Some((oid, args)) = self.option_parts(&vty) {
            if self.option_parts(&rt).is_none() {
                return self.err(span, format!("`try` on an `Option` requires the function to return `Option<_>`, but it returns `{}`", self.ts(&rt)));
            }
            let a = &self.ck.adts[oid as usize];
            let (some, none) = (
                a.variant_index("Some").unwrap(),
                a.variant_index("None").unwrap(),
            );
            let val = self.new_local("some", args[0].clone(), false, span, true);
            let ret = x(
                K::Return(Some(Box::new(x(K::Variant(none, Vec::new()), rt, span)))),
                Ty::Never,
                span,
            );
            let arms = vec![
                xir::Arm {
                    pat: Pat::Variant(some, vec![Pat::Bind(val, BindMode::Value)]),
                    guard: None,
                    body: self.wrap_scope(
                        self.use_place(Place::Local(val), args[0].clone(), span),
                        vec![val],
                    ),
                    span,
                },
                xir::Arm {
                    pat: Pat::Variant(none, Vec::new()),
                    guard: None,
                    body: ret,
                    span,
                },
            ];
            let m = x(K::Match(Place::Local(tmp), arms), args[0].clone(), span);
            return Ok(x(
                K::Block(xir::Block {
                    stmts: vec![xir::Stmt::Let(tmp, v)],
                    tail: Some(Box::new(m)),
                    scope: Vec::new(),
                }),
                args[0].clone(),
                span,
            ));
        }
        self.err(
            span,
            format!(
                "`try` expects a `Result` or `Option`, found `{}`",
                self.ts(&vty)
            ),
        )
    }

    fn wrap_scope(&self, body: X, scope: Vec<LocalId>) -> X {
        let ty = body.ty.clone();
        let span = body.span;
        x(
            K::Block(xir::Block {
                stmts: Vec::new(),
                tail: Some(Box::new(body)),
                scope,
            }),
            ty,
            span,
        )
    }

    /// `e catch err { handler }`
    fn catch_expr(
        &mut self,
        inner: &ast::Expr,
        binding: Option<&Ident>,
        handler: &ast::Block,
        expected: Option<&Ty>,
        span: Span,
    ) -> Result<X> {
        let v = self.expr(inner, None)?;
        let vty = v.ty.clone();
        let tmp = self.synthetic("catch", vty.clone(), span);
        let (ok_idx, err_idx, val_ty, err_ty) = if let Some((rid, args)) = self.result_parts(&vty) {
            let a = &self.ck.adts[rid as usize];
            (
                a.variant_index("Ok").unwrap(),
                a.variant_index("Err").unwrap(),
                args[0].clone(),
                Some(args[1].clone()),
            )
        } else if let Some((oid, args)) = self.option_parts(&vty) {
            if binding.is_some() {
                return self.err(
                    span,
                    "`catch` on an `Option` has no error value to bind; write `catch { ... }`",
                );
            }
            let a = &self.ck.adts[oid as usize];
            (
                a.variant_index("Some").unwrap(),
                a.variant_index("None").unwrap(),
                args[0].clone(),
                None,
            )
        } else {
            return self.err(
                span,
                format!(
                    "`catch` expects a `Result` or `Option`, found `{}`",
                    self.ts(&vty)
                ),
            );
        };
        let _ = expected;
        let val = self.new_local("ok", val_ty.clone(), false, span, true);
        self.push_scope();
        let mut err_pat = Vec::new();
        if let Some(et) = &err_ty {
            match binding {
                Some(b) => {
                    let id = self.new_local(&b.name, et.clone(), false, b.span, false);
                    self.declare(&b.name, id);
                    err_pat.push(Pat::Bind(id, BindMode::Value));
                }
                None => err_pat.push(Pat::Wild),
            }
        }
        let h = self.block(handler, Some(&val_ty));
        let scope = self.pop_scope();
        let h = self.coerce(h?, &val_ty, handler.span)?;
        let arms = vec![
            xir::Arm {
                pat: Pat::Variant(ok_idx, vec![Pat::Bind(val, BindMode::Value)]),
                guard: None,
                body: self.wrap_scope(
                    self.use_place(Place::Local(val), val_ty.clone(), span),
                    vec![val],
                ),
                span,
            },
            xir::Arm {
                pat: Pat::Variant(err_idx, err_pat),
                guard: None,
                body: self.wrap_scope(h, scope),
                span: handler.span,
            },
        ];
        let m = x(K::Match(Place::Local(tmp), arms), val_ty.clone(), span);
        Ok(x(
            K::Block(xir::Block {
                stmts: vec![xir::Stmt::Let(tmp, v)],
                tail: Some(Box::new(m)),
                scope: Vec::new(),
            }),
            val_ty,
            span,
        ))
    }
}

/// An array literal of untyped literals: `[0; 4]`, `[1, 2, 3]`.
fn literal_array(e: &ast::Expr) -> bool {
    match &e.kind {
        E::ArrayRepeat(v, _) => untyped_literal(v),
        E::Array(items) => !items.is_empty() && items.iter().all(untyped_literal),
        _ => false,
    }
}

fn block_is_untyped_literal(b: &ast::Block) -> bool {
    matches!(b.stmts.as_slice(), [ast::Stmt::Expr(e, false)] if untyped_literal(e))
}

/// An integer literal (possibly negated) without a type suffix.
pub fn untyped_literal(e: &ast::Expr) -> bool {
    match &e.kind {
        E::Int(_, None) | E::Float(_, None) | E::Char(_) => true,
        E::Unary(UnOp::Neg, inner) => untyped_literal(inner),
        E::Binary(op, a, b) if !op.is_comparison() && !matches!(op, BinOp::And | BinOp::Or) => {
            untyped_literal(a) && untyped_literal(b)
        }
        _ => false,
    }
}

fn is_str_ref(t: &Ty) -> bool {
    matches!(t, Ty::Ref(inner, _) if **inner == Ty::Str)
}

pub fn pattern_binds(p: &ast::Pattern) -> bool {
    match &p.kind {
        // a bare uppercase name is probably a unit variant; lowering decides
        ast::PatKind::Bind(..) => true,
        ast::PatKind::Tuple(ps) | ast::PatKind::Or(ps) => ps.iter().any(pattern_binds),
        ast::PatKind::Variant(_, Some(ps)) => ps.iter().any(pattern_binds),
        _ => false,
    }
}
