//! Vim's `listchars`: the glyphs `list` draws where a character would
//! otherwise show nothing.
//!
//! These are display substitutions, never buffer contents. A byte keeps the
//! cells it always occupied — a tab is still four columns wide — so the
//! cursor, mouse mapping, selections and jump labels are unaffected by
//! turning `list` on.

use std::fmt;

/// What to draw in place of each kind of invisible character.
///
/// A `None` means that kind is left alone, which is how `:set listchars=`
/// with an item omitted behaves in vim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListChars {
    /// The head of a tab and the character filling out the rest of its width.
    pub tab: Option<(char, char)>,
    /// Whitespace at the end of a line.
    pub trail: Option<char>,
    /// The line ending itself.
    pub eol: Option<char>,
    /// A non-breaking space.
    pub nbsp: Option<char>,
    /// Any space that is not trailing.
    pub space: Option<char>,
}

impl Default for ListChars {
    /// `tab:\u{25b8} ,trail:\u{b7},eol:\u{21b2},nbsp:\u{23b5},space:\u{b7}`.
    ///
    /// Vim leaves this at `eol:$`, which marks only line endings with a
    /// character that also occurs in text. Cano names every kind instead, so
    /// turning `list` on tells you something without configuring it first;
    /// `:set listchars=eol:$` restores vim's.
    fn default() -> Self {
        Self {
            tab: Some(('\u{25b8}', ' ')),
            trail: Some('\u{b7}'),
            eol: Some('\u{21b2}'),
            nbsp: Some('\u{23b5}'),
            space: Some('\u{b7}'),
        }
    }
}

impl ListChars {
    /// A set that draws nothing, which is what an explicit spec starts from.
    pub const fn none() -> Self {
        Self {
            tab: None,
            trail: None,
            eol: None,
            nbsp: None,
            space: None,
        }
    }
}

impl fmt::Display for ListChars {
    /// Writes the spec back out in the form `:set listchars=` accepts, so
    /// `:set listchars?` round-trips through the parser.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut items: Vec<String> = Vec::new();
        if let Some((head, fill)) = self.tab {
            items.push(format!("tab:{head}{fill}"));
        }
        for (name, glyph) in [
            ("trail", self.trail),
            ("eol", self.eol),
            ("nbsp", self.nbsp),
            ("space", self.space),
        ] {
            if let Some(glyph) = glyph {
                items.push(format!("{name}:{glyph}"));
            }
        }
        f.write_str(&items.join(","))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListCharsError {
    NotUtf8,
    Malformed(String),
    UnknownItem(String),
    WrongLength { item: String, wanted: &'static str },
}

impl fmt::Display for ListCharsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 => f.write_str("listchars must be valid UTF-8"),
            Self::Malformed(item) => write!(f, "Malformed listchars item: {item}"),
            Self::UnknownItem(item) => write!(f, "Unknown listchars item: {item}"),
            Self::WrongLength { item, wanted } => {
                write!(f, "listchars {item} takes {wanted}")
            }
        }
    }
}

impl std::error::Error for ListCharsError {}

