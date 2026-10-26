//! Lexer: source text -> tokens. Newlines are significant (they terminate
//! statements), so they are emitted as tokens and the parser decides where
//! to skip them.

use crate::diag::{Diagnostic, Result, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kw {
    Module, Import, Export, Pub, Private,
    Let, Mut, Const, Static,
    Fn, Return,
    If, Else, Match, For, While, Loop, Break, Continue,
    Struct, Enum, Trait, Impl, Type,
    Where, Extends,
    Own, Borrow, Move, Copy, Drop, Unsafe,
    Async, Await, Spawn,
    Try, Catch, Throw,
    True, False,
    As, In, Is, SelfValue, SelfType,
}

pub const KEYWORDS: &[(&str, Kw)] = &[
    ("module", Kw::Module), ("import", Kw::Import), ("export", Kw::Export),
    ("pub", Kw::Pub), ("private", Kw::Private),
    ("let", Kw::Let), ("mut", Kw::Mut), ("const", Kw::Const), ("static", Kw::Static),
    ("fn", Kw::Fn), ("return", Kw::Return),
    ("if", Kw::If), ("else", Kw::Else), ("match", Kw::Match), ("for", Kw::For),
    ("while", Kw::While), ("loop", Kw::Loop), ("break", Kw::Break), ("continue", Kw::Continue),
    ("struct", Kw::Struct), ("enum", Kw::Enum), ("trait", Kw::Trait), ("impl", Kw::Impl),
    ("type", Kw::Type), ("where", Kw::Where), ("extends", Kw::Extends),
    ("own", Kw::Own), ("borrow", Kw::Borrow), ("move", Kw::Move), ("copy", Kw::Copy),
    ("drop", Kw::Drop), ("unsafe", Kw::Unsafe),
    ("async", Kw::Async), ("await", Kw::Await), ("spawn", Kw::Spawn),
    ("try", Kw::Try), ("catch", Kw::Catch), ("throw", Kw::Throw),
    ("true", Kw::True), ("false", Kw::False),
    ("as", Kw::As), ("in", Kw::In), ("is", Kw::Is), ("self", Kw::SelfValue), ("Self", Kw::SelfType),
];

pub fn kw_name(kw: Kw) -> &'static str {
    KEYWORDS.iter().find(|(_, k)| *k == kw).map(|(s, _)| *s).unwrap()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(u128, Option<String>),
    Float(f64, Option<String>),
    Str(Vec<u8>),
    Char(u8),
    Kw(Kw),
    Underscore,
    // punctuation
    LParen, RParen, LBrace, RBrace, LBracket, RBracket,
    Comma, Semi, Colon, Dot, DotDot, DotDotEq, Ellipsis, Arrow, FatArrow, Question, At, Hash,
    Plus, Minus, Star, Slash, Percent, Amp, Pipe, Caret, Shl, Shr, Bang, Tilde,
    AndAnd, OrOr,
    Eq, EqEq, Ne, Lt, Le, Gt, Ge,
    PlusEq, MinusEq, StarEq, SlashEq, PercentEq, AmpEq, PipeEq, CaretEq, ShlEq, ShrEq,
    Newline,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("identifier `{s}`"),
            Tok::Int(..) => "integer literal".into(),
            Tok::Float(..) => "float literal".into(),
            Tok::Str(_) => "string literal".into(),
            Tok::Char(_) => "character literal".into(),
            Tok::Kw(k) => format!("keyword `{}`", kw_name(*k)),
            Tok::Newline => "newline".into(),
            Tok::Eof => "end of file".into(),
            other => format!("`{}`", punct_str(other)),
        }
    }
}

