//! German and Spanish labels. `i18n.tsv` holds one line per English source string:
//! `english<TAB>deutsch<TAB>español` (`\n` and `\t` escaped). French and English live inline in
//! the `tr("fr", "en")` calls; the test below keeps the table in sync with them.

use std::collections::HashMap;
use std::sync::OnceLock;

const TABLE: &str = include_str!("i18n.tsv");

fn unescape(s: &'static str) -> &'static str {
    if !s.contains('\\') {
        return s;
    }
    // Rare (a handful of entries): leak once, the table lives for the whole session anyway.
    Box::leak(s.replace("\\n", "\n").replace("\\t", "\t").into_boxed_str())
}

fn table() -> &'static HashMap<&'static str, [&'static str; 2]> {
    static T: OnceLock<HashMap<&'static str, [&'static str; 2]>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        for line in TABLE.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(en), Some(de), Some(es)) = (it.next(), it.next(), it.next()) else { continue };
            m.insert(unescape(en), [unescape(de), unescape(es)]);
        }
        m
    })
}

/// `col`: 0 = German, 1 = Spanish. Falls back to the English text.
pub fn lookup(en: &'static str, col: usize) -> &'static str {
    match table().get(en) {
        Some(row) if !row[col].is_empty() => row[col],
        _ => en,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every English string passed to `tr` in the sources has a German and a Spanish entry.
    #[test]
    fn table_covers_sources() {
        let missing: Vec<String> = crate::i18n::tests::source_strings()
            .into_iter()
            .filter(|en| !table().contains_key(en.as_str()))
            .collect();
        assert!(missing.is_empty(), "{} strings missing from i18n.tsv: {missing:#?}", missing.len());
    }

    #[test]
    fn lookup_falls_back_to_english() {
        assert_eq!(lookup("zz-not-a-label", 0), "zz-not-a-label");
    }

    /// Minimal scanner for `tr("…", "…")` calls (string literals, possibly over several lines).
    pub fn source_strings() -> Vec<String> {
        let mut out = vec![];
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(p) = stack.pop() {
            for e in std::fs::read_dir(&p).unwrap().flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs") {
                    let src = std::fs::read_to_string(&path).unwrap();
                    let mut rest = src.as_str();
                    while let Some(i) = rest.find("tr(") {
                        let before = rest[..i].chars().last();
                        rest = &rest[i + 3..];
                        if before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
                            continue;
                        }
                        let Some((_, r)) = literal(rest) else { continue };
                        let r = r.trim_start();
                        let Some(r) = r.strip_prefix(',') else { continue };
                        if let Some((en, _)) = literal(r) {
                            out.push(en);
                        }
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn literal(s: &str) -> Option<(String, &str)> {
        let s = s.trim_start();
        let s = s.strip_prefix('"')?;
        let mut v = String::new();
        let mut chars = s.char_indices();
        while let Some((i, ch)) = chars.next() {
            match ch {
                '"' => return Some((v, &s[i + 1..])),
                '\\' => match chars.next()?.1 {
                    'n' => v.push('\n'),
                    't' => v.push('\t'),
                    '\n' => {
                        // line continuation: skip leading whitespace of the next line
                        while let Some((_, c2)) = chars.clone().next() {
                            if c2.is_whitespace() {
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    other => v.push(other),
                },
                _ => v.push(ch),
            }
        }
        None
    }
}
