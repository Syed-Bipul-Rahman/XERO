//! Semantic analysis: name resolution, type checking, monomorphization and
//! lowering of the AST to XIR.
//!
//! Generic functions are checked per instantiation (template style): every
//! instance is type-checked with concrete types, so the produced XIR is fully
//! monomorphic and all trait method calls are statically dispatched.

mod closure;
mod consteval;
mod exhaust;
mod expr;
mod pat;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::ast::{self, FnDecl, GenericParam, Ident, ItemKind, SelfKind, TypeExpr, TypeKind, Vis};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{match_template, AdtId, FloatTy, IntTy, Ty, TyFmt};
use crate::xir;

pub use consteval::ConstValue;

pub type ModId = u32;
pub type FnId = u32;
pub type TraitId = u32;
pub type ImplId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntrinsicFn {
    SizeOf,
    AlignOf,
    Trap,
    Asm,
    SliceFromRaw,
    SliceFromRawMut,
    StrFromRaw,
    PtrRead,
    PtrWrite,
    DropInPlace,
    Forget,
    VolatileLoad,
    VolatileStore,
    AtomicLoad,
    AtomicStore,
    AtomicSwap,
    AtomicAdd,
    AtomicSub,
    AtomicCas,
    Fence,
}

const INTRINSICS: &[(&str, IntrinsicFn)] = &[
    ("size_of", IntrinsicFn::SizeOf),
    ("align_of", IntrinsicFn::AlignOf),
    ("trap", IntrinsicFn::Trap),
    ("asm", IntrinsicFn::Asm),
    ("slice_from_raw", IntrinsicFn::SliceFromRaw),
    ("slice_from_raw_mut", IntrinsicFn::SliceFromRawMut),
    ("str_from_raw", IntrinsicFn::StrFromRaw),
    ("ptr_read", IntrinsicFn::PtrRead),
    ("ptr_write", IntrinsicFn::PtrWrite),
    ("drop_in_place", IntrinsicFn::DropInPlace),
    ("forget", IntrinsicFn::Forget),
    ("volatile_load", IntrinsicFn::VolatileLoad),
    ("volatile_store", IntrinsicFn::VolatileStore),
    ("atomic_load", IntrinsicFn::AtomicLoad),
    ("atomic_store", IntrinsicFn::AtomicStore),
    ("atomic_swap", IntrinsicFn::AtomicSwap),
    ("atomic_add", IntrinsicFn::AtomicAdd),
    ("atomic_sub", IntrinsicFn::AtomicSub),
    ("atomic_cas", IntrinsicFn::AtomicCas),
    ("fence", IntrinsicFn::Fence),
];

