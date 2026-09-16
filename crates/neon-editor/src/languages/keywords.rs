//! Built-in keyword completion tables for non-Flow languages.
//!
//! Lightweight static completions give editors instant feedback before an
//! LSP server connects; at the runtime layer LSP completions supersede these
//! for real semantic candidates. The kernel stays provider-pluggable: these
//! tables are part of the kernel only because they are pure data.

use super::LanguageKind;

/// Static keyword candidates for one language (lowercase ASCII).
pub fn keywords_for(kind: LanguageKind) -> &'static [&'static str] {
    match kind {
        LanguageKind::NuiFlow => &[],
        LanguageKind::Typescript => &[
            "abstract", "any", "as", "async", "await", "boolean", "break", "case", "catch",
            "class", "const", "constructor", "continue", "debugger", "declare", "default",
            "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for",
            "from", "function", "get", "if", "implements", "import", "in", "infer",
            "instanceof", "interface", "is", "keyof", "let", "module", "namespace", "never",
            "new", "null", "number", "object", "of", "package", "private", "protected",
            "public", "readonly", "return", "satisfies", "set", "static", "string", "super",
            "switch", "symbol", "this", "throw", "true", "try", "type", "typeof", "undefined",
            "unique", "unknown", "var", "void", "while", "with", "yield",
        ],
        LanguageKind::Rust => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
            "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
            "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
            "super", "trait", "true", "type", "unsafe", "use", "where", "while",
        ],
        LanguageKind::Cpp => &[
            "alignas", "alignof", "and", "asm", "auto", "bool", "break", "case", "catch",
            "char", "class", "const", "consteval", "constexpr", "constinit", "const_cast",
            "continue", "co_await", "co_return", "co_yield", "decltype", "default", "delete",
            "do", "double", "dynamic_cast", "else", "enum", "explicit", "export", "extern",
            "false", "float", "for", "friend", "goto", "if", "inline", "int", "long",
            "mutable", "namespace", "new", "noexcept", "not", "nullptr", "operator", "or",
            "private", "protected", "public", "register", "reinterpret_cast", "requires",
            "return", "short", "signed", "sizeof", "static", "static_assert", "static_cast",
            "struct", "switch", "template", "this", "thread_local", "throw", "true", "try",
            "typedef", "typeid", "typename", "union", "unsigned", "using", "virtual", "void",
            "volatile", "wchar_t", "while", "xor",
        ],
    }
}
