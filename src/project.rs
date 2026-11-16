//! Projects: a directory with a `xero.toml` manifest.
//!
//! ```toml
//! [package]
//! name = "hello"
//! version = "0.1.0"
//!
//! [dependencies]
//! geometry = { path = "../geometry" }
//! textutil = { git = "https://github.com/someone/textutil", rev = "v1.2.0" }
//! ```
//!
//! A package's modules live in `src/`: a program's root is `src/main.xr`,
//! a library's is `src/lib.xr`. A dependency `geometry` is imported as
//! `import geometry` (its `src/lib.xr`) or `import geometry.shapes` (its
//! `src/shapes.xr`). Tests are the `fn test_...()` functions in
//! `tests/*.xr`; each runs in its own process.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const MANIFEST: &str = "xero.toml";

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub deps: Vec<Dependency>,
}

#[derive(Debug, Clone)]
pub struct Dependency {
    pub name: String,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum Source {
    Path(PathBuf),
    Git { url: String, rev: Option<String> },
}

/// A loaded project: its directory and manifest.
#[derive(Debug, Clone)]
pub struct Project {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Project {
    /// The project containing `start` (it or a parent has `xero.toml`).
    pub fn find(start: &Path) -> Result<Project, String> {
        let mut dir = start.to_path_buf();
        loop {
            let m = dir.join(MANIFEST);
            if m.is_file() {
                let text = std::fs::read_to_string(&m)
                    .map_err(|e| format!("cannot read `{}`: {e}", m.display()))?;
                let manifest =
                    parse_manifest(&text).map_err(|e| format!("{}: {e}", m.display()))?;
                return Ok(Project { dir, manifest });
            }
            if !dir.pop() {
                return Err(format!(
                    "no `{MANIFEST}` found in `{}` or any parent directory (create a project with `xeroc new <name>`, or name a file: `xeroc build main.xr`)",
                    start.display()
                ));
            }
        }
    }

    pub fn src(&self) -> PathBuf {
        self.dir.join("src")
    }

    /// `src/main.xr` for a program, else `src/lib.xr` (and whether it is a library).
    pub fn root(&self) -> Result<(PathBuf, bool), String> {
        let main = self.src().join("main.xr");
        if main.is_file() {
            return Ok((main, false));
        }
        let lib = self.src().join("lib.xr");
        if lib.is_file() {
            return Ok((lib, true));
        }
        Err(format!(
            "`{}` has neither `src/main.xr` nor `src/lib.xr`",
            self.dir.display()
        ))
    }

    pub fn build_dir(&self) -> PathBuf {
        self.dir.join("build")
    }

    /// Every dependency, transitively, as (name, its `src` directory).
    /// Git dependencies are fetched into `build/deps` the first time.
    pub fn packages(&self) -> Result<Vec<(String, PathBuf)>, String> {
        let mut out: Vec<(String, PathBuf)> = Vec::new();
        let mut work: Vec<(Project, Dependency)> = self
            .manifest
            .deps
            .iter()
            .map(|d| (self.clone(), d.clone()))
            .collect();
        while let Some((owner, dep)) = work.pop() {
            let dir = match &dep.source {
                Source::Path(p) => owner.dir.join(p),
                Source::Git { url, rev } => self.fetch_git(&dep.name, url, rev.as_deref())?,
            };
            let dir = dir.canonicalize().map_err(|_| {
                format!(
                    "dependency `{}` of `{}`: `{}` does not exist",
                    dep.name,
                    owner.manifest.name,
                    dir.display()
                )
            })?;
            if let Some((_, seen)) = out.iter().find(|(n, _)| *n == dep.name) {
                if *seen != dir.join("src") {
                    return Err(format!(
                        "two different packages are both named `{}`",
                        dep.name
                    ));
                }
                continue;
            }
            let sub = Project::find(&dir).map_err(|e| format!("dependency `{}`: {e}", dep.name))?;
            if sub.dir != dir {
                return Err(format!(
                    "dependency `{}`: `{}` has no `{MANIFEST}`",
                    dep.name,
                    dir.display()
                ));
            }
            if sub.manifest.name != dep.name {
                return Err(format!(
                    "dependency `{}` is the package `{}`; name it `{}` in `[dependencies]`",
                    dep.name, sub.manifest.name, sub.manifest.name
                ));
            }
            out.push((dep.name.clone(), dir.join("src")));
            for d in &sub.manifest.deps {
                work.push((sub.clone(), d.clone()));
            }
        }
        Ok(out)
    }

    fn fetch_git(&self, name: &str, url: &str, rev: Option<&str>) -> Result<PathBuf, String> {
        let tag = rev.unwrap_or("default");
        let safe: String = tag
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let dir = self.build_dir().join("deps").join(format!("{name}-{safe}"));
        if dir.join(MANIFEST).is_file() {
            return Ok(dir);
        }
        std::fs::create_dir_all(dir.parent().unwrap())
            .map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
        eprintln!(
            "   fetching {name} from {url}{}",
            rev.map(|r| format!(" ({r})")).unwrap_or_default()
        );
        let mut cmd = Command::new("git");
        cmd.args([
            "-c",
            "advice.detachedHead=false",
            "clone",
            "--quiet",
            "--depth",
            "1",
        ]);
        if let Some(r) = rev {
            cmd.args(["--branch", r]);
        }
        cmd.arg(url).arg(&dir);
        let status = cmd
            .status()
            .map_err(|e| format!("cannot run git to fetch `{name}`: {e}"))?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("fetching `{name}` from `{url}` failed"));
        }
        Ok(dir)
    }
}

