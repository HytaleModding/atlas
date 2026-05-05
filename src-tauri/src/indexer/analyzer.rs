//! Code-aware tokenizer.
//!
//! Java identifiers come in three flavours we want searchable:
//!   - Whole identifier: `getComponent` → search "getComponent" hits
//!   - camelCase parts: `getComponent` → search "component" hits
//!   - underscore parts: `CONSTANT_NAME` → search "constant" hits
//!
//! Plus dotted paths (`com.hypixel.hytale`) should tokenize as three
//! consecutive tokens so phrase queries like `"com.hypixel.hytale"` work.
//!
//! The implementation splits on any byte that is not ASCII alphanumeric
//! or `_`, producing an identifier sequence. Each identifier is emitted
//! as its full lowercased form, followed by its camel/underscore parts
//! at the SAME position so a query for the part will hit the document
//! at the same place as the full token. Positions only increment when
//! we cross an identifier boundary (whitespace, dot, punctuation, etc.).

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

pub const CODE_TOKENIZER: &str = "code";

/// Stateless code tokenizer. Cloned per thread by Tantivy.
#[derive(Clone, Default)]
pub struct CodeTokenizer;

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let tokens = tokenize(text);
        CodeTokenStream {
            tokens,
            idx: 0,
            current: Token::default(),
        }
    }
}

/// Owns a pre-computed Vec of tokens. No borrow against the source text.
#[derive(Debug, Default)]
pub struct CodeTokenStream {
    tokens: Vec<Token>,
    idx: usize,
    current: Token,
}

impl TokenStream for CodeTokenStream {
    fn advance(&mut self) -> bool {
        if self.idx >= self.tokens.len() {
            return false;
        }
        self.current = self.tokens[self.idx].clone();
        self.idx += 1;
        true
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

/// Pre-compute the full token list for a text. Called once per document
/// field, then advanced as a Vec<Token> stream.
fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut position: usize = 0;
    while i < bytes.len() {
        // Skip non-identifier bytes
        while i < bytes.len() && !is_ident_byte(bytes[i]) {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && is_ident_byte(bytes[i]) {
            i += 1;
        }
        let end = i;
        let ident = &text[start..end];

        // Emit the full identifier (lowercased).
        let lower = ident.to_ascii_lowercase();
        out.push(Token {
            offset_from: start,
            offset_to: end,
            position,
            text: lower,
            position_length: 1,
        });

        // Emit camelCase / underscore subparts at the SAME position so
        // a query for "component" scores against the same slot as
        // "getComponent". Skip if there's only one part.
        let parts = split_identifier(ident);
        if parts.len() > 1 {
            for (p_start, p_end) in parts {
                if p_end - p_start == end - start {
                    continue; // identical to the full token
                }
                let part = &ident[p_start..p_end];
                if part.is_empty() {
                    continue;
                }
                out.push(Token {
                    offset_from: start + p_start,
                    offset_to: start + p_end,
                    position,
                    text: part.to_ascii_lowercase(),
                    position_length: 1,
                });
            }
        }

        position += 1;
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Split an identifier into sub-parts by camelCase and underscores.
/// Returns (start, end) byte ranges inside the identifier.
///
/// Examples:
///   `getComponent`    → [(0,3)"get", (3,12)"Component"]
///   `HTMLParser`      → [(0,4)"HTML", (4,10)"Parser"]
///   `snake_case_name` → [(0,5), (6,10), (11,15)]
///   `CONSTANT_NAME`   → [(0,8)"CONSTANT", (9,13)"NAME"]
///   `x`               → [(0,1)]
fn split_identifier(ident: &str) -> Vec<(usize, usize)> {
    let bytes = ident.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'_' {
            if i > start {
                out.push((start, i));
            }
            i += 1;
            start = i;
            continue;
        }
        if i > start {
            let prev = bytes[i - 1];
            // lowercase / digit → uppercase: split before current
            if b.is_ascii_uppercase() && (prev.is_ascii_lowercase() || prev.is_ascii_digit()) {
                out.push((start, i));
                start = i;
            } else if b.is_ascii_uppercase() && prev.is_ascii_uppercase() {
                // Acronym boundary: split before the FINAL uppercase if it
                // precedes a lowercase run (HTMLParser → HTML, Parser).
                if i + 1 < bytes.len() && bytes[i + 1].is_ascii_lowercase() {
                    out.push((start, i));
                    start = i;
                }
            } else {
                // Letter→digit boundary: split. We deliberately do NOT split
                // digit→letter so identifier tails like `2d`, `64bit`, `v2`
                // stay one searchable subpart.
                if prev.is_ascii_alphabetic() && b.is_ascii_digit() {
                    out.push((start, i));
                    start = i;
                }
            }
        }
        i += 1;
    }
    if start < bytes.len() {
        out.push((start, bytes.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(ts: &[Token]) -> Vec<&str> {
        ts.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn plain_identifier() {
        let ts = tokenize("PageManager");
        assert_eq!(texts(&ts), vec!["pagemanager", "page", "manager"]);
    }

    #[test]
    fn camel_case_get_component() {
        let ts = tokenize("getComponent");
        assert_eq!(texts(&ts), vec!["getcomponent", "get", "component"]);
    }

    #[test]
    fn acronym_split() {
        let ts = tokenize("HTMLParser");
        assert_eq!(texts(&ts), vec!["htmlparser", "html", "parser"]);
    }

    #[test]
    fn underscore_constant() {
        let ts = tokenize("CONSTANT_NAME");
        assert_eq!(texts(&ts), vec!["constant_name", "constant", "name"]);
    }

    #[test]
    fn dotted_fqn_phrase_positions() {
        let ts = tokenize("com.hypixel.hytale");
        let positions: Vec<u32> = ts.iter().map(|t| t.position as u32).collect();
        assert_eq!(texts(&ts), vec!["com", "hypixel", "hytale"]);
        assert_eq!(positions, vec![0, 1, 2]);
    }

    #[test]
    fn number_in_identifier() {
        let ts = tokenize("world2d");
        assert_eq!(texts(&ts), vec!["world2d", "world", "2d"]);
    }

    #[test]
    fn mixed_code_fragment() {
        let ts = tokenize("PageManager pm = player.getComponent()");
        let texts = texts(&ts);
        assert!(texts.contains(&"pagemanager"));
        assert!(texts.contains(&"page"));
        assert!(texts.contains(&"pm"));
        assert!(texts.contains(&"player"));
        assert!(texts.contains(&"getcomponent"));
        assert!(texts.contains(&"get"));
        assert!(texts.contains(&"component"));
    }
}
