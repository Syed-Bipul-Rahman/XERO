//! Recursive-descent parser: tokens -> AST.
//!
//! Statements end at a newline or `;`. Inside `(...)` and `[...]` newlines
//! are insignificant; inside `{...}` blocks they terminate statements.

use crate::ast::*;
use crate::diag::{Diagnostic, Result, Span};
use crate::lexer::{Kw, Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    file: u32,
    /// Stack of "newlines are significant" flags.
    nl: Vec<bool>,
    /// Struct literals are not allowed in `if`/`while`/`match`/`for` heads.
    no_struct: bool,
}

pub fn parse_module(file: u32, toks: Vec<Token>) -> Result<Module> {
    let mut p = Parser {
        toks,
        pos: 0,
        file,
        nl: vec![true],
        no_struct: false,
    };
    p.module()
}

impl Parser {
    // ---------------------------------------------------------------- tokens

    fn raw(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn skip_nl_if_insignificant(&mut self) {
        if !*self.nl.last().unwrap() {
            while self.toks[self.pos].tok == Tok::Newline {
                self.pos += 1;
            }
        }
    }

    fn peek(&mut self) -> &Tok {
        self.skip_nl_if_insignificant();
        &self.toks[self.pos].tok
    }

    fn peek_at(&mut self, n: usize) -> &Tok {
        self.skip_nl_if_insignificant();
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }

    fn span(&mut self) -> Span {
        self.skip_nl_if_insignificant();
        self.toks[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.toks[self.pos.saturating_sub(1)].span
    }

    fn bump(&mut self) -> Token {
        self.skip_nl_if_insignificant();
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn skip_newlines(&mut self) {
        while self.toks[self.pos].tok == Tok::Newline {
            self.pos += 1;
        }
    }

    fn at(&mut self, t: &Tok) -> bool {
        self.peek() == t
    }

    fn at_kw(&mut self, k: Kw) -> bool {
        *self.peek() == Tok::Kw(k)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: Kw) -> bool {
        self.eat(&Tok::Kw(k))
    }

    fn err<T>(&mut self, msg: impl Into<String>) -> Result<T> {
        let span = self.span();
        Err(Diagnostic::new(span, msg))
    }

    fn expect(&mut self, t: &Tok) -> Result<Span> {
        if self.at(t) {
            Ok(self.bump().span)
        } else {
            let found = self.peek().describe();
            self.err(format!(
                "expected `{}`, found {}",
                crate::lexer::punct_str(t),
                found
            ))
        }
    }

    fn expect_kw(&mut self, k: Kw) -> Result<Span> {
        if self.at_kw(k) {
            Ok(self.bump().span)
        } else {
            let found = self.peek().describe();
            self.err(format!(
                "expected `{}`, found {}",
                crate::lexer::kw_name(k),
                found
            ))
        }
    }

    fn ident(&mut self) -> Result<Ident> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            t => self.err(format!("expected identifier, found {}", t.describe())),
        }
    }

    /// Consume a `>` that closes a generic list, splitting `>>`, `>=`, `>>=`.
    fn expect_gt(&mut self) -> Result<()> {
        let span = self.span();
        let rest = match self.peek() {
            Tok::Gt => {
                self.bump();
                return Ok(());
            }
            Tok::Shr => Tok::Gt,
            Tok::Ge => Tok::Eq,
            Tok::ShrEq => Tok::Ge,
            t => {
                let d = t.describe();
                return self.err(format!("expected `>`, found {d}"));
            }
        };
        self.toks[self.pos] = Token {
            tok: rest,
            span: Span::new(span.file, span.lo + 1, span.hi),
        };
        Ok(())
    }

    fn with_nl<T>(
        &mut self,
        significant: bool,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.nl.push(significant);
        let r = f(self);
        self.nl.pop();
        r
    }

    fn with_no_struct<T>(&mut self, no: bool, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let old = self.no_struct;
        self.no_struct = no;
        let r = f(self);
        self.no_struct = old;
        r
    }

    /// End of a statement or item: newline, `;`, or a closing brace / EOF (not consumed).
    fn end_stmt(&mut self) -> Result<bool> {
        match self.raw() {
            Tok::Newline => {
                self.skip_newlines();
                Ok(false)
            }
            Tok::Semi => {
                self.pos += 1;
                self.skip_newlines();
                Ok(true)
            }
            Tok::RBrace | Tok::Eof => Ok(false),
            t => {
                let d = t.describe();
                self.err(format!(
                    "expected end of statement (newline or `;`), found {d}"
                ))
            }
        }
    }

    // ----------------------------------------------------------------- items

    fn module(&mut self) -> Result<Module> {
        self.skip_newlines();
        let mut name = None;
        if self.eat_kw(Kw::Module) {
            let mut id = self.ident()?;
            while self.eat(&Tok::Dot) {
                let next = self.ident()?;
                id = Ident {
                    name: format!("{}.{}", id.name, next.name),
                    span: id.span.to(next.span),
                };
            }
            name = Some(id);
            self.end_stmt()?;
        }
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if *self.raw() == Tok::Eof {
                break;
            }
            items.push(self.item()?);
            self.end_stmt()?;
        }
        Ok(Module {
            file: self.file,
            name,
            items,
        })
    }

