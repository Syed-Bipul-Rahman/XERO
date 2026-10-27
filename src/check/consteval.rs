//! Compile-time evaluation of `const` items, `static` initializers and array lengths.

use super::{Checker, Def, Env, ModId};
use crate::ast::{BinOp, Expr, ExprKind, UnOp};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{FloatTy, IntTy, Ty};
use crate::xir::ConstVal;

#[derive(Clone, Debug, PartialEq)]
pub enum ConstValue {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(Vec<u8>),
    Array(Vec<ConstValue>),
    Repeat(Box<ConstValue>, u64),
    Tuple(Vec<ConstValue>),
}

impl Checker {
    pub fn const_int(&mut self, e: &Expr, module: ModId) -> Result<i128> {
        match self.const_eval(e, module, None)? {
            ConstValue::Int(n) => Ok(n),
            _ => Err(Diagnostic::new(
                e.span,
                "expected a constant integer expression",
            )),
        }
    }

    /// Type and value of a `const` item.
    pub fn const_item(&mut self, id: u32, span: Span) -> Result<(Ty, ConstValue)> {
        if let Some(c) = &self.consts[id as usize].cache {
            return Ok(c.clone());
        }
        if self.consts[id as usize].in_progress {
            return Err(Diagnostic::new(
                span,
                format!(
                    "constant `{}` depends on itself",
                    self.consts[id as usize].name
                ),
            ));
        }
        self.consts[id as usize].in_progress = true;
        let (ty_ast, value, module) = {
            let c = &self.consts[id as usize];
            (c.ty.clone(), c.value.clone(), c.module)
        };
        let declared = match &ty_ast {
            Some(t) => Some(self.resolve_type(t, module, &Env::default())?),
            None => None,
        };
        let v = self.const_eval(&value, module, declared.as_ref());
        self.consts[id as usize].in_progress = false;
        let v = v?;
        let ty = match declared {
            Some(t) => t,
            None => infer_ty(&v).ok_or_else(|| {
                Diagnostic::new(
                    value.span,
                    "cannot infer the type of this constant; add a type annotation",
                )
            })?,
        };
        to_xir(self, &v, &ty, value.span)?;
        self.consts[id as usize].cache = Some((ty.clone(), v.clone()));
        Ok((ty, v))
    }

