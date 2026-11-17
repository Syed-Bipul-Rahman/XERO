//! `xeroc fmt`: the canonical layout of Xero source.
//!
//! Xero's line breaks are part of its syntax, so the formatter keeps them
//! (and the comments) where the author put them, and normalizes the rest:
//! indentation (four spaces per open `{`, `(` or `[`, one level per line),
//! the spacing around operators and punctuation, runs of blank lines
//! (at most one, none at the start of a block), trailing whitespace and the
//! final newline. Where a token's role depends on context (`<` and `>` in
//! generics or comparisons, unary `-`, `*` and `&`, closure bars, ranges),
//! the author's spacing is kept. The result is checked to have exactly the
//! same tokens as the input, so formatting never changes a program.

use crate::lexer::{self, punct_str, Kw, Tok, Token};

/// Format `src`; `Err` if it does not lex, or (never expected) if the
/// result would not mean the same.
pub fn format_source(src: &str) -> Result<String, String> {
    let toks = lexer::lex(0, src).map_err(|d| d.message)?;
    let out = layout(src, &toks);
    // the safety net: same tokens, same line breaks
    let again =
        lexer::lex(0, &out).map_err(|d| format!("formatting broke the file: {}", d.message))?;
    if !same_tokens(&toks, &again, src, &out) {
        return Err(
            "internal error: formatting would change the program; the file was left alone".into(),
        );
    }
    Ok(out)
}

fn same_tokens(a: &[Token], b: &[Token], sa: &str, sb: &str) -> bool {
    let key = |t: &Token, s: &str| -> String {
        match &t.tok {
            Tok::Newline => "\n".into(),
            _ => s[t.span.lo as usize..t.span.hi as usize].to_string(),
        }
    };
    let ka: Vec<String> = a.iter().map(|t| key(t, sa)).collect();
    let kb: Vec<String> = b.iter().map(|t| key(t, sb)).collect();
    ka == kb
}

/// One source token with the original text, and what preceded it.
struct Item<'a> {
    tok: &'a Tok,
    text: &'a str,
    /// Line breaks before it (2+ means a blank line).
    breaks: usize,
    /// Whitespace directly before it on the same line.
    spaced: bool,
    /// Comments between the previous token and this one.
    comments: Vec<Comment>,
}

fn layout(src: &str, toks: &[Token]) -> String {
    let mut items: Vec<Item> = Vec::new();
    let mut prev_end = 0usize;
    for t in toks {
        if matches!(t.tok, Tok::Newline) {
            continue;
        }
        let lo = t.span.lo as usize;
        let hi = t.span.hi as usize;
        let gap = &src[prev_end.min(lo)..lo];
        let (breaks, spaced, comments) = scan_gap(gap, prev_end == 0);
        items.push(Item {
            tok: &t.tok,
            text: &src[lo..hi],
            breaks,
            spaced,
            comments,
        });
        prev_end = hi;
    }
    // comments after the last token
    let tail = &src[prev_end.min(src.len())..];
    let (_, _, tail_comments) = scan_gap(tail, prev_end == 0);

    let mut out = String::new();
    let mut line = String::new();
    // per open delimiter: the indent of the line that opened it
    let mut opens: Vec<(usize, u8)> = Vec::new();
    // per open `{`: whether it is an import list (`.{a, b}`), kept tight
    let mut braces: Vec<bool> = Vec::new();
    let mut indent = 0usize;
    let mut line_started = false;
    // (lines never get trailing spaces: spaces only go before tokens, and
    // comments are kept exactly as written)
    let flush = |out: &mut String, line: &mut String| {
        out.push_str(line);
        out.push('\n');
        line.clear();
    };
    let mut prev: Option<usize> = None;
    for i in 0..items.len() {
        let it = &items[i];
        // comments before this token
        let mut after_inline_block = false;
        for c in &it.comments {
            if c.own_line || !line_started {
                if line_started {
                    flush(&mut out, &mut line);
                    line_started = false;
                }
                let after_open = prev.is_some_and(|p| matches!(items[p].tok, Tok::LBrace));
                if c.blank_before && !after_open && !out.is_empty() {
                    out.push('\n');
                }
                let ind = cur_indent(&opens, it.tok);
                line.push_str(&"    ".repeat(ind));
                line.push_str(&c.text);
                if c.text.starts_with("/*") && it.breaks == 0 {
                    // `/* note */ code` on one line
                    line_started = true;
                    after_inline_block = true;
                } else {
                    flush(&mut out, &mut line);
                }
            } else {
                line.push_str(&" ".repeat(c.pad));
                line.push_str(&c.text);
                if c.text.starts_with("/*") && it.breaks == 0 {
                    after_inline_block = true;
                } else {
                    flush(&mut out, &mut line);
                    line_started = false;
                }
            }
        }
        let new_line = it.breaks > 0 || prev.is_none();
        if new_line && line_started {
            flush(&mut out, &mut line);
            line_started = false;
        }
        if !line_started {
            // blank lines: at most one, none right after an opening brace
            // or before a closing one
            let after_open =
                prev.is_some_and(|p| matches!(items[p].tok, Tok::LBrace)) && it.comments.is_empty();
            let before_close = matches!(it.tok, Tok::RBrace);
            if it.breaks >= 2 && !after_open && !before_close && !out.is_empty() {
                out.push('\n');
            }
            indent = cur_indent(&opens, it.tok);
            // a line continuing an expression is indented one more level
            let continues = prev.is_some_and(|p| ends_expression_open(items[p].tok))
                || matches!(it.tok, Tok::Dot);
            if continues && cont_allowed(&opens) {
                indent += 1;
            }
            line.push_str(&"    ".repeat(indent));
            line.push_str(it.text);
            line_started = true;
        } else {
            let pi = prev.unwrap();
            let p = &items[pi];
            // the token before `p`, if on the same line
            let pp = if p.breaks == 0 && pi > 0 {
                Some(&items[pi - 1])
            } else {
                None
            };
            let tight = braces.last().copied().unwrap_or(false);
            if after_inline_block || space_between(p, it, pp, tight) {
                line.push(' ');
            }
            line.push_str(it.text);
        }
        // delimiters
        match it.tok {
            Tok::LBrace | Tok::LParen | Tok::LBracket => opens.push((indent, 0)),
            Tok::RBrace | Tok::RParen | Tok::RBracket => {
                opens.pop();
            }
            _ => {}
        }
        match it.tok {
            Tok::LBrace => {
                let after_dot = prev.is_some_and(|p| matches!(items[p].tok, Tok::Dot));
                braces.push(after_dot)
            }
            Tok::RBrace => {
                braces.pop();
            }
            _ => {}
        }
        prev = Some(i);
    }
    if line_started {
        flush(&mut out, &mut line);
    }
    for c in &tail_comments {
        if c.blank_before && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&c.text);
        out.push('\n');
    }
    // no blank lines at the end
    while out.ends_with("\n\n") {
        out.pop();
    }
    if out.is_empty() {
        return out;
    }
    out
}

