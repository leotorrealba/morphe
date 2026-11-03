//! Recursive-descent parser for OpenSCAD expressions and statements.
//!
//! [`Parser`] holds the token stream and a cursor; [`parse_expr`] is the
//! convenience entry point that lexes a source string and parses it as a
//! single expression, and [`parse_program`] the one that parses a whole
//! program as a statement list.
//!
//! Precedence follows upstream OpenSCAD's stratified expression grammar
//! (`src/core/parser.y` since the 2019.05 rewrite, unchanged in 2021.01
//! and master), low to high:
//!
//! ```text
//! expr           : logic_or | logic_or '?' expr ':' expr
//!                | TOK_LET / TOK_FUNCTION / TOK_ASSERT / TOK_ECHO ... expr
//! logic_or       : logic_and | logic_or OR logic_and
//! logic_and      : equality | logic_and AND equality
//! equality       : comparison | equality (EQ | NE) comparison
//! comparison     : addition | comparison ('<' | LE | '>' | GE) addition
//! addition       : multiplication | addition ('+' | '-') multiplication
//! multiplication : unary | multiplication ('*' | '/' | '%') unary
//! unary          : exponent | ('+' | '-' | '!') unary
//! exponent       : call | call '^' unary
//! call           : primary | call '(' arguments ')' | call '[' expr ']'
//!                | call '.' TOK_ID
//! ```
//!
//! So relational binds tighter than equality (`1 < 2 == true` is
//! `(1 < 2) == true`), prefix operators bind tighter than `*` (`!0 * 5` is
//! `(!0) * 5`) but looser than `^` (`-2 ^ 2` is `-(2 ^ 2)`), and `^` is
//! right-associative with a unary right operand (`2 ^ -1`). The `let`,
//! `function`, `echo` and `assert` expressions have greedy bodies. As an
//! extension, morphe also accepts them in operand position (`1 + let(a = 1)
//! a`), which upstream's grammar rejects; every program upstream accepts
//! parses the same here.

use std::fmt;

use crate::ast::{Arg, BinaryOp, Expr, ExprKind, Modifiers, Param, Stmt, StmtKind, UnaryOp};
use crate::lexer::{lex, LexError, Span, Token};

/// A parse error with a precise source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at bytes {}..{}",
            self.message, self.span.start, self.span.end
        )
    }
}

impl std::error::Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(err: LexError) -> Self {
        ParseError {
            message: err.to_string(),
            span: err.span,
        }
    }
}

/// Lex `src` and parse it as a single expression, requiring that all input
/// is consumed.
pub fn parse_expr(src: &str) -> Result<Expr, ParseError> {
    let tokens = lex(src)?;
    let mut parser = Parser::new(tokens, src.len());
    let expr = parser.parse_expression()?;
    if let Some(token) = parser.peek() {
        return Err(ParseError {
            message: format!("expected end of input, found {}", describe(token)),
            span: parser.peek_span(),
        });
    }
    Ok(expr)
}

/// Lex `src` and parse it as a whole program: a sequence of statements.
/// Stray empty statements (`;`) are tolerated and produce no node.
pub fn parse_program(src: &str) -> Result<Vec<Stmt>, ParseError> {
    parse_program_at(src, 0)
}

/// [`parse_program`] with every span — in the AST and in a returned
/// [`ParseError`] — shifted by `offset` bytes.
///
/// Lets one evaluation hold ASTs from several source files (`include` /
/// `use`) in a single span space: each file is parsed at a distinct base
/// offset, and a span maps back to its file and local position by
/// subtracting that base.
pub fn parse_program_at(src: &str, offset: usize) -> Result<Vec<Stmt>, ParseError> {
    let shift = |span: Span| Span {
        start: span.start + offset,
        end: span.end + offset,
    };
    let tokens = lex(src).map_err(|err| {
        let mut err = ParseError::from(err);
        err.span = shift(err.span);
        err
    })?;
    let tokens = tokens
        .into_iter()
        .map(|(token, span)| (token, shift(span)))
        .collect();
    let mut parser = Parser::new(tokens, offset + src.len());
    let mut stmts = Vec::new();
    while !parser.is_at_end() {
        if parser.eat(&Token::Semi).is_some() {
            continue;
        }
        // `use <path>` is only valid at the top level: upstream `input` is
        // `/* empty */ | input TOK_USE | input statement`, and `statement`
        // has no use form.
        if parser.peek() == Some(&Token::Use) {
            stmts.push(parser.parse_include_stmt(false)?);
            continue;
        }
        stmts.push(parser.parse_statement()?);
    }
    Ok(stmts)
}

/// Maximum expression/statement nesting depth before parsing aborts
/// (instead of overflowing the stack).
const MAX_DEPTH: usize = 256;

/// Message of the depth-limit error. Speculative (rewinding) parse paths
/// must never swallow this error.
const DEPTH_ERROR: &str = "expression nesting too deep";

/// Right binding power of prefix `! + -` (upstream `unary: ('+' | '-' |
/// '!') unary | exponent`): the operand absorbs only `^` and postfix
/// operators, so `!a * b` is `(!a) * b` while `-a ^ b` is `-(a ^ b)`.
const PREFIX_RBP: u8 = 14;

/// Infix binding powers `(op, left_bp, right_bp)`, one level per upstream
/// `parser.y` grammar rule (`logic_or`, `logic_and`, `equality`,
/// `comparison`, `addition`, `multiplication`, `exponent`). Left-assoc
/// levels use `right = left + 1`; `^` (`exponent: call '^' unary`) is
/// right-assoc (`right = left - 1`) and its right operand may start with a
/// prefix operator.
fn infix_binding(token: &Token) -> Option<(BinaryOp, u8, u8)> {
    Some(match token {
        Token::OrOr => (BinaryOp::Or, 2, 3),
        Token::AndAnd => (BinaryOp::And, 4, 5),
        Token::EqEq => (BinaryOp::Eq, 6, 7),
        Token::NotEq => (BinaryOp::Ne, 6, 7),
        Token::Lt => (BinaryOp::Lt, 8, 9),
        Token::Le => (BinaryOp::Le, 8, 9),
        Token::Gt => (BinaryOp::Gt, 8, 9),
        Token::Ge => (BinaryOp::Ge, 8, 9),
        Token::Plus => (BinaryOp::Add, 10, 11),
        Token::Minus => (BinaryOp::Sub, 10, 11),
        Token::Star => (BinaryOp::Mul, 12, 13),
        Token::Slash => (BinaryOp::Div, 12, 13),
        Token::Percent => (BinaryOp::Mod, 12, 13),
        Token::Caret => (BinaryOp::Pow, 16, 15),
        _ => return None,
    })
}

/// Parser over a lexed token stream.
pub struct Parser {
    tokens: Vec<(Token, Span)>,
    pos: usize,
    /// Zero-width span at the end of the source, used for end-of-input errors.
    eof: Span,
    /// Current recursion depth, bounded by [`MAX_DEPTH`].
    depth: usize,
}

impl Parser {
    /// `src_len` is the source length in bytes, used to point end-of-input
    /// errors at the end of the source.
    pub fn new(mut tokens: Vec<(Token, Span)>, src_len: usize) -> Self {
        // Upstream has no `intersection_for` keyword: it is a plain TOK_ID
        // resolved as a builtin module at evaluation time, so it can be
        // assigned to, called, or used in expressions. Normalize our lexer's
        // keyword back to an identifier for exact grammar conformance.
        for (token, _) in &mut tokens {
            if *token == Token::IntersectionFor {
                *token = Token::Ident("intersection_for".to_string());
            }
        }
        Parser {
            tokens,
            pos: 0,
            eof: Span {
                start: src_len,
                end: src_len,
            },
            depth: 0,
        }
    }

    /// True when every token has been consumed.
    pub fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    /// Parse one expression starting at the current position.
    pub fn parse_expression(&mut self) -> Result<Expr, ParseError> {
        self.enter()?;
        let result = self.parse_ternary();
        self.depth -= 1;
        result
    }