    pub fn const_eval(
        &mut self,
        e: &Expr,
        module: ModId,
        expected: Option<&Ty>,
    ) -> Result<ConstValue> {
        let elem_expected = |t: Option<&Ty>| match t {
            Some(Ty::Array(e, _)) => Some((**e).clone()),
            _ => None,
        };
        Ok(match &e.kind {
            ExprKind::Int(v, _) => ConstValue::Int(*v as i128),
            ExprKind::Char(c) => ConstValue::Int(*c as i128),
            ExprKind::Float(v, _) => ConstValue::Float(*v),
            ExprKind::Bool(b) => ConstValue::Bool(*b),
            ExprKind::Str(s) => ConstValue::Str(s.clone()),
            ExprKind::Name(id, args) if args.is_empty() => match self.lookup(module, &id.name) {
                Some(Def::Const(cid)) => self.const_item(cid, e.span)?.1,
                _ => {
                    return Err(Diagnostic::new(
                        e.span,
                        format!("`{}` is not a constant", id.name),
                    ))
                }
            },
            ExprKind::Field(base, name, _) => {
                let ExprKind::Name(m, _) = &base.kind else {
                    return Err(Diagnostic::new(e.span, "expected a constant expression"));
                };
                match self.lookup(module, &m.name) {
                    Some(Def::Module(mid)) => {
                        match self.lookup_in(module, mid, &name.name, name.span)? {
                            Def::Const(cid) => self.const_item(cid, e.span)?.1,
                            _ => {
                                return Err(Diagnostic::new(
                                    e.span,
                                    format!("`{}` is not a constant", name.name),
                                ))
                            }
                        }
                    }
                    _ => return Err(Diagnostic::new(e.span, "expected a constant expression")),
                }
            }
            ExprKind::Unary(op, inner) => match (op, self.const_eval(inner, module, expected)?) {
                (UnOp::Neg, ConstValue::Int(v)) => ConstValue::Int(-v),
                (UnOp::Neg, ConstValue::Float(v)) => ConstValue::Float(-v),
                (UnOp::Not, ConstValue::Bool(b)) => ConstValue::Bool(!b),
                (UnOp::Not, ConstValue::Int(v)) => match expected {
                    Some(Ty::Int(it)) => ConstValue::Int(wrap(!v, *it, self.target.ptr_bits)),
                    _ => ConstValue::Int(!v),
                },
                _ => {
                    return Err(Diagnostic::new(
                        e.span,
                        "invalid operand for unary operator in constant",
                    ))
                }
            },
            ExprKind::Binary(op, a, b) => {
                let x = self.const_eval(a, module, expected)?;
                let y = self.const_eval(b, module, expected)?;
                fold_binary(*op, x, y).ok_or_else(|| Diagnostic::new(e.span, "invalid constant expression (overflow, division by zero, or mismatched operands)"))?
            }
            ExprKind::Cast(inner, ty) => {
                let t = self.resolve_type(ty, module, &Env::default())?;
                let v = self.const_eval(inner, module, None)?;
                match (v, &t) {
                    (ConstValue::Int(n), Ty::Int(it)) => {
                        ConstValue::Int(wrap(n, *it, self.target.ptr_bits))
                    }
                    (ConstValue::Int(n), Ty::Float(_)) => ConstValue::Float(n as f64),
                    (ConstValue::Float(f), Ty::Int(it)) => {
                        ConstValue::Int(wrap(f as i128, *it, self.target.ptr_bits))
                    }
                    (ConstValue::Float(f), Ty::Float(_)) => ConstValue::Float(f),
                    (ConstValue::Bool(b), Ty::Int(_)) => ConstValue::Int(b as i128),
                    _ => {
                        return Err(Diagnostic::new(
                            e.span,
                            "unsupported cast in constant expression",
                        ))
                    }
                }
            }
            ExprKind::Array(items) => {
                let et = elem_expected(expected);
                ConstValue::Array(
                    items
                        .iter()
                        .map(|i| self.const_eval(i, module, et.as_ref()))
                        .collect::<Result<_>>()?,
                )
            }
            ExprKind::ArrayRepeat(v, n) => {
                let et = elem_expected(expected);
                let v = self.const_eval(v, module, et.as_ref())?;
                let n = self.const_int(n, module)?;
                if n < 0 {
                    return Err(Diagnostic::new(e.span, "array length must be non-negative"));
                }
                ConstValue::Repeat(Box::new(v), n as u64)
            }
            ExprKind::Tuple(items) => {
                let tys: Vec<Option<Ty>> = match expected {
                    Some(Ty::Tuple(ts)) => ts.iter().cloned().map(Some).collect(),
                    _ => vec![None; items.len()],
                };
                let mut out = Vec::new();
                for (i, it) in items.iter().enumerate() {
                    out.push(self.const_eval(
                        it,
                        module,
                        tys.get(i).cloned().flatten().as_ref(),
                    )?);
                }
                ConstValue::Tuple(out)
            }
            ExprKind::Call(callee, args) if args.is_empty() => {
                // size_of<T>() / align_of<T>() are not folded here (layout is the backend's job).
                let _ = callee;
                return Err(Diagnostic::new(
                    e.span,
                    "function calls are not allowed in constant expressions",
                ));
            }
            _ => return Err(Diagnostic::new(
                e.span,
                "expected a constant expression (literals, operators, casts and other constants)",
            )),
        })
    }
}

fn wrap(v: i128, it: IntTy, ptr_bits: u32) -> i128 {
    let bits = it.bits(ptr_bits);
    let mask: i128 = if bits >= 128 { -1 } else { (1i128 << bits) - 1 };
    let u = v & mask;
    if it.signed() && bits < 128 && u >> (bits - 1) & 1 == 1 {
        u - (1i128 << bits)
    } else {
        u
    }
}

