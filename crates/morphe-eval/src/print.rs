//! Source-like rendering of expressions, used to print function values.
//!
//! Mirrors the upstream `print` methods of `src/core/Expression.cc`
//! (`Literal`, `Range`, `Vector`, `Lookup`, `MemberLookup`, `FunctionCall`,
//! `UnaryOp`, `BinaryOp`, `TernaryOp`, `ArrayLookup`, `Let`, `Assert`,
//! `Echo`, `FunctionDefinition`, and the list-comprehension classes): every
//! binary and ternary operation is fully parenthesised, so `function(x)
//! x*2+1` prints as `function(x) ((x * 2) + 1)`. Two differences are
//! inherent to morphe's AST: explicit parentheses in the source are not
//! kept (upstream does not keep them either, its operators re-add them),
//! and unary `+` never appears (the parser drops it, as upstream's does).

use std::fmt::{self, Write};

use morphe_lang::ast::{Arg, BinaryOp, Expr, ExprKind, Param, UnaryOp};

use crate::value::format_number;

/// Render `expr` as upstream's `Expression::print` would.
#[must_use]
pub fn expr_to_string(expr: &Expr) -> String {
    let mut out = String::new();
    write_expr(&mut out, expr).expect("writing to a String cannot fail");
    out
}

/// Render a parameter list as upstream's `AssignmentList` printing does:
/// `a, b = default`.
pub(crate) fn write_params(out: &mut impl Write, params: &[Param]) -> fmt::Result {
    for (i, param) in params.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        out.write_str(&param.name)?;
        if let Some(default) = &param.default {
            out.write_str(" = ")?;
            write_expr(out, default)?;
        }
    }
    Ok(())
}

fn write_args(out: &mut impl Write, args: &[Arg]) -> fmt::Result {
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        if let Some(name) = &arg.name {
            out.write_str(name)?;
            out.write_str(" = ")?;
        }
        write_expr(out, &arg.value)?;
    }
    Ok(())
}

/// Upstream `QuotedString`: a double-quoted string with `"`, `\`, tab,
/// newline and carriage return escaped.
fn write_quoted(out: &mut impl Write, s: &str) -> fmt::Result {
    out.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\t' => out.write_str("\\t")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            other => out.write_char(other)?,
        }
    }
    out.write_char('"')
}

fn op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "||",
        BinaryOp::And => "&&",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Pow => "^",
    }
}

