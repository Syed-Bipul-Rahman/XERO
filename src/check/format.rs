//! Format strings: `format("{} of {}", a, b)`, and `print`/`println`
//! (`eprint`/`eprintln`) with arguments. The string is parsed here, at
//! compile time, and the call becomes code that appends each piece to a
//! `String`; nothing is parsed at run time.

use super::expr::{FnCx, X};
use super::Def;
use crate::ast::{self, ExprKind as E, Ident, PatKind, Pattern, Stmt};
use crate::diag::{Diagnostic, Result, Span};

/// One `{...}` of a format string.
struct Spec {
    arg: usize,
    fill: u8,
    /// `<`, `>`, `^`, or 0 for the default (numbers right, others left).
    align: u8,
    zero: bool,
    width: Option<usize>,
    precision: Option<usize>,
    /// 0 for `Display`, else 2, 8 or 16.
    radix: u32,
    upper: bool,
}

enum Piece {
    Text(String),
    Arg(Spec),
}

fn parse(fmt: &str, nargs: usize, span: Span) -> Result<Vec<Piece>> {
    let err = |m: String| Err(Diagnostic::new(span, m));
    let b = fmt.as_bytes();
    let mut out = Vec::new();
    let mut text = String::new();
    let mut next = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' if b.get(i + 1) == Some(&b'{') => {
                text.push('{');
                i += 2;
            }
            b'}' if b.get(i + 1) == Some(&b'}') => {
                text.push('}');
                i += 2;
            }
            b'}' => {
                return err(
                    "unmatched `}` in format string (write `}}` for a literal brace)".into(),
                )
            }
            b'{' => {
                let Some(len) = fmt[i + 1..].find('}') else {
                    return err(
                        "unclosed `{` in format string (write `{{` for a literal brace)".into(),
                    );
                };
                let inner = &fmt[i + 1..i + 1 + len];
                i += len + 2;
                let (pos, spec) = match inner.split_once(':') {
                    Some((p, s)) => (p, s),
                    None => (inner, ""),
                };
                let arg = if pos.is_empty() {
                    next += 1;
                    next - 1
                } else {
                    match pos.parse::<usize>() {
                        Ok(n) => n,
                        Err(_) => {
                            return err(format!(
                                "`{{{inner}}}`: expected an argument number, like `{{0}}`"
                            ))
                        }
                    }
                };
                if arg >= nargs {
                    return err(format!(
                        "the format string refers to argument {} but there {} {nargs}",
                        arg + 1,
                        if nargs == 1 { "is" } else { "are" }
                    ));
                }
                let mut s = Spec {
                    arg,
                    fill: b' ',
                    align: 0,
                    zero: false,
                    width: None,
                    precision: None,
                    radix: 0,
                    upper: false,
                };
                let sb = spec.as_bytes();
                let mut j = 0;
                let is_align = |c: u8| c == b'<' || c == b'>' || c == b'^';
                if sb.len() >= 2 && is_align(sb[1]) {
                    s.fill = sb[0];
                    s.align = sb[1];
                    j = 2;
                } else if !sb.is_empty() && is_align(sb[0]) {
                    s.align = sb[0];
                    j = 1;
                }
                if sb.get(j) == Some(&b'0') {
                    s.zero = true;
                    j += 1;
                }
                let start = j;
                while j < sb.len() && sb[j].is_ascii_digit() {
                    j += 1;
                }
                if j > start {
                    s.width = Some(spec[start..j].parse().unwrap());
                }
                if sb.get(j) == Some(&b'.') {
                    j += 1;
                    let start = j;
                    while j < sb.len() && sb[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j == start {
                        return err(format!("`{{{inner}}}`: expected digits after `.`"));
                    }
                    s.precision = Some(spec[start..j].parse().unwrap());
                }
                match &spec[j..] {
                    "" => {}
                    "x" => s.radix = 16,
                    "X" => {
                        s.radix = 16;
                        s.upper = true
                    }
                    "b" => s.radix = 2,
                    "o" => s.radix = 8,
                    t => {
                        return err(format!(
                            "`{{{inner}}}`: unknown format `{t}` (use `x`, `X`, `b` or `o`, a width like `5`, or a precision like `.2`)"
                        ))
                    }
                }
                if !text.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut text)));
                }
                out.push(Piece::Arg(s));
            }
            _ => {
                // copy the UTF-8 sequence
                let start = i;
                i += 1;
                while i < b.len() && (b[i] & 0xC0) == 0x80 {
                    i += 1;
                }
                text.push_str(&fmt[start..i]);
            }
        }
    }
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    Ok(out)
}

