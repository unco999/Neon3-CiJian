//! Recursive-descent parser for the neon-gpu-script language.

use crate::ast::{
    Arg, ArgValue, CallExpr, ExportStmt, InputDecl, LetStmt, QualifiedName, Scene, Script, Stmt,
};
use crate::error::{Pos, ScriptError};
use crate::lexer::{Token, TokenKind};

pub fn parse(tokens: &[Token]) -> Result<Script, ScriptError> {
    let mut p = Parser { tokens, idx: 0 };
    let mut schema_version = None;
    let mut scenes = Vec::new();

    while !p.at_end() {
        let (kw, pos) = p.peek_ident()?;
        match kw.as_str() {
            "schema_version" => {
                p.bump();
                p.expect(TokenKind::Colon, "expected `:` after schema_version")?;
                let (n, _) = p.expect_number()?;
                schema_version = Some(n);
            }
            "scene" => scenes.push(p.parse_scene()?),
            other => {
                return Err(ScriptError::Parse {
                    pos,
                    msg: format!(
                        "unexpected keyword `{other}` (expected `scene` or `schema_version`)"
                    ),
                });
            }
        }
    }

    Ok(Script {
        schema_version,
        scenes,
    })
}

struct Parser<'a> {
    tokens: &'a [Token],
    idx: usize,
}