    /// Bump the recursion depth, failing instead of overflowing the stack.
    /// On failure the depth is left unchanged, so callers only unwind
    /// (`self.depth -= 1`) after a successful `enter()` — no frame leaks on
    /// the error path.
    fn enter(&mut self) -> Result<(), ParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error(DEPTH_ERROR.to_string()));
        }
        self.depth += 1;
        Ok(())
    }

    /// Fail with the depth-limit error when an iteratively-built chain (a
    /// left-assoc operator run or a postfix chain) `spine` levels long,
    /// stacked on the current recursion depth, would nest the AST deeper
    /// than [`MAX_DEPTH`]. The AST's derived `Clone`/`Drop` recurse per
    /// level, so tree depth — not only parser recursion — must be bounded.
    fn check_spine(&self, spine: usize) -> Result<(), ParseError> {
        if self.depth + spine > MAX_DEPTH {
            return Err(self.error(DEPTH_ERROR.to_string()));
        }
        Ok(())
    }

    // --- token-stream helpers ---

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|(t, _)| t)
    }

    fn peek_span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|(_, s)| *s)
            .unwrap_or(self.eof)
    }

    /// Consume the current token if it equals `token`, returning its span.
    fn eat(&mut self, token: &Token) -> Option<Span> {
        if self.peek() == Some(token) {
            let span = self.peek_span();
            self.pos += 1;
            Some(span)
        } else {
            None
        }
    }

    /// Consume the current token if it equals `token`; otherwise error with
    /// `msg` plus a description of what was found instead.
    fn expect(&mut self, token: &Token, msg: &str) -> Result<Span, ParseError> {
        self.eat(token).ok_or_else(|| ParseError {
            message: format!("{msg}, found {}", self.describe_current()),
            span: self.peek_span(),
        })
    }

    fn describe_current(&self) -> String {
        match self.peek() {
            Some(token) => describe(token),
            None => "end of input".to_string(),
        }
    }

    fn error(&self, message: String) -> ParseError {
        ParseError {
            message,
            span: self.peek_span(),
        }
    }

    // --- precedence levels ---

    /// `cond ? then : else`, right-associative.
    fn parse_ternary(&mut self) -> Result<Expr, ParseError> {
        let cond = self.parse_binary_expr(0)?;
        if self.eat(&Token::Question).is_none() {
            return Ok(cond);
        }
        let then_branch = self.parse_expression()?;
        self.expect(&Token::Colon, "expected `:` in ternary expression")?;
        let else_branch = self.parse_expression()?;
        let span = Span {
            start: cond.span.start,
            end: else_branch.span.end,
        };
        Ok(Expr {
            kind: ExprKind::Ternary {
                cond: Box::new(cond),
                then_branch: Box::new(then_branch),
                else_branch: Box::new(else_branch),
            },
            span,
        })
    }

    /// Pratt loop over the [`infix_binding`] table: parse a prefix operand,
    /// then fold in infix operators whose left binding power is at least
    /// `min_bp`. This builds the same trees as upstream's stratified
    /// grammar rules.
    fn parse_binary_expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        self.enter()?;
        let result = self.parse_binary_inner(min_bp);
        self.depth -= 1;
        result
    }

    fn parse_binary_inner(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_prefix()?;
        // Left-assoc folding grows the tree's LEFT spine one level per
        // operator without recursing, so the `enter()` guard never sees it;
        // a 100k-term `1+1+…` chain then overflowed the native stack in the
        // AST's recursive Clone/Drop. Count the spine against MAX_DEPTH.
        let mut spine = 0usize;
        while let Some((op, left_bp, right_bp)) = self.peek().and_then(infix_binding) {
            if left_bp < min_bp {
                break;
            }
            spine += 1;
            self.check_spine(spine)?;
            self.pos += 1;
            let rhs = self.parse_binary_expr(right_bp)?;
            let span = Span {
                start: lhs.span.start,
                end: rhs.span.end,
            };
            lhs = Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        Ok(lhs)
    }

    /// An operand: prefix `! + -` (upstream `unary`, binding tighter than
    /// `*` and looser than `^`), a `let(...)` / `function(...)` /
    /// `echo(...)` / `assert(...)` expression, or a postfix chain.
    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        let op = match self.peek() {
            Some(Token::Minus) => Some(UnaryOp::Neg),
            Some(Token::Plus) => Some(UnaryOp::Pos),
            Some(Token::Bang) => Some(UnaryOp::Not),
            _ => None,
        };
        let Some(op) = op else {
            match self.peek() {
                Some(Token::Let) => return self.parse_let(),
                Some(Token::Function) => return self.parse_function_literal(),
                // `echo` and `assert` lex as identifiers (they are also
                // module names); followed by `(` in expression position they
                // are upstream's TOK_ECHO / TOK_ASSERT expressions.
                Some(Token::Ident(name))
                    if (name == "echo" || name == "assert")
                        && matches!(self.tokens.get(self.pos + 1), Some((Token::LParen, _))) =>
                {
                    return self.parse_echo_assert();
                }
                _ => return self.parse_postfix(),
            }
        };
        let op_span = self.peek_span();
        self.pos += 1;
        let operand = self.parse_binary_expr(PREFIX_RBP)?;
        let span = Span {
            start: op_span.start,
            end: operand.span.end,
        };
        Ok(Expr {
            kind: ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
            span,
        })
    }

    /// Postfix chain: calls `f(...)`, indexing `[...]`, member access `.x`.
    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        // Each postfix op nests the tree one level deeper without
        // recursing (see `parse_binary_inner`), so count it explicitly.
        let mut spine = 0usize;
        loop {
            if matches!(
                self.peek(),
                Some(Token::LParen | Token::LBracket | Token::Dot)
            ) {
                spine += 1;
                self.check_spine(spine)?;
            }
            match self.peek() {
                Some(Token::LParen) => {
                    self.pos += 1;
                    let (args, rparen) = self.parse_args()?;
                    let span = Span {
                        start: expr.span.start,
                        end: rparen.end,
                    };
                    expr = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                        span,
                    };
                }
                Some(Token::LBracket) => {
                    self.pos += 1;
                    let index = self.parse_expression()?;
                    let rbracket = self.expect(&Token::RBracket, "expected `]` after index")?;
                    let span = Span {
                        start: expr.span.start,
                        end: rbracket.end,
                    };
                    expr = Expr {
                        kind: ExprKind::Index {
                            target: Box::new(expr),
                            index: Box::new(index),
                        },
                        span,
                    };
                }
                Some(Token::Dot) => {
                    self.pos += 1;
                    let (member, member_span) = match self.tokens.get(self.pos) {
                        Some((Token::Ident(name), span)) => (name.clone(), *span),
                        _ => {
                            return Err(self.error(format!(
                                "expected member name after `.`, found {}",
                                self.describe_current()
                            )));
                        }
                    };
                    self.pos += 1;
                    let span = Span {
                        start: expr.span.start,
                        end: member_span.end,
                    };
                    expr = Expr {
                        kind: ExprKind::Member {
                            target: Box::new(expr),
                            member,
                        },
                        span,
                    };
                }
                _ => return Ok(expr),
            }
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let span = self.peek_span();
        let Some((token, _)) = self.tokens.get(self.pos) else {
            return Err(self.error("expected expression, found end of input".to_string()));
        };
        let token = token.clone();
        match token {
            Token::Number(n) => {
                self.pos += 1;
                Ok(Expr {
                    kind: ExprKind::Number(n),
                    span,
                })
            }
            Token::Str(s) => {
                self.pos += 1;
                Ok(Expr {
                    kind: ExprKind::Str(s),
                    span,
                })
            }
            Token::True => {
                self.pos += 1;
                Ok(Expr {
                    kind: ExprKind::Bool(true),
                    span,
                })
            }
            Token::False => {
                self.pos += 1;
                Ok(Expr {
                    kind: ExprKind::Bool(false),
                    span,
                })
            }
            Token::Undef => {
                self.pos += 1;
                Ok(Expr {
                    kind: ExprKind::Undef,
                    span,
                })
            }
            Token::Ident(name) => {
                self.pos += 1;
                Ok(Expr {
                    kind: ExprKind::Ident(name),
                    span,
                })
            }
            Token::DollarIdent(name) => {
                self.pos += 1;
                // The lexer strips the `$`; the AST stores names with it so
                // `$x` can never collide with a plain `x`.
                Ok(Expr {
                    kind: ExprKind::DollarIdent(format!("${name}")),
                    span,
                })
            }
            Token::LParen => {
                self.pos += 1;
                let mut inner = self.parse_expression()?;
                let rparen = self.expect(&Token::RParen, "expected `)`")?;
                inner.span = Span {
                    start: span.start,
                    end: rparen.end,
                };
                Ok(inner)
            }
            Token::LBracket => self.parse_bracketed(span),
            other => Err(self.error(format!("expected expression, found {}", describe(&other)))),
        }
    }

    /// `[...]`: empty vector, range, or vector literal whose elements may be
    /// list-comprehension elements (upstream `vector_elements` /
    /// `list_comprehension_elements`).
    fn parse_bracketed(&mut self, lbracket: Span) -> Result<Expr, ParseError> {
        self.pos += 1; // '['
        if let Some(rbracket) = self.eat(&Token::RBracket) {
            return Ok(Expr {
                kind: ExprKind::Vector(Vec::new()),
                span: Span {
                    start: lbracket.start,
                    end: rbracket.end,
                },
            });
        }
        if self.peek() == Some(&Token::Comma) {
            while self.eat(&Token::Comma).is_some() {}
            let rbracket = self.expect(&Token::RBracket, "expected `]` to close vector")?;
            return Ok(Expr {
                kind: ExprKind::Vector(Vec::new()),
                span: Span {
                    start: lbracket.start,
                    end: rbracket.end,
                },
            });
        }
        let first = self.parse_vector_element()?;
        // Ranges are `'[' expr ':' expr [':' expr] ']'` upstream: only a
        // plain expression (never a comprehension element) can start one.
        if !is_lc_element(&first) && self.eat(&Token::Colon).is_some() {
            // Range: [start:end] or [start:step:end].
            let second = self.parse_expression()?;
            let (step, end) = if self.eat(&Token::Colon).is_some() {
                (Some(second), self.parse_expression()?)
            } else {
                (None, second)
            };
            let rbracket = self.expect(&Token::RBracket, "expected `]` to close range")?;
            return Ok(Expr {
                kind: ExprKind::Range {
                    start: Box::new(first),
                    step: step.map(Box::new),
                    end: Box::new(end),
                },
                span: Span {
                    start: lbracket.start,
                    end: rbracket.end,
                },
            });
        }
        // Vector literal; trailing comma allowed.
        let mut elements = vec![first];
        while self.eat(&Token::Comma).is_some() {
            while self.eat(&Token::Comma).is_some() {}
            if self.peek() == Some(&Token::RBracket) {
                break;
            }
            elements.push(self.parse_vector_element()?);
        }
        let rbracket = self.expect(&Token::RBracket, "expected `]` to close vector")?;
        Ok(Expr {
            kind: ExprKind::Vector(elements),
            span: Span {
                start: lbracket.start,
                end: rbracket.end,
            },
        })
    }

    /// Upstream `vector_element: list_comprehension_elements_p | expr` —
    /// one vector element: a comprehension element or a plain expression.
    fn parse_vector_element(&mut self) -> Result<Expr, ParseError> {
        self.enter()?;
        let result = self.parse_vector_element_inner();
        self.depth -= 1;
        result
    }

    fn parse_vector_element_inner(&mut self) -> Result<Expr, ParseError> {
        // Called under `parse_vector_element`'s depth frame, so the
        // comprehension check runs unguarded here (no double frame).
        if let Some(element) = self.maybe_lc_element_inner()? {
            return Ok(element);
        }
        self.parse_expression()
    }

    /// Try to parse a `list_comprehension_elements_p` at the cursor. `for`,
    /// `each`, and `if` commit immediately; `let` and `(` are ambiguous with
    /// plain expressions, so they rewind and return `None` when the
    /// comprehension interpretation does not pan out (mirroring how the
    /// upstream LALR parser only reduces a comprehension `let` / parenthesized
    /// comprehension when a comprehension element actually follows).
    fn maybe_lc_element(&mut self) -> Result<Option<Expr>, ParseError> {
        self.enter()?;
        let result = self.maybe_lc_element_inner();
        self.depth -= 1;
        result
    }

    fn maybe_lc_element_inner(&mut self) -> Result<Option<Expr>, ParseError> {
        match self.peek() {
            Some(Token::For) => self.parse_lc_for().map(Some),
            Some(Token::Each) => self.parse_lc_each().map(Some),
            Some(Token::If) => self.parse_lc_if().map(Some),
            Some(Token::Let) => self.maybe_lc_let(),
            Some(Token::LParen) => self.maybe_lc_paren(),
            _ => Ok(None),
        }
    }

    /// Upstream `TOK_FOR '(' arguments ')' vector_element` and the C-style
    /// `TOK_FOR '(' arguments ';' expr ';' arguments ')' vector_element`.
    fn parse_lc_for(&mut self) -> Result<Expr, ParseError> {
        let for_span = self.peek_span();
        self.pos += 1; // 'for'
        self.expect(&Token::LParen, "expected `(` after `for`")?;
        let init = self.parse_lc_arguments()?;
        if self.eat(&Token::Semi).is_some() {
            // C-style generator.
            let cond = self.parse_expression()?;
            self.expect(&Token::Semi, "expected `;` after `for` condition")?;
            let update = self.parse_lc_arguments()?;
            self.expect(&Token::RParen, "expected `)` after `for` bindings")?;
            let body = self.parse_vector_element()?;
            let span = Span {
                start: for_span.start,
                end: body.span.end,
            };
            return Ok(Expr {
                kind: ExprKind::LcForC {
                    init,
                    cond: Box::new(cond),
                    update,
                    body: Box::new(body),
                },
                span,
            });
        }
        self.expect(&Token::RParen, "expected `)` after `for` bindings")?;
        let body = self.parse_vector_element()?;
        let span = Span {
            start: for_span.start,
            end: body.span.end,
        };
        Ok(Expr {
            kind: ExprKind::LcFor {
                args: init,
                body: Box::new(body),
            },
            span,
        })
    }

    /// Upstream `TOK_EACH vector_element`.
    fn parse_lc_each(&mut self) -> Result<Expr, ParseError> {
        let each_span = self.peek_span();
        self.pos += 1; // 'each'
        let body = self.parse_vector_element()?;
        let span = Span {
            start: each_span.start,
            end: body.span.end,
        };
        Ok(Expr {
            kind: ExprKind::LcEach {
                body: Box::new(body),
            },
            span,
        })
    }

    /// Upstream `TOK_IF '(' expr ')' vector_element [TOK_ELSE
    /// vector_element]`; a dangling `else` binds to the nearest `if`.
    fn parse_lc_if(&mut self) -> Result<Expr, ParseError> {
        let if_span = self.peek_span();
        self.pos += 1; // 'if'
        self.expect(&Token::LParen, "expected `(` after `if`")?;
        let cond = self.parse_expression()?;
        self.expect(&Token::RParen, "expected `)` after `if` condition")?;
        let then_branch = self.parse_vector_element()?;
        let (else_branch, end) = if self.eat(&Token::Else).is_some() {
            let e = self.parse_vector_element()?;
            let end = e.span.end;
            (Some(Box::new(e)), end)
        } else {
            (None, then_branch.span.end)
        };
        Ok(Expr {
            kind: ExprKind::LcIf {
                cond: Box::new(cond),
                then_branch: Box::new(then_branch),
                else_branch,
            },
            span: Span {
                start: if_span.start,
                end,
            },
        })
    }

    /// Upstream `TOK_LET '(' arguments ')' list_comprehension_elements_p`:
    /// the body must itself be a comprehension element. When it is not, the
    /// cursor rewinds and the caller parses the whole thing as a plain
    /// expression instead (upstream: "The last set element may not be a
    /// `let` (as that would instead be parsed as an expression)").
    fn maybe_lc_let(&mut self) -> Result<Option<Expr>, ParseError> {
        let start = self.pos;
        let let_span = self.peek_span();
        self.pos += 1; // 'let'
        if self.eat(&Token::LParen).is_none() {
            self.pos = start;
            return Ok(None);
        }
        let args = match self.parse_lc_arguments() {
            Ok(args) => args,
            // The depth limit is a hard abort, never grounds to rewind.
            Err(err) if err.message == DEPTH_ERROR => return Err(err),
            Err(_) => {
                self.pos = start;
                return Ok(None);
            }
        };
        if self.eat(&Token::RParen).is_none() {
            self.pos = start;
            return Ok(None);
        }
        let Some(body) = self.maybe_lc_element()? else {
            self.pos = start;
            return Ok(None);
        };
        let span = Span {
            start: let_span.start,
            end: body.span.end,
        };
        Ok(Some(Expr {
            kind: ExprKind::LcLet {
                args,
                body: Box::new(body),
            },
            span,
        }))
    }

    /// Upstream `list_comprehension_elements_p: '('
    /// list_comprehension_elements ')'` — a parenthesized comprehension
    /// element. The parenthesized content must *directly* be a comprehension
    /// element (it cannot be doubly parenthesized); anything else rewinds so
    /// the caller parses a plain parenthesized expression.
    fn maybe_lc_paren(&mut self) -> Result<Option<Expr>, ParseError> {
        let start = self.pos;
        let lparen = self.peek_span();
        self.pos += 1; // '('
        if !matches!(
            self.peek(),
            Some(Token::For | Token::Each | Token::If | Token::Let)
        ) {
            self.pos = start;
            return Ok(None);
        }
        let Some(mut inner) = self.maybe_lc_element()? else {
            self.pos = start;
            return Ok(None);
        };
        let rparen = self.expect(
            &Token::RParen,
            "expected `)` after parenthesized comprehension element",
        )?;
        inner.span = Span {
            start: lparen.start,
            end: rparen.end,
        };
        Ok(Some(inner))
    }

    /// Comprehension binding list: the `arguments` grammar (named or
    /// positional, at most one trailing comma), terminated by `)` or — in
    /// the C-style `for` — `;`, both left unconsumed. May be empty.
    fn parse_lc_arguments(&mut self) -> Result<Vec<Arg>, ParseError> {
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Token::RParen | Token::Semi)) {
            return Ok(args);
        }
        loop {
            args.push(self.parse_arg()?);
            if self.eat(&Token::Comma).is_some()
                && !matches!(self.peek(), Some(Token::RParen | Token::Semi))
            {
                continue;
            }
            return Ok(args);
        }
    }

    /// Call arguments after the `(` has been consumed. Returns the args and
    /// the span of the closing `)`. At most one trailing comma is allowed,
    /// matching upstream
    /// `arguments: /* empty */ | argument_list optional_trailing_comma`.
    fn parse_args(&mut self) -> Result<(Vec<Arg>, Span), ParseError> {
        let mut args = Vec::new();
        if let Some(rparen) = self.eat(&Token::RParen) {
            return Ok((args, rparen));
        }
        loop {
            args.push(self.parse_arg()?);
            if self.eat(&Token::Comma).is_some() && self.peek() != Some(&Token::RParen) {
                continue;
            }
            let rparen = self.expect(&Token::RParen, "expected `,` or `)` in argument list")?;
            return Ok((args, rparen));
        }
    }

    /// One call argument: `name = expr` (named) or `expr` (positional).
    fn parse_arg(&mut self) -> Result<Arg, ParseError> {
        let named = match (self.tokens.get(self.pos), self.tokens.get(self.pos + 1)) {
            (Some((Token::Ident(name), span)), Some((Token::Assign, _))) => {
                Some((name.clone(), *span))
            }
            (Some((Token::DollarIdent(name), span)), Some((Token::Assign, _))) => {
                Some((format!("${name}"), *span))
            }
            _ => None,
        };
        if let Some((name, name_span)) = named {
            self.pos += 2; // name '='
            let value = self.parse_expression()?;
            let span = Span {
                start: name_span.start,
                end: value.span.end,
            };
            return Ok(Arg {
                name: Some(name),
                value,
                span,
            });
        }
        let value = self.parse_expression()?;
        let span = value.span;
        Ok(Arg {
            name: None,
            value,
            span,
        })
    }

    /// `let(a = 1, b = 2) body` (upstream `expr: TOK_LET '(' arguments ')'
    /// expr %prec LET`): bindings use the `arguments` grammar — named or
    /// positional, at most one trailing comma — and the body is greedy
    /// (parsed at the lowest precedence).
    fn parse_let(&mut self) -> Result<Expr, ParseError> {
        let let_span = self.peek_span();
        self.pos += 1; // 'let'
        self.expect(&Token::LParen, "expected `(` after `let`")?;
        let (args, _rparen) = self.parse_args()?;
        let body = self.parse_expression()?;
        let span = Span {
            start: let_span.start,
            end: body.span.end,
        };
        Ok(Expr {
            kind: ExprKind::Let {
                args,
                body: Box::new(body),
            },
            span,
        })
    }

    /// `echo(args) [expr]` / `assert(args) [expr]` (upstream `expr:
    /// TOK_ECHO '(' arguments ')' expr_or_empty`, likewise `TOK_ASSERT`).
    /// The body is optional: it is absent when the next token cannot start
    /// an expression (`;`, `)`, `,`, `]`, ... or end of input).
    fn parse_echo_assert(&mut self) -> Result<Expr, ParseError> {
        let keyword_span = self.peek_span();
        let is_echo = matches!(self.peek(), Some(Token::Ident(name)) if name == "echo");
        self.pos += 2; // keyword '('
        let (args, rparen) = self.parse_args()?;
        let body = if self.starts_expression() {
            Some(Box::new(self.parse_expression()?))
        } else {
            None
        };
        let span = Span {
            start: keyword_span.start,
            end: body.as_ref().map_or(rparen.end, |b| b.span.end),
        };
        let kind = if is_echo {
            ExprKind::Echo { args, body }
        } else {
            ExprKind::Assert { args, body }
        };
        Ok(Expr { kind, span })
    }

    /// Whether the current token can begin an expression.
    fn starts_expression(&self) -> bool {
        matches!(
            self.peek(),
            Some(
                Token::Number(_)
                    | Token::Str(_)
                    | Token::True
                    | Token::False
                    | Token::Undef
                    | Token::Ident(_)
                    | Token::DollarIdent(_)
                    | Token::LParen
                    | Token::LBracket
                    | Token::Minus
                    | Token::Plus
                    | Token::Bang
                    | Token::Let
                    | Token::Function
            )
        )
    }

    /// `function(params) body` (upstream `expr: TOK_FUNCTION '('
    /// parameters ')' expr %prec NO_ELSE`): parameters use the definition
    /// grammar, and the body is greedy like `let`'s.
    fn parse_function_literal(&mut self) -> Result<Expr, ParseError> {
        let function_span = self.peek_span();
        self.pos += 1; // 'function'
        self.expect(&Token::LParen, "expected `(` after `function`")?;
        let params = self.parse_params()?;
        let body = self.parse_expression()?;
        let span = Span {
            start: function_span.start,
            end: body.span.end,
        };
        Ok(Expr {
            kind: ExprKind::FunctionLiteral {
                params: std::sync::Arc::new(params),
                body: std::sync::Arc::new(body),
            },
            span,
        })
    }

    // --- statements ---

    /// Parse one statement starting at the current position.
    pub fn parse_statement(&mut self) -> Result<Stmt, ParseError> {
        self.enter()?;
        let result = self.parse_statement_inner();
        self.depth -= 1;
        result
    }

    /// Upstream `statement`: `';' | '{' inner_input '}' | module_instantiation
    /// | assignment | TOK_MODULE ... | TOK_FUNCTION ...`. `include` is
    /// additionally accepted here (upstream handles it in the lexer, before
    /// the grammar); `use` is not — it is only valid at the top level.
    fn parse_statement_inner(&mut self) -> Result<Stmt, ParseError> {
        let span = self.peek_span();
        match self.peek() {
            None => Err(self.error("expected statement, found end of input".to_string())),
            // Empty statement: tolerated, parses as an empty block.
            Some(Token::Semi) => {
                self.pos += 1;
                Ok(Stmt {
                    kind: StmtKind::Block(Vec::new()),
                    span,
                })
            }
            Some(Token::LBrace) => self.parse_block_stmt(),
            Some(Token::Module) => self.parse_module_def(),
            Some(Token::Function) => self.parse_function_def(),
            Some(
                Token::Bang
                | Token::Hash
                | Token::Percent
                | Token::Star
                | Token::If
                | Token::For
                | Token::Let
                | Token::Each,
            ) => self.parse_instantiation(),
            Some(Token::Include) => self.parse_include_stmt(true),
            Some(Token::Use) => {
                Err(self.error("`use` is only allowed at the top level".to_string()))
            }
            Some(Token::Ident(_)) => match self.tokens.get(self.pos + 1).map(|(t, _)| t) {
                Some(Token::Assign) => self.parse_assignment(),
                Some(Token::LParen) => self.parse_instantiation(),
                _ => Err(self.error("expected `=` or `(` after identifier".to_string())),
            },
            Some(Token::DollarIdent(_)) => match self.tokens.get(self.pos + 1).map(|(t, _)| t) {
                Some(Token::Assign) => self.parse_assignment(),
                _ => Err(self.error("expected `=` after `$`-identifier".to_string())),
            },
            Some(other) => {
                Err(self.error(format!("expected statement, found {}", describe(other))))
            }
        }
    }

    /// Statement block `{ stmt* }` (upstream `'{' inner_input '}'`): full
    /// statements, including module/function definitions. Stray `;` inside
    /// the block are tolerated.
    fn parse_block_stmt(&mut self) -> Result<Stmt, ParseError> {
        let lbrace = self.expect(&Token::LBrace, "expected `{`")?;
        let mut stmts = Vec::new();
        loop {
            if let Some(rbrace) = self.eat(&Token::RBrace) {
                return Ok(Stmt {
                    kind: StmtKind::Block(stmts),
                    span: Span {
                        start: lbrace.start,
                        end: rbrace.end,
                    },
                });
            }
            if self.is_at_end() {
                return Err(
                    self.error("expected `}` to close block, found end of input".to_string())
                );
            }
            if self.eat(&Token::Semi).is_some() {
                continue;
            }
            stmts.push(self.parse_statement()?);
        }
    }

    /// Upstream `child_statement`: `';' | '{' child_statements '}' |
    /// module_instantiation`. Used for instantiation children and if/else
    /// bodies. Assignments and module/function definitions are *not* valid
    /// here (only inside a child block, and even there only assignments).
    /// `include` is additionally accepted, as in statement position.
    fn parse_child_statement(&mut self) -> Result<Stmt, ParseError> {
        self.enter()?;
        let result = self.parse_child_statement_inner();
        self.depth -= 1;
        result
    }

    fn parse_child_statement_inner(&mut self) -> Result<Stmt, ParseError> {
        let span = self.peek_span();
        match self.peek() {
            // Empty child: parses as an empty block.
            Some(Token::Semi) => {
                self.pos += 1;
                Ok(Stmt {
                    kind: StmtKind::Block(Vec::new()),
                    span,
                })
            }
            Some(Token::LBrace) => self.parse_child_block(),
            Some(
                Token::Bang
                | Token::Hash
                | Token::Percent
                | Token::Star
                | Token::If
                | Token::For
                | Token::Let
                | Token::Each,
            ) => self.parse_instantiation(),
            Some(Token::Ident(_))
                if matches!(self.tokens.get(self.pos + 1), Some((Token::LParen, _))) =>
            {
                self.parse_instantiation()
            }
            Some(Token::Include) => self.parse_include_stmt(true),
            _ => Err(self.error(format!(
                "expected `;`, `{{`, or a module instantiation, found {}",
                self.describe_current()
            ))),
        }
    }

    /// Child block `{ ... }` (upstream `child_statements`): child statements
    /// plus assignments — but no module/function definitions.
    fn parse_child_block(&mut self) -> Result<Stmt, ParseError> {
        let lbrace = self.expect(&Token::LBrace, "expected `{`")?;
        let mut stmts = Vec::new();
        loop {
            if let Some(rbrace) = self.eat(&Token::RBrace) {
                return Ok(Stmt {
                    kind: StmtKind::Block(stmts),
                    span: Span {
                        start: lbrace.start,
                        end: rbrace.end,
                    },
                });
            }
            if self.is_at_end() {
                return Err(
                    self.error("expected `}` to close block, found end of input".to_string())
                );
            }
            if self.eat(&Token::Semi).is_some() {
                continue;
            }
            // `child_statements: ... | child_statements assignment`.
            let is_assignment = matches!(
                (self.tokens.get(self.pos), self.tokens.get(self.pos + 1)),
                (
                    Some((Token::Ident(_) | Token::DollarIdent(_), _)),
                    Some((Token::Assign, _))
                )
            );
            if is_assignment {
                stmts.push(self.parse_assignment()?);
            } else {
                stmts.push(self.parse_child_statement()?);
            }
        }
    }

    /// `name = expr;` — the dispatcher guarantees the current token is an
    /// identifier (plain or `$`) followed by `=`.
    fn parse_assignment(&mut self) -> Result<Stmt, ParseError> {
        let (name, name_span) = match self.tokens.get(self.pos) {
            Some((Token::Ident(name), span)) => (name.clone(), *span),
            Some((Token::DollarIdent(name), span)) => (format!("${name}"), *span),
            _ => unreachable!("parse_assignment called off an identifier"),
        };
        self.pos += 2; // name '='
        let value = self.parse_expression()?;
        let semi = self.expect(&Token::Semi, "expected `;` after assignment")?;
        Ok(Stmt {
            kind: StmtKind::Assign { name, value },
            span: Span {
                start: name_span.start,
                end: semi.end,
            },
        })
    }

    /// Upstream `module_instantiation`: optional modifier prefixes
    /// (`'!' | '#' | '%' | '*'` — each wraps another `module_instantiation`),
    /// then either an `ifelse_statement` or `module_id '(' arguments ')'`
    /// followed by its child statement. `module_id` is `TOK_ID | TOK_FOR |
    /// TOK_LET | TOK_ASSERT | TOK_ECHO | TOK_EACH` — so `for`, `let`, `each`
    /// (and `echo`/`assert`, which our lexer keeps as identifiers) parse as
    /// ordinary instantiations resolved at evaluation time.
    fn parse_instantiation(&mut self) -> Result<Stmt, ParseError> {
        let start = self.peek_span();
        let mut modifiers = Modifiers::default();
        loop {
            match self.peek() {
                Some(Token::Bang) => modifiers.root = true,
                Some(Token::Hash) => modifiers.debug = true,
                Some(Token::Percent) => modifiers.background = true,
                Some(Token::Star) => modifiers.disable = true,
                _ => break,
            }
            self.pos += 1;
        }
        // `ifelse_statement` is a `module_instantiation` alternative, so
        // modifiers apply to it too: `!if (x) cube();`.
        if self.peek() == Some(&Token::If) {
            return self.parse_if_stmt(start, modifiers);
        }
        let name = match self.peek() {
            Some(Token::Ident(name)) => name.clone(),
            Some(Token::For) => "for".to_string(),
            Some(Token::Let) => "let".to_string(),
            Some(Token::Each) => "each".to_string(),
            _ => {
                return Err(self.error(format!(
                    "expected module name, found {}",
                    self.describe_current()
                )));
            }
        };
        self.pos += 1;
        self.expect(&Token::LParen, "expected `(` after module name")?;
        let (args, _rparen) = self.parse_args()?;
        let (children, end) = self.parse_children()?;
        Ok(Stmt {
            kind: StmtKind::ModuleCall {
                name,
                args,
                modifiers,
                children,
            },
            span: Span {
                start: start.start,
                end,
            },
        })
    }

    /// The child part of a module instantiation: `;` (no children), a child
    /// block (children are the block's statements), or a single child
    /// statement. Returns the children and the end offset.
    fn parse_children(&mut self) -> Result<(Vec<Stmt>, usize), ParseError> {
        if let Some(semi) = self.eat(&Token::Semi) {
            return Ok((Vec::new(), semi.end));
        }
        if self.peek() == Some(&Token::LBrace) {
            let block = self.parse_child_block()?;
            let end = block.span.end;
            let StmtKind::Block(stmts) = block.kind else {
                unreachable!("parse_child_block returns a block");
            };
            return Ok((stmts, end));
        }
        let child = self.parse_child_statement()?;
        let end = child.span.end;
        Ok((vec![child], end))
    }

    /// `module name(params) { ... }` or `module name(params) stmt;`.
    fn parse_module_def(&mut self) -> Result<Stmt, ParseError> {
        let start = self.peek_span();
        self.pos += 1; // 'module'
        let name = match self.peek() {
            Some(Token::Ident(name)) => name.clone(),
            _ => {
                return Err(self.error(format!(
                    "expected module name after `module`, found {}",
                    self.describe_current()
                )));
            }
        };
        self.pos += 1;
        self.expect(&Token::LParen, "expected `(` after module name")?;
        let params = self.parse_params()?;
        let body = self.parse_statement()?;
        let end = body.span.end;
        Ok(Stmt {
            kind: StmtKind::ModuleDef {
                name,
                params,
                body: Box::new(body),
            },
            span: Span {
                start: start.start,
                end,
            },
        })
    }

    /// `function name(params) = expr;`.
    fn parse_function_def(&mut self) -> Result<Stmt, ParseError> {
        let start = self.peek_span();
        self.pos += 1; // 'function'
        let name = match self.peek() {
            Some(Token::Ident(name)) => name.clone(),
            _ => {
                return Err(self.error(format!(
                    "expected function name after `function`, found {}",
                    self.describe_current()
                )));
            }
        };
        self.pos += 1;
        self.expect(&Token::LParen, "expected `(` after function name")?;
        let params = self.parse_params()?;
        self.expect(&Token::Assign, "expected `=` in function definition")?;
        let body = self.parse_expression()?;
        let semi = self.expect(&Token::Semi, "expected `;` after function definition")?;
        Ok(Stmt {
            kind: StmtKind::FunctionDef { name, params, body },
            span: Span {
                start: start.start,
                end: semi.end,
            },
        })
    }

    /// Definition parameter list after the `(` has been consumed:
    /// `name` or `name = default`, comma-separated; `$`-names allowed.
    fn parse_params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if self.eat(&Token::RParen).is_some() {
            return Ok(params);
        }
        loop {
            let (name, name_span) = match self.tokens.get(self.pos) {
                Some((Token::Ident(name), span)) => (name.clone(), *span),
                Some((Token::DollarIdent(name), span)) => (format!("${name}"), *span),
                _ => {
                    return Err(self.error(format!(
                        "expected parameter name, found {}",
                        self.describe_current()
                    )));
                }
            };
            self.pos += 1;
            let (default, end) = if self.eat(&Token::Assign).is_some() {
                let value = self.parse_expression()?;
                let end = value.span.end;
                (Some(value), end)
            } else {
                (None, name_span.end)
            };
            params.push(Param {
                name,
                default,
                span: Span {
                    start: name_span.start,
                    end,
                },
            });
            // At most one trailing comma, matching upstream
            // `parameters: /* empty */ | parameter_list optional_trailing_comma`.
            if self.eat(&Token::Comma).is_some() && self.peek() != Some(&Token::RParen) {
                continue;
            }
            self.expect(&Token::RParen, "expected `,` or `)` in parameter list")?;
            return Ok(params);
        }
    }

    /// `if (cond) child [else child]` (upstream `ifelse_statement` /
    /// `if_statement`, whose bodies are `child_statement`s); else-if chains
    /// nest through the else branch. `start` and `modifiers` come from the
    /// enclosing `module_instantiation` wrapper.
    fn parse_if_stmt(&mut self, start: Span, modifiers: Modifiers) -> Result<Stmt, ParseError> {
        self.pos += 1; // 'if'
        self.expect(&Token::LParen, "expected `(` after `if`")?;
        let cond = self.parse_expression()?;
        self.expect(&Token::RParen, "expected `)` after `if` condition")?;
        let then_branch = self.parse_child_statement()?;
        let (else_branch, end) = if self.eat(&Token::Else).is_some() {
            let stmt = self.parse_child_statement()?;
            let end = stmt.span.end;
            (Some(Box::new(stmt)), end)
        } else {
            (None, then_branch.span.end)
        };
        Ok(Stmt {
            kind: StmtKind::If {
                cond,
                then_branch: Box::new(then_branch),
                else_branch,
                modifiers,
            },
            span: Span {
                start: start.start,
                end,
            },
        })
    }

    /// `include <path>` / `use <path>` (the evaluator loads the files).
    /// Upstream `use` takes no trailing `;` (one
    /// after it is an empty statement, which we absorb) and `include` may
    /// appear with or without one; an optional `;` is accepted for both.
    /// `use` is dispatched only from [`parse_program`] (top level).
    fn parse_include_stmt(&mut self, is_include: bool) -> Result<Stmt, ParseError> {
        let start = self.peek_span();
        let keyword = if is_include { "include" } else { "use" };
        self.pos += 1; // 'include' / 'use'
        let (path, path_span) = match self.tokens.get(self.pos) {
            Some((Token::Path(path), span)) => (path.clone(), *span),
            _ => {
                return Err(self.error(format!(
                    "expected `<path>` after `{keyword}`, found {}",
                    self.describe_current()
                )));
            }
        };
        self.pos += 1;
        let end = match self.eat(&Token::Semi) {
            Some(semi) => semi.end,
            None => path_span.end,
        };
        let kind = if is_include {
            StmtKind::Include { path }
        } else {
            StmtKind::Use { path }
        };
        Ok(Stmt {
            kind,
            span: Span {
                start: start.start,
                end,
            },
        })
    }
}

