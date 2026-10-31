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
            self.holders
                .entry(*k)
                .or_default()
                .extend(v.iter().copied());
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
    /// Locals whose destructor can read what they borrow (a thread handle
    /// joins its thread there): their loans last until they are destroyed.
    drop_readers: HashSet<LocalId>,
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
            drop_readers: f
                .locals
                .iter()
                .enumerate()
                .filter(|(_, l)| ck.drop_uses_borrows(&l.ty))
                .map(|(i, _)| i as LocalId)
                .collect(),
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
            self.errors
                .push(Diagnostic::new(span, msg).note(note_span, note));
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
            Place::Index(b, _) | Place::Slice(b, ..) => {
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
            Place::Slice(b, lo, hi) => {
                let l = self.place_exprs(b);
                self.in_flight.push(l.clone());
                for e in [lo, hi].into_iter().flatten() {
                    self.expr(e);
                }
                self.in_flight.pop();
                l
            }
        }
    }

    fn is_mutable(&self, p: &Place) -> Result<(), String> {
        match p {
            Place::Local(l) => {
                let loc = &self.f.locals[*l as usize];
                if loc.mutable || loc.synthetic {
                    Ok(())
                } else {
                    Err(format!(
                        "cannot mutate immutable variable `{}`; declare it with `let mut {}`",
                        loc.name, loc.name
                    ))
                }
            }
            Place::Static(name, m) => {
                if *m {
                    Ok(())
                } else {
                    Err(format!(
                        "cannot mutate immutable static `{}`",
                        name.rsplit('.').next().unwrap_or(name)
                    ))
                }
            }
            Place::Field(b, _) | Place::Index(b, _) | Place::Slice(b, ..) => self.is_mutable(b),
            Place::Deref(e) => match &e.ty {
                Ty::Ref(_, true) | Ty::Ptr(_) => Ok(()),
                Ty::Ref(_, false) => {
                    Err("cannot mutate through a `&` reference; use `&mut`".into())
                }
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
            let until_drop = self.drop_readers.contains(h) && !self.st.moved.contains_key(h);
            if lu >= self.pos || until_drop {
                out.extend(loans.iter().copied());
            }
        }
        for f in &self.in_flight {
            out.extend(f.iter().copied());
        }
        out.into_iter().collect()
    }

    fn overlaps(a: &[Proj], b: &[Proj]) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| x == y || matches!((x, y), (Proj::Index, Proj::Index)))
    }

    /// Check an access against live loans and the moved set.
    fn access(&mut self, p: &Place, kind: Access, span: Span) {
        if self.mode != Mode::Check {
            return;
        }
        let Some((root, path)) = Self::place_path(p) else {
            return;
        };
        let Root::Local(l) = root.clone();
        // use after move
        if kind != Access::Write || !path.is_empty() {
            if let Some(&mspan) = self.st.moved.get(&l) {
                let msg = if kind == Access::Write {
                    format!("cannot assign to a part of moved value {}", self.name(l))
                } else {
                    format!("use of moved value {}", self.name(l))
                };
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
            if matches!(kind, Access::Write | Access::Move)
                && loan.path.len() > path.len()
                && loan.path[path.len()..].contains(&Proj::Deref)
            {
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
                let how = if loan.mutable {
                    "mutably borrowed"
                } else {
                    "borrowed"
                };
                let msg = format!("cannot {what} {} because it is {how}", self.name(l));
                let note = if loan.mutable {
                    "mutable borrow created here (and used later)"
                } else {
                    "borrow created here (and used later)"
                };
                self.error_note(span, msg, lspan, note);
                return;
            }
        }
    }

    fn new_loan(&mut self, p: &Place, mutable: bool, span: Span) -> Option<usize> {
        let (root, path) = Self::place_path(p)?;
        self.loans.push(Loan {
            root,
            path,
            mutable,
            span,
        });
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
                Place::Index(..) | Place::Slice(..) => {
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
            Place::Field(b, i) => self
                .ck
                .fields_of(&self.place_ty(b)?)?
                .get(*i as usize)
                .cloned(),
            Place::Index(b, _) => match self.place_ty(b)? {
                Ty::Array(e, _) | Ty::Slice(e) => Some(*e),
                _ => None,
            },
            Place::Slice(b, ..) => match self.place_ty(b)? {
                Ty::Array(e, _) | Ty::Slice(e) => Some(Ty::Slice(e)),
                Ty::Str => Some(Ty::Str),
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
        self.exprs_in_flight_each(xs).0
    }

    /// Evaluate call arguments; returns all loans and each argument's loans.
    fn exprs_in_flight_each(&mut self, xs: &[Expr]) -> (Loans, Vec<Loans>) {
        let mut per: Vec<Loans> = Vec::new();
        let mut all = Loans::new();
        let base = self.in_flight.len();
        let mut reserved: Vec<(usize, Span, Place)> = Vec::new();
        for e in xs {
            // two-phase borrows: `&mut place` arguments are reserved until the call
            if let K::Borrow(p, true) = &e.kind {
                self.tick();
                let pl = self.place_exprs(p);
                self.access(p, Access::Shared, e.span);
                let mut mine = Loans::new();
                if let Some(id) = self.new_loan(p, true, e.span) {
                    reserved.push((id, e.span, p.clone()));
                    mine.insert(id);
                } else {
                    self.access(p, Access::Mutable, e.span);
                }
                mine.extend(pl.iter().copied().filter(|_| matches!(p, Place::Deref(_))));
                all.extend(mine.iter().copied());
                per.push(mine);
                continue;
            }
            let l = self.expr(e);
            all.extend(l.iter().copied());
            per.push(l.clone());
            self.in_flight.push(l);
        }
        // activate reserved mutable loans
        if self.mode == Mode::Check {
            let others: Vec<usize> = self.in_flight[base..].iter().flatten().copied().collect();
            for (id, span, place) in &reserved {
                let me = self.loans[*id].clone();
                let conflicting = others
                    .iter()
                    .copied()
                    .chain(reserved.iter().map(|r| r.0).filter(|r| r != id))
                    .find(|o| {
                        let ol = &self.loans[*o];
                        ol.root == me.root && Self::overlaps(&ol.path, &me.path)
                    });
                if let Some(o) = conflicting {
                    let Root::Local(l) = me.root.clone();
                    let ospan = self.loans[o].span;
                    let msg = format!(
                        "cannot mutably borrow {} because it is also borrowed by another argument",
                        self.name(l)
                    );
                    self.error_note(*span, msg, ospan, "other borrow here");
                } else {
                    // activation: the full `&mut` check against live loans and mutability
                    self.access(place, Access::Mutable, *span);
                }
            }
        }
        self.in_flight.truncate(base);
        (all, per)
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
                if self.ck.has_refs(&e.ty) {
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
                if self.ck.has_refs(&e.ty) {
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
            K::FnRef(_) => Loans::new(),
            K::CallPtr(f, args) => {
                let lf = self.expr(f);
                self.in_flight.push(lf);
                let l = self.exprs_in_flight(args);
                self.in_flight.pop();
                if self.ck.has_refs(&e.ty) {
                    l
                } else {
                    Loans::new()
                }
            }
            K::Call(name, args) => {
                let (_, per) = self.exprs_in_flight_each(args);
                let storing = self.ck.storing.contains(name);
                self.flow_into_mut_args(args, &per, e.span, storing);
                if !self.ck.has_refs(&e.ty) {
                    return Loans::new();
                }
                // the result keeps alive what it can point into (see `summaries`)
                let mut out = Loans::new();
                match self.ck.summaries.get(name) {
                    Some(sum) => {
                        for &(i, inner) in sum {
                            let Some(direct) = per.get(i) else { continue };
                            let deep = self.inner_loans(direct);
                            if !inner {
                                out.extend(direct.iter().copied());
                            }
                            out.extend(deep);
                        }
                    }
                    None => {
                        for direct in &per {
                            let deep = self.inner_loans(direct);
                            out.extend(direct.iter().copied());
                            out.extend(deep);
                        }
                    }
                }
                out
            }
            K::Intrinsic(_, args) => {
                let l = self.exprs_in_flight(args);
                if self.ck.has_refs(&e.ty) {
                    l
                } else {
                    Loans::new()
                }
            }
            K::Struct(xs) | K::Tuple(xs) | K::Array(xs) | K::Variant(_, xs) => {
                self.exprs_in_flight(xs)
            }
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
                        if place_has_deref(p) {
                            // memory behind a reference may outlive this function's locals
                            self.check_store_behind_ref(&lv, e.span);
                        } else if let Some(l) = Self::root_local(p) {
                            self.st.holders.entry(l).or_default().extend(lv);
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
                let mut out = State {
                    diverged: true,
                    ..Default::default()
                };
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
                let mut merged = State {
                    diverged: true,
                    ..Default::default()
                };
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
                    self.bind_pat(&arm.pat, Some(p.clone()), &scrut_loans, arm.span);
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

    /// Loans held by what `loans` point to, transitively (references stored
    /// inside borrowed values).
    fn inner_loans(&self, loans: &Loans) -> Loans {
        let mut out = Loans::new();
        let mut work: Vec<usize> = loans.iter().copied().collect();
        let mut seen: HashSet<LocalId> = HashSet::new();
        while let Some(id) = work.pop() {
            let Root::Local(root) = self.loans[id].root.clone();
            if !seen.insert(root) {
                continue;
            }
            if let Some(h) = self.st.holders.get(&root) {
                for &x in h {
                    if out.insert(x) {
                        work.push(x);
                    }
                }
            }
        }
        out
    }

    /// A call `f(&mut x, &y)` where `x` can hold references may store `&y`
    /// inside `x`: from then on `x` holds `y`'s loans. If the `&mut` points
    /// outside this function (a parameter), no local may be stored.
    /// `storing`: the callee writes through raw pointers, so it can also
    /// store into values reached by shared references (`ch.send(&x)`).
    fn flow_into_mut_args(&mut self, args: &[Expr], per: &[Loans], span: Span, storing: bool) {
        for (i, a) in args.iter().enumerate() {
            let Ty::Ref(inner, m) = &a.ty else {
                continue;
            };
            if !*m && !storing {
                continue;
            }
            if !self.ck.has_refs(inner) {
                continue;
            }
            let mut others: Loans = per
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .flat_map(|(_, l)| l.iter().copied())
                .collect();
            let deep = self.inner_loans(&others);
            others.extend(deep);
            if others.is_empty() {
                continue;
            }
            let targets: Vec<usize> = per[i]
                .iter()
                .copied()
                .filter(|id| self.loans[*id].mutable || storing)
                .collect();
            let mut outside = targets.is_empty();
            for t in targets {
                let loan = self.loans[t].clone();
                if loan.via_deref() {
                    outside = true;
                    continue;
                }
                let Root::Local(root) = loan.root.clone();
                let add: Vec<usize> = others
                    .iter()
                    .copied()
                    .filter(|o| self.loans[*o].root != loan.root)
                    .collect();
                self.st.holders.entry(root).or_default().extend(add);
            }
            if outside {
                self.check_store_behind_ref(&others, span);
            }
        }
    }

    /// Storing references to locals into memory reached through a reference.
    fn check_store_behind_ref(&mut self, loans: &Loans, span: Span) {
        if self.mode != Mode::Check {
            return;
        }
        for id in loans {
            let loan = self.loans[*id].clone();
            if !loan.via_deref() {
                let Root::Local(l) = loan.root.clone();
                let msg = format!("{} does not live long enough: a reference to it would be stored in memory that outlives this function", self.name(l));
                self.error_note(span, msg, loan.span, "borrowed here");
            }
        }
    }

    fn pat_moves(&self, p: &Pat) -> bool {
        match p {
            Pat::Bind(id, BindMode::Value) => !self.ck.is_copy(&self.f.locals[*id as usize].ty),
            Pat::Tuple(ps) | Pat::Variant(_, ps) | Pat::Or(ps) => {
                ps.iter().any(|p| self.pat_moves(p))
            }
            _ => false,
        }
    }

    /// Declare pattern bindings; by-reference bindings borrow the scrutinee.
    /// `scrut` tracks the sub-place being matched (field projections and
    /// dereferences included) so by-reference bindings borrow exactly it.
    fn bind_pat(&mut self, p: &Pat, scrut: Option<Place>, scrut_loans: &Loans, span: Span) {
        match p {
            Pat::Bind(id, mode) => {
                self.declare(*id);
                let mut held = Loans::new();
                if let (BindMode::Ref(m), Some(sp)) = (mode, &scrut) {
                    self.access(sp, if *m { Access::Mutable } else { Access::Shared }, span);
                    if let Some(l) = self.new_loan(sp, *m, span) {
                        held.insert(l);
                    }
                    if place_has_deref(sp) {
                        held.extend(scrut_loans.iter().copied());
                    }
                } else if self.ck.has_refs(&self.f.locals[*id as usize].ty) {
                    held.extend(scrut_loans.iter().copied());
                }
                self.st.holders.insert(*id, held);
                if let Some(s) = self.live_scope.last_mut() {
                    s.push(*id);
                }
            }
            Pat::Tuple(ps) | Pat::Variant(_, ps) => {
                for (i, q) in ps.iter().enumerate() {
                    let sub = scrut.clone().map(|s| Place::Field(Box::new(s), i as u32));
                    self.bind_pat(q, sub, scrut_loans, span);
                }
            }
            Pat::Or(ps) => {
                for q in ps {
                    self.bind_pat(q, scrut.clone(), scrut_loans, span);
                }
            }
            Pat::Deref(inner) => {
                let sub = scrut.map(|s| {
                    let ty = self.place_ty(&s).unwrap_or(Ty::unit());
                    Place::Deref(Box::new(Expr {
                        kind: K::Use(s, UseKind::Copy),
                        ty,
                        span,
                    }))
                });
                self.bind_pat(inner, sub, scrut_loans, span)
            }
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
            self.check_dying(
                &dying,
                &tail,
                b.tail.as_ref().map(|t| t.span).unwrap_or(span),
            );
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
                let msg = format!(
                    "{} does not live long enough: the borrow escapes the block that owns it",
                    self.name(l)
                );
                self.error_note(tail_span, msg, lspan, "borrowed here");
            }
        }
        // locals are destroyed in reverse order: one declared earlier is destroyed
        // later, so its destructor must not read a borrow of one declared after it
        for (i, &h) in dying.iter().enumerate() {
            if !self.ck.drop_uses_borrows(&self.f.locals[h as usize].ty)
                || self.st.moved.contains_key(&h)
            {
                continue;
            }
            let later: HashSet<LocalId> = dying[i + 1..].iter().copied().collect();
            let Some(ls) = self.st.holders.get(&h).cloned() else {
                continue;
            };
            for id in ls {
                let loan = self.loans[id].clone();
                let Root::Local(l) = loan.root.clone();
                if later.contains(&l) && !loan.via_deref() {
                    let msg = format!(
                        "{} does not live long enough: it is destroyed while {} still borrows it, and {}'s destructor runs afterwards",
                        self.name(l),
                        self.name(h),
                        self.name(h)
                    );
                    self.error_note(
                        loan.span,
                        msg,
                        self.f.locals[h as usize].span,
                        "declare this after the value it borrows, or drop it first",
                    );
                }
            }
        }
        // outer variables still used later must not hold loans on dying locals
        let outer: Vec<LocalId> = self.live_scope[..self.live_scope.len() - 1]
            .iter()
            .flatten()
            .copied()
            .collect();
        for h in outer {
            if dying_set.contains(&h) {
                continue;
            }
            let lu = self.last_use.get(&h).copied().unwrap_or(0);
            // a destructor that can read borrowed data is a use at scope end
            let drop_reads = self.ck.drop_uses_borrows(&self.f.locals[h as usize].ty)
                && !self.st.moved.contains_key(&h);
            if lu <= self.pos && !drop_reads {
                continue;
            }
            let Some(ls) = self.st.holders.get(&h).cloned() else {
                continue;
            };
            for id in ls {
                let loan = &self.loans[id];
                let Root::Local(l) = loan.root.clone();
                if dying_set.contains(&l) && !loan.via_deref() {
                    let lspan = loan.span;
                    let msg = format!("{} does not live long enough: it is still borrowed by {} after its scope ends", self.name(l), self.name(h));
                    self.error_note(
                        lspan,
                        msg,
                        self.f.locals[h as usize].span,
                        "the borrowing variable is declared here and used later",
                    );
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
                let what = if is_return {
                    "returns a reference to"
                } else {
                    "leaks a reference to"
                };
                let msg = format!(
                    "function {what} {}, which is destroyed when the function returns",
                    self.name(l)
                );
                self.error_note(span, msg, lspan, "borrowed here");
            }
        }
    }
}

fn place_has_deref(p: &Place) -> bool {
    match p {
        Place::Deref(_) => true,
        Place::Field(b, _) | Place::Index(b, _) | Place::Slice(b, ..) => place_has_deref(b),
        _ => false,
    }
}

// ------------------------------------------------------------------------
// Result summaries: what a function's returned references point into.
//
// A location is described relative to a parameter's value: level 1 is the
// memory the argument value refers to directly (`&self.field`), level 2+
// is memory reached through references stored inside it (`&self.input[..]`
// where `input: &str`). At a call site, a level-1 result keeps the
// argument's own loans alive; a level-2+ result only the loans the
// argument's referent holds, so `p.word()` borrows the text, not `p`.

/// (parameter index, `true` for level 2+ only).
pub type Summary = BTreeSet<(usize, bool)>;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Mem {
    /// Memory at `level` relative to parameter `i` (0 = the parameter variable).
    Param(usize, u32),
    /// A local of the function being summarized.
    Frame(LocalId),
}

type Origins = BTreeSet<(usize, u32)>;

struct OriginCx<'a> {
    ck: &'a Checker,
    f: &'a Func,
    sums: &'a HashMap<String, Summary>,
    locals: HashMap<LocalId, Origins>,
    result: Origins,
    breaks: Vec<Origins>,
    /// What values written through raw pointers point to (they may be
    /// reachable from the result, e.g. `Box.new(value)`).
    stored: Origins,
}

pub fn summaries(ck: &Checker, funcs: &[Func]) -> HashMap<String, Summary> {
    let mut sums: HashMap<String, Summary> = funcs
        .iter()
        .map(|f| (f.name.clone(), Summary::new()))
        .collect();
    for _ in 0..16 {
        let mut changed = false;
        for f in funcs {
            if !ck.has_refs(&f.ret) && !contains_ptr(&f.ret) {
                continue;
            }
            let mut cx = OriginCx {
                ck,
                f,
                sums: &sums,
                locals: HashMap::new(),
                result: Origins::new(),
                breaks: Vec::new(),
                stored: Origins::new(),
            };
            // locals are flow-insensitive: iterate until their origins settle
            for _ in 0..4 {
                let tail = cx.expr(&f.body);
                cx.result.extend(tail);
            }
            let mut new: Summary = cx
                .result
                .iter()
                .chain(cx.stored.iter())
                .map(|&(i, lvl)| (i, lvl >= 2))
                .collect();
            // a by-value argument holding borrows may end up inside the result
            // (stored through a raw pointer, as `Box.new` does)
            for (i, &p) in f.params.iter().enumerate() {
                let t = &f.locals[p as usize].ty;
                if !matches!(t, Ty::Ref(..)) && ck.has_refs(t) {
                    new.insert((i, false));
                }
            }
            if sums.get(&f.name) != Some(&new) {
                let mut merged = sums.get(&f.name).cloned().unwrap_or_default();
                merged.extend(new);
                sums.insert(f.name.clone(), merged);
                changed = true;
            }
        }
        if !changed {
            return sums;
        }
    }
    // did not settle (deep recursion): assume results point into every argument
    for f in funcs {
        if ck.has_refs(&f.ret) {
            sums.insert(
                f.name.clone(),
                (0..f.params.len()).map(|i| (i, false)).collect(),
            );
        }
    }
    sums
}

impl OriginCx<'_> {
    fn param_index(&self, l: LocalId) -> Option<usize> {
        self.f.params.iter().position(|p| *p == l)
    }

    /// References stored in `mem` point to these locations.
    fn targets(&self, mem: &[Mem]) -> Origins {
        let mut out = Origins::new();
        for m in mem {
            match *m {
                Mem::Param(i, k) => {
                    out.insert((i, k + 1));
                }
                Mem::Frame(l) => out.extend(self.locals.get(&l).cloned().unwrap_or_default()),
            }
        }
        out
    }

    /// Raw pointers to owned memory (a `Vec`'s buffer, a `Box`'s contents)
    /// point into memory that belongs to their owner: same level.
    fn same_level(&self, mem: &[Mem]) -> Origins {
        let mut out = Origins::new();
        for m in mem {
            match *m {
                Mem::Param(i, k) => {
                    out.insert((i, k.max(1)));
                }
                Mem::Frame(l) => out.extend(self.locals.get(&l).cloned().unwrap_or_default()),
            }
        }
        out
    }

    /// Where a value of type `ty` read from `mem` can point.
    fn value_origins(&self, mem: &[Mem], ty: &Ty) -> Origins {
        let mut o = Origins::new();
        if self.ck.has_refs(ty) {
            o.extend(self.targets(mem));
        }
        if contains_ptr(ty) {
            o.extend(self.same_level(mem));
        }
        o
    }

    fn place_mem(&mut self, p: &Place) -> Vec<Mem> {
        match p {
            Place::Local(l) => match self.param_index(*l) {
                Some(i) => vec![Mem::Param(i, 0)],
                None => vec![Mem::Frame(*l)],
            },
            Place::Static(..) => Vec::new(),
            Place::Field(b, _) => self.place_mem(b),
            Place::Index(b, i) => {
                self.expr(i);
                self.place_mem(b)
            }
            Place::Slice(b, lo, hi) => {
                for e in [lo, hi].into_iter().flatten() {
                    self.expr(e);
                }
                self.place_mem(b)
            }
            Place::Deref(e) => match &e.kind {
                K::Use(inner, _) => {
                    let m = self.place_mem(inner);
                    let o = if matches!(e.ty, Ty::Ptr(_)) {
                        self.same_level(&m)
                    } else {
                        self.targets(&m)
                    };
                    o.into_iter().map(|(i, k)| Mem::Param(i, k)).collect()
                }
                _ => self
                    .expr(e)
                    .into_iter()
                    .map(|(i, k)| Mem::Param(i, k))
                    .collect(),
            },
        }
    }

    fn bind_pat(&mut self, pat: &Pat, mem: &[Mem]) {
        match pat {
            Pat::Bind(l, BindMode::Value) => {
                let t = self.targets(mem);
                self.locals.entry(*l).or_default().extend(t);
            }
            Pat::Bind(l, BindMode::Ref(_)) => {
                let v: Origins = mem
                    .iter()
                    .filter_map(|m| match *m {
                        Mem::Param(i, k) if k >= 1 => Some((i, k)),
                        _ => None,
                    })
                    .collect();
                self.locals.entry(*l).or_default().extend(v);
            }
            Pat::Tuple(ps) | Pat::Variant(_, ps) | Pat::Or(ps) => {
                for q in ps {
                    self.bind_pat(q, mem);
                }
            }
            Pat::Deref(inner) => {
                let deeper: Vec<Mem> = self
                    .targets(mem)
                    .into_iter()
                    .map(|(i, k)| Mem::Param(i, k))
                    .collect();
                self.bind_pat(inner, &deeper);
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr) -> Origins {
        // raw pointers carry where they point too, so `&*(self.ptr + i)` is traced
        let refs = self.ck.has_refs(&e.ty) || contains_ptr(&e.ty);
        match &e.kind {
            K::Use(p, _) => {
                let m = self.place_mem(p);
                self.value_origins(&m, &e.ty)
            }
            K::Borrow(p, _) => self
                .place_mem(p)
                .into_iter()
                .filter_map(|m| match m {
                    Mem::Param(i, k) if k >= 1 => Some((i, k)),
                    _ => None,
                })
                .collect(),
            K::Call(name, args) => {
                let os: Vec<Origins> = args.iter().map(|a| self.expr(a)).collect();
                if !refs {
                    return Origins::new();
                }
                match self.sums.get(name) {
                    Some(sum) => {
                        let mut out = Origins::new();
                        for &(i, inner) in sum {
                            if let Some(o) = os.get(i) {
                                // level-2+ results point one level deeper than the argument
                                out.extend(
                                    o.iter()
                                        .map(|&(p, k)| if inner { (p, k + 1) } else { (p, k) }),
                                );
                            }
                        }
                        out
                    }
                    None => os.into_iter().flatten().collect(),
                }
            }
            K::CallPtr(f, args) => {
                self.expr(f);
                let os: Origins = args.iter().flat_map(|a| self.expr(a)).collect();
                if refs {
                    os
                } else {
                    Origins::new()
                }
            }
            K::Intrinsic(i, args) => {
                let os: Vec<Origins> = args.iter().map(|a| self.expr(a)).collect();
                match i {
                    // a slice/str built from a pointer points where the pointer does
                    xir::Intrinsic::MakeFat | xir::Intrinsic::DataPtr => {
                        os.into_iter().next().unwrap_or_default()
                    }
                    // a value read through a pointer: what that memory holds
                    xir::Intrinsic::PtrWrite | xir::Intrinsic::VolatileStore => {
                        if let Some(v) = os.get(1) {
                            self.stored.extend(v.iter().copied());
                        }
                        Origins::new()
                    }
                    xir::Intrinsic::PtrRead | xir::Intrinsic::VolatileLoad => {
                        let mem: Vec<Mem> = os
                            .into_iter()
                            .next()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(i, k)| Mem::Param(i, k))
                            .collect();
                        self.value_origins(&mem, &e.ty)
                    }
                    _ => Origins::new(),
                }
            }
            K::ArrayToSlice(a) | K::ArrayRepeat(a, _) => self.expr(a),
            K::Cast(a, _) => {
                let o = self.expr(a);
                if refs {
                    o
                } else {
                    Origins::new()
                }
            }
            K::Struct(xs) | K::Tuple(xs) | K::Array(xs) | K::Variant(_, xs) => {
                xs.iter().flat_map(|x| self.expr(x)).collect()
            }
            K::Unary(_, a) => {
                self.expr(a);
                Origins::new()
            }
            K::Binary(_, a, b) => {
                let o = self.expr(a);
                self.expr(b);
                // pointer arithmetic keeps the pointer's origin
                if matches!(e.ty, Ty::Ptr(_)) {
                    o
                } else {
                    Origins::new()
                }
            }
            K::Assign(p, v) => {
                let o = self.expr(v);
                if place_through_raw(p) {
                    self.stored.extend(o.iter().copied());
                }
                let mut cur = p;
                loop {
                    match cur {
                        Place::Local(l) => {
                            self.locals.entry(*l).or_default().extend(o);
                            break;
                        }
                        Place::Field(b, _) => cur = b,
                        _ => {
                            self.place_mem(cur);
                            break;
                        }
                    }
                }
                Origins::new()
            }
            K::CompoundAssign(_, p, v) => {
                self.expr(v);
                self.place_mem(p);
                Origins::new()
            }
            K::Block(b) => {
                for s in &b.stmts {
                    match s {
                        Stmt::Let(l, v) => {
                            let o = self.expr(v);
                            self.locals.entry(*l).or_default().extend(o);
                        }
                        Stmt::LetPat(pat, v) => {
                            let o = self.expr(v);
                            let mem: Vec<Mem> = o
                                .iter()
                                .map(|&(i, k)| Mem::Param(i, k.saturating_sub(1)))
                                .collect();
                            self.bind_pat(pat, &mem);
                        }
                        Stmt::Expr(x) => {
                            self.expr(x);
                        }
                    }
                }
                match &b.tail {
                    Some(t) => self.expr(t),
                    None => Origins::new(),
                }
            }
            K::If(c, t, els) => {
                self.expr(c);
                let mut o = self.expr(t);
                if let Some(x) = els {
                    o.extend(self.expr(x));
                }
                o
            }
            K::Loop(b, _) => {
                self.breaks.push(Origins::new());
                self.expr(b);
                self.breaks.pop().unwrap_or_default()
            }
            K::Break(_, v) => {
                if let Some(v) = v {
                    let o = self.expr(v);
                    if let Some(top) = self.breaks.last_mut() {
                        top.extend(o);
                    }
                }
                Origins::new()
            }
            K::Return(v) => {
                if let Some(v) = v {
                    let o = self.expr(v);
                    self.result.extend(o);
                }
                Origins::new()
            }
            K::Match(p, arms) => {
                let mem = self.place_mem(p);
                let mut out = Origins::new();
                for a in arms {
                    self.bind_pat(&a.pat, &mem);
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    out.extend(self.expr(&a.body));
                }
                out
            }
            K::Is(p, _) | K::Drop(p) => {
                self.place_mem(p);
                Origins::new()
            }
            K::Int(_)
            | K::Float(_)
            | K::Bool(_)
            | K::Str(_)
            | K::Unit
            | K::Continue(_)
            | K::FnRef(_) => Origins::new(),
        }
    }
}

fn contains_ptr(t: &Ty) -> bool {
    match t {
        Ty::Ptr(_) => true,
        Ty::Array(e, _) | Ty::Slice(e) | Ty::Ref(e, _) => contains_ptr(e),
        Ty::Tuple(ts) => ts.iter().any(contains_ptr),
        _ => false,
    }
}

/// Does the place go through a raw pointer dereference?
fn place_through_raw(p: &Place) -> bool {
    match p {
        Place::Deref(e) => {
            matches!(e.ty, Ty::Ptr(_))
                || matches!(&e.kind, K::Use(inner, _) if place_through_raw(inner))
        }
        Place::Field(b, _) | Place::Index(b, _) | Place::Slice(b, ..) => place_through_raw(b),
        _ => false,
    }
}

/// Functions that write through raw pointers, directly or through a call:
/// they can store an argument somewhere reachable from another argument
/// even through a shared reference (interior mutability: `Channel.send`).
pub fn storing_fns(funcs: &[Func]) -> HashSet<String> {
    fn scan(e: &Expr, direct: &mut bool, calls: &mut Vec<String>) {
        match &e.kind {
            K::Intrinsic(
                xir::Intrinsic::PtrWrite
                | xir::Intrinsic::VolatileStore
                | xir::Intrinsic::AtomicStore,
                _,
            ) => *direct = true,
            K::Assign(p, _) if place_through_raw(p) => *direct = true,
            K::Call(name, _) => calls.push(name.clone()),
            _ => {}
        }
        crate::check::xir_children(e, &mut |c| scan(c, direct, calls));
    }
    let info: Vec<(String, bool, Vec<String>)> = funcs
        .iter()
        .map(|f| {
            let (mut direct, mut calls) = (false, Vec::new());
            scan(&f.body, &mut direct, &mut calls);
            (f.name.clone(), direct, calls)
        })
        .collect();
    let mut storing: HashSet<String> = info.iter().filter(|i| i.1).map(|i| i.0.clone()).collect();
    loop {
        let before = storing.len();
        for (name, _, calls) in &info {
            if !storing.contains(name) && calls.iter().any(|c| storing.contains(c)) {
                storing.insert(name.clone());
            }
        }
        if storing.len() == before {
            return storing;
        }
    }
}