fn fold_binary(op: BinOp, x: ConstValue, y: ConstValue) -> Option<ConstValue> {
    use ConstValue::*;
    Some(match (x, y) {
        (Int(a), Int(b)) => match op {
            BinOp::Add => Int(a.checked_add(b)?),
            BinOp::Sub => Int(a.checked_sub(b)?),
            BinOp::Mul => Int(a.checked_mul(b)?),
            BinOp::Div => Int(a.checked_div(b)?),
            BinOp::Rem => Int(a.checked_rem(b)?),
            BinOp::BitAnd => Int(a & b),
            BinOp::BitOr => Int(a | b),
            BinOp::BitXor => Int(a ^ b),
            BinOp::Shl => Int(a.checked_shl(u32::try_from(b).ok()?)?),
            BinOp::Shr => Int(a.checked_shr(u32::try_from(b).ok()?)?),
            BinOp::Eq => Bool(a == b),
            BinOp::Ne => Bool(a != b),
            BinOp::Lt => Bool(a < b),
            BinOp::Le => Bool(a <= b),
            BinOp::Gt => Bool(a > b),
            BinOp::Ge => Bool(a >= b),
            _ => return None,
        },
        (Float(a), Float(b)) => match op {
            BinOp::Add => Float(a + b),
            BinOp::Sub => Float(a - b),
            BinOp::Mul => Float(a * b),
            BinOp::Div => Float(a / b),
            BinOp::Lt => Bool(a < b),
            BinOp::Gt => Bool(a > b),
            _ => return None,
        },
        (Bool(a), Bool(b)) => match op {
            BinOp::And => Bool(a && b),
            BinOp::Or => Bool(a || b),
            BinOp::Eq => Bool(a == b),
            BinOp::Ne => Bool(a != b),
            _ => return None,
        },
        _ => return None,
    })
}

fn infer_ty(v: &ConstValue) -> Option<Ty> {
    Some(match v {
        ConstValue::Int(_) => Ty::Int(IntTy::I32),
        ConstValue::Float(_) => Ty::Float(FloatTy::F64),
        ConstValue::Bool(_) => Ty::Bool,
        ConstValue::Str(_) => Ty::reference(Ty::Str, false),
        ConstValue::Array(items) => {
            Ty::Array(Box::new(infer_ty(items.first()?)?), items.len() as u64)
        }
        ConstValue::Repeat(v, n) => Ty::Array(Box::new(infer_ty(v)?), *n),
        ConstValue::Tuple(items) => Ty::Tuple(items.iter().map(infer_ty).collect::<Option<_>>()?),
    })
}

/// Check a constant against its type and convert it for the backend.
pub fn to_xir(ck: &Checker, v: &ConstValue, ty: &Ty, span: Span) -> Result<ConstVal> {
    let mismatch = || {
        Diagnostic::new(
            span,
            format!("constant value does not match type `{}`", ck.ty_str(ty)),
        )
    };
    Ok(match (v, ty) {
        (ConstValue::Int(n), Ty::Int(it)) => {
            let (lo, hi) = it.range(ck.target.ptr_bits);
            if *n < lo || *n > hi {
                return Err(Diagnostic::new(
                    span,
                    format!("value {n} does not fit in `{}`", it.name()),
                ));
            }
            ConstVal::Int(*n)
        }
        (ConstValue::Int(n), Ty::Float(_)) => ConstVal::Float(*n as f64),
        (ConstValue::Float(f), Ty::Float(_)) => ConstVal::Float(*f),
        (ConstValue::Bool(b), Ty::Bool) => ConstVal::Bool(*b),
        (ConstValue::Str(s), Ty::Ref(inner, false)) if **inner == Ty::Str => {
            ConstVal::Str(s.clone())
        }
        (ConstValue::Array(items), Ty::Array(et, n)) => {
            if items.len() as u64 != *n {
                return Err(Diagnostic::new(
                    span,
                    format!("expected an array of {n} elements, found {}", items.len()),
                ));
            }
            ConstVal::Array(
                items
                    .iter()
                    .map(|i| to_xir(ck, i, et, span))
                    .collect::<Result<_>>()?,
            )
        }
        (ConstValue::Repeat(v, k), Ty::Array(et, n)) => {
            if k != n {
                return Err(Diagnostic::new(
                    span,
                    format!("expected an array of {n} elements, found {k}"),
                ));
            }
            ConstVal::Repeat(Box::new(to_xir(ck, v, et, span)?), *n)
        }
        (ConstValue::Tuple(items), Ty::Tuple(ts)) if items.len() == ts.len() => ConstVal::Tuple(
            items
                .iter()
                .zip(ts)
                .map(|(i, t)| to_xir(ck, i, t, span))
                .collect::<Result<_>>()?,
        ),
        _ => return Err(mismatch()),
    })
}