impl FnCx<'_> {
    /// If `callee(args)` is a formatting call, check it as one.
    pub fn format_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<Option<X>> {
        let E::Name(id, targs) = &callee.kind else {
            return Ok(None);
        };
        let name = id.name.as_str();
        if !targs.is_empty()
            || !matches!(name, "format" | "print" | "println" | "eprint" | "eprintln")
        {
            return Ok(None);
        }
        // only the prelude's functions, not a local or a function of the same name
        if self.lookup_local(name).is_some() || self.is_capture(name) {
            return Ok(None);
        }
        let Some(prelude) = self.ck.prelude else {
            return Ok(None);
        };
        let is_prelude = match self.ck.lookup(self.module, name) {
            Some(Def::Fn(fid)) => self.ck.fns[fid as usize].module == prelude,
            _ => false,
        };
        if !is_prelude || args.is_empty() {
            return Ok(None);
        }
        let E::Str(lit) = &args[0].kind else {
            if args.len() > 1 {
                return Err(Diagnostic::new(args[0].span, format!("`{name}` with arguments needs a string literal first: `{name}(\"{{}} and {{}}\", a, b)`")));
            }
            return Ok(None);
        };
        let text = String::from_utf8(lit.clone())
            .map_err(|_| Diagnostic::new(args[0].span, "format string is not valid UTF-8"))?;
        if args.len() == 1 && (name != "format" || (!text.contains('{') && !text.contains('}'))) {
            return Ok(None);
        }
        let pieces = parse(&text, args.len() - 1, args[0].span)?;
        let used: Vec<usize> = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Arg(s) => Some(s.arg),
                _ => None,
            })
            .collect();
        for k in 0..args.len() - 1 {
            if !used.contains(&k) {
                return Err(Diagnostic::new(
                    args[k + 1].span,
                    "this argument is not used by the format string",
                ));
            }
        }
        let block = self.format_block(&pieces, &args[1..], name, span);
        self.expr(&block, None).map(Some)
    }

    /// `{ let a0 = &(arg0); ...; let mut out = String.new(); ...; out }`,
    /// then printed for `print`/`println`.
    fn format_block(
        &self,
        pieces: &[Piece],
        args: &[ast::Expr],
        name: &str,
        span: Span,
    ) -> ast::Expr {
        let mk = |kind| ast::Expr { kind, span };
        let id = |n: &str| Ident {
            name: n.to_string(),
            span,
        };
        let var = |n: &str| mk(E::Name(id(n), Vec::new()));
        let call = |f: &str, a: Vec<ast::Expr>| mk(E::Call(Box::new(var(f)), a));
        let int = |v: u128| mk(E::Int(v, None));
        let let_ = |n: &str, mutable: bool, init: ast::Expr| Stmt::Let {
            pat: Pattern {
                kind: PatKind::Bind(id(n), mutable),
                span,
            },
            ty: None,
            init,
            span,
        };
        let out_mut = || mk(E::Ref(true, Box::new(var("fmt#out"))));
        let mut stmts = Vec::new();
        for (k, a) in args.iter().enumerate() {
            stmts.push(let_(
                &format!("fmt#a{k}"),
                false,
                mk(E::Ref(false, Box::new(a.clone()))),
            ));
        }
        let new_string = mk(E::Call(
            Box::new(mk(E::Field(Box::new(var("String")), id("new"), Vec::new()))),
            Vec::new(),
        ));
        stmts.push(let_("fmt#out", true, new_string));
        for p in pieces {
            match p {
                Piece::Text(t) => stmts.push(Stmt::Expr(
                    call(
                        "fmt__piece",
                        vec![out_mut(), mk(E::Str(t.clone().into_bytes()))],
                    ),
                    true,
                )),
                Piece::Arg(s) => {
                    let a = var(&format!("fmt#a{}", s.arg));
                    if s.width.is_some() {
                        let len = mk(E::Call(
                            Box::new(mk(E::Field(
                                Box::new(var("fmt#out")),
                                id("len"),
                                Vec::new(),
                            ))),
                            Vec::new(),
                        ));
                        stmts.push(let_("fmt#start", false, len));
                    }
                    let write = if s.radix != 0 {
                        call(
                            "fmt__radix",
                            vec![a, int(s.radix as u128), mk(E::Bool(s.upper)), out_mut()],
                        )
                    } else if let Some(pr) = s.precision {
                        call("fmt__float", vec![a, int(pr as u128), out_mut()])
                    } else {
                        call("fmt__display", vec![a, out_mut()])
                    };
                    stmts.push(Stmt::Expr(write, true));
                    if let Some(w) = s.width {
                        // numbers align right by default, everything else left
                        let numeric = s.radix != 0 || s.precision.is_some() || s.zero;
                        let align = match s.align {
                            0 if numeric => b'>',
                            0 => b'<',
                            c => c,
                        };
                        stmts.push(Stmt::Expr(
                            call(
                                "fmt__pad_auto",
                                vec![
                                    out_mut(),
                                    var("fmt#start"),
                                    int(w as u128),
                                    mk(E::Char(align)),
                                    mk(E::Char(s.fill)),
                                    mk(E::Bool(s.zero)),
                                    var(&format!("fmt#a{}", s.arg)),
                                    mk(E::Bool(s.align == 0)),
                                ],
                            ),
                            true,
                        ));
                    }
                }
            }
        }
        if name == "format" {
            stmts.push(Stmt::Expr(var("fmt#out"), false));
        } else {
            let printed = mk(E::Ref(false, Box::new(var("fmt#out"))));
            stmts.push(Stmt::Expr(call(name, vec![printed]), false));
        }
        mk(E::Block(ast::Block { stmts, span }))
    }
}
