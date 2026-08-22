//! A small, dependency-free source highlighter.
//!
//! Not a grammar — comments, strings, numbers, keywords and capitalized types,
//! which is enough to make code read as code. The daemon ran files through
//! syntect; that lived server-side only because the editor did, and dragging
//! syntect and its grammar set across to every client to match it exactly would
//! be paying a lot for the difference between "code reads as code" and
//! "identical to TextMate".
//!
//! It is deliberately the same scope as the Mac client's `Syntax.swift`, which
//! has been the shipped highlighter on that side for as long as the file tree
//! has existed. Two clients agreeing is worth more here than either agreeing
//! with syntect.
//!
//! State is one bool — whether a block comment is open — carried from line to
//! line, so a file highlights in one pass and a redraw of the visible rows
//! costs nothing.

/// What a run of characters is, semantically. The palette is applied by the
/// caller, the way [`crate::chrome::Role`] works for rail rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Plain,
    Comment,
    Str,
    Number,
    Keyword,
    /// A capitalized word with a lowercase letter in it — `Buffer`, `HashMap`.
    /// A crude proxy for "a type", and crude is the point: it needs no symbol
    /// table and it is right often enough to help.
    Type,
}

/// The language of a file, insofar as an extension reveals it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    Rust,
    /// C, C++, Java, Kotlin, Go, Swift, JavaScript, TypeScript — one family for
    /// highlighting purposes: `//` line comments, `/* */` blocks, and a
    /// keyword set that overlaps enough that separating them would buy nothing
    /// a reader could see.
    CLike,
    Python,
    Shell,
    /// Structured data: no keywords, but strings, numbers and `#` comments.
    Data,
    Markup,
    /// Not code. Rendered plain.
    #[default]
    Plain,
}

impl Lang {
    /// The language a filename implies.
    pub fn of(name: &str) -> Lang {
        let file = name.rsplit('/').next().unwrap_or(name);
        // Files whose name is their type. `Makefile` has no extension and
        // `.gitignore` is all extension, so both miss the table below.
        match file.to_ascii_lowercase().as_str() {
            "dockerfile" | "makefile" | "rakefile" | "gemfile" | "brewfile" | "justfile" => {
                return Lang::Shell
            }
            ".gitignore" | ".gitattributes" | ".dockerignore" | ".env" => return Lang::Plain,
            _ => {}
        }
        let ext = match file.rsplit_once('.') {
            // A leading dot is the whole name (`.zshrc`), not an extension.
            Some((head, ext)) if !head.is_empty() => ext.to_ascii_lowercase(),
            _ => return Lang::Plain,
        };
        match ext.as_str() {
            "rs" => Lang::Rust,
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "java" | "kt" | "kts" | "go"
            | "swift" | "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "php" | "cs" | "scala"
            | "dart" | "css" | "scss" | "sass" | "less" => Lang::CLike,
            "py" | "pyw" | "pyi" | "rb" => Lang::Python,
            "sh" | "bash" | "zsh" | "fish" => Lang::Shell,
            "json" | "jsonc" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" => Lang::Data,
            "html" | "htm" | "xml" | "svg" | "plist" | "md" | "markdown" | "mdx" => Lang::Markup,
            _ => Lang::Plain,
        }
    }