/// Types from other std modules that are visible everywhere, like the prelude.
pub const PRELUDE_TYPES: &[(&str, &str)] = &[
    ("std.vec", "Vec"),
    ("std.string", "String"),
    ("std.mem", "Box"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Def {
    Fn(FnId),
    Adt(AdtId),
    Trait(TraitId),
    Alias(u32),
    Const(u32),
    Static(u32),
    Module(ModId),
    Variant(AdtId, u32),
    Intrinsic(IntrinsicFn),
}

#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub def: Def,
    pub vis: Vis,
    pub span: Span,
    /// Imported names are not re-exported.
    pub imported: bool,
}

pub struct ModuleDef {
    pub path: String,
    pub file: u32,
    pub scope: HashMap<String, Binding>,
    pub is_std: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnOwner {
    Free,
    Impl(ImplId),
    Trait(TraitId),
}

pub struct FnDef {
    pub decl: FnDecl,
    pub module: ModId,
    pub owner: FnOwner,
    pub vis: Vis,
}

#[derive(Clone)]
pub enum AdtKind {
    Struct(Vec<ast::FieldDecl>),
    Enum(Vec<ast::VariantDecl>),
}

pub struct AdtDef {
    pub name: String,
    pub module: ModId,
    pub generics: Vec<GenericParam>,
    pub is_copy: bool,
    pub kind: AdtKind,
    pub span: Span,
    pub vis: Vis,
    /// Field types (struct) or per-variant payload types (enum), as templates
    /// over the ADT's generic parameters.
    pub fields: Vec<Ty>,
    pub variants: Vec<Vec<Ty>>,
    pub discriminants: Vec<i128>,
    pub drop_impl: Option<ImplId>,
}

impl AdtDef {
    pub fn is_enum(&self) -> bool {
        matches!(self.kind, AdtKind::Enum(_))
    }

    pub fn field_index(&self, name: &str) -> Option<usize> {
        match &self.kind {
            AdtKind::Struct(fs) => fs.iter().position(|f| f.name.name == name),
            AdtKind::Enum(_) => None,
        }
    }

    pub fn variant_index(&self, name: &str) -> Option<u32> {
        match &self.kind {
            AdtKind::Enum(vs) => vs
                .iter()
                .position(|v| v.name.name == name)
                .map(|i| i as u32),
            AdtKind::Struct(_) => None,
        }
    }

    pub fn variant_name(&self, idx: u32) -> &str {
        match &self.kind {
            AdtKind::Enum(vs) => &vs[idx as usize].name.name,
            AdtKind::Struct(_) => "",
        }
    }
}

pub struct TraitDef {
    pub name: String,
    pub module: ModId,
    pub decl: ast::TraitDecl,
    pub supertraits: Vec<TraitId>,
    /// Default method bodies, by name.
    pub defaults: HashMap<String, FnId>,
}

pub struct ImplDef {
    pub module: ModId,
    pub generics: Vec<GenericParam>,
    pub self_ty_ast: TypeExpr,
    pub trait_ast: Option<TypeExpr>,
    /// Template over the impl's generic params.
    pub self_ty: Ty,
    pub trait_id: Option<TraitId>,
    pub methods: HashMap<String, FnId>,
    pub drop_fn: Option<FnId>,
    pub span: Span,
}

pub struct AliasDef {
    pub module: ModId,
    pub generics: Vec<GenericParam>,
    pub ty: TypeExpr,
}

pub struct ConstDef {
    pub name: String,
    pub module: ModId,
    pub ty: Option<TypeExpr>,
    pub value: ast::Expr,
    pub cache: Option<(Ty, ConstValue)>,
    pub in_progress: bool,
}

pub struct StaticDef {
    pub name: String,
    pub module: ModId,
    pub mutable: bool,
    pub ty: TypeExpr,
    pub value: ast::Expr,
    pub symbol: String,
    pub resolved: Option<Ty>,
}

/// A function instance to generate: definition + concrete generic arguments.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FnKey {
    pub fid: FnId,
    /// Impl generic args followed by the function's own generic args.
    pub targs: Vec<Ty>,
    /// Concrete `Self` for trait default methods.
    pub self_ty: Option<Ty>,
}

/// Generic environment while resolving types.
#[derive(Clone, Default)]
pub struct Env {
    pub names: HashMap<String, Ty>,
    pub self_ty: Option<Ty>,
}

pub struct Target {
    pub ptr_bits: u32,
}

pub struct Checker {
    pub modules: Vec<ModuleDef>,
    pub fns: Vec<FnDef>,
    pub adts: Vec<AdtDef>,
    pub traits: Vec<TraitDef>,
    pub impls: Vec<ImplDef>,
    pub aliases: Vec<AliasDef>,
    pub consts: Vec<ConstDef>,
    pub statics: Vec<StaticDef>,
    pub prelude: Option<ModId>,
    pub target: Target,
    pub diags: Vec<Diagnostic>,

    instances: HashMap<FnKey, String>,
    queue: VecDeque<(FnKey, String)>,
    pub out_funcs: Vec<xir::Func>,
    externs: HashMap<String, xir::Extern>,
    used_statics: HashSet<u32>,
    pub drop_fns: HashMap<Ty, String>,
    alias_depth: u32,
    /// Closure types, indexed by `Ty::Closure(id)`.
    pub closures: Vec<ClosureDef>,
    /// Stable ids: (enclosing function symbol, closure span) -> closure id,
    /// so re-checking a function reuses its closures' ids.
    pub closure_ids: HashMap<(String, Span), u32>,
    /// What each function's result borrows from: (parameter index,
    /// `true` if only data the parameter refers to, not the parameter's own memory).
    pub summaries: HashMap<String, BTreeSet<(usize, bool)>>,
}

/// One closure expression, after checking.
#[derive(Clone, Debug)]
pub struct ClosureDef {
    /// Types of the captured values (the closure's environment struct).
    pub fields: Vec<Ty>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Symbol of the generated function `(env, params...) -> ret`.
    pub fn_name: String,
    /// How calls reach the environment: `Fn` (0, `&env`), `FnMut` (1, `&mut env`:
    /// it mutates its captures) or `FnOnce` (2, by value: it moves a capture
    /// out, so it can be called only once).
    pub kind: u8,
}

/// Input to the checker: parsed modules, by module path.
pub struct ParsedModule {
    pub path: String,
    pub ast: ast::Module,
    pub is_std: bool,
}

impl Checker {
    pub fn new(target: Target) -> Checker {
        Checker {
            modules: Vec::new(),
            fns: Vec::new(),
            adts: Vec::new(),
            traits: Vec::new(),
            impls: Vec::new(),
            aliases: Vec::new(),
            consts: Vec::new(),
            statics: Vec::new(),
            prelude: None,
            target,
            diags: Vec::new(),
            instances: HashMap::new(),
            queue: VecDeque::new(),
            out_funcs: Vec::new(),
            externs: HashMap::new(),
            used_statics: HashSet::new(),
            drop_fns: HashMap::new(),
            alias_depth: 0,
            closures: Vec::new(),
            closure_ids: HashMap::new(),
            summaries: HashMap::new(),
        }
    }

    fn error(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    // ------------------------------------------------------------ collection

    /// Run the whole front end. `root` indexes the module containing `main`.
    pub fn run(
        mut self,
        parsed: Vec<ParsedModule>,
        root: usize,
        is_lib: bool,
    ) -> std::result::Result<(xir::Program, Checker), Vec<Diagnostic>> {
        let mut impl_decls = Vec::new();
        let mut imports = Vec::new();
        for (mi, pm) in parsed.into_iter().enumerate() {
            let mid = mi as ModId;
            if pm.path == "std.prelude" {
                self.prelude = Some(mid);
            }
            self.modules.push(ModuleDef {
                path: pm.path.clone(),
                file: pm.ast.file,
                scope: HashMap::new(),
                is_std: pm.is_std,
            });
            for item in pm.ast.items {
                self.collect_item(mid, item, &mut impl_decls, &mut imports);
            }
        }
        for (mid, vis, imp, span) in imports {
            self.resolve_import(mid, vis, &imp, span);
        }
        self.resolve_adts();
        self.resolve_traits();
        for (mid, decl) in impl_decls {
            self.collect_impl(mid, decl);
        }
        self.validate_impls();
        if !self.diags.is_empty() {
            return Err(self.diags);
        }

        // Roots: every non-generic function of user modules, plus `main`.
        let mut entry = None;
        for fid in 0..self.fns.len() {
            let f = &self.fns[fid];
            let m = &self.modules[f.module as usize];
            let generic = !f.decl.generics.params.is_empty();
            let in_generic_impl = match f.owner {
                FnOwner::Impl(iid) => !self.impls[iid as usize].generics.is_empty(),
                FnOwner::Trait(_) => true,
                FnOwner::Free => false,
            };
            if generic || in_generic_impl || f.decl.body.is_none() {
                continue;
            }
            if m.is_std && !f.decl.is_export {
                continue;
            }
            let key = FnKey {
                fid: fid as FnId,
                targs: Vec::new(),
                self_ty: None,
            };
            let name = self.instance(key);
            if fid_is_main(&self.fns[fid]) && self.fns[fid].module == root as ModId {
                let ret_ast = self.fns[fid].decl.ret.clone();
                let fmod = self.fns[fid].module;
                let ret = match &ret_ast {
                    Some(t) => self
                        .resolve_type(t, fmod, &Env::default())
                        .unwrap_or(Ty::unit()),
                    None => Ty::unit(),
                };
                if !(ret.is_unit() || ret == Ty::Int(IntTy::I32) || ret.is_never()) {
                    let sp = self.fns[fid].decl.name.span;
                    self.error(Diagnostic::new(sp, "`main` must return `()` or `i32`"));
                }
                if !self.fns[fid].decl.params.is_empty() {
                    let sp = self.fns[fid].decl.name.span;
                    self.error(Diagnostic::new(sp, "`main` takes no parameters"));
                }
                entry = Some((name, ret));
            }
        }
        if entry.is_none() && !is_lib {
            let file = self.modules[root].file;
            self.error(Diagnostic::new(
                Span::new(file, 0, 0),
                "no `main` function found (use `--lib` to build a library object)",
            ));
        }
        self.drain_queue();

        if !self.diags.is_empty() {
            return Err(self.diags);
        }
        let mut statics = Vec::new();
        let mut used: Vec<u32> = self.used_statics.iter().copied().collect();
        used.sort();
        for sid in used {
            match self.static_init(sid) {
                Ok((ty, init)) => {
                    let s = &self.statics[sid as usize];
                    statics.push(xir::Static {
                        name: s.symbol.clone(),
                        ty,
                        init,
                        mutable: s.mutable,
                    });
                }
                Err(d) => self.error(d),
            }
        }
        if !self.diags.is_empty() {
            return Err(self.diags);
        }
        let mut externs: Vec<xir::Extern> =
            std::mem::take(&mut self.externs).into_values().collect();
        externs.sort_by(|a, b| a.name.cmp(&b.name));
        let funcs = std::mem::take(&mut self.out_funcs);
        Ok((
            xir::Program {
                funcs,
                externs,
                statics,
                entry,
            },
            self,
        ))
    }

    fn define(&mut self, mid: ModId, name: &Ident, def: Def, vis: Vis) {
        let scope = &mut self.modules[mid as usize].scope;
        if let Some(prev) = scope.get(&name.name) {
            let d = Diagnostic::new(
                name.span,
                format!("`{}` is defined more than once in this module", name.name),
            )
            .note(prev.span, "previous definition here");
            self.diags.push(d);
            return;
        }
        scope.insert(
            name.name.clone(),
            Binding {
                def,
                vis,
                span: name.span,
                imported: false,
            },
        );
    }

    fn collect_item(
        &mut self,
        mid: ModId,
        item: ast::Item,
        impls: &mut Vec<(ModId, ast::ImplDecl)>,
        imports: &mut Vec<(ModId, Vis, ast::Import, Span)>,
    ) {
        let vis = item.vis;
        match item.kind {
            ItemKind::Import(imp) => imports.push((mid, vis, imp, item.span)),
            ItemKind::Fn(decl) => {
                if decl.body.is_none() && !decl.is_unsafe {
                    self.error(Diagnostic::new(
                        decl.name.span,
                        "a function without a body declares a foreign (C) symbol and must be marked `unsafe fn`",
                    ));
                }
                if decl.body.is_none() && !decl.generics.params.is_empty() {
                    self.error(Diagnostic::new(
                        decl.name.span,
                        "foreign functions cannot be generic",
                    ));
                }
                if decl.self_param.is_some() {
                    self.error(Diagnostic::new(
                        decl.name.span,
                        "`self` parameter is only allowed in `impl` and `trait` methods",
                    ));
                }
                let name = decl.name.clone();
                let fid = self.fns.len() as FnId;
                self.fns.push(FnDef {
                    decl,
                    module: mid,
                    owner: FnOwner::Free,
                    vis,
                });
                self.define(mid, &name, Def::Fn(fid), vis);
            }
            ItemKind::Struct(s) => {
                let id = self.adts.len() as AdtId;
                let name = s.name.clone();
                self.adts.push(AdtDef {
                    name: s.name.name.clone(),
                    module: mid,
                    generics: s.generics.params.clone(),
                    is_copy: s.is_copy,
                    kind: AdtKind::Struct(s.fields),
                    span: s.name.span,
                    vis,
                    fields: Vec::new(),
                    variants: Vec::new(),
                    discriminants: Vec::new(),
                    drop_impl: None,
                });
                self.define(mid, &name, Def::Adt(id), vis);
            }
            ItemKind::Enum(e) => {
                let id = self.adts.len() as AdtId;
                let name = e.name.clone();
                let mut seen = HashSet::new();
                for v in &e.variants {
                    if !seen.insert(v.name.name.clone()) {
                        self.error(Diagnostic::new(
                            v.name.span,
                            format!("duplicate variant `{}`", v.name.name),
                        ));
                    }
                }
                self.adts.push(AdtDef {
                    name: e.name.name.clone(),
                    module: mid,
                    generics: e.generics.params.clone(),
                    is_copy: e.is_copy,
                    kind: AdtKind::Enum(e.variants),
                    span: e.name.span,
                    vis,
                    fields: Vec::new(),
                    variants: Vec::new(),
                    discriminants: Vec::new(),
                    drop_impl: None,
                });
                self.define(mid, &name, Def::Adt(id), vis);
                // `Some`/`None`/`Ok`/`Err` are usable unqualified everywhere
                if self.modules[mid as usize].path == "std.prelude"
                    && (name.name == "Option" || name.name == "Result")
                {
                    if let AdtKind::Enum(vs) = &self.adts[id as usize].kind {
                        let vs: Vec<Ident> = vs.iter().map(|v| v.name.clone()).collect();
                        for (i, v) in vs.iter().enumerate() {
                            self.define(mid, v, Def::Variant(id, i as u32), Vis::Pub);
                        }
                    }
                }
            }
            ItemKind::Trait(t) => {
                let id = self.traits.len() as TraitId;
                let name = t.name.clone();
                if !t.generics.params.is_empty() {
                    self.error(Diagnostic::new(
                        t.name.span,
                        "generic traits are not supported yet",
                    ));
                }
                let mut defaults = HashMap::new();
                for m in &t.methods {
                    if m.body.is_some() {
                        let fid = self.fns.len() as FnId;
                        self.fns.push(FnDef {
                            decl: m.clone(),
                            module: mid,
                            owner: FnOwner::Trait(id),
                            vis: Vis::Pub,
                        });
                        defaults.insert(m.name.name.clone(), fid);
                    }
                    if m.self_param.is_none() {
                        self.error(Diagnostic::new(m.name.span, "trait methods must take `self`, `&self` or `&mut self` (static trait functions are not supported yet)"));
                    }
                }
                self.traits.push(TraitDef {
                    name: t.name.name.clone(),
                    module: mid,
                    decl: t,
                    supertraits: Vec::new(),
                    defaults,
                });
                self.define(mid, &name, Def::Trait(id), vis);
            }
            ItemKind::Impl(i) => impls.push((mid, i)),
            ItemKind::TypeAlias { name, generics, ty } => {
                let id = self.aliases.len() as u32;
                self.aliases.push(AliasDef {
                    module: mid,
                    generics: generics.params,
                    ty,
                });
                self.define(mid, &name, Def::Alias(id), vis);
            }
            ItemKind::Const { name, ty, value } => {
                let id = self.consts.len() as u32;
                self.consts.push(ConstDef {
                    name: name.name.clone(),
                    module: mid,
                    ty,
                    value,
                    cache: None,
                    in_progress: false,
                });
                self.define(mid, &name, Def::Const(id), vis);
            }
            ItemKind::Static {
                name,
                mutable,
                ty,
                value,
            } => {
                let id = self.statics.len() as u32;
                let symbol = format!("{}.{}", self.modules[mid as usize].path, name.name);
                self.statics.push(StaticDef {
                    name: name.name.clone(),
                    module: mid,
                    mutable,
                    ty,
                    value,
                    symbol,
                    resolved: None,
                });
                self.define(mid, &name, Def::Static(id), vis);
            }
        }
    }

    pub fn find_module(&self, path: &str) -> Option<ModId> {
        self.modules
            .iter()
            .position(|m| m.path == path)
            .map(|i| i as ModId)
    }

    fn resolve_import(&mut self, mid: ModId, vis: Vis, imp: &ast::Import, span: Span) {
        let path: Vec<&str> = imp.path.iter().map(|i| i.name.as_str()).collect();
        let full = path.join(".");
        let Some(target) = self.find_module(&full) else {
            self.error(Diagnostic::new(span, format!("module `{full}` not found")));
            return;
        };
        match &imp.names {
            None => {
                let bind = imp
                    .alias
                    .clone()
                    .unwrap_or_else(|| imp.path.last().unwrap().clone());
                let scope = &mut self.modules[mid as usize].scope;
                if scope.contains_key(&bind.name) {
                    self.error(Diagnostic::new(
                        bind.span,
                        format!("`{}` is already defined in this module", bind.name),
                    ));
                    return;
                }
                scope.insert(
                    bind.name.clone(),
                    Binding {
                        def: Def::Module(target),
                        vis,
                        span: bind.span,
                        imported: true,
                    },
                );
            }
            Some(names) => {
                for n in names {
                    let Some(b) = self.modules[target as usize].scope.get(&n.name).copied() else {
                        self.error(Diagnostic::new(
                            n.span,
                            format!("module `{full}` has no item named `{}`", n.name),
                        ));
                        continue;
                    };
                    if b.vis != Vis::Pub || b.imported {
                        self.error(Diagnostic::new(
                            n.span,
                            format!("`{}` is private to module `{full}`", n.name),
                        ));
                        continue;
                    }
                    let scope = &mut self.modules[mid as usize].scope;
                    if scope.contains_key(&n.name) {
                        self.error(Diagnostic::new(
                            n.span,
                            format!("`{}` is already defined in this module", n.name),
                        ));
                        continue;
                    }
                    scope.insert(
                        n.name.clone(),
                        Binding {
                            def: b.def,
                            vis,
                            span: n.span,
                            imported: true,
                        },
                    );
                }
            }
        }
    }

    /// Look up a name visible in module `mid`: own scope, then the prelude.
    pub fn lookup(&self, mid: ModId, name: &str) -> Option<Def> {
        if let Some(b) = self.modules[mid as usize].scope.get(name) {
            return Some(b.def);
        }
        if let Some(p) = self.prelude {
            if let Some(b) = self.modules[p as usize].scope.get(name) {
                if b.vis == Vis::Pub && !b.imported {
                    return Some(b.def);
                }
            }
        }
        // core types re-exported by the prelude
        for (mpath, n) in PRELUDE_TYPES {
            if *n == name {
                if let Some(m) = self.find_module(mpath) {
                    if let Some(b) = self.modules[m as usize].scope.get(name) {
                        return Some(b.def);
                    }
                }
            }
        }
        INTRINSICS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, i)| Def::Intrinsic(*i))
    }

    /// Look up `name` inside module `target` from module `from` (respecting `pub`).
    pub fn lookup_in(&self, from: ModId, target: ModId, name: &str, span: Span) -> Result<Def> {
        match self.modules[target as usize].scope.get(name) {
            Some(b) if b.vis == Vis::Pub && !b.imported || from == target => Ok(b.def),
            Some(_) => Err(Diagnostic::new(
                span,
                format!(
                    "`{name}` is private to module `{}`",
                    self.modules[target as usize].path
                ),
            )),
            None => Err(Diagnostic::new(
                span,
                format!(
                    "module `{}` has no item named `{name}`",
                    self.modules[target as usize].path
                ),
            )),
        }
    }

    fn adt_env(&self, adt: AdtId) -> Env {
        let mut env = Env::default();
        for (i, g) in self.adts[adt as usize].generics.iter().enumerate() {
            env.names.insert(g.name.name.clone(), Ty::Param(i as u32));
        }
        env
    }

    fn resolve_adts(&mut self) {
        for id in 0..self.adts.len() {
            let env = self.adt_env(id as AdtId);
            let module = self.adts[id].module;
            let mut fields = Vec::new();
            let mut variants = Vec::new();
            let mut discs = Vec::new();
            let mut errs = Vec::new();
            let kind = self.adts[id].kind.clone();
            match &kind {
                AdtKind::Struct(fs) => {
                    let mut seen = HashSet::new();
                    for f in fs {
                        if !seen.insert(f.name.name.clone()) {
                            errs.push(Diagnostic::new(
                                f.name.span,
                                format!("duplicate field `{}`", f.name.name),
                            ));
                        }
                        match self.resolve_type(&f.ty, module, &env) {
                            Ok(t) => fields.push(t),
                            Err(d) => {
                                errs.push(d);
                                fields.push(Ty::unit());
                            }
                        }
                    }
                }
                AdtKind::Enum(vs) => {
                    let mut next: i128 = 0;
                    for v in vs {
                        let mut tys = Vec::new();
                        for t in &v.fields {
                            match self.resolve_type(t, module, &env) {
                                Ok(t) => tys.push(t),
                                Err(d) => errs.push(d),
                            }
                        }
                        if let Some(e) = &v.discriminant {
                            if !v.fields.is_empty() {
                                errs.push(Diagnostic::new(e.span, "explicit discriminants are only allowed on variants without fields"));
                            }
                            match self.const_int(e, module) {
                                Ok(n) => next = n,
                                Err(d) => errs.push(d),
                            }
                        }
                        if discs.contains(&next) {
                            errs.push(Diagnostic::new(
                                v.name.span,
                                format!("discriminant value {next} is assigned more than once"),
                            ));
                        }
                        discs.push(next);
                        next += 1;
                        variants.push(tys);
                    }
                }
            }
            for f in fields.iter().chain(variants.iter().flatten()) {
                if f.is_unsized() {
                    let span = self.adts[id].span;
                    errs.push(Diagnostic::new(span, "fields must have a statically known size (use `&str`/`&[T]` behind a pointer)"));
                    break;
                }
            }
            for e in errs {
                self.error(e);
            }
            let a = &mut self.adts[id];
            a.fields = fields;
            a.variants = variants;
            a.discriminants = discs;
        }
        // Infinite-size check: a by-value cycle through fields.
        for id in 0..self.adts.len() {
            let args: Vec<Ty> = (0..self.adts[id].generics.len() as u32)
                .map(Ty::Param)
                .collect();
            let mut stack = Vec::new();
            if self.contains_by_value(&Ty::Adt(id as AdtId, args), id as AdtId, &mut stack, true) {
                let span = self.adts[id].span;
                let name = self.adts[id].name.clone();
                self.error(Diagnostic::new(span, format!("recursive type `{name}` has infinite size; use a raw pointer for indirection")));
            }
        }
    }

    /// Does a value of type `t` contain a `target` by value (through fields,
    /// tuples, arrays and enum payloads, but not through pointers)?
    fn contains_by_value(&self, t: &Ty, target: AdtId, stack: &mut Vec<Ty>, root: bool) -> bool {
        match t {
            Ty::Adt(id, args) => {
                if *id == target && !root {
                    return true;
                }
                if stack.contains(t) || stack.len() > 32 {
                    return false;
                }
                stack.push(t.clone());
                let a = &self.adts[*id as usize];
                let inner: Vec<Ty> = a
                    .fields
                    .iter()
                    .chain(a.variants.iter().flatten())
                    .map(|f| f.subst(args))
                    .collect();
                let found = inner
                    .iter()
                    .any(|f| self.contains_by_value(f, target, stack, false));
                stack.pop();
                found
            }
            Ty::Array(e, _) => self.contains_by_value(e, target, stack, false),
            Ty::Tuple(ts) => ts
                .iter()
                .any(|t| self.contains_by_value(t, target, stack, false)),
            _ => false,
        }
    }

    fn resolve_traits(&mut self) {
        for tid in 0..self.traits.len() {
            let module = self.traits[tid].module;
            let sups = self.traits[tid].decl.supertraits.clone();
            let mut ids = Vec::new();
            for s in &sups {
                match self.resolve_trait_ref(s, module) {
                    Ok(t) => ids.push(t),
                    Err(d) => self.error(d),
                }
            }
            self.traits[tid].supertraits = ids;
        }
    }

    pub fn resolve_trait_ref(&self, t: &TypeExpr, module: ModId) -> Result<TraitId> {
        let TypeKind::Path(path, args) = &t.kind else {
            return Err(Diagnostic::new(t.span, "expected a trait name"));
        };
        if !args.is_empty() {
            return Err(Diagnostic::new(
                t.span,
                "generic traits are not supported yet",
            ));
        }
        match self.resolve_path_def(path, module)? {
            Def::Trait(id) => Ok(id),
            _ => Err(Diagnostic::new(
                t.span,
                format!("`{}` is not a trait", path.last().unwrap().name),
            )),
        }
    }

    fn resolve_path_def(&self, path: &[Ident], module: ModId) -> Result<Def> {
        let first = &path[0];
        let mut def = self.lookup(module, &first.name).ok_or_else(|| {
            Diagnostic::new(
                first.span,
                format!("cannot find `{}` in this scope", first.name),
            )
        })?;
        for seg in &path[1..] {
            match def {
                Def::Module(m) => def = self.lookup_in(module, m, &seg.name, seg.span)?,
                _ => {
                    return Err(Diagnostic::new(
                        seg.span,
                        format!("`{}` is not a module", first.name),
                    ))
                }
            }
        }
        Ok(def)
    }

    fn collect_impl(&mut self, mid: ModId, decl: ast::ImplDecl) {
        let iid = self.impls.len() as ImplId;
        let mut env = Env::default();
        for (i, g) in decl.generics.params.iter().enumerate() {
            env.names.insert(g.name.name.clone(), Ty::Param(i as u32));
        }
        let self_ty = match self.resolve_type(&decl.self_ty, mid, &env) {
            Ok(t) => t,
            Err(d) => {
                self.error(d);
                return;
            }
        };
        if let Ty::Adt(id, _) = &self_ty {
            if self.adts[*id as usize].module != mid && !self.modules[mid as usize].is_std {
                // Inherent impls on foreign types act like extension functions; allowed.
            }
        }
        let trait_id = match &decl.trait_ref {
            Some(t) => match self.resolve_trait_ref(t, mid) {
                Ok(id) => Some(id),
                Err(d) => {
                    self.error(d);
                    return;
                }
            },
            None => None,
        };
        let mut methods = HashMap::new();
        for (vis, m) in decl.methods {
            if m.body.is_none() {
                self.error(Diagnostic::new(
                    m.name.span,
                    "methods in an `impl` block need a body",
                ));
                continue;
            }
            let fid = self.fns.len() as FnId;
            let name = m.name.clone();
            let vis = if trait_id.is_some() { Vis::Pub } else { vis };
            self.fns.push(FnDef {
                decl: m,
                module: mid,
                owner: FnOwner::Impl(iid),
                vis,
            });
            if methods.insert(name.name.clone(), fid).is_some() {
                self.error(Diagnostic::new(
                    name.span,
                    format!("duplicate method `{}`", name.name),
                ));
            }
        }
        let mut drop_fn = None;
        if let Some(body) = decl.drop {
            if trait_id.is_some() {
                self.error(Diagnostic::new(
                    body.span,
                    "`drop` blocks belong in an inherent `impl Type { ... }` block",
                ));
            }
            let span = body.span;
            let fid = self.fns.len() as FnId;
            let fdecl = FnDecl {
                name: Ident {
                    name: "drop".into(),
                    span,
                },
                generics: ast::Generics::default(),
                self_param: Some((SelfKind::RefMut, false, span)),
                params: Vec::new(),
                variadic: false,
                ret: None,
                body: Some(body),
                is_unsafe: false,
                is_export: false,
                span,
            };
            self.fns.push(FnDef {
                decl: fdecl,
                module: mid,
                owner: FnOwner::Impl(iid),
                vis: Vis::Pub,
            });
            drop_fn = Some(fid);
            match &self_ty {
                Ty::Adt(id, args) => {
                    let a = &self.adts[*id as usize];
                    let generic_ok = args
                        .iter()
                        .enumerate()
                        .all(|(i, t)| *t == Ty::Param(i as u32))
                        && args.len() == decl.generics.params.len();
                    if a.module != mid {
                        self.error(Diagnostic::new(
                            span,
                            "a `drop` block must be in the same module as the type it destroys",
                        ));
                    } else if !generic_ok {
                        self.error(Diagnostic::new(span, "a `drop` block must apply to every instance of the type (`impl<T> Type<T> { drop { ... } }`)"));
                    } else if a.is_copy {
                        self.error(Diagnostic::new(
                            span,
                            format!("`copy` type `{}` cannot have a `drop` block", a.name),
                        ));
                    } else if a.drop_impl.is_some() {
                        self.error(Diagnostic::new(
                            span,
                            format!("type `{}` already has a `drop` block", a.name),
                        ));
                    } else {
                        self.adts[*id as usize].drop_impl = Some(iid);
                    }
                }
                _ => self.error(Diagnostic::new(
                    span,
                    "`drop` blocks are only allowed on structs and enums",
                )),
            }
        }
        self.impls.push(ImplDef {
            module: mid,
            generics: decl.generics.params,
            self_ty_ast: decl.self_ty,
            trait_ast: decl.trait_ref,
            self_ty,
            trait_id,
            methods,
            drop_fn,
            span: decl.span,
        });
    }

    fn validate_impls(&mut self) {
        for iid in 0..self.impls.len() {
            let Some(tid) = self.impls[iid].trait_id else {
                continue;
            };
            let span = self.impls[iid].span;
            let tname = self.traits[tid as usize].name.clone();
            // methods must match the trait
            let required: Vec<(String, bool, Option<SelfKind>, usize)> = self.traits[tid as usize]
                .decl
                .methods
                .iter()
                .map(|m| {
                    (
                        m.name.name.clone(),
                        m.body.is_some(),
                        m.self_param.map(|s| s.0),
                        m.params.len(),
                    )
                })
                .collect();
            for (name, has_default, self_kind, nparams) in &required {
                match self.impls[iid].methods.get(name) {
                    Some(&fid) => {
                        let d = &self.fns[fid as usize].decl;
                        if d.self_param.map(|s| s.0) != *self_kind || d.params.len() != *nparams {
                            let sp = d.name.span;
                            self.error(Diagnostic::new(sp, format!("method `{name}` does not match the signature declared in trait `{tname}`")));
                        }
                    }
                    None if !has_default => {
                        self.error(Diagnostic::new(
                            span,
                            format!("missing method `{name}` in implementation of trait `{tname}`"),
                        ));
                    }
                    None => {}
                }
            }
            let names: Vec<(String, Span)> = self.impls[iid]
                .methods
                .iter()
                .map(|(n, f)| (n.clone(), self.fns[*f as usize].decl.name.span))
                .collect();
            for (n, sp) in names {
                if !required.iter().any(|r| r.0 == n) {
                    self.error(Diagnostic::new(
                        sp,
                        format!("method `{n}` is not a member of trait `{tname}`"),
                    ));
                }
            }
            // overlapping impls of the same trait for the same type
            for other in 0..iid {
                if self.impls[other].trait_id == Some(tid)
                    && templates_overlap(&self.impls[other].self_ty, &self.impls[iid].self_ty)
                {
                    let ty = self.ty_str(&self.impls[iid].self_ty);
                    let prev = self.impls[other].span;
                    self.error(
                        Diagnostic::new(
                            span,
                            format!("conflicting implementations of trait `{tname}` for `{ty}`"),
                        )
                        .note(prev, "first implementation here"),
                    );
                }
            }
            // supertraits: checked for non-generic impls now
            if self.impls[iid].generics.is_empty() {
                let ty = self.impls[iid].self_ty.clone();
                for sup in self.traits[tid as usize].supertraits.clone() {
                    if !self.implements(&ty, sup) {
                        let tys = self.ty_str(&ty);
                        let sname = self.traits[sup as usize].name.clone();
                        self.error(Diagnostic::new(span, format!("trait `{tname}` extends `{sname}`, but `{tys}` does not implement `{sname}`")));
                    }
                }
            }
        }
    }

    // ----------------------------------------------------------------- types

    pub fn ty_str(&self, t: &Ty) -> String {
        let adt_name = |id: AdtId| self.adts[id as usize].name.clone();
        let param_name = |i: u32| format!("T{i}");
        TyFmt {
            ty: t,
            adt_name: &adt_name,
            param_name: &param_name,
        }
        .to_string()
    }

    /// Module-qualified type name used in symbol mangling.
    pub fn ty_mangle(&self, t: &Ty) -> String {
        let adt_name = |id: AdtId| {
            let a = &self.adts[id as usize];
            format!("{}.{}", self.modules[a.module as usize].path, a.name)
        };
        let param_name = |i: u32| format!("T{i}");
        TyFmt {
            ty: t,
            adt_name: &adt_name,
            param_name: &param_name,
        }
        .to_string()
    }

    pub fn resolve_type(&mut self, t: &TypeExpr, module: ModId, env: &Env) -> Result<Ty> {
        Ok(match &t.kind {
            TypeKind::Ref(m, inner) => {
                Ty::Ref(Box::new(self.resolve_type(inner, module, env)?), *m)
            }
            TypeKind::Ptr(inner) => Ty::Ptr(Box::new(self.resolve_type(inner, module, env)?)),
            TypeKind::Slice(inner) => Ty::Slice(Box::new(self.resolve_type(inner, module, env)?)),
            TypeKind::Array(inner, len) => {
                let elem = self.resolve_type(inner, module, env)?;
                let n = self.const_int(len, module)?;
                if n < 0 {
                    return Err(Diagnostic::new(
                        len.span,
                        "array length must be non-negative",
                    ));
                }
                Ty::Array(Box::new(elem), n as u64)
            }
            TypeKind::Tuple(ts) => Ty::Tuple(
                ts.iter()
                    .map(|t| self.resolve_type(t, module, env))
                    .collect::<Result<_>>()?,
            ),
            TypeKind::Never => Ty::Never,
            TypeKind::FnPtr(ps, r) => Ty::FnPtr(
                ps.iter().map(|t| self.resolve_type(t, module, env)).collect::<Result<_>>()?,
                Box::new(match r {
                    Some(r) => self.resolve_type(r, module, env)?,
                    None => Ty::unit(),
                }),
            ),
            TypeKind::FnTrait(..) => {
                return Err(Diagnostic::new(
                    t.span,
                    "`Fn(...)` is a bound for generic parameters (`F: Fn(i32) -> i32`); for a value type use a function pointer `fn(i32) -> i32`",
                ))
            }
            TypeKind::Infer => {
                return Err(Diagnostic::new(
                    t.span,
                    "type placeholder `_` is not allowed here",
                ))
            }
            TypeKind::SelfType => env.self_ty.clone().ok_or_else(|| {
                Diagnostic::new(
                    t.span,
                    "`Self` is only available inside `impl` and `trait` blocks",
                )
            })?,
            TypeKind::Path(path, args) => {
                if path.len() == 1 {
                    let name = &path[0].name;
                    if let Some(ty) = env.names.get(name) {
                        if !args.is_empty() {
                            return Err(Diagnostic::new(
                                t.span,
                                "type parameters cannot take generic arguments",
                            ));
                        }
                        return Ok(ty.clone());
                    }
                    if self.modules[module as usize].scope.get(name).is_none() {
                        if let Some(p) = prim_type(name) {
                            if !args.is_empty() {
                                return Err(Diagnostic::new(
                                    t.span,
                                    format!("`{name}` takes no generic arguments"),
                                ));
                            }
                            return Ok(p);
                        }
                    }
                }
                let def = self.resolve_path_def(path, module)?;
                let args: Vec<Ty> = args
                    .iter()
                    .map(|a| self.resolve_type(a, module, env))
                    .collect::<Result<_>>()?;
                match def {
                    Def::Adt(id) => {
                        let a = &self.adts[id as usize];
                        if a.module != module && a.vis != Vis::Pub && !self.modules[module as usize].is_std {
                            return Err(Diagnostic::new(t.span, format!("type `{}` is private", a.name)));
                        }
                        if args.len() != a.generics.len() {
                            return Err(Diagnostic::new(
                                t.span,
                                format!("type `{}` expects {} generic argument(s), found {}", a.name, a.generics.len(), args.len()),
                            ));
                        }
                        Ty::Adt(id, args)
                    }
                    Def::Alias(id) => {
                        let (amod, generics, body) = {
                            let a = &self.aliases[id as usize];
                            (a.module, a.generics.clone(), a.ty.clone())
                        };
                        if args.len() != generics.len() {
                            return Err(Diagnostic::new(t.span, format!("type alias expects {} generic argument(s), found {}", generics.len(), args.len())));
                        }
                        let mut aenv = Env::default();
                        for (g, a) in generics.iter().zip(args) {
                            aenv.names.insert(g.name.name.clone(), a);
                        }
                        self.alias_depth += 1;
                        if self.alias_depth > 64 {
                            self.alias_depth = 0;
                            return Err(Diagnostic::new(t.span, "type alias expands recursively"));
                        }
                        let r = self.resolve_type(&body, amod, &aenv);
                        self.alias_depth -= 1;
                        r?
                    }
                    Def::Trait(_) => return Err(Diagnostic::new(t.span, "trait objects (`dyn Trait`) are not supported yet; use a generic parameter `<T: Trait>`")),
                    _ => return Err(Diagnostic::new(t.span, format!("`{}` is not a type", path.last().unwrap().name))),
                }
            }
        })
    }

    /// Struct field types of a concrete ADT instance.
    pub fn struct_fields(&self, id: AdtId, args: &[Ty]) -> Vec<Ty> {
        self.adts[id as usize]
            .fields
            .iter()
            .map(|f| f.subst(args))
            .collect()
    }

    pub fn enum_variants(&self, id: AdtId, args: &[Ty]) -> Vec<Vec<Ty>> {
        self.adts[id as usize]
            .variants
            .iter()
            .map(|v| v.iter().map(|f| f.subst(args)).collect())
            .collect()
    }

    pub fn is_copy(&self, t: &Ty) -> bool {
        match t {
            Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Never | Ty::Ptr(_) => true,
            Ty::Ref(_, m) => !m,
            Ty::Str | Ty::Slice(_) => false,
            Ty::Array(e, _) => self.is_copy(e),
            Ty::Tuple(ts) => ts.iter().all(|t| self.is_copy(t)),
            Ty::Adt(id, args) => {
                let a = &self.adts[*id as usize];
                a.is_copy
                    && a.fields
                        .iter()
                        .chain(a.variants.iter().flatten())
                        .all(|f| self.is_copy(&f.subst(args)))
            }
            Ty::Param(_) => false,
            Ty::FnPtr(..) => true,
            Ty::Closure(id) => self.closures[*id as usize]
                .fields
                .iter()
                .all(|f| self.is_copy(f)),
        }
    }

    /// Field types of a struct, tuple or closure environment.
    pub fn fields_of(&self, t: &Ty) -> Option<Vec<Ty>> {
        match t {
            Ty::Adt(id, args) if !self.adts[*id as usize].is_enum() => {
                Some(self.struct_fields(*id, args))
            }
            Ty::Tuple(ts) => Some(ts.clone()),
            Ty::Closure(id) => Some(self.closures[*id as usize].fields.clone()),
            _ => None,
        }
    }

    /// Parameter types, return type and FnMut-ness of a callable type.
    pub fn callable_sig(&self, t: &Ty) -> Option<(Vec<Ty>, Ty, u8)> {
        match t {
            Ty::Closure(id) => {
                let c = &self.closures[*id as usize];
                Some((c.params.clone(), c.ret.clone(), c.kind))
            }
            Ty::FnPtr(ps, r) => Some((ps.clone(), (**r).clone(), 0)),
            _ => None,
        }
    }

    /// Can a value of type `t` hold references (and so keep borrows alive)?
    /// Looks through struct/enum fields and closure captures.
    pub fn has_refs(&self, t: &Ty) -> bool {
        self.has_refs_depth(t, 0)
    }

    fn has_refs_depth(&self, t: &Ty, depth: u32) -> bool {
        if depth > 64 {
            return true;
        }
        match t {
            Ty::Ref(..) => true,
            // memory owned through a raw pointer (a Vec's buffer) counts too
            Ty::Array(e, _) | Ty::Slice(e) | Ty::Ptr(e) => self.has_refs_depth(e, depth + 1),
            Ty::Tuple(ts) => ts.iter().any(|t| self.has_refs_depth(t, depth + 1)),
            Ty::Closure(id) => self.closures[*id as usize]
                .fields
                .iter()
                .any(|t| self.has_refs_depth(t, depth + 1)),
            Ty::Adt(id, args) => {
                let a = &self.adts[*id as usize];
                a.fields
                    .iter()
                    .chain(a.variants.iter().flatten())
                    .any(|f| self.has_refs_depth(&f.subst(args), depth + 1))
            }
            _ => false,
        }
    }

    /// Can destroying a value of type `t` read data it borrows? True when a
    /// `drop` block can see the borrows. `Vec`, `Box` and `String` only free
    /// memory and destroy their elements, so they just pass the question on.
    pub fn drop_uses_borrows(&self, t: &Ty) -> bool {
        self.drop_uses_borrows_depth(t, 0)
    }

    fn drop_uses_borrows_depth(&self, t: &Ty, depth: u32) -> bool {
        if depth > 64 {
            return true;
        }
        match t {
            Ty::Array(e, _) | Ty::Ptr(e) => self.drop_uses_borrows_depth(e, depth + 1),
            Ty::Tuple(ts) => ts
                .iter()
                .any(|t| self.drop_uses_borrows_depth(t, depth + 1)),
            Ty::Closure(id) => self.closures[*id as usize]
                .fields
                .iter()
                .any(|t| self.drop_uses_borrows_depth(t, depth + 1)),
            Ty::Adt(id, args) => {
                let a = &self.adts[*id as usize];
                let trusted = self.modules[a.module as usize].is_std
                    && matches!(a.name.as_str(), "Vec" | "Box" | "String");
                if a.drop_impl.is_some() && !trusted && self.has_refs(t) {
                    return true;
                }
                a.fields
                    .iter()
                    .chain(a.variants.iter().flatten())
                    .any(|f| self.drop_uses_borrows_depth(&f.subst(args), depth + 1))
            }
            _ => false,
        }
    }

    pub fn needs_drop(&self, t: &Ty) -> bool {
        self.needs_drop_depth(t, 0)
    }

    fn needs_drop_depth(&self, t: &Ty, depth: u32) -> bool {
        if depth > 64 {
            return false;
        }
        match t {
            Ty::Array(e, n) => *n > 0 && self.needs_drop_depth(e, depth + 1),
            Ty::Tuple(ts) => ts.iter().any(|t| self.needs_drop_depth(t, depth + 1)),
            Ty::Closure(id) => self.closures[*id as usize]
                .fields
                .iter()
                .any(|t| self.needs_drop_depth(t, depth + 1)),
            Ty::Adt(id, args) => {
                let a = &self.adts[*id as usize];
                a.drop_impl.is_some()
                    || a.fields
                        .iter()
                        .chain(a.variants.iter().flatten())
                        .any(|f| self.needs_drop_depth(&f.subst(args), depth + 1))
            }
            _ => false,
        }
    }

    /// Make sure the user `drop` functions needed to destroy `t` are instantiated.
    pub fn ensure_drop(&mut self, t: &Ty) {
        if !self.needs_drop(t) {
            return;
        }
        match t {
            Ty::Array(e, _) => self.ensure_drop(e),
            Ty::Tuple(ts) => {
                for t in ts.clone() {
                    self.ensure_drop(&t);
                }
            }
            Ty::Closure(id) => {
                for t in self.closures[*id as usize].fields.clone() {
                    self.ensure_drop(&t);
                }
            }
            Ty::Adt(id, args) => {
                if let Some(iid) = self.adts[*id as usize].drop_impl {
                    if !self.drop_fns.contains_key(t) {
                        let fid = self.impls[iid as usize].drop_fn.unwrap();
                        let name = self.instance(FnKey {
                            fid,
                            targs: args.clone(),
                            self_ty: None,
                        });
                        self.drop_fns.insert(t.clone(), name);
                    }
                }
                let a = &self.adts[*id as usize];
                let inner: Vec<Ty> = a
                    .fields
                    .iter()
                    .chain(a.variants.iter().flatten())
                    .map(|f| f.subst(args))
                    .collect();
                for f in inner {
                    self.ensure_drop(&f);
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ traits/impls

    /// Does concrete type `t` implement trait `tid`?
    pub fn implements(&mut self, t: &Ty, tid: TraitId) -> bool {
        self.find_trait_impl(t, tid).is_some()
    }

    /// Find the impl of `tid` for `t`, returning impl id and its generic args.
    pub fn find_trait_impl(&mut self, t: &Ty, tid: TraitId) -> Option<(ImplId, Vec<Ty>)> {
        for iid in 0..self.impls.len() {
            if self.impls[iid].trait_id != Some(tid) {
                continue;
            }
            if let Some(args) = self.match_impl(iid as ImplId, t) {
                return Some((iid as ImplId, args));
            }
        }
        None
    }

    /// Match an impl's self type against `t`; checks the impl's bounds.
    pub fn match_impl(&mut self, iid: ImplId, t: &Ty) -> Option<Vec<Ty>> {
        let n = self.impls[iid as usize].generics.len();
        let mut slots = vec![None; n];
        if !match_template(&self.impls[iid as usize].self_ty, t, &mut slots) {
            return None;
        }
        let args: Vec<Ty> = slots.into_iter().map(|s| s.unwrap_or(Ty::unit())).collect();
        // check bounds
        let generics = self.impls[iid as usize].generics.clone();
        let module = self.impls[iid as usize].module;
        let env = generic_env(&generics, &args);
        for (g, a) in generics.iter().zip(&args) {
            for b in &g.bounds {
                if !self.bound_holds(b, module, &env, a).unwrap_or(false) {
                    return None;
                }
            }
        }
        Some(args)
    }

    /// Check `T: Bound` requirements of generic params with concrete args.
    pub fn check_bounds(
        &mut self,
        generics: &[GenericParam],
        args: &[Ty],
        module: ModId,
        span: Span,
    ) -> Result<()> {
        self.check_bounds_in(generics, args, module, span, Env::default())
    }

    /// `check_bounds` where the bounds may also mention names from `base`
    /// (the enclosing impl's generic parameters and `Self`).
    pub fn check_bounds_in(
        &mut self,
        generics: &[GenericParam],
        args: &[Ty],
        module: ModId,
        span: Span,
        base: Env,
    ) -> Result<()> {
        let mut env = base;
        env.names.extend(generic_env(generics, args).names);
        for (g, a) in generics.iter().zip(args) {
            for b in &g.bounds {
                if let TypeKind::FnTrait(bound_kind, ..) = &b.kind {
                    if !self.bound_holds(b, module, &env, a)? {
                        let want = self.fn_bound_str(b, module, &env)?;
                        let have = match self.callable_sig(a) {
                            Some((ps, r, m)) => format!(
                                "{}({}) -> {}",
                                kind_name(m),
                                ps.iter()
                                    .map(|t| self.ty_str(t))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                self.ty_str(&r)
                            ),
                            None => format!("`{}`, which is not callable", self.ty_str(a)),
                        };
                        let have_kind = self.callable_sig(a).map(|s| s.2).unwrap_or(0);
                        let hint = if have_kind > *bound_kind && have_kind == 2 {
                            "; the closure moves a captured value out, so it can only be called once and the bound must be `FnOnce`"
                        } else if have_kind > *bound_kind {
                            "; the closure mutates captured state, so the bound must be `FnMut`"
                        } else {
                            ""
                        };
                        return Err(Diagnostic::new(
                            span,
                            format!(
                                "expected a callable `{}: {want}`, found {have}{hint}",
                                g.name.name
                            ),
                        ));
                    }
                    continue;
                }
                let tid = self.resolve_trait_ref(b, module)?;
                if !self.implements(a, tid) {
                    let tys = self.ty_str(a);
                    let tname = self.traits[tid as usize].name.clone();
                    return Err(Diagnostic::new(span, format!("the trait `{tname}` is not implemented for `{tys}` (required by `{}: {tname}`)", g.name.name)));
                }
            }
        }
        Ok(())
    }

    /// Does `arg` satisfy bound `b` (a trait or an `Fn(...)`/`FnMut(...)` bound)?
    pub fn bound_holds(
        &mut self,
        b: &TypeExpr,
        module: ModId,
        env: &Env,
        arg: &Ty,
    ) -> Result<bool> {
        if let TypeKind::FnTrait(bound_kind, ps, r) = &b.kind {
            let Some((aps, ar, akind)) = self.callable_sig(arg) else {
                return Ok(false);
            };
            let ps: Vec<Ty> = ps
                .iter()
                .map(|t| self.resolve_type(t, module, env))
                .collect::<Result<_>>()?;
            let r = match r {
                Some(r) => self.resolve_type(r, module, env)?,
                None => Ty::unit(),
            };
            return Ok(ps == aps && r == ar && akind <= *bound_kind);
        }
        let tid = self.resolve_trait_ref(b, module)?;
        Ok(self.implements(arg, tid))
    }

    fn fn_bound_str(&mut self, b: &TypeExpr, module: ModId, env: &Env) -> Result<String> {
        let TypeKind::FnTrait(bound_kind, ps, r) = &b.kind else {
            return Ok(String::new());
        };
        let ps: Vec<String> = ps
            .iter()
            .map(|t| self.resolve_type(t, module, env).map(|t| self.ty_str(&t)))
            .collect::<Result<_>>()?;
        let r = match r {
            Some(r) => {
                let t = self.resolve_type(r, module, env)?;
                self.ty_str(&t)
            }
            None => "()".into(),
        };
        Ok(format!(
            "{}({}) -> {r}",
            kind_name(*bound_kind),
            ps.join(", ")
        ))
    }

    // ------------------------------------------------------------- instances

    /// Get (or schedule) the instance for `key`, returning its symbol name.
    pub fn instance(&mut self, key: FnKey) -> String {
        if let Some(n) = self.instances.get(&key) {
            return n.clone();
        }
        let f = &self.fns[key.fid as usize];
        let name = if f.decl.is_export || (f.decl.body.is_none()) {
            f.decl.name.name.clone()
        } else {
            let mpath = &self.modules[f.module as usize].path;
            let owner = match f.owner {
                FnOwner::Free => String::new(),
                FnOwner::Impl(iid) => {
                    let im = &self.impls[iid as usize];
                    let n = im.generics.len();
                    let self_ty = im.self_ty.subst(&key.targs[..n.min(key.targs.len())]);
                    let tr = im
                        .trait_id
                        .map(|t| format!(" as {}", self.traits[t as usize].name))
                        .unwrap_or_default();
                    format!("<{}{}>.", self.ty_mangle(&self_ty), tr)
                }
                FnOwner::Trait(tid) => {
                    format!(
                        "<{} as {}>.",
                        self.ty_mangle(key.self_ty.as_ref().unwrap()),
                        self.traits[tid as usize].name
                    )
                }
            };
            let fn_targs = match f.owner {
                FnOwner::Impl(iid) => {
                    &key.targs[self.impls[iid as usize].generics.len().min(key.targs.len())..]
                }
                _ => &key.targs[..],
            };
            let targs = if fn_targs.is_empty() {
                String::new()
            } else {
                format!(
                    "<{}>",
                    fn_targs
                        .iter()
                        .map(|t| self.ty_mangle(t))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            if owner.is_empty() {
                format!("{}.{}{}", mpath, f.decl.name.name, targs)
            } else {
                format!("{}{}{}", owner, f.decl.name.name, targs)
            }
        };
        self.instances.insert(key.clone(), name.clone());
        if self.fns[key.fid as usize].decl.body.is_some() {
            self.queue.push_back((key, name.clone()));
        }
        name
    }

    fn drain_queue(&mut self) {
        let mut count = 0;
        while let Some((key, name)) = self.queue.pop_front() {
            count += 1;
            if count > 20000 {
                let span = self.fns[key.fid as usize].decl.name.span;
                self.error(Diagnostic::new(
                    span,
                    "too many generic instantiations (infinitely recursive generic function?)",
                ));
                return;
            }
            match expr::check_fn(self, &key, name) {
                Ok((f, closures)) => {
                    // a function and the closures defined in it
                    for f in std::iter::once(f).chain(closures) {
                        let mut tys = Vec::new();
                        for l in &f.locals {
                            tys.push(l.ty.clone());
                        }
                        collect_expr_tys(&f.body, &mut tys);
                        for t in tys {
                            self.ensure_drop(&t);
                        }
                        self.out_funcs.push(f);
                    }
                }
                Err(d) => self.error(d),
            }
        }
        // Borrow checking runs once every function is known, so calls can use
        // what each callee's result borrows from.
        self.summaries = crate::borrowck::summaries(self, &self.out_funcs);
        for i in 0..self.out_funcs.len() {
            if let Err(ds) = crate::borrowck::check(self, &self.out_funcs[i]) {
                self.diags.extend(ds);
            }
        }
    }

    pub fn declare_extern(&mut self, fid: FnId) -> Result<String> {
        let decl = self.fns[fid as usize].decl.clone();
        let module = self.fns[fid as usize].module;
        let name = decl.name.name.clone();
        if !self.externs.contains_key(&name) {
            let env = Env::default();
            let mut params = Vec::new();
            for p in &decl.params {
                let t = self.resolve_type(&p.ty, module, &env)?;
                if !ffi_safe(&t) {
                    return Err(Diagnostic::new(p.ty.span, format!("type `{}` cannot cross the C boundary; use integers, floats, bool or raw pointers", self.ty_str(&t))));
                }
                params.push(t);
            }
            let ret = match &decl.ret {
                Some(t) => self.resolve_type(t, module, &env)?,
                None => Ty::unit(),
            };
            if !ffi_safe(&ret) && !ret.is_unit() && !ret.is_never() {
                return Err(Diagnostic::new(decl.ret.as_ref().unwrap().span, "foreign functions may only return integers, floats, bool, raw pointers or nothing"));
            }
            self.externs.insert(
                name.clone(),
                xir::Extern {
                    name: name.clone(),
                    params,
                    ret,
                    variadic: decl.variadic,
                },
            );
        }
        Ok(name)
    }

    pub fn use_static(&mut self, sid: u32) {
        self.used_statics.insert(sid);
    }

    pub fn static_type(&mut self, sid: u32) -> Result<Ty> {
        if let Some(t) = &self.statics[sid as usize].resolved {
            return Ok(t.clone());
        }
        let (ty, module) = (
            self.statics[sid as usize].ty.clone(),
            self.statics[sid as usize].module,
        );
        let t = self.resolve_type(&ty, module, &Env::default())?;
        if self.needs_drop(&t) {
            return Err(Diagnostic::new(
                ty.span,
                "statics cannot hold values that need `drop`",
            ));
        }
        if self.has_refs(&t) && !matches!(&t, Ty::Ref(inner, false) if **inner == Ty::Str) {
            return Err(Diagnostic::new(
                ty.span,
                "statics cannot hold references (except `&str` literals)",
            ));
        }
        self.statics[sid as usize].resolved = Some(t.clone());
        Ok(t)
    }

    fn static_init(&mut self, sid: u32) -> Result<(Ty, xir::ConstVal)> {
        let ty = self.static_type(sid)?;
        let (value, module) = (
            self.statics[sid as usize].value.clone(),
            self.statics[sid as usize].module,
        );
        let v = self.const_eval(&value, module, Some(&ty))?;
        let v = consteval::to_xir(self, &v, &ty, value.span)?;
        Ok((ty, v))
    }
}

fn fid_is_main(f: &FnDef) -> bool {
    f.decl.name.name == "main" && f.owner == FnOwner::Free
}

fn kind_name(k: u8) -> &'static str {
    match k {
        0 => "Fn",
        1 => "FnMut",
        _ => "FnOnce",
    }
}

/// Map generic parameter names to concrete arguments.
fn generic_env(generics: &[GenericParam], args: &[Ty]) -> Env {
    let mut env = Env::default();
    for (g, a) in generics.iter().zip(args) {
        env.names.insert(g.name.name.clone(), a.clone());
    }
    env
}

pub fn prim_type(name: &str) -> Option<Ty> {
    if let Some(i) = IntTy::from_name(name) {
        return Some(Ty::Int(i));
    }
    Some(match name {
        "f32" => Ty::Float(FloatTy::F32),
        "f64" => Ty::Float(FloatTy::F64),
        "bool" => Ty::Bool,
        "str" => Ty::Str,
        _ => return None,
    })
}

/// Does `t` mention a reference anywhere in its type arguments? (Shallow:
/// use `Checker::has_refs` to also look inside struct and closure fields.)
pub fn contains_ref(t: &Ty) -> bool {
    match t {
        Ty::Ref(..) => true,
        Ty::Array(e, _) | Ty::Slice(e) => contains_ref(e),
        Ty::Tuple(ts) | Ty::Adt(_, ts) => ts.iter().any(contains_ref),
        _ => false,
    }
}

fn ffi_safe(t: &Ty) -> bool {
    matches!(
        t,
        Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Ptr(_) | Ty::FnPtr(..)
    )
}

/// Could two impl self-type templates match the same concrete type?
fn templates_overlap(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Param(_), _) | (_, Ty::Param(_)) => true,
        (Ty::Ref(x, m1), Ty::Ref(y, m2)) => m1 == m2 && templates_overlap(x, y),
        (Ty::Ptr(x), Ty::Ptr(y)) | (Ty::Slice(x), Ty::Slice(y)) => templates_overlap(x, y),
        (Ty::Array(x, n), Ty::Array(y, m)) => n == m && templates_overlap(x, y),
        (Ty::Tuple(xs), Ty::Tuple(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| templates_overlap(x, y))
        }
        (Ty::Adt(i, xs), Ty::Adt(j, ys)) => {
            i == j && xs.iter().zip(ys).all(|(x, y)| templates_overlap(x, y))
        }
        _ => a == b,
    }
}

fn collect_expr_tys(e: &xir::Expr, out: &mut Vec<Ty>) {
    out.push(e.ty.clone());
    xir_children(e, &mut |c| collect_expr_tys(c, out));
}

/// Visit direct sub-expressions of an XIR expression.
pub fn xir_children(e: &xir::Expr, f: &mut dyn FnMut(&xir::Expr)) {
    use xir::ExprKind as K;
    fn place(p: &xir::Place, f: &mut dyn FnMut(&xir::Expr)) {
        match p {
            xir::Place::Field(b, _) => place(b, f),
            xir::Place::Index(b, i) => {
                place(b, f);
                f(i);
            }
            xir::Place::Deref(e) => f(e),
            xir::Place::Slice(b, lo, hi) => {
                place(b, f);
                if let Some(lo) = lo {
                    f(lo);
                }
                if let Some(hi) = hi {
                    f(hi);
                }
            }
            _ => {}
        }
    }
    match &e.kind {
        K::Use(p, _) | K::Borrow(p, _) | K::Drop(p) | K::Is(p, _) => place(p, f),
        K::Unary(_, a)
        | K::Cast(a, _)
        | K::ArrayToSlice(a)
        | K::ArrayRepeat(a, _)
        | K::Loop(a, _) => f(a),
        K::Binary(_, a, b) => {
            f(a);
            f(b);
        }
        K::CallPtr(callee, args) => {
            f(callee);
            args.iter().for_each(|a| f(a));
        }
        K::Call(_, args)
        | K::Intrinsic(_, args)
        | K::Struct(args)
        | K::Tuple(args)
        | K::Array(args)
        | K::Variant(_, args) => args.iter().for_each(|a| f(a)),
        K::Assign(p, v) | K::CompoundAssign(_, p, v) => {
            place(p, f);
            f(v);
        }
        K::Block(b) => {
            for s in &b.stmts {
                match s {
                    xir::Stmt::Let(_, e) | xir::Stmt::LetPat(_, e) | xir::Stmt::Expr(e) => f(e),
                }
            }
            if let Some(t) = &b.tail {
                f(t);
            }
        }
        K::If(c, t, e) => {
            f(c);
            f(t);
            if let Some(e) = e {
                f(e);
            }
        }
        K::Break(_, v) | K::Return(v) => {
            if let Some(v) = v {
                f(v);
            }
        }
        K::Match(p, arms) => {
            place(p, f);
            for a in arms {
                if let Some(g) = &a.guard {
                    f(g);
                }
                f(&a.body);
            }
        }
        K::FnRef(_) => {}
        K::Int(_) | K::Float(_) | K::Bool(_) | K::Str(_) | K::Unit | K::Continue(_) => {}
    }
}