impl<'a> Parser<'a> {
    fn at_end(&self) -> bool {
        self.idx >= self.tokens.len()
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.idx].clone();
        self.idx += 1;
        t
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.idx)
    }

    fn peek_ident(&self) -> Result<(String, Pos), ScriptError> {
        match self.peek() {
            Some(Token {
                kind: TokenKind::Ident(s),
                pos,
            }) => Ok((s.clone(), *pos)),
            Some(t) => Err(ScriptError::Parse {
                pos: t.pos,
                msg: format!("expected identifier, found `{:?}`", t.kind),
            }),
            None => Err(ScriptError::Parse {
                pos: 0,
                msg: "unexpected end of script".into(),
            }),
        }
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Result<Token, ScriptError> {
        match self.peek() {
            Some(t) if t.kind == kind => Ok(self.bump()),
            Some(t) => Err(ScriptError::Parse {
                pos: t.pos,
                msg: format!("expected {what}, found `{:?}`", t.kind),
            }),
            None => Err(ScriptError::Parse {
                pos: 0,
                msg: format!("expected {what}, found end of script"),
            }),
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<(String, Pos), ScriptError> {
        match self.peek() {
            Some(Token {
                kind: TokenKind::Ident(s),
                pos,
            }) => {
                let (s, pos) = (s.clone(), *pos);
                self.bump();
                Ok((s, pos))
            }
            Some(t) => Err(ScriptError::Parse {
                pos: t.pos,
                msg: format!("expected {what}, found `{:?}`", t.kind),
            }),
            None => Err(ScriptError::Parse {
                pos: 0,
                msg: format!("expected {what}, found end of script"),
            }),
        }
    }

    fn expect_number(&mut self) -> Result<(f64, Pos), ScriptError> {
        match self.peek() {
            Some(Token {
                kind: TokenKind::Number(n),
                pos,
            }) => {
                let (n, pos) = (*n, *pos);
                self.bump();
                Ok((n, pos))
            }
            Some(t) => Err(ScriptError::Parse {
                pos: t.pos,
                msg: format!("expected number, found `{:?}`", t.kind),
            }),
            None => Err(ScriptError::Parse {
                pos: 0,
                msg: "expected number, found end of script".into(),
            }),
        }
    }

    fn parse_qualified(&mut self, what: &str) -> Result<(QualifiedName, Pos), ScriptError> {
        let (domain, pos) = self.expect_ident(what)?;
        self.expect(TokenKind::Dot, "expected `.` in qualified name")?;
        let (name, _) = self.expect_ident("name after `.`")?;
        if let Some(Token {
            kind: TokenKind::Dot,
            pos: dot_pos,
        }) = self.peek()
        {
            return Err(ScriptError::Parse {
                pos: *dot_pos,
                msg: "qualified names are two-part `domain.name`; nested namespaces are not supported"
                    .into(),
            });
        }
        Ok((QualifiedName { domain, name }, pos))
    }

    fn parse_scene(&mut self) -> Result<Scene, ScriptError> {
        self.expect(TokenKind::Ident("scene".into()), "`scene`")?;
        let (name, pos) = self.expect_ident("scene name")?;
        if matches!(
            self.peek(),
            Some(Token {
                kind: TokenKind::Eq,
                ..
            })
        ) {
            self.bump();
        }
        self.expect(TokenKind::Lbrace, "`{` after scene name")?;

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut body = Vec::new();

        loop {
            match self.peek() {
                Some(Token {
                    kind: TokenKind::Rbrace,
                    ..
                }) => {
                    self.bump();
                    break;
                }
                Some(Token {
                    kind: TokenKind::Ident(kw),
                    pos,
                }) => {
                    let kw = kw.clone();
                    let kw_pos = *pos;
                    match kw.as_str() {
                        "input" => {
                            self.bump();
                            self.expect(TokenKind::Colon, "`:` after input")?;
                            loop {
                                let (world, dpos) = self.parse_qualified("world resource name")?;
                                let alias = match self.peek() {
                                    Some(Token {
                                        kind: TokenKind::Ident(a),
                                        ..
                                    }) if a == "as" => {
                                        self.bump();
                                        let (a, _) = self.expect_ident("alias after `as`")?;
                                        a
                                    }
                                    _ => world.name.clone(),
                                };
                                inputs.push(InputDecl {
                                    world,
                                    alias,
                                    pos: dpos,
                                });
                                match self.peek() {
                                    Some(Token {
                                        kind: TokenKind::Comma,
                                        ..
                                    }) => {
                                        self.bump();
                                    }
                                    _ => break,
                                }
                            }
                        }
                        "output" => {
                            self.bump();
                            self.expect(TokenKind::Colon, "`:` after output")?;
                            loop {
                                let (q, _) = self.parse_qualified("output world resource name")?;
                                outputs.push(q);
                                match self.peek() {
                                    Some(Token {
                                        kind: TokenKind::Comma,
                                        ..
                                    }) => {
                                        self.bump();
                                    }
                                    _ => break,
                                }
                            }
                        }
                        "body" => {
                            self.bump();
                            self.expect(TokenKind::Colon, "`:` after body")?;
                            loop {
                                match self.peek() {
                                    Some(Token {
                                        kind: TokenKind::Rbrace,
                                        ..
                                    }) => break,
                                    None => {
                                        return Err(ScriptError::Parse {
                                            pos: kw_pos,
                                            msg: "unterminated scene body".into(),
                                        });
                                    }
                                    _ => body.push(self.parse_stmt()?),
                                }
                            }
                        }
                        other => {
                            return Err(ScriptError::Parse {
                                pos: kw_pos,
                                msg: format!(
                                    "unexpected keyword `{other}` in scene (expected `input`, `output`, `body` or `}}`)"
                                ),
                            });
                        }
                    }
                }
                Some(t) => {
                    return Err(ScriptError::Parse {
                        pos: t.pos,
                        msg: format!("expected keyword or `}}`, found `{:?}`", t.kind),
                    });
                }
                None => {
                    return Err(ScriptError::Parse {
                        pos: 0,
                        msg: "unterminated scene (missing `}`)".into(),
                    });
                }
            }
        }

        Ok(Scene {
            name,
            pos,
            inputs,
            outputs,
            body,
        })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ScriptError> {
        let (kw, pos) = self.peek_ident()?;
        match kw.as_str() {
            "let" => self.parse_let().map(Stmt::Let),
            "export" => self.parse_export().map(Stmt::Export),
            other => Err(ScriptError::Parse {
                pos,
                msg: format!("expected `let` or `export`, found `{other}`"),
            }),
        }
    }

    fn parse_let(&mut self) -> Result<LetStmt, ScriptError> {
        self.expect(TokenKind::Ident("let".into()), "`let`")?;
        let (name, pos) = self.expect_ident("value name")?;
        self.expect(TokenKind::Eq, "`=` after value name")?;
        let (kernel, _) = self.expect_ident("kernel name")?;
        let args = self.parse_call_args()?;
        Ok(LetStmt {
            name,
            pos,
            kernel,
            args,
        })
    }

    fn parse_call_args(&mut self) -> Result<Vec<Arg>, ScriptError> {
        self.expect(TokenKind::Lparen, "`(` after kernel name")?;
        let mut args = Vec::new();
        loop {
            match self.peek() {
                Some(Token {
                    kind: TokenKind::Rparen,
                    ..
                }) => {
                    self.bump();
                    break;
                }
                None => {
                    return Err(ScriptError::Parse {
                        pos: 0,
                        msg: "unterminated kernel call".into(),
                    });
                }
                _ => {
                    args.push(self.parse_arg()?);
                    match self.peek() {
                        Some(Token {
                            kind: TokenKind::Comma,
                            ..
                        }) => {
                            self.bump();
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(args)
    }

    fn parse_arg(&mut self) -> Result<Arg, ScriptError> {
        let (first, pos) = match self.peek() {
            Some(Token {
                kind: TokenKind::Number(n),
                ..
            }) => {
                let n = *n;
                self.bump();
                return Ok(Arg::Pos(ArgValue::Number(n)));
            }
            Some(Token {
                kind: TokenKind::Ident(s),
                pos,
            }) => {
                let (s, pos) = (s.clone(), *pos);
                self.bump();
                (s, pos)
            }
            Some(t) => {
                return Err(ScriptError::Parse {
                    pos: t.pos,
                    msg: format!("expected argument, found `{:?}`", t.kind),
                });
            }
            None => {
                return Err(ScriptError::Parse {
                    pos: 0,
                    msg: "expected argument, found end of script".into(),
                });
            }
        };
        // `first` is an identifier here.
        if matches!(
            self.peek(),
            Some(Token {
                kind: TokenKind::Lparen,
                ..
            })
        ) {
            let args = self.parse_call_args()?;
            return Ok(Arg::Call(Box::new(CallExpr {
                kernel: first,
                args,
                pos,
            })));
        }
        match self.peek() {
            Some(Token {
                kind: TokenKind::Eq,
                ..
            }) => {
                self.bump();
                let value = match self.peek() {
                    Some(Token {
                        kind: TokenKind::Number(n),
                        ..
                    }) => {
                        let n = *n;
                        self.bump();
                        ArgValue::Number(n)
                    }
                    Some(Token {
                        kind: TokenKind::Str(s),
                        ..
                    }) => {
                        let s = s.clone();
                        self.bump();
                        ArgValue::Str(s)
                    }
                    Some(Token {
                        kind: TokenKind::Ident(s),
                        ..
                    }) => {
                        let s = s.clone();
                        self.bump();
                        ArgValue::Ident(s)
                    }
                    Some(t) => {
                        return Err(ScriptError::Parse {
                            pos: t.pos,
                            msg: format!(
                                "expected constant or value reference, found `{:?}`",
                                t.kind
                            ),
                        });
                    }
                    None => {
                        return Err(ScriptError::Parse {
                            pos,
                            msg: "expected constant or value reference, found end of script".into(),
                        });
                    }
                };
                Ok(Arg::Named {
                    key: first,
                    value,
                    pos,
                })
            }
            _ => Ok(Arg::Pos(ArgValue::Ident(first))),
        }
    }

    fn parse_export(&mut self) -> Result<ExportStmt, ScriptError> {
        self.expect(TokenKind::Ident("export".into()), "`export`")?;
        let (target, pos) = self.parse_qualified("export target world resource name")?;
        self.expect(TokenKind::Eq, "`=` after export target")?;
        let (source, _) = self.expect_ident("source value name")?;
        Ok(ExportStmt {
            target,
            source,
            pos,
        })
    }
}
