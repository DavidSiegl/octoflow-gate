//! A Python source file's bytes as text, decoded as Python decodes them: by a byte order mark or a PEP 263 coding
//! cookie (<https://peps.python.org/pep-0263/>), else as UTF-8. gate.py's `_decode_python`, for the encodings the
//! standard library can decode: UTF-8, ASCII and Latin-1 (docs/port-differences.md).

use std::borrow::Cow;

use model::Unreadable;

const BOM: &[u8] = b"\xef\xbb\xbf";

/// The encodings a cookie can name that this front end decodes.
enum Encoding {
    Utf8,
    Ascii,
    Latin1,
}

/// `source` as text, without its byte order mark, the line ends left as they are. Errs, about the whole file, when its
/// cookie names an encoding other than UTF-8, ASCII or Latin-1, when a cookie beside a byte order mark names
/// anything but UTF-8, or when it holds bytes its encoding cannot decode.
pub fn decode(source: &[u8]) -> Result<Cow<'_, str>, Unreadable> {
    let unreadable = |why: String| Unreadable {
        line: None,
        why: format!("could not be read (cannot be decoded as its coding cookie or UTF-8 says: {why})"),
    };
    let marked = source.starts_with(BOM);
    let bytes = if marked { &source[BOM.len()..] } else { source };
    let encoding = match cookie(bytes) {
        None => Encoding::Utf8,
        Some(name) => match encoding(&name) {
            Some(Encoding::Utf8) => Encoding::Utf8,
            Some(_) if marked => return Err(unreadable(format!("a byte order mark beside the cookie `{name}`"))),
            Some(named) => named,
            None => {
                return Err(unreadable(format!("`{name}` is not an encoding this gate reads: UTF-8, ASCII or Latin-1")))
            }
        },
    };
    match encoding {
        Encoding::Utf8 => std::str::from_utf8(bytes).map(Cow::Borrowed).map_err(|e| unreadable(e.to_string())),
        Encoding::Ascii => match bytes.iter().position(|b| !b.is_ascii()) {
            None => Ok(Cow::Borrowed(std::str::from_utf8(bytes).unwrap_or_default())),
            Some(k) => Err(unreadable(format!("byte {k} is not ASCII"))),
        },
        Encoding::Latin1 => Ok(Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect())),
    }
}

/// The encoding the cookie's `name` names, as Python's codec aliases spell the three; None for any other.
fn encoding(name: &str) -> Option<Encoding> {
    let name = name.to_ascii_lowercase().replace('_', "-");
    let family = |names: &[&str]| names.iter().any(|n| name == *n || name.starts_with(&format!("{n}-")));
    if family(&["utf-8"]) || ["utf8", "u8", "utf"].contains(&name.as_str()) {
        Some(Encoding::Utf8)
    } else if ["ascii", "us-ascii", "646"].contains(&name.as_str()) {
        Some(Encoding::Ascii)
    } else if family(&["latin-1", "iso-8859-1", "iso-latin-1"])
        || ["latin1", "iso8859-1", "l1", "8859", "cp819"].contains(&name.as_str())
    {
        Some(Encoding::Latin1)
    } else {
        None
    }
}

/// The encoding a coding cookie names in the first two lines of `bytes`, as tokenize.detect_encoding finds it: on the
/// first line, or on the second when the first is blank or only a comment.
fn cookie(bytes: &[u8]) -> Option<String> {
    let mut lines = bytes.split(|&b| b == b'\n');
    let first = lines.next()?;
    if let Some(name) = cookie_in(first) {
        return Some(name);
    }
    let indent = first.iter().take_while(|b| matches!(b, b' ' | b'\t' | b'\x0c')).count();
    if !matches!(first.get(indent), None | Some(b'#' | b'\r')) {
        return None;
    }
    cookie_in(lines.next()?)
}

/// The name in `line` when it is a comment holding a cookie: `^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)`, with ASCII
/// word characters.
fn cookie_in(line: &[u8]) -> Option<String> {
    let indent = line.iter().take_while(|b| matches!(b, b' ' | b'\t' | b'\x0c')).count();
    let comment = line.get(indent..).filter(|rest| rest.starts_with(b"#"))?;
    const KEY: &[u8] = b"coding";
    for at in 0..comment.len() {
        let Some(rest) = comment[at..].strip_prefix(KEY) else { continue };
        let Some(rest) = rest.strip_prefix(b":").or_else(|| rest.strip_prefix(b"=")) else { continue };
        let blank = rest.iter().take_while(|b| matches!(b, b' ' | b'\t')).count();
        let name: Vec<u8> = rest[blank..]
            .iter()
            .copied()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            .collect();
        if !name.is_empty() {
            return String::from_utf8(name).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_utf_8_unless_a_cookie_in_its_first_two_lines_says_otherwise() {
        assert_eq!(decode("x = 'é'\n".as_bytes()).unwrap(), "x = 'é'\n");
        assert_eq!(decode(b"\xef\xbb\xbfx = 1\n").unwrap(), "x = 1\n");
        assert_eq!(decode(b"# -*- coding: latin-1 -*-\n'\xe9'\n").unwrap(), "# -*- coding: latin-1 -*-\n'é'\n");
        let second = b"#!/usr/bin/python\n# vim: set fileencoding=iso-8859-1 :\n'\xe9'\n";
        assert!(decode(second).unwrap().ends_with("'é'\n"));
        // A cookie on the third line, or after a line of code, is none.
        assert!(decode(b"\n\n# coding: latin-1\n'\xe9'\n").is_err());
        assert!(decode(b"x = 1\n# coding: latin-1\n'\xe9'\n").is_err());
    }

    #[test]
    fn what_cannot_be_decoded_is_unreadable_as_a_whole() {
        for source in [
            &b"x = '\xff'\n"[..],
            b"# coding: ascii\nx = '\xe9'\n",
            b"# coding: rot13\nx = 1\n",
            b"# -*- coding: nonsense -*-\n",
            b"\xef\xbb\xbf# coding: latin-1\n",
        ] {
            let e = decode(source).unwrap_err();
            assert_eq!(e.line, None);
            let opening = "could not be read (cannot be decoded as its coding cookie or UTF-8 says: ";
            assert!(e.why.starts_with(opening), "{e:?}");
        }
        assert!(decode(b"\xef\xbb\xbf# coding: utf-8\n").is_ok());
        assert!(decode(b"# coding: UTF_8\n").is_ok());
    }
}
