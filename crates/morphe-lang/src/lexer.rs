//! Lexer for the OpenSCAD language.
//!
//! Iterator-style API: [`Lexer::new`] yields `Result<(Token, Span), LexError>`;
//! [`lex`] collects the whole source into a `Vec`. Spans are byte offsets into
//! the source; [`line_col`] converts an offset to 1-based line/column.

use std::fmt;

/// Byte-offset range of a token in the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// A single OpenSCAD token.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Identifier, e.g. `cube`.
    Ident(String),
    /// `$`-prefixed identifier, e.g. `$fn` (stored without the `$`).
    DollarIdent(String),
    /// Number literal; OpenSCAD numbers are all doubles.
    Number(f64),
    /// String literal (escapes resolved).
    Str(String),
    /// `<...>` path following `include` or `use` (stored without the brackets).
    Path(String),
    // Keywords.
    Module,
    Function,
    If,
    Else,
    For,
    IntersectionFor,
    Let,
    Each,
    True,
    False,
    Undef,
    Include,
    Use,
    // Operators and punctuation.
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    NotEq,
    AndAnd,
    OrOr,
    Bang,
    Question,
    Colon,
    Assign,
    Semi,
    Comma,
    Dot,
    LBracket,
    RBracket,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Hash,
}

/// What went wrong while lexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexErrorKind {
    UnterminatedString,
    UnterminatedBlockComment,
    /// Unclosed `<...>` path after `include`/`use`.
    UnterminatedPath,
    InvalidChar(char),
    /// Unsupported escape sequence in a string literal.
    InvalidEscape(char),
    /// Number literal that does not parse as an f64.
    InvalidNumber,
}

/// A lex error with the byte span it occurred at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexError {
    pub kind: LexErrorKind,
    pub span: Span,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            LexErrorKind::UnterminatedString => write!(f, "unterminated string literal"),
            LexErrorKind::UnterminatedBlockComment => write!(f, "unterminated block comment"),
            LexErrorKind::UnterminatedPath => write!(f, "unterminated include/use path"),
            LexErrorKind::InvalidChar(c) => write!(f, "invalid character `{c}`"),
            LexErrorKind::InvalidEscape(c) => write!(f, "invalid escape sequence `\\{c}`"),
            LexErrorKind::InvalidNumber => write!(f, "invalid number literal"),
        }?;
        write!(f, " at bytes {}..{}", self.span.start, self.span.end)
    }
}

impl std::error::Error for LexError {}

/// Compute the 1-based (line, column) of a byte offset in `src`.
/// Columns count characters, not bytes.
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, c) in src.char_indices() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Lex the entire source into a token list.
pub fn lex(src: &str) -> Result<Vec<(Token, Span)>, LexError> {
    Lexer::new(src).collect()
}