    fn item(&mut self) -> Result<Item> {
        let start = self.span();
        let vis = if self.eat_kw(Kw::Pub) {
            Vis::Pub
        } else {
            self.eat_kw(Kw::Private);
            Vis::Private
        };
        let kind = match self.peek().clone() {
            Tok::Kw(Kw::Import) => {
                self.bump();
                let mut path = vec![self.ident()?];
                let mut names = None;
                while self.eat(&Tok::Dot) {
                    if self.at(&Tok::LBrace) {
                        self.bump();
                        let mut list = Vec::new();
                        self.with_nl(false, |p| {
                            while !p.at(&Tok::RBrace) {
                                list.push(p.ident()?);
                                if !p.eat(&Tok::Comma) {
                                    break;
                                }
                            }
                            p.expect(&Tok::RBrace)
                        })?;
                        names = Some(list);
                        break;
                    }
                    path.push(self.ident()?);
                }
                let alias = if self.eat_kw(Kw::As) { Some(self.ident()?) } else { None };
                ItemKind::Import(Import { path, names, alias })
            }
            Tok::Kw(Kw::Fn | Kw::Unsafe | Kw::Export) => ItemKind::Fn(self.fn_decl()?),
            Tok::Kw(Kw::Copy) => {
                self.bump();
                match self.peek() {
                    Tok::Kw(Kw::Struct) => ItemKind::Struct(self.struct_decl(true)?),
                    Tok::Kw(Kw::Enum) => ItemKind::Enum(self.enum_decl(true)?),
                    _ => return self.err("expected `struct` or `enum` after `copy`"),
                }
            }
            Tok::Kw(Kw::Struct) => ItemKind::Struct(self.struct_decl(false)?),
            Tok::Kw(Kw::Enum) => ItemKind::Enum(self.enum_decl(false)?),
            Tok::Kw(Kw::Trait) => ItemKind::Trait(self.trait_decl()?),
            Tok::Kw(Kw::Impl) => ItemKind::Impl(self.impl_decl()?),
            Tok::Kw(Kw::Type) => {
                self.bump();
                let name = self.ident()?;
                let generics = self.generics()?;
                self.expect(&Tok::Eq)?;
                let ty = self.ty()?;
                ItemKind::TypeAlias { name, generics, ty }
            }
            Tok::Kw(Kw::Const) => {
                self.bump();
                let name = self.ident()?;
                let ty = if self.eat(&Tok::Colon) { Some(self.ty()?) } else { None };
                self.expect(&Tok::Eq)?;
                let value = self.expr()?;
                ItemKind::Const { name, ty, value }
            }
            Tok::Kw(Kw::Static) => {
                self.bump();
                let mutable = self.eat_kw(Kw::Mut);
                let name = self.ident()?;
                self.expect(&Tok::Colon)?;
                let ty = self.ty()?;
                self.expect(&Tok::Eq)?;
                let value = self.expr()?;
                ItemKind::Static { name, mutable, ty, value }
            }
            Tok::Kw(k @ (Kw::Async | Kw::Spawn | Kw::Await)) => {
                return self.err(format!(
                    "`{}` is reserved for Xero's concurrency model and not yet implemented in this compiler version",
                    crate::lexer::kw_name(k)
                ))
            }
            t => return self.err(format!("expected an item (fn, struct, enum, trait, impl, import, ...), found {}", t.describe())),
        };
        Ok(Item {
            vis,
            kind,
            span: start.to(self.prev_span()),
        })
    }

    fn generics(&mut self) -> Result<Generics> {
        let mut g = Generics::default();
        if !self.at(&Tok::Lt) {
            return Ok(g);
        }
        self.bump();
        self.with_nl(false, |p| {
            while !p.at(&Tok::Gt) {
                let name = p.ident()?;
                let mut bounds = Vec::new();
                if p.eat(&Tok::Colon) {
                    bounds.push(p.ty()?);
                    while p.eat(&Tok::Plus) {
                        bounds.push(p.ty()?);
                    }
                }
                g.params.push(GenericParam { name, bounds });
                if !p.eat(&Tok::Comma) {
                    break;
                }
            }
            p.expect_gt()
        })?;
        Ok(g)
    }