/// The subset of TOML a manifest uses: `[sections]`, `key = "string"`, and
/// inline tables `key = { a = "x", b = "y" }`; `#` comments.
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let mut section = String::new();
    let mut name = None;
    let mut version = String::from("0.0.0");
    let mut deps = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        let at = |m: String| format!("line {}: {m}", n + 1);
        if line.is_empty() {
            continue;
        }
        if let Some(s) = line.strip_prefix('[') {
            section = s
                .strip_suffix(']')
                .ok_or_else(|| at("expected `]`".into()))?
                .trim()
                .to_string();
            if section != "package" && section != "dependencies" {
                return Err(at(format!("unknown section `[{section}]`")));
            }
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| at("expected `key = value`".into()))?;
        let key = key.trim().trim_matches('"').to_string();
        let value = value.trim();
        match section.as_str() {
            "package" => {
                let v = string(value).map_err(at)?;
                match key.as_str() {
                    "name" => name = Some(v),
                    "version" => version = v,
                    "description" | "license" | "authors" => {}
                    k => return Err(at(format!("unknown key `{k}` in `[package]`"))),
                }
            }
            "dependencies" => {
                let table = inline_table(value).map_err(at)?;
                let get = |k: &str| table.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
                let source = match (get("path"), get("git")) {
                    (Some(p), None) => Source::Path(PathBuf::from(p)),
                    (None, Some(url)) => Source::Git {
                        url,
                        rev: get("rev"),
                    },
                    _ => {
                        return Err(at(format!(
                            "dependency `{key}` needs `path = \"...\"` or `git = \"...\"`"
                        )))
                    }
                };
                deps.push(Dependency { name: key, source });
            }
            _ => return Err(at("a key outside any section".into())),
        }
    }
    let name = name.ok_or("`[package]` needs a `name`")?;
    if !valid_name(&name) {
        return Err(format!(
            "`{name}` is not a valid package name (letters, digits and `_`, starting with a letter)"
        ));
    }
    Ok(Manifest {
        name,
        version,
        deps,
    })
}

pub fn valid_name(n: &str) -> bool {
    let mut cs = n.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic())
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn string(v: &str) -> Result<String, String> {
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .map(String::from)
        .ok_or_else(|| format!("expected a quoted string, found `{v}`"))
}

fn inline_table(v: &str) -> Result<Vec<(String, String)>, String> {
    let inner = v
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(|| format!("expected `{{ path = \"...\" }}`, found `{v}`"))?;
    let mut out = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("expected `key = \"value\"` in `{part}`"))?;
        out.push((k.trim().to_string(), string(v.trim())?));
    }
    Ok(out)
}

/// `xeroc new`: a project directory with a manifest, `src/main.xr` (or
/// `src/lib.xr`), a test and a `.gitignore`.
pub fn new_project(dir: &Path, lib: bool) -> Result<String, String> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("expected a directory name")?
        .to_string();
    if !valid_name(&name) {
        return Err(format!(
            "`{name}` is not a valid package name (letters, digits and `_`, starting with a letter)"
        ));
    }
    if dir.exists() {
        return Err(format!("`{}` already exists", dir.display()));
    }
    let w = |p: PathBuf, text: String| -> Result<(), String> {
        std::fs::create_dir_all(p.parent().unwrap())
            .and_then(|_| std::fs::write(&p, text))
            .map_err(|e| format!("cannot write `{}`: {e}", p.display()))
    };
    w(
        dir.join(MANIFEST),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n"),
    )?;
    if lib {
        w(
            dir.join("src/lib.xr"),
            format!("// The `{name}` library: `import {name}`.\n\nmodule {name}\n\npub fn add(a: i32, b: i32) -> i32 {{\n    a + b\n}}\n"),
        )?;
        w(
            dir.join("tests/basics.xr"),
            format!("import {name}\n\nfn test_add() {{\n    assert_eq({name}.add(2, 2), 4)\n}}\n"),
        )?;
    } else {
        w(
            dir.join("src/main.xr"),
            "import greeting\n\nfn main() {\n    println(greeting.hello(\"world\"))\n}\n".into(),
        )?;
        w(
            dir.join("src/greeting.xr"),
            "module greeting\n\npub fn hello(name: &str) -> String {\n    format(\"Hello, {}!\", name)\n}\n".into(),
        )?;
        w(
            dir.join("tests/greeting.xr"),
            "import greeting\n\nfn test_hello() {\n    assert_eq(greeting.hello(\"Xero\").as_str(), \"Hello, Xero!\")\n}\n".into(),
        )?;
    }
    w(dir.join(".gitignore"), "/build\n".into())?;
    Ok(name)
}

/// The test functions in a test file: `fn test_...()` at the start of a line.
pub fn test_names(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let l = line.strip_prefix("pub ").unwrap_or(line);
        if let Some(rest) = l.strip_prefix("fn ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.starts_with("test_") && rest[name.len()..].trim_start().starts_with("()") {
                out.push(name);
            }
        }
    }
    out
}

/// The `main` added to a test file: runs the test named by argument 1.
pub fn test_main(names: &[String]) -> String {
    let mut s = String::from(
        "fn main() {\n    if os_argc() < 2 {\n        return\n    }\n    let which = unsafe {\n        let p = *(os_argv() + 1)\n        let mut n: usize = 0\n        while *(p + n) != 0 {\n            n += 1\n        }\n        str_from_raw(p, n)\n    }\n",
    );
    for n in names {
        s.push_str(&format!(
            "    if which == \"{n}\" {{\n        {n}()\n        return\n    }}\n"
        ));
    }
    s.push_str("}\n");
    s
}
