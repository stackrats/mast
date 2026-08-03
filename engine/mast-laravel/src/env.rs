//! Lossless `.env` model over Docker Compose's documented dotenv syntax
//! (plan §6): `KEY=VALUE` and `KEY: VALUE`, optional `export`, full-line and
//! inline comments, single-quoted (literal, multiline) and double-quoted
//! (escapes, multiline) values, interpolation left UN-resolved (raw), unknown
//! lines preserved verbatim with edits on them refused.
//!
//! Losslessness contract: `EnvFile::parse(s).to_string() == s` for any input
//! — every item stores its raw text and its own line terminator. Edits
//! re-render only the touched entry.
//!
//! Editing safety: `set()` writes a LITERAL value. Because the same file is
//! read by two parsers with different dollar conventions (compose expands
//! `$VAR`/`$$`; Laravel's phpdotenv does not know `$$`), values containing
//! `$` are single-quoted (literal in both); a value containing both `$` and
//! `'` cannot be represented safely for both parsers and is refused.

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvError {
    #[error("key {0} appears multiple times — edit is ambiguous")]
    DuplicateKey(String),
    #[error("cannot represent value safely for both compose and Laravel: {0}")]
    Unrepresentable(String),
    #[error("invalid key: {0}")]
    InvalidKey(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    None,
    Single,
    Double,
}

#[derive(Debug, Clone)]
pub struct EnvEntry {
    /// Exact original text (no terminator); `None` after an edit until
    /// re-rendered.
    raw: Option<String>,
    pub key: String,
    /// Decoded value — quotes stripped, double-quote escapes applied,
    /// interpolation left raw.
    pub value: String,
    pub quoting: Quoting,
    pub export: bool,
    /// Separator text between key and value, e.g. "=", ": ", " = ".
    separator: String,
    /// Raw inline-comment suffix including leading whitespace, e.g. " # x".
    pub inline_comment: Option<String>,
}

#[derive(Debug, Clone)]
pub enum EnvItem {
    Entry(EnvEntry),
    /// Full-line comment, raw.
    Comment(String),
    /// Whitespace-only line, raw.
    Blank(String),
    /// Unparseable line, preserved verbatim; never edited.
    Unknown(String),
}

#[derive(Debug, Clone)]
pub struct EnvFile {
    /// (item, line terminator — "\n", "\r\n", or "" at EOF).
    items: Vec<(EnvItem, String)>,
    /// Terminator used for new/re-rendered entries.
    dominant_newline: String,
}

// ---------- parsing ----------

fn is_key_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(is_key_start) && chars.all(is_key_char)
}

/// Find the end of the physical line starting at `from`: returns
/// (content_end, terminator_end). Content excludes `\r\n`/`\n`.
fn line_bounds(src: &str, from: usize) -> (usize, usize) {
    match src[from..].find('\n') {
        Some(offset) => {
            let nl = from + offset;
            let content_end = if nl > from && src.as_bytes()[nl - 1] == b'\r' { nl - 1 } else { nl };
            (content_end, nl + 1)
        }
        None => (src.len(), src.len()),
    }
}

