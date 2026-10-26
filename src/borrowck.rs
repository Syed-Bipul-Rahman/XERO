//! Ownership and borrow checking on XIR.
//!
//! Enforces, at compile time and with zero runtime cost:
//! * no use of a value after it has been moved (flow-sensitive, including
//!   moves in previous loop iterations),
//! * mutation only through `let mut` bindings or `&mut` references,
//! * aliasing XOR mutation: while a `&mut` loan is live nothing else may
//!   touch the borrowed place; while a `&` loan is live it may not be
//!   mutated or moved,
//! * no reference outlives the value it points to (block exits, returns).
//!
//! Loans are live from creation until the last use of the variable holding
//! them (non-lexical lifetimes approximated by evaluation order; a use inside
//! a loop keeps loans of outer variables live for the whole loop).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::check::Checker;
use crate::diag::{Diagnostic, Span};
use crate::types::Ty;
use crate::xir::{self, BindMode, Expr, ExprKind as K, Func, LocalId, Pat, Place, Stmt, UseKind};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Root {
    Local(LocalId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Proj {
    Field(u32),
    Index,
    Deref,
}

#[derive(Clone, Debug)]
struct Loan {
    root: Root,
    path: Vec<Proj>,
    mutable: bool,
    span: Span,
}

impl Loan {
    fn via_deref(&self) -> bool {
        self.path.contains(&Proj::Deref)
    }
}

type Loans = BTreeSet<usize>;

#[derive(Clone, Default)]
struct State {
    moved: HashMap<LocalId, Span>,
    holders: HashMap<LocalId, Loans>,
    diverged: bool,
}

impl State {
    fn merge(&mut self, other: &State) {
        if other.diverged {
            return;
        }
        if self.diverged {
            *self = other.clone();
            return;
        }
        for (k, v) in &other.moved {
            self.moved.entry(*k).or_insert(*v);
        }
        for (k, v) in &other.holders {
            self.holders.entry(*k).or_default().extend(v.iter().copied());
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Access {
    Read,
    Write,
    Move,
    Shared,
    Mutable,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Collect,
    Check,
}

struct Ck<'a> {
    ck: &'a Checker,
    f: &'a Func,
    mode: Mode,
    pos: u32,
    last_use: HashMap<LocalId, u32>,
    decl_pos: HashMap<LocalId, u32>,
    loop_ranges: Vec<(u32, u32)>,
    loans: Vec<Loan>,
    st: State,
    in_flight: Vec<Loans>,
    /// break states per loop id, continue states per loop id
    breaks: HashMap<u32, Vec<State>>,
    continues: HashMap<u32, Vec<State>>,
    /// locals currently in scope
    live_scope: Vec<Vec<LocalId>>,
    errors: Vec<Diagnostic>,
    seen: HashSet<(u32, u32, String)>,
}

pub fn check(ck: &Checker, f: &Func) -> Result<(), Vec<Diagnostic>> {
    // pass 1: liveness
    let mut c = Ck::new(ck, f, Mode::Collect);
    c.run();
    let mut last_use = c.last_use;
    let decl_pos = c.decl_pos;
    let ranges = c.loop_ranges;
    for _ in 0..4 {
        for &(s, e) in &ranges {
            for (l, u) in last_use.iter_mut() {
                let d = decl_pos.get(l).copied().unwrap_or(0);
                if d < s && *u >= s && *u <= e {
                    *u = (*u).max(e);
                }
            }
        }
    }
    // pass 2: checking
    let mut c = Ck::new(ck, f, Mode::Check);
    c.last_use = last_use;
    c.decl_pos = decl_pos;
    c.run();
    if c.errors.is_empty() {
        Ok(())
    } else {
        Err(c.errors)
    }
}

impl<'a> Ck<'a> {
    fn new(ck: &'a Checker, f: &'a Func, mode: Mode) -> Self {
        Ck {
            ck,
            f,
            mode,
            pos: 0,
            last_use: HashMap::new(),
            decl_pos: HashMap::new(),
            loop_ranges: Vec::new(),
            loans: Vec::new(),
            st: State::default(),
            in_flight: Vec::new(),
            breaks: HashMap::new(),
            continues: HashMap::new(),
            live_scope: Vec::new(),
            errors: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn run(&mut self) {
        let f = self.f;
        self.live_scope.push(f.params.clone());
        for &p in &f.params {
            self.decl_pos.insert(p, 0);
        }
        let v = self.expr(&f.body);
        if !self.st.diverged {
            self.check_escape(&v, f.body.span, true);
        }
        self.live_scope.pop();
    }

    fn error(&mut self, span: Span, msg: String) {
        if self.mode != Mode::Check {
            return;
        }
        if self.seen.insert((span.lo, span.hi, msg.clone())) {
            self.errors.push(Diagnostic::new(span, msg));
        }
    }

    fn error_note(&mut self, span: Span, msg: String, note_span: Span, note: &str) {
        if self.mode != Mode::Check {
            return;
        }
        if self.seen.insert((span.lo, span.hi, msg.clone())) {
            self.errors.push(Diagnostic::new(span, msg).note(note_span, note));
        }
    }

    fn name(&self, l: LocalId) -> String {
        let loc = &self.f.locals[l as usize];
        if loc.synthetic {
            "temporary value".into()
        } else {
            format!("`{}`", loc.name)
        }
    }

    fn tick(&mut self) -> u32 {
        self.pos += 1;
        self.pos
    }

    fn note_use(&mut self, l: LocalId) {
        if self.mode == Mode::Collect {
            let p = self.pos;
            let e = self.last_use.entry(l).or_insert(0);
            *e = (*e).max(p);
        }
    }

    fn declare(&mut self, l: LocalId) {
        let p = self.tick();
        if self.mode == Mode::Collect {
            self.decl_pos.insert(l, p);
        }
        self.st.moved.remove(&l);
        self.st.holders.remove(&l);
    }

    // ---------------------------------------------------------------- places

    /// Root, projection path for conflict detection.
    fn place_path(p: &Place) -> Option<(Root, Vec<Proj>)> {
        match p {
            Place::Local(l) => Some((Root::Local(*l), Vec::new())),
            Place::Static(..) => None,
            Place::Field(b, i) => {
                let (r, mut path) = Self::place_path(b)?;
                path.push(Proj::Field(*i));
                Some((r, path))
            }
            Place::Index(b, _) => {
                let (r, mut path) = Self::place_path(b)?;
                path.push(Proj::Index);
                Some((r, path))
            }
            Place::Deref(e) => match &e.kind {
                K::Use(inner, _) => {
                    let (r, mut path) = Self::place_path(inner)?;
                    path.push(Proj::Deref);
                    Some((r, path))
                }
                _ => None,
            },
        }
    }

    fn root_local(p: &Place) -> Option<LocalId> {
        match Self::place_path(p) {
            Some((Root::Local(l), _)) => Some(l),
            None => None,
        }
    }

    /// Evaluate the sub-expressions inside a place; returns loans flowing
    /// out of it (for reads of reference-typed data).
    fn place_exprs(&mut self, p: &Place) -> Loans {
        match p {
            Place::Local(l) => {
                self.note_use(*l);
                self.st.holders.get(l).cloned().unwrap_or_default()
            }
            Place::Static(..) => Loans::new(),
            Place::Field(b, _) => self.place_exprs(b),
            Place::Index(b, i) => {
                let mut l = self.place_exprs(b);
                self.in_flight.push(l.clone());
                let li = self.expr(i);
                self.in_flight.pop();
                l.extend(li);
                l
            }
            Place::Deref(e) => self.expr(e),
        }
    }

    fn is_mutable(&self, p: &Place) -> Result<(), String> {
        match p {
            Place::Local(l) => {
                let loc = &self.f.locals[*l as usize];
                if loc.mutable || loc.synthetic {
                    Ok(())
                } else {
                    Err(format!("cannot mutate immutable variable `{}`; declare it with `let mut {}`", loc.name, loc.name))
                }
            }
            Place::Static(name, m) => {
                if *m {
                    Ok(())
                } else {
                    Err(format!("cannot mutate immutable static `{}`", name.rsplit('.').next().unwrap_or(name)))
                }
            }
            Place::Field(b, _) | Place::Index(b, _) => self.is_mutable(b),
            Place::Deref(e) => match &e.ty {
                Ty::Ref(_, true) | Ty::Ptr(_) => Ok(()),
                Ty::Ref(_, false) => Err("cannot mutate through a `&` reference; use `&mut`".into()),
                _ => Ok(()),
            },
        }
    }

    fn live_loans(&self) -> Vec<usize> {
        let mut out = BTreeSet::new();
        let in_scope: HashSet<LocalId> = self.live_scope.iter().flatten().copied().collect();
        for (h, loans) in &self.st.holders {
            if !in_scope.contains(h) {
                continue;
            }
            let lu = self.last_use.get(h).copied().unwrap_or(0);
            if lu >= self.pos {
                out.extend(loans.iter().copied());
            }
        }
        for f in &self.in_flight {
            out.extend(f.iter().copied());
        }
        out.into_iter().collect()
    }

    fn overlaps(a: &[Proj], b: &[Proj]) -> bool {
        a.iter().zip(b).all(|(x, y)| x == y || matches!((x, y), (Proj::Index, Proj::Index)))
    }

    /// Check an access against live loans and the moved set.
    fn access(&mut self, p: &Place, kind: Access, span: Span) {
        if self.mode != Mode::Check {
            return;
        }
        let Some((root, path)) = Self::place_path(p) else { return };
        let Root::Local(l) = root.clone();
        // use after move
        if kind != Access::Write || !path.is_empty() {
            if let Some(&mspan) = self.st.moved.get(&l) {
                let msg = if kind == Access::Write { format!("cannot assign to a part of moved value {}", self.name(l)) } else { format!("use of moved value {}", self.name(l)) };
                self.error_note(span, msg, mspan, "value moved here");
                return;
            }
        }
        // mutability
        if matches!(kind, Access::Write | Access::Mutable) {
            if let Err(m) = self.is_mutable(p) {
                self.error(span, m);
                return;
            }
        }
        for id in self.live_loans() {
            let loan = &self.loans[id];
            if loan.root != root {
                continue;
            }
            if !Self::overlaps(&loan.path, &path) {
                continue;
            }
            // overwriting/moving a reference itself doesn't affect what it points to
            if matches!(kind, Access::Write | Access::Move) && loan.path.len() > path.len() && loan.path[path.len()..].contains(&Proj::Deref) {
                continue;
            }
            let conflict = match (loan.mutable, kind) {
                (true, _) => true,
                (false, Access::Read | Access::Shared) => false,
                (false, _) => true,
            };
            if conflict {
                let what = match kind {
                    Access::Read => "use",
                    Access::Write => "assign to",
                    Access::Move => "move out of",
                    Access::Shared => "borrow",
                    Access::Mutable => "mutably borrow",
                };
                let lspan = loan.span;
                let how = if loan.mutable { "mutably borrowed" } else { "borrowed" };
                let msg = format!("cannot {what} {} because it is {how}", self.name(l));
                let note = if loan.mutable { "mutable borrow created here (and used later)" } else { "borrow created here (and used later)" };
                self.error_note(span, msg, lspan, note);
                return;
            }
        }
    }

    fn new_loan(&mut self, p: &Place, mutable: bool, span: Span) -> Option<usize> {
        let (root, path) = Self::place_path(p)?;
        self.loans.push(Loan { root, path, mutable, span });
        Some(self.loans.len() - 1)
    }

    fn mark_moved(&mut self, p: &Place, span: Span) {
        if self.mode != Mode::Check && self.mode != Mode::Collect {
            return;
        }
        match p {
            Place::Local(l) => {
                self.st.moved.insert(*l, span);
            }
            Place::Field(..) => {
                // consuming a field consumes the whole (owned) value
                if let Some(l) = Self::root_local(p) {
                    self.st.moved.insert(l, span);
                }
            }
            _ => {}
        }
    }

    /// Moving out of a place is only allowed for owned locals and their fields.
    fn check_movable(&mut self, p: &Place, ty: &Ty, span: Span) {
        let mut cur = p;
        loop {
            match cur {
                Place::Local(_) => break,
                Place::Field(b, _) => {
                    // a field of a type with a `drop` block cannot be moved out
                    if let Some(bt) = self.place_ty(b) {
                        if let Ty::Adt(id, _) = &bt {
                            if self.ck.adts[*id as usize].drop_impl.is_some() {
                                let n = self.ck.adts[*id as usize].name.clone();
                                self.error(span, format!("cannot move a field out of `{n}`, which has a `drop` block; borrow it instead"));
                                return;
                            }
                        }
                    }
                    cur = b;
                }
                Place::Index(..) => {
                    self.error(span, format!("cannot move a `{}` out of an array or slice element; borrow it with `&` instead", self.ck.ty_str(ty)));
                    return;
                }
                Place::Deref(_) => {
                    self.error(span, format!("cannot move a `{}` out of a reference; borrow it or use `copy` on a copyable type", self.ck.ty_str(ty)));
                    return;
                }
                Place::Static(..) => {
                    self.error(span, "cannot move out of a static".into());
                    return;
                }
            }
        }
    }

    fn place_ty(&self, p: &Place) -> Option<Ty> {
        match p {
            Place::Local(l) => Some(self.f.locals[*l as usize].ty.clone()),
            Place::Static(..) => None,
            Place::Field(b, i) => match self.place_ty(b)? {
                Ty::Adt(id, args) => self.ck.struct_fields(id, &args).get(*i as usize).cloned(),
                Ty::Tuple(ts) => ts.get(*i as usize).cloned(),
                _ => None,
            },
            Place::Index(b, _) => match self.place_ty(b)? {
                Ty::Array(e, _) | Ty::Slice(e) => Some(*e),
                _ => None,
            },
            Place::Deref(e) => match &e.ty {
                Ty::Ref(t, _) | Ty::Ptr(t) => Some((**t).clone()),
                _ => None,
            },
        }
    }

    // ------------------------------------------------------------ expressions

    fn exprs_in_flight(&mut self, xs: &[Expr]) -> Loans {
        let mut all = Loans::new();
        let base = self.in_flight.len();
        let mut reserved: Vec<(usize, Span, Place)> = Vec::new();
        for e in xs {
            // two-phase borrows: `&mut place` arguments are reserved until the call
            if let K::Borrow(p, true) = &e.kind {
                self.tick();
                let pl = self.place_exprs(p);
                self.access(p, Access::Shared, e.span);
                if let Some(id) = self.new_loan(p, true, e.span) {
                    reserved.push((id, e.span, p.clone()));
                    all.insert(id);
                } else {
                    self.access(p, Access::Mutable, e.span);
                }
                all.extend(pl.iter().copied().filter(|_| matches!(p, Place::Deref(_))));
                continue;
            }
            let l = self.expr(e);
            all.extend(l.iter().copied());
            self.in_flight.push(l);
        }
        // activate reserved mutable loans
        if self.mode == Mode::Check {
            let others: Vec<usize> = self.in_flight[base..].iter().flatten().copied().collect();
            for (id, span, place) in &reserved {
                let me = self.loans[*id].clone();
                let conflicting = others.iter().copied().chain(reserved.iter().map(|r| r.0).filter(|r| r != id)).find(|o| {
                    let ol = &self.loans[*o];
                    ol.root == me.root && Self::overlaps(&ol.path, &me.path)
                });
                if let Some(o) = conflicting {
                    let Root::Local(l) = me.root.clone();
                    let ospan = self.loans[o].span;
                    let msg = format!("cannot mutably borrow {} because it is also borrowed by another argument", self.name(l));
                    self.error_note(*span, msg, ospan, "other borrow here");
                } else {
                    // activation: the full `&mut` check against live loans and mutability
                    self.access(place, Access::Mutable, *span);
                }
            }
        }
        self.in_flight.truncate(base);
        all
    }

    fn expr(&mut self, e: &Expr) -> Loans {
        self.tick();
        match &e.kind {
            K::Int(_) | K::Float(_) | K::Bool(_) | K::Str(_) | K::Unit => Loans::new(),
            K::Use(p, kind) => {
                let loans = self.place_exprs(p);
                match kind {
                    UseKind::Copy => self.access(p, Access::Read, e.span),
                    UseKind::Move => {
                        self.access(p, Access::Move, e.span);
                        if self.mode == Mode::Check {
                            self.check_movable(p, &e.ty, e.span);
                        }
                        self.mark_moved(p, e.span);
                    }
                }
                if crate::check::contains_ref(&e.ty) {
                    loans
                } else {
                    Loans::new()
                }
            }
            K::Borrow(p, m) => {
                let inner = self.place_exprs(p);
                self.access(p, if *m { Access::Mutable } else { Access::Shared }, e.span);
                let mut out = Loans::new();
                if let Some(id) = self.new_loan(p, *m, e.span) {
                    out.insert(id);
                }
                // reborrows through a reference keep the original loans alive
                if matches!(p, Place::Deref(_)) || place_has_deref(p) {
                    out.extend(inner);
                }
                out
            }
            K::Unary(_, a) => {
                self.expr(a);
                Loans::new()
            }
            K::Cast(a, _) => {
                let l = self.expr(a);
                if crate::check::contains_ref(&e.ty) {
                    l
                } else {
                    Loans::new()
                }
            }
            K::ArrayToSlice(a) => self.expr(a),
            K::Binary(_, a, b) => {
                let la = self.expr(a);
                self.in_flight.push(la);
                self.expr(b);
                self.in_flight.pop();
                Loans::new()
            }
            K::Call(_, args) | K::Intrinsic(_, args) => {
                let l = self.exprs_in_flight(args);
                if crate::check::contains_ref(&e.ty) {
                    l
                } else {
                    Loans::new()
                }
            }
            K::Struct(xs) | K::Tuple(xs) | K::Array(xs) | K::Variant(_, xs) => self.exprs_in_flight(xs),
            K::ArrayRepeat(a, _) => self.expr(a),
            K::Assign(p, v) => {
                let lv = self.expr(v);
                self.tick();
                self.in_flight.push(lv.clone());
                let _ = self.place_exprs(p);
                self.in_flight.pop();
                self.access(p, Access::Write, e.span);
                match p {
                    Place::Local(l) => {
                        self.st.moved.remove(l);
                        self.st.holders.insert(*l, lv);
                    }
                    _ => {
                        if let Some(l) = Self::root_local(p) {
                            if !place_has_deref(p) {
                                self.st.holders.entry(l).or_default().extend(lv);
                            }
                        }
                    }
                }
                Loans::new()
            }
            K::CompoundAssign(_, p, v) => {
                self.expr(v);
                self.tick();
                let _ = self.place_exprs(p);
                self.access(p, Access::Read, e.span);
                self.access(p, Access::Write, e.span);
                Loans::new()
            }
            K::Block(b) => self.block(b, e.span),
            K::If(c, t, els) => {
                self.expr(c);
                let start = self.st.clone();
                let lt = self.expr(t);
                let after_then = std::mem::replace(&mut self.st, start);
                let le = match els {
                    Some(x) => self.expr(x),
                    None => Loans::new(),
                };
                self.st.merge(&after_then);
                if after_then.diverged && els.is_some() && self.st.diverged {
                    self.st.diverged = true;
                }
                let mut out = lt;
                out.extend(le);
                out
            }
            K::Loop(body, id) => {
                let start_pos = self.pos;
                let entry = self.st.clone();
                self.breaks.insert(*id, Vec::new());
                self.continues.insert(*id, Vec::new());
                self.expr(body);
                let end_pos = self.pos;
                if self.mode == Mode::Collect {
                    self.loop_ranges.push((start_pos, end_pos));
                } else {
                    // second iteration: values moved in the first one are now moved
                    let mut head = entry.clone();
                    head.merge(&self.st);
                    for c in self.continues.get(id).cloned().unwrap_or_default() {
                        head.merge(&c);
                    }
                    self.st = head;
                    self.pos = start_pos;
                    self.continues.insert(*id, Vec::new());
                    self.expr(body);
                    self.pos = end_pos;
                }
                let breaks = self.breaks.remove(id).unwrap_or_default();
                self.continues.remove(id);
                let mut out = State { diverged: true, ..Default::default() };
                for b in &breaks {
                    out.merge(b);
                }
                self.st = out;
                Loans::new()
            }
            K::Break(id, v) => {
                if let Some(v) = v {
                    self.expr(v);
                }
                let s = self.st.clone();
                self.breaks.entry(*id).or_default().push(s);
                self.st.diverged = true;
                Loans::new()
            }
            K::Continue(id) => {
                let s = self.st.clone();
                self.continues.entry(*id).or_default().push(s);
                self.st.diverged = true;
                Loans::new()
            }
            K::Return(v) => {
                if let Some(v) = v {
                    let l = self.expr(v);
                    self.check_escape(&l, v.span, true);
                }
                self.st.diverged = true;
                Loans::new()
            }
            K::Match(p, arms) => {
                let scrut_loans = self.place_exprs(p);
                let moves = arms.iter().any(|a| self.pat_moves(&a.pat));
                if !moves {
                    self.access(p, Access::Read, e.span);
                }
                let start = self.st.clone();
                let mut merged = State { diverged: true, ..Default::default() };
                let mut out = Loans::new();
                for arm in arms {
                    self.st = start.clone();
                    self.tick();
                    if self.pat_moves(&arm.pat) {
                        self.access(p, Access::Move, arm.span);
                        if self.mode == Mode::Check {
                            if let Some(t) = self.place_ty(p) {
                                self.check_movable(p, &t, arm.span);
                            }
                        }
                        self.mark_moved(p, arm.span);
                    }
                    self.bind_pat(&arm.pat, Some(p), &scrut_loans, arm.span);
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    let l = self.expr(&arm.body);
                    out.extend(l);
                    merged.merge(&self.st);
                }
                self.st = merged;
                out
            }
            K::Is(p, _) => {
                self.place_exprs(p);
                self.access(p, Access::Read, e.span);
                Loans::new()
            }
            K::Drop(p) => {
                self.place_exprs(p);
                self.access(p, Access::Move, e.span);
                self.mark_moved(p, e.span);
                Loans::new()
            }
        }
    }

    fn pat_moves(&self, p: &Pat) -> bool {
        match p {
            Pat::Bind(id, BindMode::Value) => !self.ck.is_copy(&self.f.locals[*id as usize].ty),
            Pat::Tuple(ps) | Pat::Variant(_, ps) | Pat::Or(ps) => ps.iter().any(|p| self.pat_moves(p)),
            _ => false,
        }
    }

    /// Declare pattern bindings; by-reference bindings borrow the scrutinee.
    fn bind_pat(&mut self, p: &Pat, scrut: Option<&Place>, scrut_loans: &Loans, span: Span) {
        match p {
            Pat::Bind(id, mode) => {
                self.declare(*id);
                let mut held = Loans::new();
                if let (BindMode::Ref(m), Some(sp)) = (mode, scrut) {
                    self.access(sp, if *m { Access::Mutable } else { Access::Shared }, span);
                    if let Some(l) = self.new_loan(sp, *m, span) {
                        held.insert(l);
                    }
                    if place_has_deref(sp) {
                        held.extend(scrut_loans.iter().copied());
                    }
                } else if crate::check::contains_ref(&self.f.locals[*id as usize].ty) {
                    held.extend(scrut_loans.iter().copied());
                }
                self.st.holders.insert(*id, held);
                if let Some(s) = self.live_scope.last_mut() {
                    s.push(*id);
                }
            }
            Pat::Tuple(ps) | Pat::Variant(_, ps) | Pat::Or(ps) => {
                for q in ps {
                    self.bind_pat(q, scrut, scrut_loans, span);
                }
            }
            Pat::Deref(inner) => self.bind_pat(inner, scrut, scrut_loans, span),
            _ => {}
        }
    }

    fn block(&mut self, b: &xir::Block, span: Span) -> Loans {
        self.live_scope.push(Vec::new());
        // synthetic locals owned by this block are in scope for its duration
        for &l in &b.scope {
            if self.f.locals[l as usize].synthetic {
                self.live_scope.last_mut().unwrap().push(l);
            }
        }
        for s in &b.stmts {
            match s {
                Stmt::Let(l, v) => {
                    let lv = self.expr(v);
                    self.declare(*l);
                    self.st.holders.insert(*l, lv);
                    if !self.f.locals[*l as usize].synthetic {
                        self.live_scope.last_mut().unwrap().push(*l);
                    } else if !self.live_scope.iter().any(|s| s.contains(l)) {
                        // synthetic local owned by an enclosing block
                        if let Some(owner) = self.owner_scope(*l) {
                            self.live_scope[owner].push(*l);
                        }
                    }
                }
                Stmt::LetPat(p, v) => {
                    let lv = self.expr(v);
                    self.bind_pat(p, None, &lv, span);
                }
                Stmt::Expr(x) => {
                    self.expr(x);
                }
            }
        }
        let tail = match &b.tail {
            Some(t) => self.expr(t),
            None => Loans::new(),
        };
        // locals of this block die here
        let dying: Vec<LocalId> = b.scope.clone();
        if !self.st.diverged {
            self.check_dying(&dying, &tail, b.tail.as_ref().map(|t| t.span).unwrap_or(span));
        }
        self.live_scope.pop();
        for l in &dying {
            self.st.holders.remove(l);
        }
        tail
    }

    fn owner_scope(&self, _l: LocalId) -> Option<usize> {
        if self.live_scope.is_empty() {
            None
        } else {
            Some(self.live_scope.len() - 1)
        }
    }

    /// Values escaping a scope must not borrow from locals that die with it.
    fn check_dying(&mut self, dying: &[LocalId], tail: &Loans, tail_span: Span) {
        if self.mode != Mode::Check || dying.is_empty() {
            return;
        }
        let dying_set: HashSet<LocalId> = dying.iter().copied().collect();
        for id in tail {
            let loan = &self.loans[*id];
            let Root::Local(l) = loan.root.clone();
            if dying_set.contains(&l) && !loan.via_deref() {
                let lspan = loan.span;
                let msg = format!("{} does not live long enough: the borrow escapes the block that owns it", self.name(l));
                self.error_note(tail_span, msg, lspan, "borrowed here");
            }
        }
        // outer variables still used later must not hold loans on dying locals
        let outer: Vec<LocalId> = self.live_scope[..self.live_scope.len() - 1].iter().flatten().copied().collect();
        for h in outer {
            if dying_set.contains(&h) {
                continue;
            }
            let lu = self.last_use.get(&h).copied().unwrap_or(0);
            if lu <= self.pos {
                continue;
            }
            let Some(ls) = self.st.holders.get(&h).cloned() else { continue };
            for id in ls {
                let loan = &self.loans[id];
                let Root::Local(l) = loan.root.clone();
                if dying_set.contains(&l) && !loan.via_deref() {
                    let lspan = loan.span;
                    let msg = format!("{} does not live long enough: it is still borrowed by {} after its scope ends", self.name(l), self.name(h));
                    self.error_note(lspan, msg, self.f.locals[h as usize].span, "the borrowing variable is declared here and used later");
                }
            }
        }
    }

    fn check_escape(&mut self, loans: &Loans, span: Span, is_return: bool) {
        if self.mode != Mode::Check {
            return;
        }
        for id in loans {
            let loan = &self.loans[*id];
            if !loan.via_deref() {
                let Root::Local(l) = loan.root.clone();
                let lspan = loan.span;
                let what = if is_return { "returns a reference to" } else { "leaks a reference to" };
                let msg = format!("function {what} {}, which is destroyed when the function returns", self.name(l));
                self.error_note(span, msg, lspan, "borrowed here");
            }
        }
    }
}

fn place_has_deref(p: &Place) -> bool {
    match p {
        Place::Deref(_) => true,
        Place::Field(b, _) | Place::Index(b, _) => place_has_deref(b),
        _ => false,
    }
}