    fn line_comments(self) -> &'static [&'static str] {
        match self {
            Lang::Rust | Lang::CLike => &["//"],
            Lang::Python | Lang::Shell | Lang::Data => &["#"],
            Lang::Markup | Lang::Plain => &[],
        }
    }

    fn block_comment(self) -> Option<(&'static str, &'static str)> {
        match self {
            Lang::Rust | Lang::CLike => Some(("/*", "*/")),
            Lang::Markup => Some(("<!--", "-->")),
            _ => None,
        }
    }

    fn string_delims(self) -> &'static [char] {
        match self {
            Lang::Markup | Lang::Plain => &[],
            Lang::Rust => &['"'],
            _ => &['"', '\''],
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
            // Hand-wrapped: rustfmt lays an array out one element per line as
            // soon as any element is over ten characters, and a word list is
            // easier to check by eye than to read down a column.
            #[rustfmt::skip]
            Lang::CLike => &[
                "abstract", "break", "case", "catch", "class", "const", "continue", "default",
                "defer", "delete", "do", "else", "enum", "export", "extends", "false", "final",
                "finally", "for", "func", "function", "go", "if", "implements", "import", "in",
                "instanceof", "interface", "let", "new", "null", "package", "private",
                "protected", "public", "return", "static", "struct", "super", "switch", "this",
                "throw", "true", "try", "type", "typeof", "var", "void", "while", "yield",
            ],
            // Ruby rides along: `def`/`class`/`end`/`nil` plus the shared
            // control-flow words cover it, and the two never appear together.
            Lang::Python => &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "end", "except", "False", "finally", "for", "from",
                "global", "if", "import", "in", "is", "lambda", "nil", "None", "nonlocal", "not",
                "or", "pass", "raise", "require", "return", "self", "True", "try", "unless",
                "while", "with", "yield",
            ],
            Lang::Shell => &[
                "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function",
                "if", "in", "local", "read", "return", "then", "until", "while",
            ],
            Lang::Data | Lang::Markup | Lang::Plain => &[],
        }
    }

    /// Whether this language gets tokenized at all.
    fn is_code(self) -> bool {
        self != Lang::Plain
    }
}

/// A highlighter part-way through a file.
///
/// Holds only whether a block comment is open, so highlighting line *n* needs
/// lines 1..n-1 to have gone through it — which is why the whole buffer is
/// highlighted at once rather than only the visible rows.
#[derive(Debug, Clone)]
pub struct Highlighter {
    lang: Lang,
    in_block: bool,
}

impl Highlighter {
    pub fn new(lang: Lang) -> Self {
        Self { lang, in_block: false }
    }

    /// Highlight a whole buffer, one run per token, in order.
    pub fn lines(lang: Lang, lines: &[String]) -> Vec<Vec<(Token, String)>> {
        let mut hl = Highlighter::new(lang);
        lines.iter().map(|l| hl.line(l)).collect()
    }

