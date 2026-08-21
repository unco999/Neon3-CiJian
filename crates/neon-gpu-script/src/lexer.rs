//! Tokenizer for the neon-gpu-script language.

use crate::error::{Pos, ScriptError};

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Number(f64),
    Str(String),
    Dot,
    Comma,
    Colon,
    Eq,
    Lbrace,
    Rbrace,
    Lparen,
    Rparen,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub pos: Pos,
}

pub fn tokenize(src: &str) -> Result<Vec<Token>, ScriptError> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();

    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            ' ' | '\t' | '\r' | '\n' => i += 1,
            '#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '.' => {
                out.push(Token {
                    kind: TokenKind::Dot,
                    pos: i,
                });
                i += 1;
            }
            ',' => {
                out.push(Token {
                    kind: TokenKind::Comma,
                    pos: i,
                });
                i += 1;
            }
            ':' => {
                out.push(Token {
                    kind: TokenKind::Colon,
                    pos: i,
                });
                i += 1;
            }
            '=' => {
                out.push(Token {
                    kind: TokenKind::Eq,
                    pos: i,
                });
                i += 1;
            }
            '{' => {
                out.push(Token {
                    kind: TokenKind::Lbrace,
                    pos: i,
                });
                i += 1;
            }
            '}' => {
                out.push(Token {
                    kind: TokenKind::Rbrace,
                    pos: i,
                });
                i += 1;
            }
            '(' => {
                out.push(Token {
                    kind: TokenKind::Lparen,
                    pos: i,
                });
                i += 1;
            }
            ')' => {
                out.push(Token {
                    kind: TokenKind::Rparen,
                    pos: i,
                });
                i += 1;
            }
            '-' => {
                let start = i;
                i += 1;
                if i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    let n = lex_number(bytes, &mut i, start)?;
                    out.push(n);
                } else {
                    return Err(ScriptError::Lex {
                        pos: start,
                        msg: "unexpected `-`".into(),
                    });
                }
            }
            '"' => {
                let start = i;
                i += 1;
                let mut s = String::new();
                while i < bytes.len() && bytes[i] != b'"' {
                    s.push(bytes[i] as char);
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(ScriptError::Lex {
                        pos: start,
                        msg: "unterminated string literal".into(),
                    });
                }
                i += 1;
                out.push(Token {
                    kind: TokenKind::Str(s),
                    pos: start,
                });
            }
            _ if c.is_ascii_digit() => {
                let pos = i;
                let n = lex_number(bytes, &mut i, pos)?;
                out.push(n);
            }
            _ if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < bytes.len() {
                    let ch = bytes[i] as char;
                    if ch.is_alphanumeric() || ch == '_' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(Token {
                    kind: TokenKind::Ident(src[start..i].to_string()),
                    pos: start,
                });
            }
            other => {
                return Err(ScriptError::Lex {
                    pos: i,
                    msg: format!("unexpected character `{other}`"),
                });
            }
        }
    }
    Ok(out)
}

fn lex_number(bytes: &[u8], i: &mut usize, start: Pos) -> Result<Token, ScriptError> {
    let mut has_dot = false;
    while *i < bytes.len() {
        let ch = bytes[*i] as char;
        if ch.is_ascii_digit() {
            *i += 1;
        } else if ch == '.' && !has_dot {
            has_dot = true;
            *i += 1;
        } else {
            break;
        }
    }
    let text = std::str::from_utf8(&bytes[start..*i]).map_err(|_| ScriptError::Lex {
        pos: start,
        msg: "invalid number literal".into(),
    })?;
    let value = text.parse::<f64>().map_err(|_| ScriptError::Lex {
        pos: start,
        msg: format!("invalid number literal `{text}`"),
    })?;
    Ok(Token {
        kind: TokenKind::Number(value),
        pos: start,
    })
}
