//! Source files, spans and diagnostics.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub file: u32,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(file: u32, lo: u32, hi: u32) -> Span {
        Span { file, lo, hi }
    }

    pub fn to(self, other: Span) -> Span {
        Span {
            file: self.file,
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }
}

pub struct SourceFile {
    pub path: String,
    pub text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(path: String, text: String) -> SourceFile {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFile {
            path,
            text,
            line_starts,
        }
    }

    /// 1-based (line, column).
    pub fn line_col(&self, pos: u32) -> (usize, usize) {
        let line = match self.line_starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (line + 1, (pos - self.line_starts[line]) as usize + 1)
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1] as usize;
        let end = self
            .line_starts
            .get(line)
            .map(|&e| e as usize)
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }
}

#[derive(Default)]
pub struct SourceMap {
    pub files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn add(&mut self, path: String, text: String) -> u32 {
        self.files.push(SourceFile::new(path, text));
        (self.files.len() - 1) as u32
    }

    pub fn file(&self, id: u32) -> &SourceFile {
        &self.files[id as usize]
    }
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
    pub notes: Vec<(Span, String)>,
}

impl Diagnostic {
    pub fn new(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    pub fn note(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.notes.push((span, message.into()));
        self
    }

    pub fn render(&self, sm: &SourceMap) -> String {
        let mut out = String::new();
        render_one(&mut out, sm, "error", self.span, &self.message);
        for (span, note) in &self.notes {
            render_one(&mut out, sm, "note", *span, note);
        }
        out
    }
}

fn render_one(out: &mut String, sm: &SourceMap, kind: &str, span: Span, msg: &str) {
    use fmt::Write;
    if (span.file as usize) >= sm.files.len() {
        let _ = writeln!(out, "{kind}: {msg}");
        return;
    }
    let file = sm.file(span.file);
    let (line, col) = file.line_col(span.lo);
    let _ = writeln!(out, "{kind}: {msg}");
    let _ = writeln!(out, "  --> {}:{}:{}", file.path, line, col);
    let text = file.line_text(line);
    let gutter = line.to_string().len();
    let _ = writeln!(out, "{:gutter$} |", "");
    let _ = writeln!(out, "{line} | {text}");
    let (end_line, end_col) = file.line_col(span.hi.max(span.lo + 1) - 1);
    let width = if end_line == line {
        (end_col + 1).saturating_sub(col).max(1)
    } else {
        1
    };
    let pad: String = text
        .chars()
        .take(col - 1)
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let _ = writeln!(out, "{:gutter$} | {}{}", "", pad, "^".repeat(width));
}

pub type Result<T> = std::result::Result<T, Diagnostic>;