/// Write `expr` in upstream `Expression::print` form.
pub(crate) fn write_expr(out: &mut impl Write, expr: &Expr) -> fmt::Result {
    match &expr.kind {
        ExprKind::Number(n) => out.write_str(&format_number(*n)),
        ExprKind::Str(s) => write_quoted(out, s),
        ExprKind::Bool(b) => out.write_str(if *b { "true" } else { "false" }),
        ExprKind::Undef => out.write_str("undef"),
        ExprKind::Ident(name) | ExprKind::DollarIdent(name) => out.write_str(name),
        ExprKind::Vector(elements) => {
            out.write_char('[')?;
            for (i, element) in elements.iter().enumerate() {
                if i > 0 {
                    out.write_str(", ")?;
                }
                write_expr(out, element)?;
            }
            out.write_char(']')
        }
        ExprKind::Range { start, step, end } => {
            out.write_char('[')?;
            write_expr(out, start)?;
            if let Some(step) = step {
                out.write_str(" : ")?;
                write_expr(out, step)?;
            }
            out.write_str(" : ")?;
            write_expr(out, end)?;
            out.write_char(']')
        }
        ExprKind::Unary { op, operand } => {
            match op {
                UnaryOp::Neg => out.write_char('-')?,
                UnaryOp::Not => out.write_char('!')?,
                UnaryOp::Pos => {}
            }
            write_expr(out, operand)
        }
        ExprKind::Binary { op, lhs, rhs } => {
            out.write_char('(')?;
            write_expr(out, lhs)?;
            write!(out, " {} ", op_str(*op))?;
            write_expr(out, rhs)?;
            out.write_char(')')
        }
        ExprKind::Ternary {
            cond,
            then_branch,
            else_branch,
        } => {
            out.write_char('(')?;
            write_expr(out, cond)?;
            out.write_str(" ? ")?;
            write_expr(out, then_branch)?;
            out.write_str(" : ")?;
            write_expr(out, else_branch)?;
            out.write_char(')')
        }
        ExprKind::Call { callee, args } => {
            // Upstream names a non-identifier callee `(<expr>)`.
            match &callee.kind {
                ExprKind::Ident(name) | ExprKind::DollarIdent(name) => out.write_str(name)?,
                _ => {
                    out.write_char('(')?;
                    write_expr(out, callee)?;
                    out.write_char(')')?;
                }
            }
            out.write_char('(')?;
            write_args(out, args)?;
            out.write_char(')')
        }
        ExprKind::Index { target, index } => {
            write_expr(out, target)?;
            out.write_char('[')?;
            write_expr(out, index)?;
            out.write_char(']')
        }
        ExprKind::Member { target, member } => {
            write_expr(out, target)?;
            out.write_char('.')?;
            out.write_str(member)
        }
        ExprKind::FunctionLiteral { params, body } => {
            out.write_str("function(")?;
            write_params(out, params)?;
            out.write_str(") ")?;
            write_expr(out, body)
        }
        ExprKind::Let { args, body } => {
            out.write_str("let(")?;
            write_args(out, args)?;
            out.write_str(") ")?;
            write_expr(out, body)
        }
        ExprKind::Echo { args, body } | ExprKind::Assert { args, body } => {
            out.write_str(if matches!(expr.kind, ExprKind::Echo { .. }) {
                "echo("
            } else {
                "assert("
            })?;
            write_args(out, args)?;
            out.write_char(')')?;
            if let Some(body) = body {
                out.write_char(' ')?;
                write_expr(out, body)?;
            }
            Ok(())
        }
        ExprKind::LcFor { args, body } => {
            out.write_str("for(")?;
            write_args(out, args)?;
            out.write_str(") (")?;
            write_expr(out, body)?;
            out.write_char(')')
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            out.write_str("for(")?;
            write_args(out, init)?;
            out.write_char(';')?;
            write_expr(out, cond)?;
            out.write_char(';')?;
            write_args(out, update)?;
            out.write_str(") ")?;
            write_expr(out, body)
        }
        ExprKind::LcEach { body } => {
            out.write_str("each (")?;
            write_expr(out, body)?;
            out.write_char(')')
        }
        ExprKind::LcIf {
            cond,
            then_branch,
            else_branch,
        } => {
            out.write_str("if(")?;
            write_expr(out, cond)?;
            out.write_str(") (")?;
            write_expr(out, then_branch)?;
            out.write_char(')')?;
            if let Some(else_branch) = else_branch {
                out.write_str(" else (")?;
                write_expr(out, else_branch)?;
                out.write_char(')')?;
            }
            Ok(())
        }
        ExprKind::LcLet { args, body } => {
            out.write_str("let(")?;
            write_args(out, args)?;
            out.write_str(") (")?;
            write_expr(out, body)?;
            out.write_char(')')
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use morphe_lang::parser::parse_expr;

    fn show(src: &str) -> String {
        expr_to_string(&parse_expr(src).expect("parses"))
    }

    #[test]
    fn operators_are_fully_parenthesised() {
        assert_eq!(show("x*2+1"), "((x * 2) + 1)");
        assert_eq!(show("a ? b : c"), "(a ? b : c)");
        assert_eq!(show("-x"), "-x");
        assert_eq!(show("!a"), "!a");
    }

    #[test]
    fn literals_and_containers() {
        assert_eq!(
            show("[1, \"a\\\"b\", true, undef]"),
            "[1, \"a\\\"b\", true, undef]"
        );
        assert_eq!(show("[0:2:10]"), "[0 : 2 : 10]");
        assert_eq!(show("[0:10]"), "[0 : 10]");
        assert_eq!(show("v[0].x"), "v[0].x");
        assert_eq!(show("f(1, r = 2)"), "f(1, r = 2)");
        assert_eq!(show("let(a = 1) a"), "let(a = 1) a");
        assert_eq!(show("echo(\"x\", n = 1) n"), "echo(\"x\", n = 1) n");
        assert_eq!(show("assert(c)"), "assert(c)");
        assert_eq!(show("[for (i = [0:2]) i]"), "[for(i = [0 : 2]) (i)]");
    }

    #[test]
    fn function_literals() {
        assert_eq!(
            show("function(x, y = 2) x + y"),
            "function(x, y = 2) (x + y)"
        );
        assert_eq!(show("(function(x) x)(2)"), "(function(x) x)(2)");
    }
}
