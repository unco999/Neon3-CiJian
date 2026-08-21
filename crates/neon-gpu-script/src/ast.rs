//! Abstract syntax tree for the neon-gpu-script language.

use crate::error::Pos;

/// A two-part qualified world name: `domain.name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedName {
    pub domain: String,
    pub name: String,
}

impl std::fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.domain, self.name)
    }
}

/// A parsed script: schema version plus a list of scenes.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub schema_version: Option<f64>,
    pub scenes: Vec<Scene>,
}

/// One compilation unit with its world contract and body.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    pub name: String,
    pub pos: Pos,
    pub inputs: Vec<InputDecl>,
    pub outputs: Vec<QualifiedName>,
    pub body: Vec<Stmt>,
}

/// `domain.name as alias` — binds a world resource to a local SSA name.
#[derive(Debug, Clone, PartialEq)]
pub struct InputDecl {
    pub world: QualifiedName,
    pub alias: String,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let(LetStmt),
    Export(ExportStmt),
}

/// `let name = kernel(args)` — a fixed-kernel call producing one SSA value.
#[derive(Debug, Clone, PartialEq)]
pub struct LetStmt {
    pub name: String,
    pub pos: Pos,
    pub kernel: String,
    pub args: Vec<Arg>,
}

/// `export domain.name = source` — the single writer of a world resource.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportStmt {
    pub target: QualifiedName,
    pub source: String,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// Positional argument: a value reference (`dmg`) or a literal constant
    /// (`2.0`). Literal constants are baked at compile time.
    Pos(ArgValue),
    /// A nested kernel call; the compiler hoists it into an anonymous SSA node.
    Call(Box<CallExpr>),
    /// Named argument: either a compile-time constant or a value reference.
    Named {
        key: String,
        value: ArgValue,
        pos: Pos,
    },
}

/// A kernel call used as a nested argument (`mul(dmg, 2.0)` inside `select`).
#[derive(Debug, Clone, PartialEq)]
pub struct CallExpr {
    pub kernel: String,
    pub args: Vec<Arg>,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArgValue {
    Number(f64),
    Str(String),
    /// A reference to a previously defined value (e.g. `seed=frame`).
    Ident(String),
}
