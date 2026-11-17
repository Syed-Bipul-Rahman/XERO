//! Debug information (`-g`): DWARF (CodeView on Windows) as LLVM
//! metadata. The code generator asks for a subprogram per function, a
//! source location for each instruction, and descriptions of the local
//! variables and their types; this module numbers the metadata nodes and
//! writes them out at the end of the module.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::diag::{SourceMap, Span};

/// Metadata numbers start high so they never meet the code generator's own
/// (`!0` for branch weights).
const FIRST_ID: u32 = 1000;

pub struct DebugInfo<'a> {
    sm: &'a SourceMap,
    /// The metadata nodes, one per line.
    pub md: String,
    next: u32,
    cu: u32,
    sub_ty: u32,
    files: HashMap<u32, u32>,
    locs: HashMap<(u32, u32, u32), u32>,
    codeview: bool,
    /// The subprogram of the function being generated, if it has one.
    pub sp: Option<u32>,
    /// The span instructions are being generated for.
    pub span: Span,
}

impl<'a> DebugInfo<'a> {
    pub fn new(
        sm: &'a SourceMap,
        root_file: u32,
        codeview: bool,
        optimized: bool,
    ) -> DebugInfo<'a> {
        let mut d = DebugInfo {
            sm,
            md: String::new(),
            next: FIRST_ID,
            cu: 0,
            sub_ty: 0,
            files: HashMap::new(),
            locs: HashMap::new(),
            codeview,
            sp: None,
            span: Span::default(),
        };
        let f = d.file(root_file);
        d.cu = d.id();
        let _ = writeln!(
            d.md,
            "!{} = distinct !DICompileUnit(language: DW_LANG_C99, file: !{f}, producer: \"xeroc {}\", isOptimized: {optimized}, runtimeVersion: 0, emissionKind: FullDebug)",
            d.cu,
            env!("CARGO_PKG_VERSION")
        );
        d.sub_ty = d.node("!DISubroutineType(types: !{null})".into());
        d
    }

    fn id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    /// A new node with the given body; its number.
    pub fn node(&mut self, body: String) -> u32 {
        let n = self.id();
        let _ = writeln!(self.md, "!{n} = {body}");
        n
    }

    /// A node number to fill in later with `define` (for recursive types).
    pub fn reserve(&mut self) -> u32 {
        self.id()
    }

    pub fn define(&mut self, n: u32, body: String) {
        let _ = writeln!(self.md, "!{n} = {body}");
    }

    pub fn file(&mut self, file: u32) -> u32 {
        if let Some(&n) = self.files.get(&file) {
            return n;
        }
        let path = self.sm.file(file).path.clone();
        let p = std::path::Path::new(&path);
        let (name, dir) = if path.starts_with('<') {
            (path.clone(), String::new())
        } else {
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            (
                abs.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                abs.parent()
                    .map(|s| s.display().to_string())
                    .unwrap_or_default(),
            )
        };
        let n = self.node(format!(
            "!DIFile(filename: \"{}\", directory: \"{}\")",
            esc(&name),
            esc(&dir)
        ));
        self.files.insert(file, n);
        n
    }

    /// (line, column) of the start of `span`.
    pub fn line_col(&self, span: Span) -> (u32, u32) {
        if (span.file as usize) >= self.sm.files.len() {
            return (0, 0);
        }
        let (l, c) = self.sm.file(span.file).line_col(span.lo);
        (l as u32, c as u32)
    }

    /// Start a function: its subprogram, made current. `symbol` is its
    /// full name (`main.area`); debuggers show the last part (`area`).
    pub fn subprogram(&mut self, symbol: &str, span: Span) -> u32 {
        let f = self.file(span.file);
        let (line, _) = self.line_col(span);
        let sp = self.id();
        let _ = writeln!(
            self.md,
            "!{sp} = distinct !DISubprogram(name: \"{n}\", scope: !{f}, file: !{f}, line: {line}, type: !{}, scopeLine: {line}, spFlags: DISPFlagDefinition, unit: !{})",
            self.sub_ty,
            self.cu,
            n = esc(short_name(symbol))
        );
        self.sp = Some(sp);
        self.span = span;
        sp
    }

    /// The location node for the current span, if in a function with debug info.
    pub fn loc(&mut self) -> Option<u32> {
        let sp = self.sp?;
        let (line, col) = self.line_col(self.span);
        if let Some(&n) = self.locs.get(&(line, col, sp)) {
            return Some(n);
        }
        let n = self.node(format!(
            "!DILocation(line: {line}, column: {col}, scope: !{sp})"
        ));
        self.locs.insert((line, col, sp), n);
        Some(n)
    }

    /// A local variable (`arg` is 1-based for parameters).
    pub fn variable(&mut self, name: &str, span: Span, arg: Option<usize>, ty: u32) -> Option<u32> {
        let sp = self.sp?;
        let f = self.file(span.file);
        let (line, _) = self.line_col(span);
        let arg = arg.map(|a| format!(", arg: {a}")).unwrap_or_default();
        Some(self.node(format!(
            "!DILocalVariable(name: \"{}\", scope: !{sp}, file: !{f}, line: {line}, type: !{ty}{arg})",
            esc(name)
        )))
    }

    /// The named metadata and module flags that finish the module.
    pub fn finish(&self) -> String {
        let mut s = self.md.clone();
        let a = self.next + 1;
        let b = self.next + 2;
        let _ = writeln!(s, "!llvm.dbg.cu = !{{!{}}}", self.cu);
        let _ = writeln!(s, "!llvm.module.flags = !{{!{a}, !{b}}}");
        if self.codeview {
            let _ = writeln!(s, "!{a} = !{{i32 2, !\"CodeView\", i32 1}}");
        } else {
            let _ = writeln!(s, "!{a} = !{{i32 7, !\"Dwarf Version\", i32 4}}");
        }
        let _ = writeln!(s, "!{b} = !{{i32 2, !\"Debug Info Version\", i32 3}}");
        s
    }
}

/// `area` from `main.area`, `len2` from `main.Point.len2`, `map` from
/// `std.prelude.Option<i32>.map` (the last part outside `<...>`).
fn short_name(symbol: &str) -> &str {
    let mut depth = 0i32;
    let mut cut = 0;
    for (i, c) in symbol.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            '.' if depth == 0 => cut = i + 1,
            _ => {}
        }
    }
    &symbol[cut..]
}

/// Escape a string for a metadata string literal.
pub fn esc(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' | '\\' => o.push_str(&format!("\\{:02X}", c as u32)),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\{:02X}", c as u32)),
            c => o.push(c),
        }
    }
    o
}
