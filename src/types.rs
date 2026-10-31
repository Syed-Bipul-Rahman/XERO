//! Semantic types.

use std::fmt;

pub type AdtId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
}

impl IntTy {
    pub fn signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::Isize
        )
    }

    pub fn bits(self, ptr_bits: u32) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
            IntTy::Isize | IntTy::Usize => ptr_bits,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::Isize => "isize",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::Usize => "usize",
        }
    }

    pub fn from_name(s: &str) -> Option<IntTy> {
        Some(match s {
            "i8" => IntTy::I8,
            "i16" => IntTy::I16,
            "i32" => IntTy::I32,
            "i64" => IntTy::I64,
            "isize" => IntTy::Isize,
            "u8" => IntTy::U8,
            "u16" => IntTy::U16,
            "u32" => IntTy::U32,
            "u64" => IntTy::U64,
            "usize" => IntTy::Usize,
            _ => return None,
        })
    }

    /// Inclusive value range for the given pointer width.
    pub fn range(self, ptr_bits: u32) -> (i128, i128) {
        let b = self.bits(ptr_bits);
        if self.signed() {
            (-(1i128 << (b - 1)), (1i128 << (b - 1)) - 1)
        } else {
            (0, (1i128 << b) - 1)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Int(IntTy),
    Float(FloatTy),
    Bool,
    Never,
    /// Unsized UTF-8 bytes; only usable behind a reference (`&str`).
    Str,
    Ref(Box<Ty>, bool),
    Ptr(Box<Ty>),
    Array(Box<Ty>, u64),
    /// Unsized; only usable behind a reference (`&[T]`).
    Slice(Box<Ty>),
    Tuple(Vec<Ty>),
    Adt(AdtId, Vec<Ty>),
    /// A generic parameter inside a signature/field template.
    Param(u32),
    /// The unique type of one closure expression (its captures live in the checker).
    Closure(u32),
    /// A plain function pointer: `fn(A, B) -> R`.
    FnPtr(Vec<Ty>, Box<Ty>),
}

impl Ty {
    pub fn unit() -> Ty {
        Ty::Tuple(Vec::new())
    }

    pub fn is_unit(&self) -> bool {
        matches!(self, Ty::Tuple(v) if v.is_empty())
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Ty::Never)
    }

    pub fn is_int(&self) -> bool {
        matches!(self, Ty::Int(_))
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Ty::Float(_))
    }

    pub fn is_numeric(&self) -> bool {
        self.is_int() || self.is_float()
    }

    pub fn is_unsized(&self) -> bool {
        matches!(self, Ty::Str | Ty::Slice(_))
    }

    /// `&str` / `&[T]` are fat pointers (pointer + length).
    pub fn is_fat_ref(&self) -> bool {
        matches!(self, Ty::Ref(inner, _) if inner.is_unsized())
    }

    pub fn reference(inner: Ty, mutable: bool) -> Ty {
        Ty::Ref(Box::new(inner), mutable)
    }

    pub fn has_params(&self) -> bool {
        match self {
            Ty::Param(_) => true,
            Ty::Ref(t, _) | Ty::Ptr(t) | Ty::Array(t, _) | Ty::Slice(t) => t.has_params(),
            Ty::Tuple(ts) | Ty::Adt(_, ts) => ts.iter().any(|t| t.has_params()),
            Ty::FnPtr(ps, r) => ps.iter().any(|t| t.has_params()) || r.has_params(),
            _ => false,
        }
    }

    /// Replace `Param(i)` with `args[i]`.
    pub fn subst(&self, args: &[Ty]) -> Ty {
        match self {
            Ty::Param(i) => args
                .get(*i as usize)
                .cloned()
                .unwrap_or_else(|| self.clone()),
            Ty::Ref(t, m) => Ty::Ref(Box::new(t.subst(args)), *m),
            Ty::Ptr(t) => Ty::Ptr(Box::new(t.subst(args))),
            Ty::Array(t, n) => Ty::Array(Box::new(t.subst(args)), *n),
            Ty::Slice(t) => Ty::Slice(Box::new(t.subst(args))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| t.subst(args)).collect()),
            Ty::Adt(id, ts) => Ty::Adt(*id, ts.iter().map(|t| t.subst(args)).collect()),
            Ty::FnPtr(ps, r) => Ty::FnPtr(
                ps.iter().map(|t| t.subst(args)).collect(),
                Box::new(r.subst(args)),
            ),
            _ => self.clone(),
        }
    }

    /// Strip references: `&&T` -> `T`.
    pub fn deref_all(&self) -> &Ty {
        match self {
            Ty::Ref(t, _) => t.deref_all(),
            _ => self,
        }
    }
}

