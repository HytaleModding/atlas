//! Strip Java comments and string-literal contents from source before it
//! enters the Tantivy tokenizer.
//!
//! The inverted index would otherwise record tokens from comment prose and
//! string-literal text, which is a redistribution risk. A lexer-style state
//! machine walks the source and replaces non-source spans with a single
//! space. Embeddings are not affected: vectors are one-way, so the embedder
//! still sees the original text.

/// Strip line comments, block comments, and string-literal contents from
/// `src`. Char literals are preserved (single ASCII chars don't carry
/// proprietary content). Java text blocks (`"""..."""`) are treated like
/// strings: the triple-quote delimiters drop out, the body is blanked.
///
/// Output length is not preserved. Line breaks are preserved so any
/// downstream tooling that cares about line counts still works.
pub fn strip_for_indexing(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let n = bytes.len();

    while i < n {
        let b = bytes[i];

        // Line comment: //... up to newline (exclusive).
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            i += 2;
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            // Don't consume the newline; let it pass through normally.
            out.push(' ');
            continue;
        }

        // Block comment (incl. javadoc): /* ... */
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                // Preserve newlines so multi-line block comments don't
                // collapse the line numbering of the rest of the file.
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            // Skip closing */
            if i + 1 < n {
                i += 2;
            } else {
                i = n;
            }
            out.push(' ');
            continue;
        }

        // Text block: """ ... """ (Java 13+). Body is blanked; newlines
        // preserved for line-number alignment.
        if b == b'"' && i + 2 < n && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
            i += 3;
            // Find closing triple-quote, honouring backslash escapes.
            while i + 2 < n && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"') {
                if bytes[i] == b'\\' && i + 1 < n {
                    // Skip escape sequence (e.g. \" \\ \n).
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            if i + 2 < n {
                i += 3;
            } else {
                i = n;
            }
            out.push(' ');
            continue;
        }

        // Regular string literal: "..." with backslash escapes. Replace
        // the entire literal (quotes included) with a single space.
        if b == b'"' {
            i += 1;
            while i < n && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    // Unterminated string; bail to avoid eating the rest
                    // of the file. Java forbids this anyway.
                    break;
                }
                i += 1;
            }
            if i < n {
                i += 1; // closing quote
            }
            out.push(' ');
            continue;
        }

        // Char literal: '...'. Preserved verbatim - single chars are not
        // meaningful section content. Handled explicitly so the closing
        // quote isn't mistaken for an unterminated string.
        if b == b'\'' {
            out.push('\'');
            i += 1;
            while i < n && bytes[i] != b'\'' {
                if bytes[i] == b'\\' && i + 1 < n {
                    out.push(bytes[i] as char);
                    out.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                out.push(bytes[i] as char);
                i += 1;
            }
            if i < n {
                out.push('\'');
                i += 1;
            }
            continue;
        }

        // Default: pass through.
        out.push(b as char);
        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_comments() {
        let src = "int x = 1; // secret comment about Hypixel\nint y = 2;";
        let out = strip_for_indexing(src);
        assert!(!out.contains("secret"));
        assert!(!out.contains("Hypixel"));
        assert!(out.contains("int x"));
        assert!(out.contains("int y"));
    }

    #[test]
    fn strips_block_comments_including_javadoc() {
        let src = "/** Internal\n * docstring with proprietary terms\n */\nclass Foo {}";
        let out = strip_for_indexing(src);
        assert!(!out.contains("Internal"));
        assert!(!out.contains("proprietary"));
        assert!(!out.contains("docstring"));
        assert!(out.contains("class Foo"));
    }

    #[test]
    fn strips_string_literal_contents_keeping_identifiers() {
        let src = r#"String msg = "PageManager not found: error code 42"; manager.reset();"#;
        let out = strip_for_indexing(src);
        assert!(!out.contains("not found"));
        assert!(!out.contains("error code"));
        // Note: identifiers OUTSIDE the string survive.
        assert!(out.contains("String msg"));
        assert!(out.contains("manager.reset"));
    }

    #[test]
    fn handles_escaped_quotes_in_strings() {
        let src = r#"String s = "he said \"hi\" to me"; int x = 1;"#;
        let out = strip_for_indexing(src);
        assert!(!out.contains("he said"));
        assert!(!out.contains("hi"));
        assert!(out.contains("int x"));
    }

    #[test]
    fn strips_text_blocks() {
        let src = "String s = \"\"\"\n  multi-line\n  proprietary\n  text\n  \"\"\";\nint y = 9;";
        let out = strip_for_indexing(src);
        assert!(!out.contains("multi-line"));
        assert!(!out.contains("proprietary"));
        assert!(out.contains("int y"));
    }

    #[test]
    fn preserves_char_literals() {
        let src = "char c = 'x'; if (c == '\\n') {}";
        let out = strip_for_indexing(src);
        assert!(out.contains("'x'"));
    }

    #[test]
    fn double_slash_inside_string_is_not_a_comment() {
        let src = r#"String url = "http://example.com/secret-path"; int n = 0;"#;
        let out = strip_for_indexing(src);
        assert!(!out.contains("example"));
        assert!(!out.contains("secret-path"));
        assert!(out.contains("int n"));
    }

    #[test]
    fn quote_inside_block_comment_is_not_a_string() {
        let src = "/* he said \"never\" again */ int z = 0;";
        let out = strip_for_indexing(src);
        assert!(!out.contains("never"));
        assert!(!out.contains("again"));
        assert!(out.contains("int z"));
    }

    #[test]
    fn preserves_line_breaks_for_line_numbering() {
        let src = "/* a\nb\nc */\nint x;";
        let out = strip_for_indexing(src);
        // Comment body collapses, but the two embedded newlines stay so
        // line counts downstream still make sense.
        let nl_count = out.chars().filter(|c| *c == '\n').count();
        assert!(nl_count >= 3);
    }

    #[test]
    fn non_source_text_passes_through_unchanged_at_caller() {
        // Sanity check: the function itself is source-shaped. The caller
        // is responsible for skipping it on docs/markdown. We still want
        // it to be safe on plain text - verify a markdown-ish input
        // doesn't blow up.
        let src = "# Heading\n\nSome prose with no code at all.";
        let out = strip_for_indexing(src);
        // A markdown body has no // /* or " so it should round-trip.
        assert_eq!(out, src);
    }
}
