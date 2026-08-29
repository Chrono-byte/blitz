// SPDX-License-Identifier: MPL-2.0
//! Lexical analyzer for tokenizing TypeScript/JavaScript source.
//!
//! The lexer transforms raw source code into a stream of [`Token`] values for
//! callers that need standalone tokenization. It handles:
//! - Keywords (let, const, function, if, etc.)
//! - Operators (+, -, *, /, ===, etc.)
//! - Literals (numbers, strings, booleans, null)
//! - Identifiers and type annotations
//! - Comments (single-line // and multi-line /* */)
//!
//! # Tokenization
//!
//! Use [`Lexer::tokenize`] to convert an entire source string into a vector
//! of tokens, or [`Lexer::next_token`] for incremental lexing.
//!
//! # Example
//!
//! ```
//! use denko::lexer::Lexer;
//!
//! let mut lexer = Lexer::new(denko::js!("let x = 42;"));
//! let tokens = lexer.tokenize().unwrap();
//! // tokens contains: Let, Ident("x"), Assign, Number(42.0), Semicolon, Eof
//! ```

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
/// Token produced by lexical analysis
pub enum Token {
  /// Number literal
  Number(f64),
  /// String literal
  String(String),
  /// True keyword
  True,
  /// False keyword
  False,
  /// Null keyword
  Null,
  /// Identifier or unreserved keyword
  Ident(String),
  /// Let keyword
  Let,
  /// Const keyword
  Const,
  /// If keyword
  If,
  /// Else keyword
  Else,
  /// While keyword
  While,
  /// Return keyword
  Return,
  /// Function keyword
  Function,
  /// Typeof keyword
  Typeof,
  /// Plus operator (+)
  Plus,
  /// Minus operator (-)
  Minus,
  /// Star operator (*)
  Star,
  /// Slash operator (/)
  Slash,
  /// Percent operator (%)
  Percent,
  /// Assignment operator (=)
  Eq,
  /// Equality operator (==)
  EqEq,
  /// Strict equality operator (===)
  EqEqEq,
  /// Not operator (!)
  Bang,
  /// Inequality operator (!=)
  BangEq,
  /// Strict inequality operator (!==)
  BangEqEq,
  /// Less than operator (<)
  Lt,
  /// Less than or equal operator (<=)
  Le,
  /// Greater than operator (>)
  Gt,
  /// Greater than or equal operator (>=)
  Ge,
  /// Logical AND operator (&&)
  AndAnd,
  /// Logical OR operator (||)
  OrOr,
  /// Left parenthesis (
  LParen,
  /// Right parenthesis )
  RParen,
  /// Left brace {
  LBrace,
  /// Right brace }
  RBrace,
  /// Left bracket [
  LBracket,
  /// Right bracket ]
  RBracket,
  /// Semicolon ;
  Semicolon,
  /// Comma ,
  Comma,
  /// Dot operator .
  Dot,
  /// Assignment operator =
  Assign,
  /// Colon :
  Colon,
  /// Arrow operator =>
  Arrow,
  /// Number type keyword
  NumberType,
  /// String type keyword
  StringType,
  /// Boolean type keyword
  BooleanType,
  /// Null type keyword
  NullType,
  /// Void type keyword
  VoidType,
  /// End of file
  Eof,
}

impl fmt::Display for Token {
  fn fmt(
    &self,
    f: &mut fmt::Formatter,
  ) -> fmt::Result {
    match self {
      Token::Number(n) => write!(f, "{}", n),
      Token::String(s) => write!(f, "\"{}\"", s),
      Token::Ident(s) => write!(f, "{}", s),
      Token::Eof => write!(f, "EOF"),
      t => write!(f, "{:?}", t),
    }
  }
}

/// Lexical analyzer that tokenizes source code
pub struct Lexer {
  /// Source code as characters
  source: Vec<char>,
  /// Current position in source
  pos: usize,
}

impl Lexer {
  /// Creates a new lexer for the given source code
  pub fn new(source: &str) -> Self {
    Self {
      source: source.chars().collect(),
      pos: 0,
    }
  }

  fn peek(&self) -> Option<char> {
    self.source.get(self.pos).copied()
  }

  fn peek_next(&self) -> Option<char> {
    self.source.get(self.pos + 1).copied()
  }

  fn advance(&mut self) -> Option<char> {
    let c = self.peek()?;
    self.pos += 1;
    Some(c)
  }

  fn advance_if(
    &mut self,
    expected: char,
  ) -> bool {
    if self.peek() == Some(expected) {
      self.pos += 1;
      true
    } else {
      false
    }
  }