fn decode_double(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('$') => out.push('$'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Try to parse one entry starting at `start`. Returns the entry plus
/// (content_end, terminator_end) of the LAST consumed physical line.
fn parse_entry(src: &str, start: usize) -> Option<(EnvEntry, usize, usize)> {
    let (line_end, _) = line_bounds(src, start);
    let line = &src[start..line_end];
    let trimmed = line.trim_start();
    let indent_len = line.len() - trimmed.len();
    let mut pos = start + indent_len;

    let mut export = false;
    if let Some(rest) = src[pos..].strip_prefix("export ")
        && rest.trim_start().chars().next().is_some_and(is_key_start)
    {
        export = true;
        pos += "export ".len();
        while src[pos..].starts_with(' ') {
            pos += 1;
        }
    }

    // Key.
    let key_start = pos;
    let key_len = src[pos..].chars().take_while(|c| is_key_char(*c)).map(char::len_utf8).sum::<usize>();
    let key = &src[key_start..key_start + key_len];
    if key.is_empty() || !valid_key(key) {
        return None;
    }
    pos = key_start + key_len;

    // Separator: optional spaces, '=' or ':', optional spaces.
    let sep_start = pos;
    while src[pos..].starts_with(' ') {
        pos += 1;
    }
    let sep_char = src[pos..].chars().next()?;
    if sep_char != '=' && sep_char != ':' {
        return None;
    }
    pos += 1;
    if sep_char == ':' && !src[pos..].starts_with(' ') && pos < line_end {
        // `KEY:VALUE` without a space is not dotenv colon syntax.
        return None;
    }
    while src[pos..].starts_with(' ') {
        pos += 1;
    }
    let separator = src[sep_start..pos].to_string();

    // Value.
    let next = src[pos..].chars().next();
    let (value, quoting, after_value) = match next {
        Some('\'') => {
            let close = src[pos + 1..].find('\'')? + pos + 1;
            (src[pos + 1..close].to_string(), Quoting::Single, close + 1)
        }
        Some('"') => {
            // Scan for an unescaped closing quote (may cross newlines).
            let bytes = src.as_bytes();
            let mut i = pos + 1;
            let close = loop {
                if i >= src.len() {
                    return None;
                }
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => break i,
                    _ => i += 1,
                }
            };
            (decode_double(&src[pos + 1..close]), Quoting::Double, close + 1)
        }
        _ => {
            // Unquoted: to end of line, stopping at an inline ` #` comment.
            let rest = &src[pos..line_end];
            let comment_at = rest
                .char_indices()
                .find(|(i, c)| *c == '#' && (*i == 0 || rest[..*i].ends_with(char::is_whitespace)))
                .map(|(i, _)| i);
            let value_end = comment_at.unwrap_or(rest.len());
            let value = rest[..value_end].trim_end().to_string();
            (value, Quoting::None, pos + rest[..value_end].trim_end().len())
        }
    };

    // Whatever follows the value on ITS line must be blank or a comment.
    let (final_line_end, final_term_end) = line_bounds(src, after_value.min(src.len()));
    let tail = &src[after_value..final_line_end];
    let inline_comment = if tail.trim().is_empty() {
        None
    } else if tail.trim_start().starts_with('#') {
        Some(tail.to_string())
    } else {
        return None;
    };

    let raw_end = if inline_comment.is_some() { final_line_end } else { after_value.max(line_end.min(final_line_end)) };
    // Raw spans from the physical line start through the end of the value's
    // final line (including any inline comment and trailing spaces).
    let raw = src[start..final_line_end].to_string();
    let _ = raw_end;

    Some((
        EnvEntry {
            raw: Some(raw),
            key: key.to_string(),
            value,
            quoting,
            export,
            separator,
            inline_comment,
        },
        final_line_end,
        final_term_end,
    ))
}

impl EnvFile {
    pub fn parse(src: &str) -> EnvFile {
        let dominant_newline =
            if src.contains("\r\n") { "\r\n".to_string() } else { "\n".to_string() };
        let mut items = Vec::new();
        let mut pos = 0;
        while pos < src.len() {
            let (line_end, term_end) = line_bounds(src, pos);
            let line = &src[pos..line_end];
            let term = src[line_end..term_end].to_string();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                items.push((EnvItem::Blank(line.to_string()), term));
                pos = term_end;
            } else if trimmed.starts_with('#') {
                items.push((EnvItem::Comment(line.to_string()), term));
                pos = term_end;
            } else if let Some((entry, entry_line_end, entry_term_end)) = parse_entry(src, pos) {
                let term = src[entry_line_end..entry_term_end].to_string();
                items.push((EnvItem::Entry(entry), term));
                pos = entry_term_end;
            } else {
                items.push((EnvItem::Unknown(line.to_string()), term));
                pos = term_end;
            }
        }
        EnvFile { items, dominant_newline }
    }

    pub fn items(&self) -> impl Iterator<Item = &EnvItem> {
        self.items.iter().map(|(item, _)| item)
    }

    pub fn entries(&self) -> impl Iterator<Item = &EnvEntry> {
        self.items().filter_map(|item| match item {
            EnvItem::Entry(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn get(&self, key: &str) -> Option<&EnvEntry> {
        self.entries().find(|e| e.key == key)
    }

    fn indices_of(&self, key: &str) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(i, (item, _))| match item {
                EnvItem::Entry(e) if e.key == key => Some(i),
                _ => None,
            })
            .collect()
    }

    /// Set `key` to a LITERAL value, preserving the entry's style where the
    /// value is representable in it. Appends a new entry when absent.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), EnvError> {
        if !valid_key(key) {
            return Err(EnvError::InvalidKey(key.to_string()));
        }
        let indices = self.indices_of(key);
        if indices.len() > 1 {
            return Err(EnvError::DuplicateKey(key.to_string()));
        }
        match indices.first() {
            Some(&i) => {
                let EnvItem::Entry(entry) = &mut self.items[i].0 else { unreachable!() };
                let quoting = choose_quoting(value, Some(entry.quoting))?;
                entry.value = value.to_string();
                entry.quoting = quoting;
                entry.raw = None; // re-rendered on output
                Ok(())
            }
            None => {
                let quoting = choose_quoting(value, None)?;
                let entry = EnvEntry {
                    raw: None,
                    key: key.to_string(),
                    value: value.to_string(),
                    quoting,
                    export: false,
                    separator: "=".into(),
                    inline_comment: None,
                };
                let term = self.dominant_newline.clone();
                // Ensure the previous last line is terminated.
                if let Some((_, last_term)) = self.items.last_mut()
                    && last_term.is_empty()
                {
                    *last_term = term.clone();
                }
                self.items.push((EnvItem::Entry(entry), term));
                Ok(())
            }
        }
    }

    /// Remove the entry for `key`. Returns whether anything was removed.
    pub fn remove(&mut self, key: &str) -> Result<bool, EnvError> {
        let indices = self.indices_of(key);
        if indices.len() > 1 {
            return Err(EnvError::DuplicateKey(key.to_string()));
        }
        match indices.first() {
            Some(&i) => {
                self.items.remove(i);
                Ok(true)
            }
            None => Ok(false),
        }
    }

}

impl std::fmt::Display for EnvFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (item, term) in &self.items {
            match item {
                EnvItem::Entry(entry) => match &entry.raw {
                    Some(raw) => f.write_str(raw)?,
                    None => f.write_str(&render_entry(entry))?,
                },
                EnvItem::Comment(raw) | EnvItem::Blank(raw) | EnvItem::Unknown(raw) => {
                    f.write_str(raw)?
                }
            }
            f.write_str(term)?;
        }
        Ok(())
    }
}