    /// `where T: Trait + Other, U: Trait`
    fn where_clause(&mut self, g: &mut Generics) -> Result<()> {
        if !self.eat_kw(Kw::Where) {
            return Ok(());
        }
        loop {
            let name = self.ident()?;
            self.expect(&Tok::Colon)?;
            let mut bounds = vec![self.ty()?];
            while self.eat(&Tok::Plus) {
                bounds.push(self.ty()?);
            }
            match g.params.iter_mut().find(|p| p.name.name == name.name) {
                Some(p) => p.bounds.extend(bounds),
                None => {
                    return Err(Diagnostic::new(
                        name.span,
                        format!(
                            "`where` clause names unknown type parameter `{}`",
                            name.name
                        ),
                    ))
                }
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
            self.skip_newlines();
        }
        Ok(())
    }

    fn fn_decl(&mut self) -> Result<FnDecl> {
        let start = self.span();
        let is_export = self.eat_kw(Kw::Export);
        let is_unsafe = self.eat_kw(Kw::Unsafe);
        self.expect_kw(Kw::Fn)?;
        let name = self.ident()?;
        let mut generics = self.generics()?;
        self.expect(&Tok::LParen)?;
        let mut self_param = None;
        let mut params = Vec::new();
        let mut variadic = false;
        self.with_nl(false, |p| {
            // self parameter
            let sp = p.span();
            if p.at(&Tok::Amp) && matches!(p.peek_at(1), Tok::Kw(Kw::SelfValue))
                || p.at(&Tok::Amp)
                    && matches!(p.peek_at(1), Tok::Kw(Kw::Mut))
                    && matches!(p.peek_at(2), Tok::Kw(Kw::SelfValue))
            {
                p.bump();
                let m = p.eat_kw(Kw::Mut);
                p.bump();
                self_param = Some((
                    if m { SelfKind::RefMut } else { SelfKind::Ref },
                    false,
                    sp.to(p.prev_span()),
                ));
                if !p.eat(&Tok::Comma) {
                    return p.expect(&Tok::RParen).map(|_| ());
                }
            } else if p.at_kw(Kw::SelfValue)
                || (p.at_kw(Kw::Mut) && matches!(p.peek_at(1), Tok::Kw(Kw::SelfValue)))
            {
                let m = p.eat_kw(Kw::Mut);
                p.bump();
                self_param = Some((SelfKind::Value, m, sp.to(p.prev_span())));
                if !p.eat(&Tok::Comma) {
                    return p.expect(&Tok::RParen).map(|_| ());
                }
            }
            while !p.at(&Tok::RParen) {
                if p.eat(&Tok::Ellipsis) {
                    variadic = true;
                    break;
                }
                let mutable = p.eat_kw(Kw::Mut);
                let name = p.ident()?;
                p.expect(&Tok::Colon)?;
                let ty = p.ty()?;
                params.push(Param { name, mutable, ty });
                if !p.eat(&Tok::Comma) {
                    break;
                }
            }
            p.expect(&Tok::RParen).map(|_| ())
        })?;
        let ret = if self.eat(&Tok::Arrow) {
            Some(self.ty()?)
        } else {
            None
        };
        self.where_clause(&mut generics)?;
        let body = if self.at(&Tok::LBrace) {
            Some(self.block()?)
        } else {
            None
        };
        Ok(FnDecl {
            name,
            generics,
            self_param,
            params,
            variadic,
            ret,
            body,
            is_unsafe,
            is_export,
            span: start.to(self.prev_span()),
        })
    }

    /// Parses `{ item, item }` / newline-separated lists inside braces.
    fn brace_list(&mut self, mut f: impl FnMut(&mut Self) -> Result<()>) -> Result<()> {
        self.expect(&Tok::LBrace)?;
        self.with_nl(true, |p| {
            loop {
                p.skip_newlines();
                if p.at(&Tok::RBrace) {
                    break;
                }
                f(p)?;
                p.skip_newlines();
                if p.eat(&Tok::Comma) || p.eat(&Tok::Semi) {
                    continue;
                }
                p.skip_newlines();
                if !p.at(&Tok::RBrace) && p.toks[p.pos - 1].tok != Tok::Newline {
                    let d = p.peek().describe();
                    return p.err(format!("expected `,`, newline or `}}`, found {d}"));
                }
            }
            p.expect(&Tok::RBrace).map(|_| ())
        })
    }

    fn struct_decl(&mut self, is_copy: bool) -> Result<StructDecl> {
        self.expect_kw(Kw::Struct)?;
        let name = self.ident()?;
        let mut generics = self.generics()?;
        self.where_clause(&mut generics)?;
        let mut fields = Vec::new();
        self.brace_list(|p| {
            let vis = if p.eat_kw(Kw::Pub) {
                Vis::Pub
            } else {
                Vis::Private
            };
            let name = p.ident()?;
            p.expect(&Tok::Colon)?;
            let ty = p.ty()?;
            fields.push(FieldDecl { vis, name, ty });
            Ok(())
        })?;
        Ok(StructDecl {
            name,
            generics,
            fields,
            is_copy,
        })
    }

    fn enum_decl(&mut self, is_copy: bool) -> Result<EnumDecl> {
        self.expect_kw(Kw::Enum)?;
        let name = self.ident()?;
        let mut generics = self.generics()?;
        self.where_clause(&mut generics)?;
        let mut variants = Vec::new();
        self.brace_list(|p| {
            let name = p.ident()?;
            let mut fields = Vec::new();
            if p.at(&Tok::LParen) {
                p.bump();
                p.with_nl(false, |p| {
                    while !p.at(&Tok::RParen) {
                        fields.push(p.ty()?);
                        if !p.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    p.expect(&Tok::RParen)
                })?;
            }
            let discriminant = if p.eat(&Tok::Eq) {
                Some(p.expr()?)
            } else {
                None
            };
            variants.push(VariantDecl {
                name,
                fields,
                discriminant,
            });
            Ok(())
        })?;
        Ok(EnumDecl {
            name,
            generics,
            variants,
            is_copy,
        })
    }

    fn trait_decl(&mut self) -> Result<TraitDecl> {
        self.expect_kw(Kw::Trait)?;
        let name = self.ident()?;
        let mut generics = self.generics()?;
        let mut supertraits = Vec::new();
        if self.eat_kw(Kw::Extends) {
            supertraits.push(self.ty()?);
            while self.eat(&Tok::Plus) || self.eat(&Tok::Comma) {
                supertraits.push(self.ty()?);
            }
        }
        self.where_clause(&mut generics)?;
        let mut methods = Vec::new();
        self.brace_list(|p| {
            methods.push(p.fn_decl()?);
            Ok(())
        })?;
        Ok(TraitDecl {
            name,
            generics,
            supertraits,
            methods,
        })
    }

    fn impl_decl(&mut self) -> Result<ImplDecl> {
        let start = self.expect_kw(Kw::Impl)?;
        let mut generics = self.generics()?;
        let first = self.ty()?;
        let (trait_ref, self_ty) = if self.eat_kw(Kw::For) {
            (Some(first), self.ty()?)
        } else {
            (None, first)
        };
        self.where_clause(&mut generics)?;
        let mut methods = Vec::new();
        let mut drop = None;
        self.brace_list(|p| {
            if p.at_kw(Kw::Drop) {
                let sp = p.bump().span;
                if drop.is_some() {
                    return Err(Diagnostic::new(sp, "duplicate `drop` block in impl"));
                }
                drop = Some(p.block()?);
                return Ok(());
            }
            let vis = if p.eat_kw(Kw::Pub) {
                Vis::Pub
            } else {
                Vis::Private
            };
            methods.push((vis, p.fn_decl()?));
            Ok(())
        })?;
        Ok(ImplDecl {
            generics,
            trait_ref,
            self_ty,
            methods,
            drop,
            span: start.to(self.prev_span()),
        })
    }

    // ----------------------------------------------------------------- types

    pub fn ty(&mut self) -> Result<TypeExpr> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Amp | Tok::AndAnd => {
                let double = self.at(&Tok::AndAnd);
                self.bump();
                let m = self.eat_kw(Kw::Mut);
                let inner = self.ty()?;
                let span = start.to(inner.span);
                let r = TypeKind::Ref(m, Box::new(inner));
                if double {
                    TypeKind::Ref(false, Box::new(TypeExpr { kind: r, span }))
                } else {
                    r
                }
            }
            Tok::Star => {
                self.bump();
                TypeKind::Ptr(Box::new(self.ty()?))
            }
            Tok::LBracket => {
                self.bump();
                self.with_nl(false, |p| {
                    let elem = p.ty()?;
                    if p.eat(&Tok::Semi) {
                        let len = p.expr()?;
                        p.expect(&Tok::RBracket)?;
                        Ok(TypeKind::Array(Box::new(elem), Box::new(len)))
                    } else {
                        p.expect(&Tok::RBracket)?;
                        Ok(TypeKind::Slice(Box::new(elem)))
                    }
                })?
            }
            Tok::LParen => {
                self.bump();
                let mut elems = Vec::new();
                self.with_nl(false, |p| {
                    while !p.at(&Tok::RParen) {
                        elems.push(p.ty()?);
                        if !p.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    p.expect(&Tok::RParen)
                })?;
                TypeKind::Tuple(elems)
            }
            Tok::Bang => {
                self.bump();
                TypeKind::Never
            }
            Tok::Underscore => {
                self.bump();
                TypeKind::Infer
            }
            Tok::Kw(Kw::SelfType) => {
                self.bump();
                TypeKind::SelfType
            }
            Tok::Kw(Kw::Fn) => {
                self.bump();
                let (params, ret) = self.fn_type_sig()?;
                TypeKind::FnPtr(params, ret)
            }
            Tok::Ident(name)
                if (name == "Fn" || name == "FnMut") && matches!(self.peek_at(1), Tok::LParen) =>
            {
                self.bump();
                let (params, ret) = self.fn_type_sig()?;
                TypeKind::FnTrait(name == "FnMut", params, ret)
            }
            Tok::Ident(_) => {
                let mut path = vec![self.ident()?];
                while self.at(&Tok::Dot) && matches!(self.peek_at(1), Tok::Ident(_)) {
                    self.bump();
                    path.push(self.ident()?);
                }
                let mut args = Vec::new();
                if self.at(&Tok::Lt) {
                    self.bump();
                    self.with_nl(false, |p| {
                        while !p.at(&Tok::Gt) {
                            args.push(p.ty()?);
                            if !p.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        p.expect_gt()
                    })?;
                }
                TypeKind::Path(path, args)
            }
            t => return self.err(format!("expected a type, found {}", t.describe())),
        };
        Ok(TypeExpr {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    /// `(A, B) -> R` after `fn` / `Fn` / `FnMut`.
    fn fn_type_sig(&mut self) -> Result<(Vec<TypeExpr>, Option<Box<TypeExpr>>)> {
        self.expect(&Tok::LParen)?;
        let params = self.with_nl(false, |p| {
            let mut params = Vec::new();
            while !p.at(&Tok::RParen) {
                params.push(p.ty()?);
                if !p.eat(&Tok::Comma) {
                    break;
                }
            }
            p.expect(&Tok::RParen)?;
            Ok(params)
        })?;
        let ret = if self.eat(&Tok::Arrow) {
            Some(Box::new(self.ty()?))
        } else {
            None
        };
        Ok((params, ret))
    }

    /// `|a, mut b, c: T| body` / `|x| -> T { ... }` (after an optional `move`).
    fn closure(&mut self, is_move: bool) -> Result<Expr> {
        let start = self.span();
        let mut params = Vec::new();
        if !self.eat(&Tok::OrOr) {
            self.expect(&Tok::Pipe)?;
            self.with_nl(false, |p| {
                while !p.at(&Tok::Pipe) {
                    let mutable = p.eat_kw(Kw::Mut);
                    let name = match p.peek().clone() {
                        Tok::Underscore => {
                            let span = p.bump().span;
                            Ident {
                                name: "_".into(),
                                span,
                            }
                        }
                        _ => p.ident()?,
                    };
                    let ty = if p.eat(&Tok::Colon) {
                        Some(p.ty()?)
                    } else {
                        None
                    };
                    params.push(ClosureParam { name, mutable, ty });
                    if !p.eat(&Tok::Comma) {
                        break;
                    }
                }
                p.expect(&Tok::Pipe).map(|_| ())
            })?;
        }
        let ret = if self.eat(&Tok::Arrow) {
            Some(self.ty()?)
        } else {
            None
        };
        let body = if ret.is_some() {
            let b = self.block()?;
            let span = b.span;
            Expr {
                kind: ExprKind::Block(b),
                span,
            }
        } else {
            // `|| count += 1` is allowed: the body may be an assignment
            self.expr_stmt()?
        };
        let span = start.to(body.span);
        Ok(Expr {
            kind: ExprKind::Closure(Box::new(ClosureExpr {
                params,
                ret,
                body,
                is_move,
            })),
            span,
        })
    }

    /// Speculatively parse `<T, U>` in expression position; only succeeds
    /// when followed by `(`, `.` or `{`, otherwise rewinds (it was `<`).
    fn try_generic_args(&mut self) -> Option<Vec<TypeExpr>> {
        if !self.at(&Tok::Lt) {
            return None;
        }
        let save = self.pos;
        let saved_toks: Vec<Token> =
            self.toks[self.pos..(self.pos + 64).min(self.toks.len())].to_vec();
        let res = (|| -> Result<Vec<TypeExpr>> {
            self.bump();
            let mut args = Vec::new();
            self.with_nl(false, |p| {
                while !p.at(&Tok::Gt) {
                    args.push(p.ty()?);
                    if !p.eat(&Tok::Comma) {
                        break;
                    }
                }
                p.expect_gt()
            })?;
            Ok(args)
        })();
        match res {
            Ok(args)
                if matches!(self.raw(), Tok::LParen | Tok::Dot)
                    || (*self.raw() == Tok::LBrace && !self.no_struct) =>
            {
                Some(args)
            }
            _ => {
                // restore (expect_gt may have rewritten a token)
                for (i, t) in saved_toks.into_iter().enumerate() {
                    self.toks[save + i] = t;
                }
                self.pos = save;
                None
            }
        }
    }

    // ------------------------------------------------------------ statements

    pub fn block(&mut self) -> Result<Block> {
        let start = self.expect(&Tok::LBrace)?;
        let stmts = self.with_nl(true, |p| {
            let mut stmts = Vec::new();
            p.with_no_struct(false, |p| {
                loop {
                    p.skip_newlines();
                    if p.at(&Tok::RBrace) {
                        break;
                    }
                    if p.eat(&Tok::Semi) {
                        continue;
                    }
                    stmts.push(p.stmt()?);
                }
                Ok(())
            })?;
            Ok(stmts)
        })?;
        let end = self.expect(&Tok::RBrace)?;
        Ok(Block {
            stmts,
            span: start.to(end),
        })
    }

    fn stmt(&mut self) -> Result<Stmt> {
        let start = self.span();
        if self.eat_kw(Kw::Let) {
            let pat = self.pattern_let()?;
            let ty = if self.eat(&Tok::Colon) {
                Some(self.ty()?)
            } else {
                None
            };
            if !self.at(&Tok::Eq) {
                return self
                    .err("`let` requires an initializer: Xero has no uninitialized variables");
            }
            self.bump();
            self.skip_newlines();
            let init = self.expr()?;
            self.end_stmt()?;
            return Ok(Stmt::Let {
                pat,
                ty,
                init,
                span: start.to(self.prev_span()),
            });
        }
        if let Tok::Kw(
            k @ (Kw::Async
            | Kw::Spawn
            | Kw::Await
            | Kw::Own
            | Kw::Fn
            | Kw::Struct
            | Kw::Enum
            | Kw::Impl
            | Kw::Trait),
        ) = self.peek().clone()
        {
            let what = crate::lexer::kw_name(k);
            return self.err(match k {
                Kw::Async | Kw::Spawn | Kw::Await => format!("`{what}` is reserved for Xero's concurrency model and not yet implemented in this compiler version"),
                Kw::Own => "`own` is reserved; ownership is the default for every binding".to_string(),
                _ => format!("`{what}` items must be declared at module level"),
            });
        }
        let e = self.expr_stmt()?;
        let semi = self.end_stmt()?;
        Ok(Stmt::Expr(e, semi))
    }

    fn expr_stmt(&mut self) -> Result<Expr> {
        let lhs = self.expr()?;
        let op = match self.raw() {
            Tok::Eq => None,
            Tok::PlusEq => Some(BinOp::Add),
            Tok::MinusEq => Some(BinOp::Sub),
            Tok::StarEq => Some(BinOp::Mul),
            Tok::SlashEq => Some(BinOp::Div),
            Tok::PercentEq => Some(BinOp::Rem),
            Tok::AmpEq => Some(BinOp::BitAnd),
            Tok::PipeEq => Some(BinOp::BitOr),
            Tok::CaretEq => Some(BinOp::BitXor),
            Tok::ShlEq => Some(BinOp::Shl),
            Tok::ShrEq => Some(BinOp::Shr),
            _ => return Ok(lhs),
        };
        self.bump();
        self.skip_newlines();
        let rhs = self.expr()?;
        let span = lhs.span.to(rhs.span);
        let kind = match op {
            None => ExprKind::Assign(Box::new(lhs), Box::new(rhs)),
            Some(op) => ExprKind::CompoundAssign(op, Box::new(lhs), Box::new(rhs)),
        };
        Ok(Expr { kind, span })
    }

    // ----------------------------------------------------------- expressions

    pub fn expr(&mut self) -> Result<Expr> {
        let e = self.binary(0)?;
        if self.at_kw(Kw::Catch) {
            self.bump();
            let binding = if matches!(self.peek(), Tok::Ident(_)) {
                Some(self.ident()?)
            } else {
                None
            };
            let handler = self.block()?;
            let span = e.span.to(handler.span);
            return Ok(Expr {
                kind: ExprKind::Catch(Box::new(e), binding, handler),
                span,
            });
        }
        Ok(e)
    }

    fn binop_info(t: &Tok) -> Option<(BinOp, u8)> {
        Some(match t {
            Tok::OrOr => (BinOp::Or, 1),
            Tok::AndAnd => (BinOp::And, 2),
            Tok::EqEq => (BinOp::Eq, 3),
            Tok::Ne => (BinOp::Ne, 3),
            Tok::Lt => (BinOp::Lt, 3),
            Tok::Le => (BinOp::Le, 3),
            Tok::Gt => (BinOp::Gt, 3),
            Tok::Ge => (BinOp::Ge, 3),
            Tok::Pipe => (BinOp::BitOr, 5),
            Tok::Caret => (BinOp::BitXor, 6),
            Tok::Amp => (BinOp::BitAnd, 7),
            Tok::Shl => (BinOp::Shl, 8),
            Tok::Shr => (BinOp::Shr, 8),
            Tok::Plus => (BinOp::Add, 9),
            Tok::Minus => (BinOp::Sub, 9),
            Tok::Star => (BinOp::Mul, 10),
            Tok::Slash => (BinOp::Div, 10),
            Tok::Percent => (BinOp::Rem, 10),
            _ => return None,
        })
    }

    /// Precedence climbing. Level 4 is ranges (`a..b`), level 3 includes `is`.
    fn binary(&mut self, min: u8) -> Result<Expr> {
        let mut lhs = if self.at(&Tok::DotDot) || self.at(&Tok::DotDotEq) {
            self.range(None)?
        } else {
            self.cast_expr()?
        };
        loop {
            let t = self.peek().clone();
            if t == Tok::Kw(Kw::Is) && min <= 3 {
                self.bump();
                let pat = self.pattern()?;
                let span = lhs.span.to(pat.span);
                lhs = Expr {
                    kind: ExprKind::Is(Box::new(lhs), pat),
                    span,
                };
                continue;
            }
            if (t == Tok::DotDot || t == Tok::DotDotEq) && min <= 4 {
                lhs = self.range(Some(lhs))?;
                continue;
            }
            let Some((op, prec)) = Self::binop_info(&t) else {
                break;
            };
            if prec < min {
                break;
            }
            self.bump();
            self.skip_newlines();
            // comparisons are non-associative; everything else is left-assoc
            let rhs = self.binary(prec + 1)?;
            if op.is_comparison() {
                if let Some((op2, 3)) = Self::binop_info(self.peek()) {
                    return self.err(format!(
                        "comparison operators cannot be chained; use `&&` (found `{}` after `{}`)",
                        op2.symbol(),
                        op.symbol()
                    ));
                }
            }
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn range(&mut self, lo: Option<Expr>) -> Result<Expr> {
        let start = lo.as_ref().map(|e| e.span).unwrap_or_else(|| self.span());
        let inclusive = self.bump().tok == Tok::DotDotEq;
        let hi = match self.peek() {
            Tok::RParen | Tok::RBracket | Tok::LBrace | Tok::Comma | Tok::Newline | Tok::Semi => {
                None
            }
            _ => Some(Box::new(self.binary(5)?)),
        };
        let span = start.to(self.prev_span());
        Ok(Expr {
            kind: ExprKind::Range(lo.map(Box::new), hi, inclusive),
            span,
        })
    }

    fn unary(&mut self) -> Result<Expr> {
        let start = self.span();
        let mk = |p: &mut Self, kind: ExprKind| Expr {
            kind,
            span: start.to(p.prev_span()),
        };
        match self.peek().clone() {
            Tok::Minus => {
                self.bump();
                let e = self.unary()?;
                Ok(mk(self, ExprKind::Unary(UnOp::Neg, Box::new(e))))
            }
            Tok::Bang | Tok::Tilde => {
                self.bump();
                let e = self.unary()?;
                Ok(mk(self, ExprKind::Unary(UnOp::Not, Box::new(e))))
            }
            Tok::Star => {
                self.bump();
                let e = self.unary()?;
                Ok(mk(self, ExprKind::Deref(Box::new(e))))
            }
            Tok::Amp | Tok::AndAnd => {
                let double = self.at(&Tok::AndAnd);
                self.bump();
                let m = self.eat_kw(Kw::Mut);
                let e = self.unary()?;
                let inner = mk(self, ExprKind::Ref(m, Box::new(e)));
                Ok(if double {
                    mk(self, ExprKind::Ref(false, Box::new(inner)))
                } else {
                    inner
                })
            }
            Tok::Kw(Kw::Borrow) => {
                self.bump();
                let m = self.eat_kw(Kw::Mut);
                let e = self.unary()?;
                Ok(mk(self, ExprKind::Ref(m, Box::new(e))))
            }
            Tok::Kw(Kw::Move) if matches!(self.peek_at(1), Tok::Pipe | Tok::OrOr) => {
                self.bump();
                self.closure(true)
            }
            Tok::Pipe | Tok::OrOr => self.closure(false),
            Tok::Kw(Kw::Move) => {
                self.bump();
                let e = self.unary()?;
                Ok(mk(self, ExprKind::Move(Box::new(e))))
            }
            Tok::Kw(Kw::Copy) => {
                self.bump();
                let e = self.unary()?;
                Ok(mk(self, ExprKind::Copy(Box::new(e))))
            }
            Tok::Kw(Kw::Try) => {
                self.bump();
                let e = self.unary()?;
                Ok(mk(self, ExprKind::Try(Box::new(e))))
            }
            _ => self.postfix(),
        }
    }

    /// `unary (as Type)*`
    fn cast_expr(&mut self) -> Result<Expr> {
        let mut e = self.unary()?;
        while self.eat_kw(Kw::As) {
            let ty = self.ty()?;
            let span = e.span.to(ty.span);
            e = Expr {
                kind: ExprKind::Cast(Box::new(e), ty),
                span,
            };
        }
        Ok(e)
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            // allow method chains to continue on the next line: `\n .map(...)`
            if *self.raw() == Tok::Newline {
                let mut j = self.pos;
                while self.toks[j].tok == Tok::Newline {
                    j += 1;
                }
                if self.toks[j].tok == Tok::Dot {
                    self.pos = j;
                } else {
                    break;
                }
            }
            match self.peek().clone() {
                Tok::LParen => {
                    self.bump();
                    let args = self.with_nl(false, |p| {
                        p.with_no_struct(false, |p| {
                            let mut args = Vec::new();
                            while !p.at(&Tok::RParen) {
                                args.push(p.expr()?);
                                if !p.eat(&Tok::Comma) {
                                    break;
                                }
                            }
                            p.expect(&Tok::RParen)?;
                            Ok(args)
                        })
                    })?;
                    let span = e.span.to(self.prev_span());
                    e = Expr {
                        kind: ExprKind::Call(Box::new(e), args),
                        span,
                    };
                }
                Tok::LBracket => {
                    self.bump();
                    let idx = self.with_nl(false, |p| {
                        p.with_no_struct(false, |p| {
                            let i = p.expr()?;
                            p.expect(&Tok::RBracket)?;
                            Ok(i)
                        })
                    })?;
                    let span = e.span.to(self.prev_span());
                    e = Expr {
                        kind: ExprKind::Index(Box::new(e), Box::new(idx)),
                        span,
                    };
                }
                Tok::Dot => {
                    self.bump();
                    match self.peek().clone() {
                        Tok::Int(n, None) => {
                            let sp = self.bump().span;
                            let span = e.span.to(sp);
                            e = Expr { kind: ExprKind::TupleField(Box::new(e), n as u32, sp), span };
                        }
                        Tok::Kw(Kw::Await) => return self.err("`await` is reserved for Xero's concurrency model and not yet implemented"),
                        _ => {
                            let name = self.ident()?;
                            let args = self.try_generic_args().unwrap_or_default();
                            let span = e.span.to(self.prev_span());
                            e = Expr { kind: ExprKind::Field(Box::new(e), name, args), span };
                        }
                    }
                }
                Tok::LBrace if !self.no_struct && self.looks_like_struct_lit(&e) => {
                    e = self.struct_lit(e)?;
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn looks_like_struct_lit(&mut self, e: &Expr) -> bool {
        let is_path = |e: &Expr| -> bool {
            let mut cur = e;
            loop {
                match &cur.kind {
                    ExprKind::Name(..) | ExprKind::SelfType(_) => return true,
                    ExprKind::Field(inner, ..) => cur = inner,
                    _ => return false,
                }
            }
        };
        if !is_path(e) {
            return false;
        }
        let mut j = self.pos + 1;
        while self.toks[j].tok == Tok::Newline {
            j += 1;
        }
        match &self.toks[j].tok {
            Tok::RBrace => true,
            Tok::Ident(_) => self.toks[j + 1].tok == Tok::Colon,
            _ => false,
        }
    }

    fn struct_lit(&mut self, path: Expr) -> Result<Expr> {
        let mut fields = Vec::new();
        self.brace_list(|p| {
            let name = p.ident()?;
            p.expect(&Tok::Colon)?;
            let v = p.with_no_struct(false, |p| p.expr())?;
            fields.push((name, v));
            Ok(())
        })?;
        let span = path.span.to(self.prev_span());
        Ok(Expr {
            kind: ExprKind::StructLit(Box::new(path), fields),
            span,
        })
    }

    fn primary(&mut self) -> Result<Expr> {
        let start = self.span();
        let t = self.peek().clone();
        let kind = match t {
            Tok::Int(v, s) => {
                self.bump();
                ExprKind::Int(v, s)
            }
            Tok::Float(v, s) => {
                self.bump();
                ExprKind::Float(v, s)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::Char(c) => {
                self.bump();
                ExprKind::Char(c)
            }
            Tok::Kw(Kw::True) => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::Kw(Kw::False) => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Ident(_) => {
                let id = self.ident()?;
                let args = self.try_generic_args().unwrap_or_default();
                ExprKind::Name(id, args)
            }
            Tok::Kw(Kw::SelfValue) => {
                self.bump();
                ExprKind::SelfValue
            }
            Tok::Kw(Kw::SelfType) => {
                self.bump();
                let args = self.try_generic_args().unwrap_or_default();
                ExprKind::SelfType(args)
            }
            Tok::LParen => {
                self.bump();
                let (elems, trailing_comma) = self.with_nl(false, |p| {
                    p.with_no_struct(false, |p| {
                        let mut elems = Vec::new();
                        let mut trailing = false;
                        while !p.at(&Tok::RParen) {
                            elems.push(p.expr()?);
                            trailing = false;
                            if !p.eat(&Tok::Comma) {
                                break;
                            }
                            trailing = true;
                        }
                        p.expect(&Tok::RParen)?;
                        Ok((elems, trailing))
                    })
                })?;
                if elems.is_empty() {
                    ExprKind::Unit
                } else if elems.len() == 1 && !trailing_comma {
                    let mut e = elems.into_iter().next().unwrap();
                    e.span = start.to(self.prev_span());
                    return Ok(e);
                } else {
                    ExprKind::Tuple(elems)
                }
            }
            Tok::LBracket => {
                self.bump();
                self.with_nl(false, |p| {
                    p.with_no_struct(false, |p| {
                        if p.eat(&Tok::RBracket) {
                            return Ok(ExprKind::Array(Vec::new()));
                        }
                        let first = p.expr()?;
                        if p.eat(&Tok::Semi) {
                            let n = p.expr()?;
                            p.expect(&Tok::RBracket)?;
                            return Ok(ExprKind::ArrayRepeat(Box::new(first), Box::new(n)));
                        }
                        let mut elems = vec![first];
                        while p.eat(&Tok::Comma) {
                            if p.at(&Tok::RBracket) {
                                break;
                            }
                            elems.push(p.expr()?);
                        }
                        p.expect(&Tok::RBracket)?;
                        Ok(ExprKind::Array(elems))
                    })
                })?
            }
            Tok::LBrace => ExprKind::Block(self.block()?),
            Tok::Kw(Kw::Unsafe) => {
                self.bump();
                ExprKind::Unsafe(self.block()?)
            }
            Tok::Kw(Kw::If) => return self.if_expr(),
            Tok::Kw(Kw::While) => {
                self.bump();
                let cond = self.with_no_struct(true, |p| p.expr())?;
                let body = self.block()?;
                ExprKind::While(Box::new(cond), body)
            }
            Tok::Kw(Kw::Loop) => {
                self.bump();
                ExprKind::Loop(self.block()?)
            }
            Tok::Kw(Kw::For) => {
                self.bump();
                let pat = self.pattern_let()?;
                self.expect_kw(Kw::In)?;
                let iter = self.with_no_struct(true, |p| p.expr())?;
                let body = self.block()?;
                ExprKind::For(pat, Box::new(iter), body)
            }
            Tok::Kw(Kw::Match) => {
                self.bump();
                let scrut = self.with_no_struct(true, |p| p.expr())?;
                let mut arms = Vec::new();
                self.expect(&Tok::LBrace)?;
                self.with_nl(true, |p| {
                    p.with_no_struct(false, |p| {
                        loop {
                            p.skip_newlines();
                            if p.at(&Tok::RBrace) {
                                break;
                            }
                            let pat = p.pattern()?;
                            let guard = if p.eat_kw(Kw::If) { Some(p.expr()?) } else { None };
                            p.expect(&Tok::FatArrow)?;
                            p.skip_newlines();
                            let body = p.expr_stmt()?;
                            let span = pat.span.to(body.span);
                            arms.push(Arm { pat, guard, body, span });
                            match p.raw() {
                                Tok::Comma | Tok::Semi | Tok::Newline => {
                                    p.pos += 1;
                                }
                                Tok::RBrace => {}
                                t => {
                                    let d = t.describe();
                                    return p.err(format!("expected `,` or newline after match arm, found {d}"));
                                }
                            }
                        }
                        Ok(())
                    })
                })?;
                self.expect(&Tok::RBrace)?;
                ExprKind::Match(Box::new(scrut), arms)
            }
            Tok::Kw(Kw::Break) => {
                self.bump();
                let v = if self.value_follows() { Some(Box::new(self.expr()?)) } else { None };
                ExprKind::Break(v)
            }
            Tok::Kw(Kw::Continue) => {
                self.bump();
                ExprKind::Continue
            }
            Tok::Kw(Kw::Return) => {
                self.bump();
                let v = if self.value_follows() { Some(Box::new(self.expr()?)) } else { None };
                ExprKind::Return(v)
            }
            Tok::Kw(Kw::Throw) => {
                self.bump();
                ExprKind::Throw(Box::new(self.expr()?))
            }
            Tok::Kw(Kw::Drop) => {
                self.bump();
                ExprKind::Drop(Box::new(self.unary()?))
            }
            Tok::Kw(k @ (Kw::Async | Kw::Spawn | Kw::Await)) => {
                return self.err(format!(
                    "`{}` is reserved for Xero's concurrency model and not yet implemented in this compiler version",
                    crate::lexer::kw_name(k)
                ))
            }
            t => return self.err(format!("expected an expression, found {}", t.describe())),
        };
        Ok(Expr {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    fn value_follows(&mut self) -> bool {
        !matches!(
            self.raw(),
            Tok::Newline | Tok::Semi | Tok::RBrace | Tok::RParen | Tok::Comma | Tok::Eof
        )
    }

    fn if_expr(&mut self) -> Result<Expr> {
        let start = self.expect_kw(Kw::If)?;
        let cond = self.with_no_struct(true, |p| p.expr())?;
        let then = self.block()?;
        // `else` may appear on the next line
        let mut j = self.pos;
        while self.toks[j].tok == Tok::Newline {
            j += 1;
        }
        let els = if self.toks[j].tok == Tok::Kw(Kw::Else) {
            self.pos = j + 1;
            if self.at_kw(Kw::If) {
                Some(Box::new(self.if_expr()?))
            } else {
                let b = self.block()?;
                let span = b.span;
                Some(Box::new(Expr {
                    kind: ExprKind::Block(b),
                    span,
                }))
            }
        } else {
            None
        };
        Ok(Expr {
            kind: ExprKind::If(Box::new(cond), then, els),
            span: start.to(self.prev_span()),
        })
    }

    // -------------------------------------------------------------- patterns

    /// Patterns allowed in `let` and `for`: bindings, `_`, tuples.
    fn pattern_let(&mut self) -> Result<Pattern> {
        let p = self.pattern()?;
        fn check(p: &Pattern) -> Result<()> {
            match &p.kind {
                PatKind::Wild | PatKind::Bind(..) => Ok(()),
                PatKind::Tuple(ps) => ps.iter().try_for_each(check),
                _ => Err(Diagnostic::new(
                    p.span,
                    "refutable pattern not allowed here; use `match` or `if ... is ...`",
                )),
            }
        }
        check(&p)?;
        Ok(p)
    }

    pub fn pattern(&mut self) -> Result<Pattern> {
        let first = self.pattern_single()?;
        if !self.at(&Tok::Pipe) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat(&Tok::Pipe) {
            self.skip_newlines();
            alts.push(self.pattern_single()?);
        }
        let span = alts[0].span.to(alts.last().unwrap().span);
        Ok(Pattern {
            kind: PatKind::Or(alts),
            span,
        })
    }

    fn pattern_single(&mut self) -> Result<Pattern> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Underscore => {
                self.bump();
                PatKind::Wild
            }
            Tok::Kw(Kw::Mut) => {
                self.bump();
                PatKind::Bind(self.ident()?, true)
            }
            Tok::LParen => {
                self.bump();
                let elems = self.with_nl(false, |p| {
                    let mut elems = Vec::new();
                    while !p.at(&Tok::RParen) {
                        elems.push(p.pattern()?);
                        if !p.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    p.expect(&Tok::RParen)?;
                    Ok(elems)
                })?;
                if elems.len() == 1 {
                    return Ok(elems.into_iter().next().unwrap());
                }
                PatKind::Tuple(elems)
            }
            Tok::Int(..)
            | Tok::Float(..)
            | Tok::Char(_)
            | Tok::Str(_)
            | Tok::Minus
            | Tok::Kw(Kw::True | Kw::False) => {
                let lo = self.literal_expr()?;
                if self.at(&Tok::DotDot) || self.at(&Tok::DotDotEq) {
                    let inclusive = self.bump().tok == Tok::DotDotEq;
                    let hi = self.literal_expr()?;
                    PatKind::Range(Box::new(lo), Box::new(hi), inclusive)
                } else {
                    PatKind::Lit(Box::new(lo))
                }
            }
            Tok::Ident(_) | Tok::Kw(Kw::SelfType) => {
                let mut path = vec![if self.at_kw(Kw::SelfType) {
                    let span = self.bump().span;
                    Ident {
                        name: "Self".into(),
                        span,
                    }
                } else {
                    self.ident()?
                }];
                while self.at(&Tok::Dot) {
                    self.bump();
                    path.push(self.ident()?);
                }
                if self.at(&Tok::LParen) {
                    self.bump();
                    let args = self.with_nl(false, |p| {
                        let mut args = Vec::new();
                        while !p.at(&Tok::RParen) {
                            args.push(p.pattern()?);
                            if !p.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        p.expect(&Tok::RParen)?;
                        Ok(args)
                    })?;
                    PatKind::Variant(path, Some(args))
                } else if path.len() == 1 {
                    PatKind::Bind(path.pop().unwrap(), false)
                } else {
                    PatKind::Variant(path, None)
                }
            }
            t => return self.err(format!("expected a pattern, found {}", t.describe())),
        };
        Ok(Pattern {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    fn literal_expr(&mut self) -> Result<Expr> {
        let start = self.span();
        let neg = self.eat(&Tok::Minus);
        let kind = match self.bump().tok {
            Tok::Int(v, s) => ExprKind::Int(v, s),
            Tok::Float(v, s) => ExprKind::Float(v, s),
            Tok::Char(c) => ExprKind::Char(c),
            Tok::Str(s) if !neg => ExprKind::Str(s),
            Tok::Kw(Kw::True) if !neg => ExprKind::Bool(true),
            Tok::Kw(Kw::False) if !neg => ExprKind::Bool(false),
            _ => return Err(Diagnostic::new(start, "expected a literal pattern")),
        };
        let lit = Expr {
            kind,
            span: start.to(self.prev_span()),
        };
        if neg {
            return Ok(Expr {
                kind: ExprKind::Unary(UnOp::Neg, Box::new(lit)),
                span: start.to(self.prev_span()),
            });
        }
        Ok(lit)
    }
}