pub fn punct_str(t: &Tok) -> &'static str {
    match t {
        Tok::Underscore => "_",
        Tok::LParen => "(", Tok::RParen => ")", Tok::LBrace => "{", Tok::RBrace => "}",
        Tok::LBracket => "[", Tok::RBracket => "]", Tok::Comma => ",", Tok::Semi => ";",
        Tok::Colon => ":", Tok::Dot => ".", Tok::DotDot => "..", Tok::DotDotEq => "..=",
        Tok::Ellipsis => "...", Tok::Arrow => "->", Tok::FatArrow => "=>", Tok::Question => "?",
        Tok::At => "@", Tok::Hash => "#",
        Tok::Plus => "+", Tok::Minus => "-", Tok::Star => "*", Tok::Slash => "/", Tok::Percent => "%",
        Tok::Amp => "&", Tok::Pipe => "|", Tok::Caret => "^", Tok::Shl => "<<", Tok::Shr => ">>",
        Tok::Bang => "!", Tok::Tilde => "~", Tok::AndAnd => "&&", Tok::OrOr => "||",
        Tok::Eq => "=", Tok::EqEq => "==", Tok::Ne => "!=", Tok::Lt => "<", Tok::Le => "<=",
        Tok::Gt => ">", Tok::Ge => ">=",
        Tok::PlusEq => "+=", Tok::MinusEq => "-=", Tok::StarEq => "*=", Tok::SlashEq => "/=",
        Tok::PercentEq => "%=", Tok::AmpEq => "&=", Tok::PipeEq => "|=", Tok::CaretEq => "^=",
        Tok::ShlEq => "<<=", Tok::ShrEq => ">>=",
        _ => "?",
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub fn lex(file: u32, src: &str) -> Result<Vec<Token>> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut out: Vec<Token> = Vec::new();
    let sp = |lo: usize, hi: usize| Span::new(file, lo as u32, hi as u32);

    while i < b.len() {
        let c = b[i];
        let start = i;
        match c {
            b' ' | b'\t' | b'\r' => i += 1,
            b'\n' => {
                if !matches!(out.last(), Some(Token { tok: Tok::Newline, .. }) | None) {
                    out.push(Token { tok: Tok::Newline, span: sp(i, i + 1) });
                }
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0;
                loop {
                    if i + 1 >= b.len() {
                        return Err(Diagnostic::new(sp(start, start + 2), "unterminated block comment"));
                    }
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
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                let word = &src[start..i];
                let tok = if word == "_" {
                    Tok::Underscore
                } else if let Some((_, kw)) = KEYWORDS.iter().find(|(s, _)| *s == word) {
                    Tok::Kw(*kw)
                } else {
                    Tok::Ident(word.to_string())
                };
                out.push(Token { tok, span: sp(start, i) });
            }
            b'0'..=b'9' if matches!(out.last(), Some(Token { tok: Tok::Dot, .. })) => {
                // Tuple field access: `t.0.1` must not lex `0.1` as a float.
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                let v: u128 = src[start..i].parse().unwrap();
                out.push(Token { tok: Tok::Int(v, None), span: sp(start, i) });
            }
            b'0'..=b'9' => {
                let (tok, end) = lex_number(src, start).map_err(|m| Diagnostic::new(sp(start, start + 1), m))?;
                i = end;
                out.push(Token { tok, span: sp(start, i) });
            }
            b'"' => {
                i += 1;
                let mut bytes = Vec::new();
                loop {
                    if i >= b.len() {
                        return Err(Diagnostic::new(sp(start, start + 1), "unterminated string literal"));
                    }
                    match b[i] {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => {
                            let (v, n) = lex_escape(b, i).map_err(|m| Diagnostic::new(sp(i, i + 2), m))?;
                            bytes.push(v);
                            i += n;
                        }
                        ch => {
                            bytes.push(ch);
                            i += 1;
                        }
                    }
                }
                out.push(Token { tok: Tok::Str(bytes), span: sp(start, i) });
            }
            b'\'' => {
                i += 1;
                let v = match b.get(i) {
                    Some(b'\\') => {
                        let (v, n) = lex_escape(b, i).map_err(|m| Diagnostic::new(sp(i, i + 2), m))?;
                        i += n;
                        v
                    }
                    Some(&ch) if ch != b'\'' && ch.is_ascii() => {
                        i += 1;
                        ch
                    }
                    _ => return Err(Diagnostic::new(sp(start, start + 1), "invalid character literal (only ASCII bytes are supported)")),
                };
                if b.get(i) != Some(&b'\'') {
                    return Err(Diagnostic::new(sp(start, i), "unterminated character literal"));
                }
                i += 1;
                out.push(Token { tok: Tok::Char(v), span: sp(start, i) });
            }
            _ => {
                let three = src.get(i..i + 3).unwrap_or("");
                let two = src.get(i..i + 2).unwrap_or("");
                let (tok, n) = match three {
                    "..." => (Tok::Ellipsis, 3),
                    "..=" => (Tok::DotDotEq, 3),
                    "<<=" => (Tok::ShlEq, 3),
                    ">>=" => (Tok::ShrEq, 3),
                    _ => match two {
                        ".." => (Tok::DotDot, 2),
                        "->" => (Tok::Arrow, 2),
                        "=>" => (Tok::FatArrow, 2),
                        "&&" => (Tok::AndAnd, 2),
                        "||" => (Tok::OrOr, 2),
                        "==" => (Tok::EqEq, 2),
                        "!=" => (Tok::Ne, 2),
                        "<=" => (Tok::Le, 2),
                        ">=" => (Tok::Ge, 2),
                        "<<" => (Tok::Shl, 2),
                        ">>" => (Tok::Shr, 2),
                        "+=" => (Tok::PlusEq, 2),
                        "-=" => (Tok::MinusEq, 2),
                        "*=" => (Tok::StarEq, 2),
                        "/=" => (Tok::SlashEq, 2),
                        "%=" => (Tok::PercentEq, 2),
                        "&=" => (Tok::AmpEq, 2),
                        "|=" => (Tok::PipeEq, 2),
                        "^=" => (Tok::CaretEq, 2),
                        _ => {
                            let t = match c {
                                b'(' => Tok::LParen,
                                b')' => Tok::RParen,
                                b'{' => Tok::LBrace,
                                b'}' => Tok::RBrace,
                                b'[' => Tok::LBracket,
                                b']' => Tok::RBracket,
                                b',' => Tok::Comma,
                                b';' => Tok::Semi,
                                b':' => Tok::Colon,
                                b'.' => Tok::Dot,
                                b'?' => Tok::Question,
                                b'@' => Tok::At,
                                b'#' => Tok::Hash,
                                b'+' => Tok::Plus,
                                b'-' => Tok::Minus,
                                b'*' => Tok::Star,
                                b'/' => Tok::Slash,
                                b'%' => Tok::Percent,
                                b'&' => Tok::Amp,
                                b'|' => Tok::Pipe,
                                b'^' => Tok::Caret,
                                b'!' => Tok::Bang,
                                b'~' => Tok::Tilde,
                                b'=' => Tok::Eq,
                                b'<' => Tok::Lt,
                                b'>' => Tok::Gt,
                                _ => {
                                    let ch = src[i..].chars().next().unwrap();
                                    return Err(Diagnostic::new(sp(i, i + ch.len_utf8()), format!("unexpected character `{ch}`")));
                                }
                            };
                            (t, 1)
                        }
                    },
                };
                // `>>` and `>>=` are split by the parser when closing generics.
                i += n;
                out.push(Token { tok, span: sp(start, i) });
            }
        }
    }
    out.push(Token { tok: Tok::Newline, span: sp(b.len(), b.len()) });
    out.push(Token { tok: Tok::Eof, span: sp(b.len(), b.len()) });
    Ok(out)
}

fn lex_escape(b: &[u8], i: usize) -> std::result::Result<(u8, usize), String> {
    match b.get(i + 1) {
        Some(b'n') => Ok((b'\n', 2)),
        Some(b't') => Ok((b'\t', 2)),
        Some(b'r') => Ok((b'\r', 2)),
        Some(b'0') => Ok((0, 2)),
        Some(b'\\') => Ok((b'\\', 2)),
        Some(b'"') => Ok((b'"', 2)),
        Some(b'\'') => Ok((b'\'', 2)),
        Some(b'x') => {
            let hex = std::str::from_utf8(b.get(i + 2..i + 4).ok_or("truncated \\x escape")?).map_err(|_| "bad \\x escape")?;
            let v = u8::from_str_radix(hex, 16).map_err(|_| "invalid \\x escape")?;
            Ok((v, 4))
        }
        _ => Err("unknown escape sequence".into()),
    }
}

const SUFFIXES: &[&str] = &["i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize", "f32", "f64"];

fn lex_number(src: &str, start: usize) -> std::result::Result<(Tok, usize), String> {
    let b = src.as_bytes();
    let mut i = start;
    let radix = if b[i] == b'0' && matches!(b.get(i + 1), Some(b'x' | b'b' | b'o')) {
        let r = match b[i + 1] {
            b'x' => 16,
            b'b' => 2,
            _ => 8,
        };
        i += 2;
        r
    } else {
        10
    };
    let digits_start = i;
    let mut is_float = false;
    loop {
        match b.get(i) {
            Some(c) if c.is_ascii_hexdigit() && (radix == 16 || c.is_ascii_digit()) => i += 1,
            Some(b'_') => i += 1,
            Some(b'.') if radix == 10 && !is_float && b.get(i + 1).is_some_and(|c| c.is_ascii_digit()) => {
                is_float = true;
                i += 1;
            }
            Some(b'e' | b'E') if radix == 10 && b.get(i + 1).is_some_and(|c| c.is_ascii_digit() || *c == b'-' || *c == b'+') => {
                is_float = true;
                i += 2;
            }
            _ => break,
        }
    }
    let text: String = src[digits_start..i].chars().filter(|&c| c != '_').collect();
    let mut suffix = None;
    let rest = &src[i..];
    for s in SUFFIXES {
        if rest.starts_with(s) && !rest[s.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            suffix = Some(s.to_string());
            i += s.len();
            break;
        }
    }
    if is_float || matches!(suffix.as_deref(), Some("f32" | "f64")) && radix == 10 {
        let v: f64 = text.parse().map_err(|_| "invalid float literal".to_string())?;
        return Ok((Tok::Float(v, suffix), i));
    }
    let v = u128::from_str_radix(&text, radix).map_err(|_| "invalid integer literal".to_string())?;
    Ok((Tok::Int(v, suffix), i))
}