    /// Highlight one line and advance the block-comment state.
    pub fn line(&mut self, line: &str) -> Vec<(Token, String)> {
        let mut out: Vec<(Token, String)> = Vec::new();
        if !self.lang.is_code() {
            if !line.is_empty() {
                out.push((Token::Plain, line.to_string()));
            }
            return out;
        }
        let chars: Vec<char> = line.chars().collect();
        let n = chars.len();
        let block = self.lang.block_comment();
        let strings = self.lang.string_delims();
        let mut i = 0usize;

        let at = |idx: usize, token: &str| -> bool {
            let t: Vec<char> = token.chars().collect();
            idx + t.len() <= n && (0..t.len()).all(|k| chars[idx + k] == t[k])
        };

        // A block comment carried in from an earlier line runs until its close
        // or to the end of this one.
        if self.in_block {
            if let Some((_, close)) = block {
                let mut j = i;
                while j < n {
                    if at(j, close) {
                        j += close.chars().count();
                        self.in_block = false;
                        break;
                    }
                    j += 1;
                }
                push(&mut out, Token::Comment, &chars[i..j]);
                i = j;
                if self.in_block {
                    return out;
                }
            }
        }

        // Plain text accumulates so words can be classified whole; anything
        // that is its own token flushes it first.
        let mut run: Vec<char> = Vec::new();
        while i < n {
            if let Some((open, close)) = block {
                if at(i, open) {
                    flush_run(&mut out, &mut run, self.lang);
                    let mut j = i + open.chars().count();
                    self.in_block = true;
                    while j < n {
                        if at(j, close) {
                            j += close.chars().count();
                            self.in_block = false;
                            break;
                        }
                        j += 1;
                    }
                    push(&mut out, Token::Comment, &chars[i..j]);
                    i = j;
                    continue;
                }
            }
            if self.lang.line_comments().iter().any(|lc| at(i, lc)) {
                flush_run(&mut out, &mut run, self.lang);
                push(&mut out, Token::Comment, &chars[i..n]);
                break;
            }
            let c = chars[i];
            if strings.contains(&c) {
                flush_run(&mut out, &mut run, self.lang);
                let mut j = i + 1;
                while j < n {
                    // A backslash eats the next character, so an escaped quote
                    // does not end the string.
                    if chars[j] == '\\' {
                        j += 2;
                        continue;
                    }
                    if chars[j] == c {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                let j = j.min(n);
                push(&mut out, Token::Str, &chars[i..j]);
                i = j;
                continue;
            }
            // A digit starts a number only when it is not inside a word, or
            // `utf8` and `x2` would come out half-coloured.
            let in_word = run.last().is_some_and(|p| p.is_alphanumeric() || *p == '_');
            if c.is_ascii_digit() && !in_word {
                flush_run(&mut out, &mut run, self.lang);
                let mut j = i;
                while j < n && (chars[j].is_ascii_hexdigit() || matches!(chars[j], '.' | '_' | 'x'))
                {
                    j += 1;
                }
                push(&mut out, Token::Number, &chars[i..j]);
                i = j;
                continue;
            }
            run.push(c);
            i += 1;
        }
        flush_run(&mut out, &mut run, self.lang);
        out
    }
}

fn push(out: &mut Vec<(Token, String)>, token: Token, chars: &[char]) {
    if chars.is_empty() {
        return;
    }
    out.push((token, chars.iter().collect()));
}

/// Split accumulated plain text into words, colouring the ones that are
/// keywords or look like types and leaving everything between them alone.
fn flush_run(out: &mut Vec<(Token, String)>, run: &mut Vec<char>, lang: Lang) {
    let mut word = String::new();
    let mut plain = String::new();
    let keywords = lang.keywords();

    let flush_word = |word: &mut String, out: &mut Vec<(Token, String)>, plain: &mut String| {
        if word.is_empty() {
            return;
        }
        let token = if keywords.contains(&word.as_str()) {
            Token::Keyword
        } else if word.chars().next().is_some_and(char::is_uppercase)
            && word.chars().any(char::is_lowercase)
        {
            Token::Type
        } else {
            Token::Plain
        };
        if token == Token::Plain {
            plain.push_str(word);
        } else {
            if !plain.is_empty() {
                out.push((Token::Plain, std::mem::take(plain)));
            }
            out.push((token, word.clone()));
        }
        word.clear();
    };

    for ch in run.iter() {
        if ch.is_alphanumeric() || *ch == '_' {
            word.push(*ch);
        } else {
            flush_word(&mut word, out, &mut plain);
            plain.push(*ch);
        }
    }
    flush_word(&mut word, out, &mut plain);
    if !plain.is_empty() {
        out.push((Token::Plain, plain));
    }
    run.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text a highlighter produced, concatenated. Whatever it decides about
    /// colour, it must not lose or reorder a character — this is the property
    /// every other assertion here rests on.
    fn text(runs: &[(Token, String)]) -> String {
        runs.iter().map(|(_, s)| s.as_str()).collect()
    }

    fn tokens_of(lang: Lang, line: &str) -> Vec<(Token, String)> {
        let out = Highlighter::new(lang).line(line);
        assert_eq!(text(&out), line, "the line was altered");
        out
    }

    fn kind(runs: &[(Token, String)], needle: &str) -> Option<Token> {
        runs.iter().find(|(_, s)| s == needle).map(|(t, _)| *t)
    }

    #[test]
    fn a_filename_names_its_language() {
        assert_eq!(Lang::of("src/main.rs"), Lang::Rust);
        assert_eq!(Lang::of("app.tsx"), Lang::CLike);
        assert_eq!(Lang::of("setup.py"), Lang::Python);
        assert_eq!(Lang::of("Makefile"), Lang::Shell);
        assert_eq!(Lang::of("deploy/Dockerfile"), Lang::Shell);
        assert_eq!(Lang::of("Cargo.toml"), Lang::Data);
        assert_eq!(Lang::of("README.md"), Lang::Markup);
        assert_eq!(Lang::of("LICENSE"), Lang::Plain);
        // A dotfile's "extension" is its whole name, not a type.
        assert_eq!(Lang::of(".zshrc"), Lang::Plain);
        assert_eq!(Lang::of(".gitignore"), Lang::Plain);
    }

    #[test]
    fn keywords_strings_numbers_and_types_are_told_apart() {
        let t = tokens_of(Lang::Rust, r#"let n: Buffer = 42; // note"#);
        assert_eq!(kind(&t, "let"), Some(Token::Keyword));
        assert_eq!(kind(&t, "Buffer"), Some(Token::Type));
        assert_eq!(kind(&t, "42"), Some(Token::Number));
        assert_eq!(kind(&t, "// note"), Some(Token::Comment));
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        let t = tokens_of(Lang::Rust, r#"let s = "a\"b"; let x = 1;"#);
        assert_eq!(kind(&t, r#""a\"b""#), Some(Token::Str), "{t:?}");
        // The code after the string is still code, not a run-on string.
        assert_eq!(kind(&t, "1"), Some(Token::Number), "{t:?}");
    }

    /// A `//` inside a string is not a comment. Getting this wrong greys out
    /// the rest of every line containing a URL.
    #[test]
    fn a_comment_marker_inside_a_string_is_string() {
        let t = tokens_of(Lang::CLike, r#"const u = "https://x/y"; // real"#);
        assert_eq!(kind(&t, r#""https://x/y""#), Some(Token::Str), "{t:?}");
        assert_eq!(kind(&t, "// real"), Some(Token::Comment), "{t:?}");
    }

    #[test]
    fn a_block_comment_carries_across_lines() {
        let lines: Vec<String> =
            ["let a = 1; /* open", "still comment", "*/ let b = 2;"].map(String::from).to_vec();
        let out = Highlighter::lines(Lang::Rust, &lines);
        assert_eq!(kind(&out[0], "1"), Some(Token::Number), "before the comment: {:?}", out[0]);
        assert_eq!(out[1], vec![(Token::Comment, "still comment".to_string())]);
        // And it closes: the code after `*/` is code again.
        assert_eq!(kind(&out[2], "let"), Some(Token::Keyword), "{:?}", out[2]);
        assert_eq!(kind(&out[2], "2"), Some(Token::Number), "{:?}", out[2]);
        for (line, runs) in lines.iter().zip(&out) {
            assert_eq!(&text(runs), line);
        }
    }

    #[test]
    fn a_digit_inside_a_word_is_not_a_number() {
        let t = tokens_of(Lang::Rust, "let utf8 = x2 + 3;");
        assert_eq!(kind(&t, "3"), Some(Token::Number));
        assert!(kind(&t, "8").is_none(), "utf8 was split: {t:?}");
        assert!(kind(&t, "2").is_none(), "x2 was split: {t:?}");
    }

    #[test]
    fn a_plain_file_is_left_alone() {
        let t = tokens_of(Lang::Plain, "let x = \"1\"; // not code");
        assert_eq!(t, vec![(Token::Plain, "let x = \"1\"; // not code".to_string())]);
    }

    /// Every language, against a line with one of everything: the text must
    /// survive intact whatever the tokenizer decides.
    #[test]
    fn no_language_loses_a_character() {
        let langs = [Lang::Rust, Lang::CLike, Lang::Python, Lang::Shell, Lang::Data, Lang::Markup];
        let lines = [
            r#"x = "a\"b" # 0x1f /* c */ // d"#,
            "",
            "   ",
            "<!-- markup --> Type_Name 3.14",
            r#"'single' "double" 999"#,
            "日本語 = \"ünïcodé\" // ✓",
        ];
        for lang in langs {
            let mut hl = Highlighter::new(lang);
            for line in lines {
                assert_eq!(text(&hl.line(line)), line, "{lang:?} altered {line:?}");
            }
        }
    }
}