/// Streaming lexer over an OpenSCAD source string.
pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    /// True right after an `include`/`use` keyword: `<...>` lexes as a Path.
    expect_path: bool,
    /// Set after an error or end of input; the iterator then fuses to `None`.
    done: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            src,
            pos: 0,
            expect_path: false,
            done: false,
        }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        // `pos + offset` may land inside a multibyte char; `get` returns None
        // there instead of panicking.
        self.src
            .get(self.pos + offset..)
            .and_then(|s| s.chars().next())
    }

    fn bump(&mut self, c: char) {
        self.pos += c.len_utf8();
    }

    fn err(&self, kind: LexErrorKind, start: usize, end: usize) -> LexError {
        LexError {
            kind,
            span: Span { start, end },
        }
    }

    /// Skip whitespace and comments. Returns an error for an unterminated
    /// block comment.
    fn skip_trivia(&mut self) -> Result<(), LexError> {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => self.bump(c),
                Some('/') if self.peek_at(1) == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump(c);
                    }
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        match self.peek() {
                            Some('*') if self.peek_at(1) == Some('/') => {
                                self.pos += 2;
                                break;
                            }
                            Some(c) => self.bump(c),
                            None => {
                                return Err(self.err(
                                    LexErrorKind::UnterminatedBlockComment,
                                    start,
                                    self.pos,
                                ));
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn lex_ident_or_keyword(&mut self) -> (Token, Span) {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                self.bump(c);
            } else {
                break;
            }
        }
        let text = &self.src[start..self.pos];
        // Note: `assert` and `echo` intentionally lex as Ident — the parser
        // handles them.
        let token = match text {
            "module" => Token::Module,
            "function" => Token::Function,
            "if" => Token::If,
            "else" => Token::Else,
            "for" => Token::For,
            "intersection_for" => Token::IntersectionFor,
            "let" => Token::Let,
            "each" => Token::Each,
            "true" => Token::True,
            "false" => Token::False,
            "undef" => Token::Undef,
            "include" => Token::Include,
            "use" => Token::Use,
            _ => Token::Ident(text.to_string()),
        };
        if matches!(token, Token::Include | Token::Use) {
            self.expect_path = true;
        }
        (
            token,
            Span {
                start,
                end: self.pos,
            },
        )
    }

    fn lex_number(&mut self) -> Result<(Token, Span), LexError> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        // Exponent: only consume `e`/`E` when digits (optionally signed) follow.
        if matches!(self.peek(), Some('e') | Some('E')) {
            let after_e = match self.peek_at(1) {
                Some('+') | Some('-') => self.peek_at(2),
                other => other,
            };
            if after_e.is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1; // e/E
                if matches!(self.peek(), Some('+') | Some('-')) {
                    self.pos += 1;
                }
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        let text = &self.src[start..self.pos];
        let value: f64 = text
            .parse()
            .map_err(|_| self.err(LexErrorKind::InvalidNumber, start, self.pos))?;
        Ok((
            Token::Number(value),
            Span {
                start,
                end: self.pos,
            },
        ))
    }

    fn lex_string(&mut self) -> Result<(Token, Span), LexError> {
        let start = self.pos;
        self.pos += 1; // opening quote
        let mut value = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(self.err(LexErrorKind::UnterminatedString, start, self.pos));
                }
                Some('"') => {
                    self.pos += 1;
                    return Ok((
                        Token::Str(value),
                        Span {
                            start,
                            end: self.pos,
                        },
                    ));
                }
                Some('\\') => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    match self.peek() {
                        None => {
                            return Err(self.err(
                                LexErrorKind::UnterminatedString,
                                start,
                                self.pos,
                            ));
                        }
                        Some(c) => {
                            self.bump(c);
                            let resolved = match c {
                                '"' => '"',
                                '\\' => '\\',
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                'x' => self.hex_escape(esc_start, 'x', 2)?,
                                'u' => self.hex_escape(esc_start, 'u', 4)?,
                                'U' => self.hex_escape(esc_start, 'U', 6)?,
                                other => {
                                    return Err(self.err(
                                        LexErrorKind::InvalidEscape(other),
                                        esc_start,
                                        self.pos,
                                    ));
                                }
                            };
                            value.push(resolved);
                        }
                    }
                }
                Some(c) => {
                    value.push(c);
                    self.bump(c);
                }
            }
        }
    }

    /// Consume exactly `digits` hex digits of a `\x`/`\u`/`\U` escape (the
    /// prefix is already consumed) and resolve them to a code point.
    fn hex_escape(
        &mut self,
        esc_start: usize,
        kind: char,
        digits: usize,
    ) -> Result<char, LexError> {
        let digits_start = self.pos;
        for _ in 0..digits {
            match self.peek() {
                Some(d) if d.is_ascii_hexdigit() => self.pos += 1,
                _ => {
                    return Err(self.err(LexErrorKind::InvalidEscape(kind), esc_start, self.pos));
                }
            }
        }
        u32::from_str_radix(&self.src[digits_start..self.pos], 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| self.err(LexErrorKind::InvalidEscape(kind), esc_start, self.pos))
    }

    /// Lex `<...>` as a Path token (only right after `include`/`use`).
    fn lex_path(&mut self) -> Result<(Token, Span), LexError> {
        let start = self.pos;
        self.pos += 1; // '<'
        loop {
            match self.peek() {
                None | Some('\n' | '\t' | '\r') => {
                    return Err(self.err(LexErrorKind::UnterminatedPath, start, self.pos));
                }
                Some('>') => {
                    let path = self.src[start + 1..self.pos].to_string();
                    self.pos += 1;
                    return Ok((
                        Token::Path(path),
                        Span {
                            start,
                            end: self.pos,
                        },
                    ));
                }
                Some(c) => self.bump(c),
            }
        }
    }

    fn lex_token(&mut self) -> Result<(Token, Span), LexError> {
        let start = self.pos;
        let c = self.peek().expect("lex_token called at end of input");

        if c == '<' && self.expect_path {
            self.expect_path = false;
            return self.lex_path();
        }
        self.expect_path = false;

        if c.is_ascii_alphabetic() || c == '_' {
            return Ok(self.lex_ident_or_keyword());
        }
        if c.is_ascii_digit() || (c == '.' && self.peek_at(1).is_some_and(|d| d.is_ascii_digit())) {
            return self.lex_number();
        }
        if c == '"' {
            return self.lex_string();
        }
        if c == '$' {
            self.pos += 1;
            if !self
                .peek()
                .is_some_and(|d| d.is_ascii_alphabetic() || d == '_')
            {
                return Err(self.err(LexErrorKind::InvalidChar('$'), start, self.pos));
            }
            while self
                .peek()
                .is_some_and(|d| d.is_ascii_alphanumeric() || d == '_')
            {
                self.pos += 1;
            }
            let name = self.src[start + 1..self.pos].to_string();
            return Ok((
                Token::DollarIdent(name),
                Span {
                    start,
                    end: self.pos,
                },
            ));
        }

        // Operators and punctuation (longest match first).
        let two = |lexer: &mut Self, token: Token| {
            lexer.pos += 2;
            Ok((
                token,
                Span {
                    start,
                    end: lexer.pos,
                },
            ))
        };
        let next = self.peek_at(1);
        match (c, next) {
            ('<', Some('=')) => return two(self, Token::Le),
            ('>', Some('=')) => return two(self, Token::Ge),
            ('=', Some('=')) => return two(self, Token::EqEq),
            ('!', Some('=')) => return two(self, Token::NotEq),
            ('&', Some('&')) => return two(self, Token::AndAnd),
            ('|', Some('|')) => return two(self, Token::OrOr),
            _ => {}
        }
        let token = match c {
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => Token::Star,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '^' => Token::Caret,
            '<' => Token::Lt,
            '>' => Token::Gt,
            '!' => Token::Bang,
            '?' => Token::Question,
            ':' => Token::Colon,
            '=' => Token::Assign,
            ';' => Token::Semi,
            ',' => Token::Comma,
            '.' => Token::Dot,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            '(' => Token::LParen,
            ')' => Token::RParen,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            '#' => Token::Hash,
            other => {
                return Err(self.err(
                    LexErrorKind::InvalidChar(other),
                    start,
                    start + other.len_utf8(),
                ));
            }
        };
        self.bump(c);
        Ok((
            token,
            Span {
                start,
                end: self.pos,
            },
        ))
    }
}