// ---------- rendering ----------

fn safe_unquoted(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | ':' | '@' | '+' | '=' | ',' | '-')
        })
}

/// Pick a quoting style that represents `value` identically for BOTH compose
/// and Laravel's parser (see module docs for the `$` rationale).
fn choose_quoting(value: &str, original: Option<Quoting>) -> Result<Quoting, EnvError> {
    let has_dollar = value.contains('$');
    let has_single = value.contains('\'');
    match original {
        Some(Quoting::None) | None if safe_unquoted(value) && !has_dollar => {
            return Ok(Quoting::None);
        }
        Some(Quoting::Double) if !has_dollar => return Ok(Quoting::Double),
        Some(Quoting::Single) if !has_single => return Ok(Quoting::Single),
        _ => {}
    }
    if value.is_empty() {
        return Ok(original.unwrap_or(Quoting::None));
    }
    if !has_single {
        return Ok(Quoting::Single);
    }
    if !has_dollar {
        return Ok(Quoting::Double);
    }
    Err(EnvError::Unrepresentable(
        "value contains both a single quote and a dollar sign".into(),
    ))
}

fn encode_double(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

fn render_entry(entry: &EnvEntry) -> String {
    let mut out = String::new();
    if entry.export {
        out.push_str("export ");
    }
    out.push_str(&entry.key);
    out.push_str(&entry.separator);
    match entry.quoting {
        Quoting::None => out.push_str(&entry.value),
        Quoting::Single => {
            out.push('\'');
            out.push_str(&entry.value);
            out.push('\'');
        }
        Quoting::Double => {
            out.push('"');
            out.push_str(&encode_double(&entry.value));
            out.push('"');
        }
    }
    if let Some(comment) = &entry.inline_comment {
        out.push_str(comment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_is_byte_exact() {
        let src = "# header\nAPP_NAME=\"My App\"\nexport DB_HOST=mysql # inline\n\nWEIRD junk line !!\nKEY: colon value\nEMPTY=\nMULTI='line one\nline two'\nlast=x";
        assert_eq!(EnvFile::parse(src).to_string(), src);
    }

    #[test]
    fn decodes_styles() {
        let file = EnvFile::parse(
            "A=plain\nB='single $NOT'\nC=\"dq \\\"esc\\\"\\nnl\"\nD=trail # c\nexport E=1\nF: colon\n",
        );
        assert_eq!(file.get("A").unwrap().value, "plain");
        assert_eq!(file.get("B").unwrap().value, "single $NOT");
        assert_eq!(file.get("C").unwrap().value, "dq \"esc\"\nnl");
        assert_eq!(file.get("D").unwrap().value, "trail");
        assert_eq!(file.get("D").unwrap().inline_comment.as_deref(), Some(" # c"));
        assert!(file.get("E").unwrap().export);
        assert_eq!(file.get("F").unwrap().value, "colon");
    }

    #[test]
    fn set_preserves_style_comment_and_neighbours() {
        let src = "A=1\nB=two # keep me\nC='three'\n";
        let mut file = EnvFile::parse(src);
        file.set("B", "changed").unwrap();
        assert_eq!(file.to_string(), "A=1\nB=changed # keep me\nC='three'\n");
        file.set("C", "new$interp-literal").unwrap();
        assert_eq!(file.to_string(), "A=1\nB=changed # keep me\nC='new$interp-literal'\n");
    }

    #[test]
    fn set_appends_and_terminates_missing_final_newline() {
        let mut file = EnvFile::parse("A=1");
        file.set("B", "2").unwrap();
        assert_eq!(file.to_string(), "A=1\nB=2\n");
    }

    #[test]
    fn dollar_and_quote_rules() {
        let mut file = EnvFile::parse("A=1\n");
        file.set("P", "pa$$word").unwrap();
        assert!(file.to_string().contains("P='pa$$word'"));
        file.set("Q", "it's fine").unwrap();
        assert!(file.to_string().contains("Q=\"it's fine\""));
        assert_eq!(
            file.set("R", "both '$'"),
            Err(EnvError::Unrepresentable(
                "value contains both a single quote and a dollar sign".into()
            ))
        );
    }

    #[test]
    fn duplicates_are_ambiguous() {
        let mut file = EnvFile::parse("A=1\nA=2\n");
        assert_eq!(file.set("A", "x"), Err(EnvError::DuplicateKey("A".into())));
        assert_eq!(file.remove("A"), Err(EnvError::DuplicateKey("A".into())));
    }

    #[test]
    fn crlf_preserved_and_used_for_new_entries() {
        let src = "A=1\r\nB=2\r\n";
        let mut file = EnvFile::parse(src);
        assert_eq!(file.to_string(), src);
        file.set("C", "3").unwrap();
        assert_eq!(file.to_string(), "A=1\r\nB=2\r\nC=3\r\n");
    }

    #[test]
    fn unknown_lines_survive_edits_verbatim(){
        let src = "A=1\n!!not env!!\nB=2\n";
        let mut file = EnvFile::parse(src);
        file.set("B", "9").unwrap();
        assert_eq!(file.to_string(), "A=1\n!!not env!!\nB=9\n");
    }
}