/// The indent of a line starting with `first`.
fn cur_indent(opens: &[(usize, u8)], first: &Tok) -> usize {
    match opens.last() {
        None => 0,
        Some(&(ind, _)) => {
            if matches!(first, Tok::RBrace | Tok::RParen | Tok::RBracket) {
                ind
            } else {
                ind + 1
            }
        }
    }
}

fn cont_allowed(_opens: &[(usize, u8)]) -> bool {
    true
}

/// Does a line ending with `t` continue on the next one? (A binary
/// operator or `=` at the end of a line.)
fn ends_expression_open(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Eq
            | Tok::PlusEq
            | Tok::MinusEq
            | Tok::StarEq
            | Tok::SlashEq
            | Tok::PercentEq
            | Tok::Plus
            | Tok::Minus
            | Tok::Star
            | Tok::Slash
            | Tok::Percent
            | Tok::AndAnd
            | Tok::OrOr
            | Tok::EqEq
            | Tok::Ne
            | Tok::Arrow
            | Tok::FatArrow
    )
}

/// A comment found between two tokens.
struct Comment {
    text: String,
    /// It starts its own line.
    own_line: bool,
    /// A blank line comes before it.
    blank_before: bool,
    /// Spaces before it, when it follows code on the same line (kept, so
    /// aligned comments stay aligned).
    pad: usize,
}