/// True when `expr` is a list-comprehension element (as opposed to a plain
/// expression), i.e. an upstream `list_comprehension_elements` production.
fn is_lc_element(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::LcFor { .. }
            | ExprKind::LcForC { .. }
            | ExprKind::LcEach { .. }
            | ExprKind::LcIf { .. }
            | ExprKind::LcLet { .. }
    )
}

/// Human-readable description of a token for error messages.
fn describe(token: &Token) -> String {
    let symbol = match token {
        Token::Ident(name) => return format!("identifier `{name}`"),
        Token::DollarIdent(name) => return format!("`${name}`"),
        Token::Number(n) => return format!("number `{n}`"),
        Token::Str(_) => return "string literal".to_string(),
        Token::Path(path) => return format!("path `<{path}>`"),
        Token::Module => "module",
        Token::Function => "function",
        Token::If => "if",
        Token::Else => "else",
        Token::For => "for",
        Token::IntersectionFor => "intersection_for",
        Token::Let => "let",
        Token::Each => "each",
        Token::True => "true",
        Token::False => "false",
        Token::Undef => "undef",
        Token::Include => "include",
        Token::Use => "use",
        Token::Plus => "+",
        Token::Minus => "-",
        Token::Star => "*",
        Token::Slash => "/",
        Token::Percent => "%",
        Token::Caret => "^",
        Token::Lt => "<",
        Token::Le => "<=",
        Token::Gt => ">",
        Token::Ge => ">=",
        Token::EqEq => "==",
        Token::NotEq => "!=",
        Token::AndAnd => "&&",
        Token::OrOr => "||",
        Token::Bang => "!",
        Token::Question => "?",
        Token::Colon => ":",
        Token::Assign => "=",
        Token::Semi => ";",
        Token::Comma => ",",
        Token::Dot => ".",
        Token::LBracket => "[",
        Token::RBracket => "]",
        Token::LParen => "(",
        Token::RParen => ")",
        Token::LBrace => "{",
        Token::RBrace => "}",
        Token::Hash => "#",
    };
    format!("`{symbol}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_program_at_shifts_every_span() {
        let base = 1000;
        let stmts = parse_program_at("x = 1;\ncube(x);", base).unwrap();
        let plain = parse_program("x = 1;\ncube(x);").unwrap();
        assert_eq!(stmts.len(), plain.len());
        for (shifted, plain) in stmts.iter().zip(&plain) {
            assert_eq!(shifted.span.start, plain.span.start + base);
            assert_eq!(shifted.span.end, plain.span.end + base);
        }
        let StmtKind::Assign { value, .. } = &stmts[0].kind else {
            panic!("expected an assignment");
        };
        assert_eq!(
            value.span,
            Span {
                start: 1004,
                end: 1005
            }
        );
        // Parse and lex errors are shifted too, including end-of-input.
        assert_eq!(
            parse_program_at("x = ;", base).unwrap_err().span.start,
            1004
        );
        assert_eq!(
            parse_program_at("x = 1", base).unwrap_err().span.start,
            1005
        );
        assert_eq!(
            parse_program_at("x = \"open", base).unwrap_err().span.start,
            1004
        );
    }

    fn p(src: &str) -> Expr {
        match parse_expr(src) {
            Ok(expr) => expr,
            Err(e) => panic!("parse of {src:?} failed: {e}"),
        }
    }

    fn perr(src: &str) -> ParseError {
        match parse_expr(src) {
            Ok(expr) => panic!("parse of {src:?} unexpectedly succeeded: {expr:?}"),
            Err(e) => e,
        }
    }

    /// Render an expression as a compact s-expression for easy assertions.
    fn sexp(e: &Expr) -> String {
        match &e.kind {
            ExprKind::Number(n) => format!("{n}"),
            ExprKind::Str(s) => format!("{s:?}"),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Undef => "undef".to_string(),
            ExprKind::Ident(n) => n.clone(),
            ExprKind::DollarIdent(n) => n.clone(),
            ExprKind::Vector(items) => {
                let mut out = String::from("(vec");
                for item in items {
                    out.push(' ');
                    out.push_str(&sexp(item));
                }
                out + ")"
            }
            ExprKind::Range { start, step, end } => match step {
                Some(step) => format!("(range {} {} {})", sexp(start), sexp(step), sexp(end)),
                None => format!("(range {} {})", sexp(start), sexp(end)),
            },
            ExprKind::Unary { op, operand } => {
                let op = match op {
                    UnaryOp::Neg => "neg",
                    UnaryOp::Pos => "pos",
                    UnaryOp::Not => "not",
                };
                format!("({op} {})", sexp(operand))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let op = match op {
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
                };
                format!("({op} {} {})", sexp(lhs), sexp(rhs))
            }
            ExprKind::Ternary {
                cond,
                then_branch,
                else_branch,
            } => format!(
                "(?: {} {} {})",
                sexp(cond),
                sexp(then_branch),
                sexp(else_branch)
            ),
            ExprKind::Call { callee, args } => {
                let mut out = format!("(call {}", sexp(callee));
                for arg in args {
                    out.push(' ');
                    match &arg.name {
                        Some(name) => out.push_str(&format!("(= {name} {})", sexp(&arg.value))),
                        None => out.push_str(&sexp(&arg.value)),
                    }
                }
                out + ")"
            }
            ExprKind::Index { target, index } => {
                format!("(index {} {})", sexp(target), sexp(index))
            }
            ExprKind::Member { target, member } => format!("(. {} {member})", sexp(target)),
            ExprKind::FunctionLiteral { params, body } => {
                let names: Vec<String> = params
                    .iter()
                    .map(|p| match &p.default {
                        Some(default) => format!("(= {} {})", p.name, sexp(default)),
                        None => p.name.clone(),
                    })
                    .collect();
                format!("(fn ({}) {})", names.join(" "), sexp(body))
            }
            ExprKind::Echo { args, body } | ExprKind::Assert { args, body } => {
                let keyword = if matches!(e.kind, ExprKind::Echo { .. }) {
                    "echo"
                } else {
                    "assert"
                };
                match body {
                    Some(body) => format!("({keyword}{} {})", args_sexp(args), sexp(body)),
                    None => format!("({keyword}{})", args_sexp(args)),
                }
            }
            ExprKind::Let { args, body } => {
                format!("(let{} {})", args_sexp(args), sexp(body))
            }
            ExprKind::LcFor { args, body } => {
                format!("(lc-for{} {})", args_sexp(args), sexp(body))
            }
            ExprKind::LcForC {
                init,
                cond,
                update,
                body,
            } => format!(
                "(lc-forc (init{}) {} (update{}) {})",
                args_sexp(init),
                sexp(cond),
                args_sexp(update),
                sexp(body)
            ),
            ExprKind::LcEach { body } => format!("(lc-each {})", sexp(body)),
            ExprKind::LcIf {
                cond,
                then_branch,
                else_branch,
            } => match else_branch {
                Some(e) => format!("(lc-if {} {} {})", sexp(cond), sexp(then_branch), sexp(e)),
                None => format!("(lc-if {} {})", sexp(cond), sexp(then_branch)),
            },
            ExprKind::LcLet { args, body } => {
                format!("(lc-let{} {})", args_sexp(args), sexp(body))
            }
        }
    }

    /// Render an argument list, with a leading space per argument.
    fn args_sexp(args: &[Arg]) -> String {
        let mut out = String::new();
        for arg in args {
            out.push(' ');
            match &arg.name {
                Some(name) => out.push_str(&format!("(= {name} {})", sexp(&arg.value))),
                None => out.push_str(&sexp(&arg.value)),
            }
        }
        out
    }

    // --- literals and atoms ---

    #[test]
    fn number_literals() {
        assert_eq!(sexp(&p("42")), "42");
        assert_eq!(sexp(&p("2.5")), "2.5");
        assert_eq!(sexp(&p("1e3")), "1000");
    }

    #[test]
    fn string_literal() {
        assert_eq!(p(r#""hi""#).kind, ExprKind::Str("hi".into()));
    }

    #[test]
    fn boolean_and_undef_literals() {
        assert_eq!(p("true").kind, ExprKind::Bool(true));
        assert_eq!(p("false").kind, ExprKind::Bool(false));
        assert_eq!(p("undef").kind, ExprKind::Undef);
    }

    #[test]
    fn identifiers() {
        assert_eq!(p("foo").kind, ExprKind::Ident("foo".into()));
        assert_eq!(p("$fn").kind, ExprKind::DollarIdent("$fn".into()));
    }

    // --- precedence and associativity ---

    #[test]
    fn add_mul_precedence() {
        assert_eq!(sexp(&p("1+2*3")), "(+ 1 (* 2 3))");
    }

    #[test]
    fn left_associative_add_sub() {
        assert_eq!(sexp(&p("1-2+3")), "(+ (- 1 2) 3)");
    }

    #[test]
    fn left_associative_mul_div_mod() {
        assert_eq!(sexp(&p("8/4%3")), "(% (/ 8 4) 3)");
    }

    #[test]
    fn power_right_assoc() {
        assert_eq!(sexp(&p("2^3^2")), "(^ 2 (^ 3 2))");
    }

    #[test]
    fn unary_minus_binds_below_power() {
        // -x^2 parses as -(x^2), matching OpenSCAD.
        assert_eq!(sexp(&p("-x^2")), "(neg (^ x 2))");
    }

    #[test]
    fn power_with_unary_rhs() {
        assert_eq!(sexp(&p("2^-3")), "(^ 2 (neg 3))");
    }

    #[test]
    fn unary_operators() {
        assert_eq!(sexp(&p("!a")), "(not a)");
        assert_eq!(sexp(&p("+a")), "(pos a)");
        assert_eq!(sexp(&p("--x")), "(neg (neg x))");
    }

    #[test]
    fn comparison_precedence_over_add() {
        assert_eq!(sexp(&p("1+2 < 3*4")), "(< (+ 1 2) (* 3 4))");
    }

    #[test]
    fn comparison_left_assoc() {
        // Relational operators are left-associative among themselves.
        assert_eq!(sexp(&p("a < b < c")), "(< (< a b) c)");
    }

    #[test]
    fn relational_binds_tighter_than_equality() {
        // Upstream parser.y: `equality: equality EQ comparison`, so a
        // comparison is an operand of `==` / `!=`.
        assert_eq!(sexp(&p("a < b == c")), "(== (< a b) c)");
        assert_eq!(sexp(&p("a == b < c")), "(== a (< b c))");
        assert_eq!(sexp(&p("1 < 2 == true")), "(== (< 1 2) true)");
        assert_eq!(sexp(&p("a != b >= c")), "(!= a (>= b c))");
        // Equality is left-associative among itself.
        assert_eq!(sexp(&p("a == b != c")), "(!= (== a b) c)");
    }

    #[test]
    fn unary_binds_tighter_than_mul() {
        // Upstream parser.y: `multiplication: multiplication '*' unary` and
        // `unary: '!' unary | exponent`.
        assert_eq!(sexp(&p("!a*b")), "(* (not a) b)");
        assert_eq!(sexp(&p("!0*5")), "(* (not 0) 5)");
        assert_eq!(sexp(&p("!a+b")), "(+ (not a) b)");
        assert_eq!(sexp(&p("-a*b")), "(* (neg a) b)");
        assert_eq!(sexp(&p("!a == b")), "(== (not a) b)");
    }

    #[test]
    fn unary_operand_of_a_mul_chain() {
        // `a / -b / c` is ((a / -b) / c): the unary takes just `b`.
        assert_eq!(sexp(&p("a / -b / c")), "(/ (/ a (neg b)) c)");
    }

    #[test]
    fn power_binds_tighter_than_unary_on_both_sides() {
        // Upstream parser.y: `exponent: call '^' unary`.
        assert_eq!(sexp(&p("-2^2")), "(neg (^ 2 2))");
        assert_eq!(sexp(&p("2^-3*4")), "(* (^ 2 (neg 3)) 4)");
        assert_eq!(sexp(&p("-2^-3^2")), "(neg (^ 2 (neg (^ 3 2))))");
        assert_eq!(sexp(&p("a^b*c")), "(* (^ a b) c)");
    }

    #[test]
    fn logical_precedence() {
        assert_eq!(sexp(&p("a || b && c")), "(|| a (&& b c))");
        assert_eq!(
            sexp(&p("a<b && c>d || e==f")),
            "(|| (&& (< a b) (> c d)) (== e f))"
        );
    }

    // --- ternary ---

    #[test]
    fn ternary_right_assoc() {
        assert_eq!(sexp(&p("a?b:c?d:e")), "(?: a b (?: c d e))");
    }

    #[test]
    fn ternary_nested_in_then_branch() {
        assert_eq!(sexp(&p("a ? b ? c : d : e")), "(?: a (?: b c d) e)");
    }

    #[test]
    fn ternary_cond_takes_logical_or() {
        assert_eq!(sexp(&p("a || b ? 1 : 2")), "(?: (|| a b) 1 2)");
    }

    // --- vectors and ranges ---

    #[test]
    fn empty_vector() {
        assert_eq!(sexp(&p("[]")), "(vec)");
    }

    #[test]
    fn vector_literal() {
        assert_eq!(sexp(&p("[1, 2+3, x]")), "(vec 1 (+ 2 3) x)");
    }

    #[test]
    fn vector_trailing_comma() {
        assert_eq!(sexp(&p("[1, 2,]")), "(vec 1 2)");
    }

    #[test]
    fn comma_only_vector_is_empty() {
        assert_eq!(sexp(&p("[,]")), "(vec)");
        assert_eq!(sexp(&p("[,,]")), "(vec)");
    }

    #[test]
    fn multi_comma_between_elements() {
        assert_eq!(sexp(&p("[1,,2]")), "(vec 1 2)");
    }

    #[test]
    fn trailing_multi_comma() {
        assert_eq!(sexp(&p("[1,2,,]")), "(vec 1 2)");
    }

    #[test]
    fn leading_comma_before_element_errors() {
        let err = perr("[,1]");
        assert!(err.message.contains("`]`"), "message: {}", err.message);
    }

    #[test]
    fn nested_vectors() {
        assert_eq!(sexp(&p("[[1,2],[3,4]]")), "(vec (vec 1 2) (vec 3 4))");
    }

    #[test]
    fn range_two_part() {
        assert_eq!(sexp(&p("[0:10]")), "(range 0 10)");
    }

    #[test]
    fn range_three_part() {
        assert_eq!(sexp(&p("[0:2:10]")), "(range 0 2 10)");
    }

    #[test]
    fn range_vs_vector_disambiguation() {
        assert_eq!(sexp(&p("[1:2]")), "(range 1 2)");
        assert_eq!(sexp(&p("[1,2]")), "(vec 1 2)");
        assert_eq!(sexp(&p("[a+1 : b*2]")), "(range (+ a 1) (* b 2))");
    }

    // --- calls, indexing, member access ---

    #[test]
    fn call_positional() {
        assert_eq!(sexp(&p("f(1, 2)")), "(call f 1 2)");
    }

    #[test]
    fn call_named_args() {
        assert_eq!(sexp(&p("f(a, b=c)")), "(call f a (= b c))");
    }

    #[test]
    fn call_dollar_named_arg() {
        assert_eq!(
            sexp(&p("cylinder(r=1, $fn=64)")),
            "(call cylinder (= r 1) (= $fn 64))"
        );
    }

    #[test]
    fn call_no_args() {
        assert_eq!(sexp(&p("f()")), "(call f)");
    }

    #[test]
    fn named_arg_is_not_equality() {
        // `a == b` inside a call is a positional comparison, not a named arg.
        assert_eq!(sexp(&p("f(a == b)")), "(call f (== a b))");
    }

    #[test]
    fn index_chain() {
        assert_eq!(sexp(&p("a[0][1]")), "(index (index a 0) 1)");
    }

    #[test]
    fn member_chain() {
        assert_eq!(sexp(&p("v.x")), "(. v x)");
        assert_eq!(sexp(&p("m.x.y")), "(. (. m x) y)");
    }

    #[test]
    fn postfix_mix() {
        assert_eq!(sexp(&p("f(x)[0].y")), "(. (index (call f x) 0) y)");
    }

    // --- function literals ---

    #[test]
    fn function_literal_parses_params_and_greedy_body() {
        // Upstream `expr: TOK_FUNCTION '(' parameters ')' expr`.
        assert_eq!(sexp(&p("function(x) x * 2")), "(fn (x) (* x 2))");
        assert_eq!(
            sexp(&p("function(a, b = 2, $c) a ? b : $c")),
            "(fn (a (= b 2) $c) (?: a b $c))"
        );
        assert_eq!(sexp(&p("function() 1")), "(fn () 1)");
        // Called directly, and curried.
        assert_eq!(
            sexp(&p("(function(x) x + 1)(2)")),
            "(call (fn (x) (+ x 1)) 2)"
        );
        assert_eq!(
            sexp(&p("function(a) function(b) a + b")),
            "(fn (a) (fn (b) (+ a b)))"
        );
        assert_eq!(sexp(&p("f(1)(2)")), "(call (call f 1) 2)");
    }

    #[test]
    fn function_literal_in_programs() {
        let stmts = parse_program("f = function(x) x; echo(f(1));").unwrap();
        let StmtKind::Assign { value, .. } = &stmts[0].kind else {
            panic!("expected an assignment");
        };
        assert_eq!(sexp(value), "(fn (x) x)");
        assert!(perr("function x").message.contains("expected `(`"));
    }

    // --- echo / assert expressions ---

    #[test]
    fn echo_and_assert_expressions() {
        // Upstream `TOK_ECHO '(' arguments ')' expr_or_empty`.
        assert_eq!(sexp(&p("echo(\"hi\") 3")), "(echo \"hi\" 3)");
        assert_eq!(
            sexp(&p("assert(x > 0, \"msg\") x * 2")),
            "(assert (> x 0) \"msg\" (* x 2))"
        );
        // The body is greedy, like let's.
        assert_eq!(sexp(&p("echo(a) a + 1")), "(echo a (+ a 1))");
        assert_eq!(
            sexp(&p("let(a = 1) echo(a) assert(a) a")),
            "(let (= a 1) (echo a (assert a a)))"
        );
        // ... and optional.
        assert_eq!(sexp(&p("echo(1)")), "(echo 1)");
        assert_eq!(sexp(&p("[assert(true), 2]")), "(vec (assert true) 2)");
        assert_eq!(sexp(&p("f(echo(m = 1))")), "(call f (echo (= m 1)))");
    }

    #[test]
    fn echo_and_assert_statements_are_unchanged() {
        let stmts = parse_program("echo(1); assert(true); x = echo(2) 3;").unwrap();
        assert!(matches!(&stmts[0].kind, StmtKind::ModuleCall { name, .. } if name == "echo"));
        assert!(matches!(&stmts[1].kind, StmtKind::ModuleCall { name, .. } if name == "assert"));
        let StmtKind::Assign { value, .. } = &stmts[2].kind else {
            panic!("expected an assignment");
        };
        assert_eq!(sexp(value), "(echo 2 3)");
    }

    // --- let expressions ---

    #[test]
    fn let_expression() {
        assert_eq!(
            sexp(&p("let(a=1, b=a+1) a+b")),
            "(let (= a 1) (= b (+ a 1)) (+ a b))"
        );
    }

    #[test]
    fn let_with_dollar_binding() {
        assert_eq!(
            sexp(&p("let($fn=12) circle(1)")),
            "(let (= $fn 12) (call circle 1))"
        );
    }

    #[test]
    fn let_trailing_comma_parses() {
        // Upstream `expr: TOK_LET '(' arguments ')' expr` — `arguments`
        // allows one trailing comma.
        assert_eq!(sexp(&p("let(a=1,) x")), "(let (= a 1) x)");
    }

    #[test]
    fn let_positional_parses() {
        // ... and positional entries; validity is decided at eval.
        assert_eq!(sexp(&p("let(1+2) x")), "(let (+ 1 2) x)");
    }

    #[test]
    fn let_in_binary_rhs() {
        // An extension: upstream's grammar only allows `let` at the `expr`
        // level (e.g. `1 + (let(a=1) a)`); morphe also accepts it as an
        // operand, with the same greedy body.
        assert_eq!(sexp(&p("1 + let(a=1) a")), "(+ 1 (let (= a 1) a))");
        assert_eq!(
            sexp(&p("1 + let(a=1) a * 2")),
            "(+ 1 (let (= a 1) (* a 2)))"
        );
        // ... and from a unary operand.
        assert_eq!(sexp(&p("-let(a=1) a")), "(neg (let (= a 1) a))");
    }

    // --- parentheses ---

    #[test]
    fn parenthesized() {
        assert_eq!(sexp(&p("(1+2)*3")), "(* (+ 1 2) 3)");
    }

    // --- spans ---

    #[test]
    fn spans_cover_tokens() {
        let e = p("1 + 2*3");
        assert_eq!(e.span, Span { start: 0, end: 7 });
        let ExprKind::Binary { rhs, .. } = &e.kind else {
            panic!("expected binary expr, got {e:?}");
        };
        assert_eq!(rhs.span, Span { start: 4, end: 7 });
    }

    #[test]
    fn paren_span_includes_parens() {
        assert_eq!(p("(1+2)").span, Span { start: 0, end: 5 });
    }

    // --- errors ---

    #[test]
    fn error_missing_closing_bracket() {
        let err = perr("[1, 2");
        assert!(err.message.contains("`]`"), "message: {}", err.message);
        assert_eq!(err.span, Span { start: 5, end: 5 });
    }

    #[test]
    fn error_missing_closing_paren() {
        let err = perr("(1+2");
        assert!(err.message.contains("`)`"), "message: {}", err.message);
        assert_eq!(err.span, Span { start: 4, end: 4 });
    }

    #[test]
    fn error_expected_expression() {
        let err = perr("1 + * 2");
        assert!(
            err.message.contains("expected expression"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 4, end: 5 });
    }

    #[test]
    fn error_missing_ternary_colon() {
        let err = perr("a ? b");
        assert!(err.message.contains("`:`"), "message: {}", err.message);
        assert_eq!(err.span, Span { start: 5, end: 5 });
    }

    #[test]
    fn error_incomplete_named_arg() {
        let err = perr("f(a=)");
        assert!(
            err.message.contains("expected expression"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 4, end: 5 });
    }

    #[test]
    fn error_empty_input_points_at_end_of_source() {
        let err = perr("");
        assert!(
            err.message.contains("end of input"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 0, end: 0 });
        // Whitespace-only input points at the end of the source, not byte 0.
        let err = perr("   ");
        assert_eq!(err.span, Span { start: 3, end: 3 });
    }

    #[test]
    fn deep_paren_nesting_errors_instead_of_overflowing() {
        let src = format!("{}1{}", "(".repeat(300), ")".repeat(300));
        let err = perr(&src);
        assert!(
            err.message.contains("expression nesting too deep"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn deep_unary_nesting_errors_instead_of_overflowing() {
        let src = format!("{}1", "-".repeat(300));
        let err = perr(&src);
        assert!(
            err.message.contains("expression nesting too deep"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn long_left_assoc_chain_errors_instead_of_overflowing() {
        // A 100k-term chain never recursed in the parser, but its 100k-deep
        // left spine overflowed the stack in the AST's recursive Clone/Drop.
        // Parse it (and drop the result) on a plain 2 MiB thread.
        let ok = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let chain = vec!["1"; 100_000].join("+");
                let expr_err = parse_expr(&chain).unwrap_err();
                let prog_err = parse_program(&format!("x = {chain};")).unwrap_err();
                let mixed = vec!["1*2"; 50_000].join("-");
                let mixed_err = parse_expr(&mixed).unwrap_err();
                [expr_err, prog_err, mixed_err]
                    .iter()
                    .all(|e| e.message.contains("expression nesting too deep"))
            })
            .unwrap()
            .join()
            .expect("parser must not overflow the stack");
        assert!(ok, "a 100k-term chain must be the depth-limit parse error");
    }

    #[test]
    fn long_postfix_chain_errors_instead_of_overflowing() {
        let ok = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                ["[0]", "()", ".x"].iter().all(|op| {
                    let src = format!("a{}", op.repeat(100_000));
                    parse_expr(&src)
                        .unwrap_err()
                        .message
                        .contains("expression nesting too deep")
                })
            })
            .unwrap()
            .join()
            .expect("parser must not overflow the stack");
        assert!(
            ok,
            "a 100k-long postfix chain must be the depth-limit error"
        );
    }

    #[test]
    fn moderate_chains_still_parse() {
        let chain = vec!["1"; 200].join("+");
        assert!(parse_expr(&chain).is_ok());
        assert!(parse_expr(&format!("a{}", "[0]".repeat(200))).is_ok());
    }

    #[test]
    fn error_trailing_tokens() {
        let err = perr("1 2");
        assert!(
            err.message.contains("expected end of input"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 2, end: 3 });
    }

    #[test]
    fn lex_error_becomes_parse_error() {
        let err = perr("\"abc");
        assert!(
            err.message.contains("unterminated string"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 0, end: 4 });
    }

    // --- list comprehensions ---

    #[test]
    fn lc_for_basic() {
        assert_eq!(
            sexp(&p("[for (i = [0:2]) i]")),
            "(vec (lc-for (= i (range 0 2)) i))"
        );
    }

    #[test]
    fn lc_for_multiple_and_positional_bindings() {
        // `for` bindings use the `arguments` grammar: named, positional,
        // and one trailing comma are all accepted at parse time.
        assert_eq!(
            sexp(&p("[for (i = a, j = b) [i, j]]")),
            "(vec (lc-for (= i a) (= j b) (vec i j)))"
        );
        assert_eq!(sexp(&p("[for ([0:2]) 1]")), "(vec (lc-for (range 0 2) 1))");
        assert_eq!(sexp(&p("[for (i = r,) i]")), "(vec (lc-for (= i r) i))");
    }

    #[test]
    fn lc_for_c_style() {
        assert_eq!(
            sexp(&p("[for (i = 0; i < 5; i = i + 1) i]")),
            "(vec (lc-forc (init (= i 0)) (< i 5) (update (= i (+ i 1))) i))"
        );
    }

    #[test]
    fn lc_for_c_style_empty_init_and_update() {
        // Upstream: `TOK_FOR '(' arguments ';' expr ';' arguments ')'` —
        // both argument lists may be empty; the condition may not.
        assert_eq!(
            sexp(&p("[for (; x; ) 1]")),
            "(vec (lc-forc (init) x (update) 1))"
        );
        // One trailing comma is allowed before the `;`, as after any
        // `argument_list`.
        assert_eq!(
            sexp(&p("[for (i = 0,; i < 2;) i]")),
            "(vec (lc-forc (init (= i 0)) (< i 2) (update) i))"
        );
    }

    #[test]
    fn lc_each() {
        assert_eq!(sexp(&p("[each v]")), "(vec (lc-each v))");
    }

    #[test]
    fn lc_each_with_vector_and_range() {
        assert_eq!(sexp(&p("[each [1, 2, 3]]")), "(vec (lc-each (vec 1 2 3)))");
        assert_eq!(sexp(&p("[each [0:2]]")), "(vec (lc-each (range 0 2)))");
        // `each`'s operand is a `vector_element`, so it composes.
        assert_eq!(
            sexp(&p("[each each [1]]")),
            "(vec (lc-each (lc-each (vec 1))))"
        );
    }

    #[test]
    fn lc_if() {
        assert_eq!(sexp(&p("[if (x) 1]")), "(vec (lc-if x 1))");
    }

    #[test]
    fn lc_if_else() {
        assert_eq!(sexp(&p("[if (x) 1 else 2]")), "(vec (lc-if x 1 2))");
    }

    #[test]
    fn lc_if_else_chain() {
        assert_eq!(
            sexp(&p("[if (a) 1 else if (b) 2 else 3]")),
            "(vec (lc-if a 1 (lc-if b 2 3)))"
        );
    }

    #[test]
    fn lc_dangling_else_binds_to_nearest_if() {
        assert_eq!(
            sexp(&p("[if (a) if (b) 1 else 2]")),
            "(vec (lc-if a (lc-if b 1 2)))"
        );
    }

    #[test]
    fn lc_if_branches_can_be_each() {
        assert_eq!(
            sexp(&p("[if (x) each [1, 2] else each [3, 4]]")),
            "(vec (lc-if x (lc-each (vec 1 2)) (lc-each (vec 3 4))))"
        );
    }

    #[test]
    fn lc_let_with_comprehension_body() {
        assert_eq!(
            sexp(&p("[let (a = 1) for (i = [0:a]) i]")),
            "(vec (lc-let (= a 1) (lc-for (= i (range 0 a)) i)))"
        );
        // Comprehension `let` bindings use the `arguments` grammar, so
        // positional entries parse (validity is decided at eval).
        assert_eq!(
            sexp(&p("[let (1 + 2) each v]")),
            "(vec (lc-let (+ 1 2) (lc-each v)))"
        );
    }

    #[test]
    fn lc_let_with_plain_body_is_expression_let() {
        // Upstream parser.y: "The last set element may not be a `let` (as
        // that would instead be parsed as an expression)".
        assert_eq!(sexp(&p("[let (a = 1) a]")), "(vec (let (= a 1) a))");
        assert_eq!(
            sexp(&p("[x, let (a = 1) a + 1, y]")),
            "(vec x (let (= a 1) (+ a 1)) y)"
        );
    }

    #[test]
    fn lc_nested_for() {
        assert_eq!(
            sexp(&p("[for (i = [0:2]) for (j = [0:2]) [i, j]]")),
            "(vec (lc-for (= i (range 0 2)) (lc-for (= j (range 0 2)) (vec i j))))"
        );
    }

    #[test]
    fn lc_for_if_each_composition() {
        assert_eq!(
            sexp(&p("[for (i = r) if (i % 2 == 0) each [i, i]]")),
            "(vec (lc-for (= i r) (lc-if (== (% i 2) 0) (lc-each (vec i i)))))"
        );
    }

    #[test]
    fn lc_mixed_with_plain_elements() {
        // Upstream `vector_elements` mixes plain exprs and comprehension
        // elements freely.
        assert_eq!(
            sexp(&p("[1, for (i = r) i, 2]")),
            "(vec 1 (lc-for (= i r) i) 2)"
        );
        assert_eq!(sexp(&p("[each v,]")), "(vec (lc-each v))");
    }

    #[test]
    fn lc_body_takes_full_expression() {
        // The element is a full expression: ternary and comparisons bind
        // inside the comprehension body.
        assert_eq!(
            sexp(&p("[for (i = r) i > 2 ? 1 : 0]")),
            "(vec (lc-for (= i r) (?: (> i 2) 1 0)))"
        );
    }

    #[test]
    fn lc_parenthesized_comprehension_element() {
        // `list_comprehension_elements_p: '(' list_comprehension_elements ')'`.
        assert_eq!(sexp(&p("[(for (i = r) i)]")), "(vec (lc-for (= i r) i))");
        assert_eq!(
            sexp(&p("[let (a = 1) (each [a])]")),
            "(vec (lc-let (= a 1) (lc-each (vec a))))"
        );
        // A parenthesized plain expression is still a plain element ...
        assert_eq!(sexp(&p("[(1 + 2) * 3]")), "(vec (* (+ 1 2) 3))");
        assert_eq!(sexp(&p("[(let (a = 1) a)]")), "(vec (let (= a 1) a))");
        // ... and can still start a range.
        assert_eq!(sexp(&p("[(a) : 2]")), "(range a 2)");
    }

    #[test]
    fn lc_for_body_can_be_expression_let() {
        // `vector_element: expr` includes a plain let expression.
        assert_eq!(
            sexp(&p("[for (i = r) let (a = i) a]")),
            "(vec (lc-for (= i r) (let (= a i) a)))"
        );
    }

    #[test]
    fn lc_spans() {
        let e = p("[for (i = r) i]");
        assert_eq!(e.span, Span { start: 0, end: 15 });
        let ExprKind::Vector(items) = &e.kind else {
            panic!("expected vector, got {e:?}");
        };
        // The comprehension element spans `for (i = r) i`.
        assert_eq!(items[0].span, Span { start: 1, end: 14 });
        let ExprKind::LcFor { args, body } = &items[0].kind else {
            panic!("expected lc-for, got {:?}", items[0]);
        };
        assert_eq!(args[0].span, Span { start: 6, end: 11 });
        assert_eq!(body.span, Span { start: 13, end: 14 });
    }

    #[test]
    fn lc_error_missing_element() {
        let err = perr("[for (i = r)]");
        assert!(
            err.message.contains("expected expression"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 12, end: 13 });
        let err = perr("[each]");
        assert!(
            err.message.contains("expected expression"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 5, end: 6 });
    }

    #[test]
    fn lc_error_unclosed_vector() {
        let err = perr("[for (i = r) i");
        assert!(err.message.contains("`]`"), "message: {}", err.message);
        assert_eq!(err.span, Span { start: 14, end: 14 });
    }

    #[test]
    fn lc_error_comprehension_cannot_start_range() {
        // Upstream ranges are `'[' expr ':' expr ']'` — a comprehension
        // element is not an `expr`, so `:` after one is rejected.
        let err = perr("[for (i = r) i : 2]");
        assert!(err.message.contains("`]`"), "message: {}", err.message);
        assert_eq!(err.span, Span { start: 15, end: 16 });
    }

    #[test]
    fn lc_error_missing_for_condition() {
        // The C-style condition is a required `expr`, not `expr_or_empty`.
        let err = perr("[for (i = 0; ; i = i + 1) i]");
        assert!(
            err.message.contains("expected expression"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 13, end: 14 });
    }

    #[test]
    fn deep_lc_nesting_errors_instead_of_overflowing() {
        let src = format!("[{}1]", "each ".repeat(300));
        let err = perr(&src);
        assert!(
            err.message.contains("nesting too deep"),
            "message: {}",
            err.message
        );
        let src = format!("[{}each v]", "let (a = 1) ".repeat(300));
        let err = perr(&src);
        assert!(
            err.message.contains("nesting too deep"),
            "message: {}",
            err.message
        );
    }

    // --- Parser struct surface (used by statement parsing) ---

    #[test]
    fn parser_stops_at_statement_boundary() {
        let src = "1 + 2 ; more";
        let tokens = lex(src).unwrap();
        let mut parser = Parser::new(tokens, src.len());
        let e = parser.parse_expression().unwrap();
        assert_eq!(sexp(&e), "(+ 1 2)");
        assert!(!parser.is_at_end());
    }

    // --- statement helpers ---

    fn prog(src: &str) -> Vec<Stmt> {
        match parse_program(src) {
            Ok(stmts) => stmts,
            Err(e) => panic!("parse of {src:?} failed: {e}"),
        }
    }

    fn stmt1(src: &str) -> Stmt {
        let mut stmts = prog(src);
        assert_eq!(
            stmts.len(),
            1,
            "expected one statement in {src:?}: {stmts:?}"
        );
        stmts.pop().unwrap()
    }

    fn sperr(src: &str) -> ParseError {
        match parse_program(src) {
            Ok(stmts) => panic!("parse of {src:?} unexpectedly succeeded: {stmts:?}"),
            Err(e) => e,
        }
    }

    fn mods_str(m: &Modifiers) -> String {
        let mut out = String::new();
        if m.root {
            out.push('!');
        }
        if m.debug {
            out.push('#');
        }
        if m.background {
            out.push('%');
        }
        if m.disable {
            out.push('*');
        }
        out
    }

    fn params_sexp(params: &[Param]) -> String {
        let mut out = String::from("(params");
        for p in params {
            out.push(' ');
            match &p.default {
                Some(d) => out.push_str(&format!("(= {} {})", p.name, sexp(d))),
                None => out.push_str(&p.name),
            }
        }
        out + ")"
    }

    /// Render a statement as a compact s-expression for easy assertions.
    fn ssexp(s: &Stmt) -> String {
        match &s.kind {
            StmtKind::Assign { name, value } => format!("(= {name} {})", sexp(value)),
            StmtKind::ModuleCall {
                name,
                args,
                modifiers,
                children,
            } => {
                let mut out = format!("(inst {}", mods_str(modifiers));
                out.push_str(name);
                for arg in args {
                    out.push(' ');
                    match &arg.name {
                        Some(n) => out.push_str(&format!("(= {n} {})", sexp(&arg.value))),
                        None => out.push_str(&sexp(&arg.value)),
                    }
                }
                for child in children {
                    out.push(' ');
                    out.push_str(&ssexp(child));
                }
                out + ")"
            }
            StmtKind::ModuleDef { name, params, body } => {
                format!("(module {name} {} {})", params_sexp(params), ssexp(body))
            }
            StmtKind::FunctionDef { name, params, body } => {
                format!("(function {name} {} {})", params_sexp(params), sexp(body))
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
                modifiers,
            } => {
                let mods = mods_str(modifiers);
                match else_branch {
                    Some(e) => format!(
                        "({mods}if {} {} {})",
                        sexp(cond),
                        ssexp(then_branch),
                        ssexp(e)
                    ),
                    None => format!("({mods}if {} {})", sexp(cond), ssexp(then_branch)),
                }
            }
            StmtKind::Block(stmts) => {
                let mut out = String::from("(block");
                for s in stmts {
                    out.push(' ');
                    out.push_str(&ssexp(s));
                }
                out + ")"
            }
            StmtKind::Include { path } => format!("(include <{path}>)"),
            StmtKind::Use { path } => format!("(use <{path}>)"),
        }
    }

    fn s1(src: &str) -> String {
        ssexp(&stmt1(src))
    }

    // --- assignments ---

    #[test]
    fn assignment_statement() {
        assert_eq!(s1("x = 1 + 2;"), "(= x (+ 1 2))");
    }

    #[test]
    fn dollar_assignment_statement() {
        assert_eq!(s1("$fn = 64;"), "(= $fn 64)");
    }

    // --- module instantiations ---

    #[test]
    fn instantiation_flat() {
        assert_eq!(s1("cube(1);"), "(inst cube 1)");
        assert_eq!(s1("sphere();"), "(inst sphere)");
    }

    #[test]
    fn instantiation_named_args() {
        assert_eq!(
            s1("cylinder(r=1, $fn=12);"),
            "(inst cylinder (= r 1) (= $fn 12))"
        );
    }

    #[test]
    fn instantiation_block_children() {
        assert_eq!(
            s1("union() { cube(1); sphere(2); }"),
            "(inst union (inst cube 1) (inst sphere 2))"
        );
    }

    #[test]
    fn instantiation_single_child_chain() {
        assert_eq!(
            s1("translate([1,0,0]) cube(1);"),
            "(inst translate (vec 1 0 0) (inst cube 1))"
        );
        assert_eq!(
            s1("translate(t) rotate(r) cube(c);"),
            "(inst translate t (inst rotate r (inst cube c)))"
        );
    }

    #[test]
    fn instantiation_child_can_be_control_flow() {
        assert_eq!(
            s1("translate(v) if (x) cube(1);"),
            "(inst translate v (if x (inst cube 1)))"
        );
    }

    #[test]
    fn echo_and_assert_are_instantiations() {
        assert_eq!(s1("echo(\"hi\");"), "(inst echo \"hi\")");
        assert_eq!(s1("assert(x > 0);"), "(inst assert (> x 0))");
    }

    // --- modifiers ---

    fn call_modifiers(src: &str) -> Modifiers {
        let s = stmt1(src);
        let StmtKind::ModuleCall { modifiers, .. } = s.kind else {
            panic!("expected instantiation, got {s:?}");
        };
        modifiers
    }

    #[test]
    fn modifier_prefixes() {
        assert_eq!(
            call_modifiers("!cube(1);"),
            Modifiers {
                root: true,
                ..Default::default()
            }
        );
        assert_eq!(
            call_modifiers("#cube(1);"),
            Modifiers {
                debug: true,
                ..Default::default()
            }
        );
        assert_eq!(
            call_modifiers("%cube(1);"),
            Modifiers {
                background: true,
                ..Default::default()
            }
        );
        assert_eq!(
            call_modifiers("*cube(1);"),
            Modifiers {
                disable: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn stacked_modifiers() {
        assert_eq!(
            call_modifiers("#%cube(1);"),
            Modifiers {
                debug: true,
                background: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn modifier_on_child() {
        assert_eq!(
            s1("translate(v) #cube(1);"),
            "(inst translate v (inst #cube 1))"
        );
    }

    // --- module definitions ---

    #[test]
    fn module_def_block_body() {
        assert_eq!(
            s1("module ring(r = 10, h) { cylinder(); }"),
            "(module ring (params (= r 10) h) (block (inst cylinder)))"
        );
    }

    #[test]
    fn module_def_single_stmt_body() {
        assert_eq!(
            s1("module m() cube(1);"),
            "(module m (params) (inst cube 1))"
        );
    }

    #[test]
    fn module_def_dollar_param() {
        assert_eq!(
            s1("module m($fn = 12) circle();"),
            "(module m (params (= $fn 12)) (inst circle))"
        );
    }

    // --- function definitions ---

    #[test]
    fn function_def() {
        assert_eq!(
            s1("function f(x, y = 2) = x + y;"),
            "(function f (params x (= y 2)) (+ x y))"
        );
    }

    // --- if / else ---

    #[test]
    fn if_statement() {
        assert_eq!(s1("if (x) cube(1);"), "(if x (inst cube 1))");
    }

    #[test]
    fn if_else_blocks() {
        assert_eq!(
            s1("if (x > 1) { cube(1); } else { sphere(2); }"),
            "(if (> x 1) (block (inst cube 1)) (block (inst sphere 2)))"
        );
    }

    #[test]
    fn else_if_chain() {
        assert_eq!(
            s1("if (a) f(); else if (b) g(); else h();"),
            "(if a (inst f) (if b (inst g) (inst h)))"
        );
    }

    #[test]
    fn if_empty_body_semicolon() {
        assert_eq!(s1("if (x);"), "(if x (block))");
    }

    // --- for / intersection_for / let statements ---

    // Upstream `module_id: TOK_ID | TOK_FOR | TOK_LET | TOK_ASSERT |
    // TOK_ECHO | TOK_EACH`: `for`, `let`, `each` (and `intersection_for`,
    // a plain identifier) parse as module instantiations whose "bindings"
    // are ordinary (usually named) arguments.

    #[test]
    fn for_statement() {
        assert_eq!(
            s1("for (i = [0:10]) cube(i);"),
            "(inst for (= i (range 0 10)) (inst cube i))"
        );
    }

    #[test]
    fn for_multiple_bindings() {
        assert_eq!(
            s1("for (i = [0:1], j = v) cube();"),
            "(inst for (= i (range 0 1)) (= j v) (inst cube))"
        );
    }

    #[test]
    fn intersection_for_statement() {
        assert_eq!(
            s1("intersection_for (i = [0:2]) rotate(i) cube();"),
            "(inst intersection_for (= i (range 0 2)) (inst rotate i (inst cube)))"
        );
    }

    #[test]
    fn let_statement() {
        assert_eq!(
            s1("let (a = 1) cube(a);"),
            "(inst let (= a 1) (inst cube a))"
        );
    }

    // --- upstream conformance: modifiers, module_id, child grammar ---

    #[test]
    fn modifier_on_if() {
        // `ifelse_statement` is a `module_instantiation`, so modifiers apply.
        assert_eq!(s1("!if (x) cube();"), "(!if x (inst cube))");
        let s = stmt1("#if (x) cube();");
        let StmtKind::If { modifiers, .. } = s.kind else {
            panic!("expected if, got {s:?}");
        };
        assert!(modifiers.debug);
    }

    #[test]
    fn modifier_on_for() {
        assert_eq!(
            s1("#for (i = [0:2]) cube();"),
            "(inst #for (= i (range 0 2)) (inst cube))"
        );
    }

    #[test]
    fn modifier_on_let_child() {
        assert_eq!(
            s1("translate(v) %let (a = 1) cube(a);"),
            "(inst translate v (inst %let (= a 1) (inst cube a)))"
        );
    }

    #[test]
    fn for_trailing_comma() {
        assert_eq!(
            s1("for (i = [0:2],) cube();"),
            "(inst for (= i (range 0 2)) (inst cube))"
        );
    }

    #[test]
    fn for_positional_binding_parses() {
        // `arguments` allows positional entries; validity is decided at eval.
        assert_eq!(
            s1("for ([0:2]) cube();"),
            "(inst for (range 0 2) (inst cube))"
        );
    }

    #[test]
    fn args_allow_one_trailing_comma_only() {
        // `arguments: /* empty */ | argument_list optional_trailing_comma`.
        assert_eq!(s1("cube(1,);"), "(inst cube 1)");
        assert_eq!(sexp(&p("f(1,)")), "(call f 1)");
        let err = sperr("cube(1,,);");
        assert!(
            err.message.contains("expected expression"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn intersection_for_is_assignable() {
        // Upstream `intersection_for` is a plain TOK_ID, not a keyword.
        assert_eq!(s1("intersection_for = 5;"), "(= intersection_for 5)");
    }

    #[test]
    fn each_call_parses_as_instantiation() {
        assert_eq!(s1("each();"), "(inst each)");
    }

    #[test]
    fn use_inside_block_errors() {
        // `use` only appears in the upstream `input` production (top level).
        let err = sperr("{ use <a.scad> }");
        assert!(
            err.message.contains("top level"),
            "message: {}",
            err.message
        );
        let err = sperr("module m() { use <a.scad> }");
        assert!(
            err.message.contains("top level"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn params_trailing_comma() {
        // `parameters: /* empty */ | parameter_list optional_trailing_comma`.
        assert_eq!(
            s1("module m(a, b = 1,) cube();"),
            "(module m (params a (= b 1)) (inst cube))"
        );
        assert_eq!(s1("function f(x,) = x;"), "(function f (params x) x)");
    }

    #[test]
    fn single_child_assignment_errors() {
        // Upstream `child_statement` has no assignment alternative.
        let err = sperr("if (x) a = 1;");
        assert!(
            err.message.contains("module instantiation"),
            "message: {}",
            err.message
        );
        let err = sperr("translate(v) a = 1;");
        assert!(
            err.message.contains("module instantiation"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn block_child_assignment_ok() {
        // `child_statements: ... | child_statements assignment`.
        assert_eq!(
            s1("union() { a = 1; cube(a); }"),
            "(inst union (= a 1) (inst cube a))"
        );
        assert_eq!(s1("if (x) { a = 1; }"), "(if x (block (= a 1)))");
    }

    #[test]
    fn module_def_inside_children_errors() {
        // `child_statements` has no module/function definition alternative.
        let err = sperr("union() { module m() cube(); }");
        assert!(
            err.message.contains("module instantiation"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn module_def_nested_in_module_body_ok() {
        // A module body is a full `statement`, so nested defs are legal there.
        assert_eq!(
            s1("module a() { module b() cube(1); b(); }"),
            "(module a (params) (block (module b (params) (inst cube 1)) (inst b)))"
        );
    }

    // --- blocks, include/use, empty statements ---

    #[test]
    fn block_statement() {
        assert_eq!(s1("{ x = 1; cube(x); }"), "(block (= x 1) (inst cube x))");
    }

    #[test]
    fn include_statement_with_and_without_semi() {
        assert_eq!(
            s1("include <lib/shapes.scad>"),
            "(include <lib/shapes.scad>)"
        );
        assert_eq!(
            s1("include <lib/shapes.scad>;"),
            "(include <lib/shapes.scad>)"
        );
    }

    #[test]
    fn use_statement_with_and_without_semi() {
        assert_eq!(s1("use <utils.scad>"), "(use <utils.scad>)");
        assert_eq!(s1("use <utils.scad>;"), "(use <utils.scad>)");
    }

    #[test]
    fn empty_statements_tolerated() {
        assert!(prog(";;").is_empty());
        assert!(prog("").is_empty());
        let stmts = prog(";; x = 1; ; cube(2); ;");
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn multi_statement_program() {
        let stmts = prog("$fn = 64; module m() cube(1); m();");
        assert_eq!(stmts.len(), 3);
    }

    // --- statement spans ---

    #[test]
    fn statement_spans_cover_source() {
        let stmts = prog("x = 1; cube(2);");
        assert_eq!(stmts[0].span, Span { start: 0, end: 6 });
        assert_eq!(stmts[1].span, Span { start: 7, end: 15 });
    }

    #[test]
    fn child_statement_spans() {
        let s = stmt1("translate(v) cube(1);");
        assert_eq!(s.span, Span { start: 0, end: 21 });
        let StmtKind::ModuleCall { children, .. } = s.kind else {
            panic!("expected instantiation");
        };
        assert_eq!(children[0].span, Span { start: 13, end: 21 });
    }

    #[test]
    fn modifier_span_starts_at_modifier() {
        assert_eq!(stmt1("#cube(1);").span, Span { start: 0, end: 9 });
    }

    // --- statement errors ---

    #[test]
    fn error_missing_semicolon_after_assignment() {
        let err = sperr("x = 1");
        assert!(err.message.contains("`;`"), "message: {}", err.message);
        assert_eq!(err.span, Span { start: 5, end: 5 });
    }

    #[test]
    fn error_unclosed_brace() {
        let err = sperr("{ x = 1;");
        assert!(err.message.contains("`}`"), "message: {}", err.message);
        assert_eq!(err.span, Span { start: 8, end: 8 });
    }

    #[test]
    fn error_missing_semicolon_after_instantiation() {
        let err = sperr("cube(1)");
        assert!(err.message.contains("`;`"), "message: {}", err.message);
        assert_eq!(err.span, Span { start: 7, end: 7 });
    }

    #[test]
    fn error_bare_identifier_statement() {
        let err = sperr("cube");
        assert!(
            err.message.contains("`=` or `(`"),
            "message: {}",
            err.message
        );
        assert_eq!(err.span, Span { start: 0, end: 4 });
    }

    #[test]
    fn error_modifier_without_instantiation() {
        let err = sperr("#5;");
        assert!(
            err.message.contains("expected module name"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn deep_block_nesting_errors_instead_of_overflowing() {
        let src = format!("{}{}", "{".repeat(300), "}".repeat(300));
        let err = sperr(&src);
        assert!(
            err.message.contains("nesting too deep"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn deep_child_chain_errors_instead_of_overflowing() {
        let src = format!("{}cube();", "t() ".repeat(300));
        let err = sperr(&src);
        assert!(
            err.message.contains("nesting too deep"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn program_lex_error_becomes_parse_error() {
        let err = sperr("x = \"abc");
        assert!(
            err.message.contains("unterminated string"),
            "message: {}",
            err.message
        );
    }
}