/// Parses a `listchars` value such as `tab:>-,trail:.,eol:$`.
///
/// Backslash escapes have already been resolved by `:set` when it split its
/// arguments, so items here are separated by plain commas.
pub fn parse(spec: &[u8]) -> Result<ListChars, ListCharsError> {
    let spec = std::str::from_utf8(spec).map_err(|_| ListCharsError::NotUtf8)?;
    let mut chars = ListChars::none();

    for item in spec.split(',') {
        if item.is_empty() {
            continue;
        }
        let Some((name, value)) = item.split_once(':') else {
            return Err(ListCharsError::Malformed(item.to_owned()));
        };
        let glyphs: Vec<char> = value.chars().collect();
        let one = |wanted| match glyphs.as_slice() {
            [only] => Ok(*only),
            _ => Err(ListCharsError::WrongLength {
                item: name.to_owned(),
                wanted,
            }),
        };
        match name {
            // A tab takes a head and the character that fills the rest of its
            // width; vim allows a third for the final cell, which is accepted
            // and ignored rather than rejected.
            "tab" => {
                chars.tab = match glyphs.as_slice() {
                    [head] => Some((*head, ' ')),
                    [head, fill] | [head, fill, _] => Some((*head, *fill)),
                    _ => {
                        return Err(ListCharsError::WrongLength {
                            item: name.to_owned(),
                            wanted: "one to three characters",
                        });
                    }
                };
            }
            "trail" => chars.trail = Some(one("one character")?),
            "eol" => chars.eol = Some(one("one character")?),
            "nbsp" => chars.nbsp = Some(one("one character")?),
            "space" => chars.space = Some(one("one character")?),
            other => return Err(ListCharsError::UnknownItem(other.to_owned())),
        }
    }
    Ok(chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_spec_parses() {
        // The escaped space in `tab:>\ ` is resolved by `:set` before it gets
        // here, so this is what the parser actually sees.
        let chars =
            parse("tab:\u{25b8} ,trail:\u{b7},eol:\u{21b2},nbsp:\u{23b5},space:\u{b7}".as_bytes())
                .unwrap();
        assert_eq!(chars.tab, Some(('\u{25b8}', ' ')));
        assert_eq!(chars.trail, Some('\u{b7}'));
        assert_eq!(chars.eol, Some('\u{21b2}'));
        assert_eq!(chars.nbsp, Some('\u{23b5}'));
        assert_eq!(chars.space, Some('\u{b7}'));
    }

    #[test]
    fn items_left_out_are_left_alone() {
        let chars = parse(b"eol:$").unwrap();
        assert_eq!(chars.eol, Some('$'));
        assert_eq!(chars.tab, None);
        assert_eq!(chars.space, None);
        // An empty spec draws nothing at all, and a trailing comma is fine.
        assert_eq!(parse(b"").unwrap(), ListChars::none());
        assert_eq!(parse(b"eol:$,").unwrap().eol, Some('$'));
        // The built-in default names every kind, which is not the same as
        // nothing and not the same as vim's `eol:$`.
        assert_eq!(parse(b"eol:$").unwrap().tab, None);
        assert_eq!(ListChars::default().tab, Some(('\u{25b8}', ' ')));
    }

    #[test]
    fn a_tab_takes_a_head_and_a_fill() {
        assert_eq!(parse(b"tab:>-").unwrap().tab, Some(('>', '-')));
        // One character fills with spaces, three is vim's form and the last
        // is accepted rather than refused.
        assert_eq!(parse(b"tab:>").unwrap().tab, Some(('>', ' ')));
        assert_eq!(parse(b"tab:>-|").unwrap().tab, Some(('>', '-')));
    }

    #[test]
    fn a_spec_written_back_out_parses_to_the_same_thing() {
        let spec = "tab:> ,trail:.,eol:$,nbsp:_,space:.";
        let chars = parse(spec.as_bytes()).unwrap();
        assert_eq!(chars.to_string(), spec);
        assert_eq!(parse(chars.to_string().as_bytes()).unwrap(), chars);
        assert_eq!(ListChars::none().to_string(), "");
        // The built-in default is exactly the documented spec, and survives
        // the round trip like any other.
        let default = ListChars::default().to_string();
        assert_eq!(
            default,
            "tab:\u{25b8} ,trail:\u{b7},eol:\u{21b2},nbsp:\u{23b5},space:\u{b7}"
        );
        assert_eq!(parse(default.as_bytes()).unwrap(), ListChars::default());
    }

    #[test]
    fn malformed_specs_say_what_is_wrong() {
        assert_eq!(
            parse(b"eol"),
            Err(ListCharsError::Malformed("eol".to_owned()))
        );
        assert_eq!(
            parse(b"bogus:x"),
            Err(ListCharsError::UnknownItem("bogus".to_owned()))
        );
        // `eos` is not a vim item; catching the typo beats guessing at it.
        assert_eq!(
            parse(b"eos:$"),
            Err(ListCharsError::UnknownItem("eos".to_owned()))
        );
        assert_eq!(
            parse(b"eol:xy"),
            Err(ListCharsError::WrongLength {
                item: "eol".to_owned(),
                wanted: "one character"
            })
        );
        assert_eq!(parse(&[0xff, 0xfe]), Err(ListCharsError::NotUtf8));
    }
}