/// Line breaks (after the last comment), same-line whitespace and the
/// comments in the text between two tokens.
fn scan_gap(gap: &str, at_start: bool) -> (usize, bool, Vec<Comment>) {
    let b = gap.as_bytes();
    let mut breaks = 0;
    let mut comments: Vec<Comment> = Vec::new();
    let mut i = 0;
    let mut spaced = false;
    let mut pad = 0;
    while i < b.len() {
        match b[i] {
            b'\n' => {
                breaks += 1;
                spaced = false;
                pad = 0;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => {
                spaced = true;
                if b[i] != b'\r' {
                    pad += if b[i] == b'\t' { 4 } else { 1 };
                }
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') || b.get(i + 1) == Some(&b'*') => {
                let start = i;
                if b[i + 1] == b'/' {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                } else {
                    let mut depth = 0;
                    while i + 1 < b.len() {
                        if b[i] == b'/' && b[i + 1] == b'*' {
                            depth += 1;
                            i += 2;
                        } else if b[i] == b'*' && b[i + 1] == b'/' {
                            depth -= 1;
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        } else {
                            i += 1;
                        }
                    }
                }
                comments.push(Comment {
                    // the text itself is never changed (only a CR before the newline)
                    text: gap[start..i].trim_end_matches('\r').to_string(),
                    own_line: breaks > 0 || (at_start && comments.is_empty()),
                    blank_before: breaks >= 2,
                    pad: pad.max(1),
                });
                breaks = 0;
                pad = 0;
                spaced = true;
            }
            _ => i += 1,
        }
    }
    (breaks, spaced, comments)
}

/// Does the token look like the end of a value (so a following `-`, `*`,
/// `&` or `(` is binary or a call)?
fn ends_value(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Ident(_)
            | Tok::Int(..)
            | Tok::Float(..)
            | Tok::Str(_)
            | Tok::Char(_)
            | Tok::RParen
            | Tok::RBracket
            | Tok::Question
            | Tok::Kw(Kw::True | Kw::False | Kw::SelfValue | Kw::SelfType | Kw::Await)
    )
}

/// Should there be a space between `p` and `n` on one line? (`pp` is the
/// token before `p`.)
fn space_between(p: &Item, n: &Item, pp: Option<&Item>, tight_braces: bool) -> bool {
    use Tok::*;
    let keep = p.spaced_after(n);
    // `-`, `*`, `&`, `&&` (a double reference) and `!` are prefix
    // operators unless a value comes before them
    let unary = |t: &Tok| {
        matches!(t, Minus | Star | Amp | AndAnd) && !pp.is_some_and(|q| ends_value(q.tok))
            || matches!(t, Bang)
    };
    match (p.tok, n.tok) {
        // never a space before these
        (_, Comma | Semi | RParen | RBracket | Question) => false,
        (_, Dot) => false,
        (Dot, _) => false,
        (LParen | LBracket, _) => false,
        (_, Colon) => false,
        (Colon | Comma | Semi, _) => true,
        // braces: `{}`, `{ a }`, but an import list stays tight: `.{Box}`
        (LBrace, RBrace) => false,
        (LBrace, _) | (_, RBrace) => !tight_braces,
        (_, LBrace) => !matches!(p.tok, LParen | LBracket),
        // prefix operators stick to their operand: `&x`, `*p`, `-1`, `!ok`
        (t, _) if unary(t) => false,
        // `fn(i32) -> i32` (a function type), `f<T>(x)`
        (Tok::Kw(crate::lexer::Kw::Fn), LParen) => false,
        (Gt | Shr, LParen) => keep,
        // calls and indexing: `f(x)`, `a[i]`
        (_, LParen | LBracket) if ends_value(p.tok) => false,
        (Lt | Pipe | DotDot | DotDotEq | At | Hash, LParen | LBracket) => keep,
        (_, LParen | LBracket) => true,
        // `>=` and `>>=` can also close generics (`Vec<i32>>= v`): as written
        (Ident(_) | Gt | Shr, Ge | ShrEq) => keep,
        // operators that are always binary
        (
            _,
            Eq | EqEq | Ne | Le | Ge | PlusEq | MinusEq | StarEq | SlashEq | PercentEq | AmpEq
            | PipeEq | CaretEq | ShlEq | ShrEq | Arrow | FatArrow | AndAnd | OrOr | Plus | Slash
            | Percent | Caret | Shl,
        ) => true,
        (
            Eq | EqEq | Ne | Le | Ge | PlusEq | MinusEq | StarEq | SlashEq | PercentEq | AmpEq
            | PipeEq | CaretEq | ShlEq | ShrEq | Arrow | FatArrow | AndAnd | OrOr | Plus | Slash
            | Percent | Caret | Shl,
            _,
        ) => true,
        // binary `-`, `*`, `&`
        (_, Minus | Star | Amp) if ends_value(p.tok) => true,
        (Minus | Star | Amp, _) => true,
        // keywords and names are separated
        (
            Kw(_) | Ident(_) | Int(..) | Float(..) | Str(_) | Char(_) | Underscore,
            Kw(_) | Ident(_) | Int(..) | Float(..) | Str(_) | Char(_) | Underscore,
        ) => true,
        (RParen | RBracket, Kw(_) | Ident(_)) => true,
        // everything else (`<`, `>`, `|`, `..`, `#`, `@`, ...) as written
        _ => keep,
    }
}

impl Item<'_> {
    /// Was there whitespace between this token and `next` in the source?
    fn spaced_after(&self, next: &Item) -> bool {
        next.spaced || next.breaks > 0
    }
}

#[allow(dead_code)]
fn tok_text(t: &Tok) -> &'static str {
    punct_str(t)
}