/// Bind template `pat` (containing `Param`s) against concrete `ty`,
/// filling `out[i]` for each `Param(i)`. Returns false on mismatch.
pub fn match_template(pat: &Ty, ty: &Ty, out: &mut Vec<Option<Ty>>) -> bool {
    match (pat, ty) {
        (Ty::Param(i), _) => {
            let i = *i as usize;
            if out.len() <= i {
                out.resize(i + 1, None);
            }
            match &out[i] {
                Some(prev) => prev == ty,
                None => {
                    out[i] = Some(ty.clone());
                    true
                }
            }
        }
        (Ty::Ref(a, m1), Ty::Ref(b, m2)) => m1 == m2 && match_template(a, b, out),
        (Ty::Ptr(a), Ty::Ptr(b)) | (Ty::Slice(a), Ty::Slice(b)) => match_template(a, b, out),
        (Ty::Array(a, n1), Ty::Array(b, n2)) => n1 == n2 && match_template(a, b, out),
        (Ty::Tuple(a), Ty::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| match_template(x, y, out))
        }
        (Ty::Adt(i1, a), Ty::Adt(i2, b)) => {
            i1 == i2
                && a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| match_template(x, y, out))
        }
        (Ty::FnPtr(a, r1), Ty::FnPtr(b, r2)) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| match_template(x, y, out))
                && match_template(r1, r2, out)
        }
        _ => pat == ty,
    }
}

/// Formats types using a name lookup for ADTs.
pub struct TyFmt<'a> {
    pub ty: &'a Ty,
    pub adt_name: &'a dyn Fn(AdtId) -> String,
    pub param_name: &'a dyn Fn(u32) -> String,
}

impl fmt::Display for TyFmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sub = |t: &Ty| {
            TyFmt {
                ty: t,
                adt_name: self.adt_name,
                param_name: self.param_name,
            }
            .to_string()
        };
        match self.ty {
            Ty::Int(i) => write!(f, "{}", i.name()),
            Ty::Float(FloatTy::F32) => write!(f, "f32"),
            Ty::Float(FloatTy::F64) => write!(f, "f64"),
            Ty::Bool => write!(f, "bool"),
            Ty::Never => write!(f, "!"),
            Ty::Str => write!(f, "str"),
            Ty::Ref(t, true) => write!(f, "&mut {}", sub(t)),
            Ty::Ref(t, false) => write!(f, "&{}", sub(t)),
            Ty::Ptr(t) => write!(f, "*{}", sub(t)),
            Ty::Array(t, n) => write!(f, "[{}; {}]", sub(t), n),
            Ty::Slice(t) => write!(f, "[{}]", sub(t)),
            Ty::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", sub(t))?;
                }
                if ts.len() == 1 {
                    write!(f, ",")?;
                }
                write!(f, ")")
            }
            Ty::Adt(id, args) => {
                write!(f, "{}", (self.adt_name)(*id))?;
                if !args.is_empty() {
                    write!(f, "<")?;
                    for (i, t) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", sub(t))?;
                    }
                    write!(f, ">")?;
                }
                Ok(())
            }
            Ty::Param(i) => write!(f, "{}", (self.param_name)(*i)),
            Ty::Closure(id) => write!(f, "closure#{id}"),
            Ty::FnPtr(ps, r) => {
                write!(f, "fn(")?;
                for (i, t) in ps.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", sub(t))?;
                }
                write!(f, ")")?;
                if !r.is_unit() {
                    write!(f, " -> {}", sub(r))?;
                }
                Ok(())
            }
        }
    }
}
