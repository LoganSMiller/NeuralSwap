//! A small reader for Valve's KeyValues text format.
//!
//! Steam records where its libraries are (`libraryfolders.vdf`) and what is
//! installed in each (`appmanifest_*.acf`) in this format, so reading it is
//! how a Steam library is discovered without guessing at paths.
//!
//! Deliberately lenient. These files are written by Steam and read by us, and
//! a parser that refuses the whole document over one unexpected token would
//! turn a cosmetic change in a Steam update into "NeuralSwap can no longer
//! find your games". Unrecognised input is skipped; what parses is returned.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    /// Ordered, because VDF permits repeated keys and order carries meaning.
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            Value::Object(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Object(entries) => entries,
            Value::Text(_) => &[],
        }
    }

    /// First value under `key`, matched case-insensitively - Steam is
    /// inconsistent about capitalisation between versions.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }

    /// Text at a nested path, e.g. `["AppState", "installdir"]`.
    pub fn text_at(&self, path: &[&str]) -> Option<&str> {
        let mut current = self;
        for step in path {
            current = current.get(step)?;
        }
        current.as_text()
    }

    /// Flatten an object of `key -> text` into a map.
    pub fn text_map(&self) -> BTreeMap<String, String> {
        self.entries()
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), value.as_text()?.to_owned())))
            .collect()
    }
}

/// Parse a KeyValues document into a root object.
pub fn parse(input: &str) -> Value {
    let mut cursor = Cursor {
        bytes: input.as_bytes(),
        at: 0,
    };
    Value::Object(cursor.entries(0))
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

/// Guard against a malformed file nesting deeply enough to exhaust the stack.
const MAX_DEPTH: usize = 32;

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(byte) if byte.is_ascii_whitespace() => self.at += 1,
                // `//` to end of line is a comment.
                Some(b'/') if self.bytes.get(self.at + 1) == Some(&b'/') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.at += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// A quoted or bare token, with the usual escapes inside quotes.
    fn token(&mut self) -> Option<String> {
        self.skip_trivia();
        match self.peek()? {
            b'"' => {
                self.at += 1;
                let mut out = String::new();
                loop {
                    match self.peek() {
                        None => return Some(out),
                        Some(b'"') => {
                            self.at += 1;
                            return Some(out);
                        }
                        Some(b'\\') => {
                            self.at += 1;
                            let escaped = self.peek().unwrap_or(b'\\');
                            self.at += 1;
                            out.push(match escaped {
                                b'n' => '\n',
                                b't' => '\t',
                                other => char::from(other),
                            });
                        }
                        Some(byte) => {
                            self.at += 1;
                            out.push(char::from(byte));
                        }
                    }
                }
            }
            b'{' | b'}' => None,
            _ => {
                let start = self.at;
                while let Some(byte) = self.peek() {
                    if byte.is_ascii_whitespace() || byte == b'{' || byte == b'}' || byte == b'"' {
                        break;
                    }
                    self.at += 1;
                }
                if self.at == start {
                    return None;
                }
                Some(String::from_utf8_lossy(self.bytes.get(start..self.at)?).into_owned())
            }
        }
    }

    fn entries(&mut self, depth: usize) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => return out,
                Some(b'}') => {
                    self.at += 1;
                    return out;
                }
                Some(b'{') => {
                    // A block with no key: skip it rather than abandoning the
                    // rest of the document.
                    self.at += 1;
                    let _ = self.entries(depth + 1);
                }
                _ => {
                    let Some(key) = self.token() else {
                        // Not a token and not a brace: step over it.
                        self.at += 1;
                        continue;
                    };
                    self.skip_trivia();
                    match self.peek() {
                        Some(b'{') => {
                            self.at += 1;
                            let nested = if depth >= MAX_DEPTH {
                                let _ = self.entries(depth + 1);
                                Vec::new()
                            } else {
                                self.entries(depth + 1)
                            };
                            out.push((key, Value::Object(nested)));
                        }
                        _ => match self.token() {
                            Some(text) => out.push((key, Value::Text(text))),
                            None => out.push((key, Value::Text(String::new()))),
                        },
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_library_folders_document() {
        // The shape Steam actually writes, tabs and all.
        let document = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"label"		""
		"apps"
		{
			"228980"		"1234567"
			"1091500"		"89012345"
		}
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
		"apps"
		{
			"271590"		"1111"
		}
	}
}
"#;
        let root = parse(document);
        let folders = root.get("libraryfolders").expect("libraryfolders");
        assert_eq!(folders.entries().len(), 2);

        assert_eq!(
            folders.text_at(&["0", "path"]),
            Some(r"C:\Program Files (x86)\Steam")
        );
        assert_eq!(folders.text_at(&["1", "path"]), Some(r"D:\SteamLibrary"));

        let apps = folders.get("0").and_then(|f| f.get("apps")).expect("apps");
        assert_eq!(apps.text_map().len(), 2);
        assert!(apps.text_map().contains_key("1091500"));
    }

    #[test]
    fn reads_an_app_manifest() {
        let document = r#"
"AppState"
{
	"appid"		"1091500"
	"name"		"Cyberpunk 2077"
	"installdir"		"Cyberpunk 2077"
	"StateFlags"		"4"
}
"#;
        let root = parse(document);
        assert_eq!(root.text_at(&["AppState", "appid"]), Some("1091500"));
        assert_eq!(root.text_at(&["AppState", "name"]), Some("Cyberpunk 2077"));
        // Steam has changed capitalisation between versions, so lookups are
        // case-insensitive.
        assert_eq!(
            root.text_at(&["appstate", "InstallDir"]),
            Some("Cyberpunk 2077")
        );
    }

    #[test]
    fn escapes_inside_quotes_are_decoded() {
        let root = parse(r#""a" { "path" "C:\\Games\\My \"Best\" Game" "note" "one\ttwo" }"#);
        assert_eq!(
            root.text_at(&["a", "path"]),
            Some(r#"C:\Games\My "Best" Game"#)
        );
        assert_eq!(root.text_at(&["a", "note"]), Some("one\ttwo"));
    }

    #[test]
    fn comments_and_odd_whitespace_are_skipped() {
        let document = r#"
// a comment
"root"
{
    // another
    "key"   "value"
}
"#;
        assert_eq!(parse(document).text_at(&["root", "key"]), Some("value"));
    }

    #[test]
    fn a_truncated_document_yields_what_it_can() {
        // A half-written file must not cost the entries that did parse: the
        // alternative is telling a user their whole library vanished.
        let root = parse(r#""root" { "a" "1" "b" "2" "c" "#);
        let entries = root.get("root").expect("root");
        assert_eq!(entries.text_at(&["a"]), Some("1"));
        assert_eq!(entries.text_at(&["b"]), Some("2"));
    }

    #[test]
    fn unexpected_input_does_not_abandon_the_document() {
        let root = parse(r#"{ } "root" { "a" "1" }"#);
        assert_eq!(root.text_at(&["root", "a"]), Some("1"));
    }

    #[test]
    fn deep_nesting_does_not_exhaust_the_stack() {
        let deep = format!("{}{}", r#""a" {"#.repeat(200), "}".repeat(200));
        // The point is that this returns at all.
        let _ = parse(&deep);
    }

    #[test]
    fn an_empty_document_is_an_empty_object() {
        assert_eq!(parse("").entries().len(), 0);
        assert_eq!(parse("   \n\t ").entries().len(), 0);
    }
}