impl Iterator for Lexer<'_> {
    type Item = Result<(Token, Span), LexError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if let Err(e) = self.skip_trivia() {
            self.done = true;
            return Some(Err(e));
        }
        if self.peek().is_none() {
            self.done = true;
            return None;
        }
        let result = self.lex_token();
        if result.is_err() {
            self.done = true;
        }
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Token> {
        lex(src).unwrap().into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn identifiers() {
        assert_eq!(
            toks("foo _bar baz42 x_y"),
            vec![
                Token::Ident("foo".into()),
                Token::Ident("_bar".into()),
                Token::Ident("baz42".into()),
                Token::Ident("x_y".into()),
            ]
        );
    }

    #[test]
    fn keywords_vs_identifiers() {
        assert_eq!(
            toks("module function if else for intersection_for let each true false undef modulex iff"),
            vec![
                Token::Module,
                Token::Function,
                Token::If,
                Token::Else,
                Token::For,
                Token::IntersectionFor,
                Token::Let,
                Token::Each,
                Token::True,
                Token::False,
                Token::Undef,
                Token::Ident("modulex".into()),
                Token::Ident("iff".into()),
            ]
        );
    }

    #[test]
    fn dollar_identifiers() {
        assert_eq!(
            toks("$fn $fa $fs $t $my_var"),
            vec![
                Token::DollarIdent("fn".into()),
                Token::DollarIdent("fa".into()),
                Token::DollarIdent("fs".into()),
                Token::DollarIdent("t".into()),
                Token::DollarIdent("my_var".into()),
            ]
        );
    }

    #[test]
    fn integer_numbers() {
        assert_eq!(
            toks("0 1 42 007"),
            vec![
                Token::Number(0.0),
                Token::Number(1.0),
                Token::Number(42.0),
                Token::Number(7.0),
            ]
        );
    }

    #[test]
    fn float_numbers() {
        assert_eq!(
            toks("2.5 0.125 10.0"),
            vec![
                Token::Number(2.5),
                Token::Number(0.125),
                Token::Number(10.0),
            ]
        );
    }

    #[test]
    fn leading_dot_float() {
        assert_eq!(toks(".5"), vec![Token::Number(0.5)]);
    }

    #[test]
    fn scientific_numbers() {
        assert_eq!(
            toks("1e3 1.5e-2 2E+4 .5e1"),
            vec![
                Token::Number(1000.0),
                Token::Number(0.015),
                Token::Number(20000.0),
                Token::Number(5.0),
            ]
        );
    }

    #[test]
    fn dot_is_not_a_number() {
        // member access: identifier '.' identifier
        assert_eq!(
            toks("v.x"),
            vec![
                Token::Ident("v".into()),
                Token::Dot,
                Token::Ident("x".into()),
            ]
        );
    }

    #[test]
    fn simple_string() {
        assert_eq!(toks(r#""hello""#), vec![Token::Str("hello".into())]);
    }

    #[test]
    fn string_escapes() {
        assert_eq!(
            toks(r#""a\"b\\c\nd\te\rf""#),
            vec![Token::Str("a\"b\\c\nd\te\rf".into())]
        );
    }

    #[test]
    fn unterminated_string_error_with_span() {
        let err = lex("x = \"oops").unwrap_err();
        assert_eq!(err.kind, LexErrorKind::UnterminatedString);
        assert_eq!(err.span, Span { start: 4, end: 9 });
    }

    #[test]
    fn invalid_escape_error() {
        let err = lex(r#""a\qb""#).unwrap_err();
        assert_eq!(err.kind, LexErrorKind::InvalidEscape('q'));
        assert_eq!(err.span, Span { start: 2, end: 4 });
    }

    #[test]
    fn single_char_operators() {
        assert_eq!(
            toks("+ - * / % ^ < > ! ? : = ; , . [ ] ( ) { } #"),
            vec![
                Token::Plus,
                Token::Minus,
                Token::Star,
                Token::Slash,
                Token::Percent,
                Token::Caret,
                Token::Lt,
                Token::Gt,
                Token::Bang,
                Token::Question,
                Token::Colon,
                Token::Assign,
                Token::Semi,
                Token::Comma,
                Token::Dot,
                Token::LBracket,
                Token::RBracket,
                Token::LParen,
                Token::RParen,
                Token::LBrace,
                Token::RBrace,
                Token::Hash,
            ]
        );
    }

    #[test]
    fn multi_char_operators() {
        assert_eq!(
            toks("<= >= == != && ||"),
            vec![
                Token::Le,
                Token::Ge,
                Token::EqEq,
                Token::NotEq,
                Token::AndAnd,
                Token::OrOr,
            ]
        );
        // adjacent, no spaces
        assert_eq!(
            toks("a<=b==c"),
            vec![
                Token::Ident("a".into()),
                Token::Le,
                Token::Ident("b".into()),
                Token::EqEq,
                Token::Ident("c".into()),
            ]
        );
    }

    #[test]
    fn line_comments_skipped() {
        assert_eq!(
            toks("a // comment ignored\nb"),
            vec![Token::Ident("a".into()), Token::Ident("b".into())]
        );
    }

    #[test]
    fn block_comments_skipped() {
        assert_eq!(
            toks("a /* multi\nline */ b"),
            vec![Token::Ident("a".into()), Token::Ident("b".into())]
        );
    }

    #[test]
    fn unterminated_block_comment_error() {
        let err = lex("a /* never ends").unwrap_err();
        assert_eq!(err.kind, LexErrorKind::UnterminatedBlockComment);
        assert_eq!(err.span, Span { start: 2, end: 15 });
    }

    #[test]
    fn include_path_mode() {
        assert_eq!(
            toks("include <lib/shapes.scad>"),
            vec![Token::Include, Token::Path("lib/shapes.scad".into())]
        );
    }

    #[test]
    fn use_path_mode() {
        assert_eq!(
            toks("use <utils.scad>\ncube();"),
            vec![
                Token::Use,
                Token::Path("utils.scad".into()),
                Token::Ident("cube".into()),
                Token::LParen,
                Token::RParen,
                Token::Semi,
            ]
        );
    }

    #[test]
    fn angle_brackets_are_comparisons_outside_path_mode() {
        assert_eq!(
            toks("a < b > c"),
            vec![
                Token::Ident("a".into()),
                Token::Lt,
                Token::Ident("b".into()),
                Token::Gt,
                Token::Ident("c".into()),
            ]
        );
    }

    #[test]
    fn unterminated_include_path_error() {
        let err = lex("include <no/close").unwrap_err();
        assert_eq!(err.kind, LexErrorKind::UnterminatedPath);
        assert_eq!(err.span, Span { start: 8, end: 17 });
    }

    #[test]
    fn invalid_char_error_with_span() {
        let err = lex("a @ b").unwrap_err();
        assert_eq!(err.kind, LexErrorKind::InvalidChar('@'));
        assert_eq!(err.span, Span { start: 2, end: 3 });
    }

    #[test]
    fn spans_are_byte_offsets() {
        let src = "foo 1.5";
        let tokens = lex(src).unwrap();
        assert_eq!(
            tokens[0],
            (Token::Ident("foo".into()), Span { start: 0, end: 3 })
        );
        assert_eq!(tokens[1], (Token::Number(1.5), Span { start: 4, end: 7 }));
    }

    #[test]
    fn line_col_computable_from_span() {
        let src = "a\nbb\n  c";
        let tokens = lex(src).unwrap();
        let c_span = tokens[2].1;
        assert_eq!(line_col(src, c_span.start), (3, 3));
        assert_eq!(line_col(src, 0), (1, 1));
        assert_eq!(line_col(src, 2), (2, 1));
    }

    #[test]
    fn iterator_api() {
        let mut lexer = Lexer::new("x=1;");
        assert_eq!(
            lexer.next(),
            Some(Ok((Token::Ident("x".into()), Span { start: 0, end: 1 })))
        );
        assert_eq!(
            lexer.next(),
            Some(Ok((Token::Assign, Span { start: 1, end: 2 })))
        );
        assert_eq!(
            lexer.next(),
            Some(Ok((Token::Number(1.0), Span { start: 2, end: 3 })))
        );
        assert_eq!(
            lexer.next(),
            Some(Ok((Token::Semi, Span { start: 3, end: 4 })))
        );
        assert_eq!(lexer.next(), None);
    }

    #[test]
    fn non_ascii_char_is_invalid_char_not_panic() {
        let err = lex("µ").unwrap_err();
        assert_eq!(err.kind, LexErrorKind::InvalidChar('µ'));
        assert_eq!(err.span, Span { start: 0, end: 2 });
        // Multibyte char right after an operator: the operator lookahead must
        // not slice mid-character.
        let err = lex("a<µ").unwrap_err();
        assert_eq!(err.kind, LexErrorKind::InvalidChar('µ'));
        assert_eq!(err.span, Span { start: 2, end: 4 });
    }

    #[test]
    fn hex_and_unicode_escapes() {
        assert_eq!(toks(r#""\x41""#), vec![Token::Str("A".into())]);
        assert_eq!(toks("\"\\u00e9\""), vec![Token::Str("é".into())]);
        assert_eq!(toks(r#""\U01F600""#), vec![Token::Str("\u{1F600}".into())]);
        assert_eq!(toks(r#""a\x20b""#), vec![Token::Str("a b".into())]);
    }

    #[test]
    fn bad_hex_escape_is_error() {
        let err = lex(r#""\xZZ""#).unwrap_err();
        assert_eq!(err.kind, LexErrorKind::InvalidEscape('x'));
        // Too few hex digits before the closing quote.
        let err = lex(r#""\u12""#).unwrap_err();
        assert_eq!(err.kind, LexErrorKind::InvalidEscape('u'));
        // Beyond the Unicode code-point range.
        let err = lex(r#""\UFFFFFF""#).unwrap_err();
        assert_eq!(err.kind, LexErrorKind::InvalidEscape('U'));
    }

    #[test]
    fn include_path_stops_at_newline() {
        let err = lex("include <foo\nbar>").unwrap_err();
        assert_eq!(err.kind, LexErrorKind::UnterminatedPath);
        assert_eq!(err.span, Span { start: 8, end: 12 });
        let err = lex("use <a\tb>").unwrap_err();
        assert_eq!(err.kind, LexErrorKind::UnterminatedPath);
    }

    #[test]
    fn realistic_snippet() {
        let src = r#"
            // set resolution
            $fn = 64;
            module ring(r = 10, h) {
                difference() {
                    cylinder(h = h, r = r);
                    cylinder(h = h + 0.1, r = r * 0.8);
                }
            }
            if (x >= 1 && !done) { #ring(); }
        "#;
        let tokens = lex(src).unwrap();
        assert!(tokens.contains(&(Token::DollarIdent("fn".into()), Span { start: 43, end: 46 })));
        assert!(tokens.iter().any(|(t, _)| *t == Token::Module));
        assert!(tokens.iter().any(|(t, _)| *t == Token::Ge));
        assert!(tokens.iter().any(|(t, _)| *t == Token::AndAnd));
        assert!(tokens.iter().any(|(t, _)| *t == Token::Hash));
    }
}