  fn skip_whitespace_and_comments(&mut self) {
    while let Some(c) = self.peek() {
      if c.is_whitespace() {
        self.advance();
      } else if c == '/' && self.peek_next() == Some('/') {
        self.advance();
        self.advance();
        while self.peek().is_some_and(|c| c != '\n') {
          self.advance();
        }
      } else if c == '/' && self.peek_next() == Some('*') {
        self.advance();
        self.advance();
        while self.peek().is_some() {
          if self.advance() == Some('*') && self.peek() == Some('/') {
            self.advance();
            self.advance();
            break;
          }
        }
      } else {
        break;
      }
    }
  }

  fn read_number(
    &mut self,
    first: char,
  ) -> Token {
    let mut s = String::from(first);
    while let Some(c) = self.peek() {
      if c.is_ascii_digit() || c == '.' {
        s.push(c);
        self.advance();
      } else {
        break;
      }
    }
    Token::Number(s.parse().unwrap_or(0.0))
  }

  fn read_string(
    &mut self,
    quote: char,
  ) -> Result<Token, String> {
    let mut s = String::new();
    while let Some(c) = self.advance() {
      if c == quote {
        crate::trace::string("lexer read_string", &s);
        return Ok(Token::String(s));
      }
      if c == '\\' {
        match self.advance() {
          Some('n') => s.push('\n'),
          Some('t') => s.push('\t'),
          Some('r') => s.push('\r'),
          Some('\\') => s.push('\\'),
          Some(q) if q == quote => s.push(quote),
          Some(x) => s.push(x),
          None => return Err("Unterminated string".into()),
        }
      } else {
        s.push(c);
      }
    }
    Err("Unterminated string".into())
  }

  fn read_ident_or_keyword(
    &mut self,
    first: char,
  ) -> Token {
    let mut s = String::from(first);
    while let Some(c) = self.peek() {
      if c.is_alphabetic() || c == '_' || c == '$' || c.is_ascii_digit() {
        s.push(c);
        self.advance();
      } else {
        break;
      }
    }
    match s.as_str() {
      "let" => Token::Let,
      "const" => Token::Const,
      "if" => Token::If,
      "else" => Token::Else,
      "while" => Token::While,
      "return" => Token::Return,
      "function" => Token::Function,
      "typeof" => Token::Typeof,
      "true" => Token::True,
      "false" => Token::False,
      "null" => Token::Null,
      "number" => Token::NumberType,
      "string" => Token::StringType,
      "boolean" => Token::BooleanType,
      "void" => Token::VoidType,

      _ => Token::Ident(s),
    }
  }

  /// Returns the next token from the source code
  ///
  /// # Errors
  ///
  /// Returns an error string if an unexpected character is encountered
  pub fn next_token(&mut self) -> Result<Token, String> {
    self.skip_whitespace_and_comments();
    let c = match self.advance() {
      Some(x) => x,
      None => return Ok(Token::Eof),
    };
    Ok(match c {
      '(' => Token::LParen,
      ')' => Token::RParen,
      '{' => Token::LBrace,
      '}' => Token::RBrace,
      '[' => Token::LBracket,
      ']' => Token::RBracket,
      ';' => Token::Semicolon,
      ',' => Token::Comma,
      '.' => Token::Dot,
      ':' => Token::Colon,
      '+' => Token::Plus,
      '-' => Token::Minus,
      '*' => Token::Star,
      '/' => Token::Slash,
      '%' => Token::Percent,
      '!' => {
        if self.advance_if('=') {
          if self.advance_if('=') {
            Token::BangEqEq
          } else {
            Token::BangEq
          }
        } else {
          Token::Bang
        }
      }
      '=' => {
        if self.advance_if('>') {
          Token::Arrow
        } else if self.advance_if('=') {
          if self.advance_if('=') {
            Token::EqEqEq
          } else {
            Token::EqEq
          }
        } else {
          Token::Assign
        }
      }
      '<' => {
        if self.advance_if('=') {
          Token::Le
        } else {
          Token::Lt
        }
      }
      '>' => {
        if self.advance_if('=') {
          Token::Ge
        } else {
          Token::Gt
        }
      }
      '&' if self.peek() == Some('&') => {
        self.advance();
        Token::AndAnd
      }
      '|' if self.peek() == Some('|') => {
        self.advance();
        Token::OrOr
      }
      '"' | '\'' => return self.read_string(c),
      '0'..='9' => self.read_number(c),
      _ if c.is_alphabetic() || c == '_' || c == '$' => self.read_ident_or_keyword(c),
      _ => return Err(format!("Unexpected character: {}", c)),
    })
  }

  /// Tokenizes the entire source code into a vector of tokens
  ///
  /// # Errors
  ///
  /// Returns an error string if an unexpected character is encountered
  pub fn tokenize(mut self) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    loop {
      let t = self.next_token()?;
      let is_eof = t == Token::Eof;
      tokens.push(t);
      if is_eof {
        break;
      }
    }
    Ok(tokens)
  }
}
