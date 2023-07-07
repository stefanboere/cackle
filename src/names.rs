use std::fmt::Debug;
use std::fmt::Display;

/// A name of something. e.g. `std::path::Path`.
#[derive(Eq, PartialEq, Hash, Clone)]
pub(crate) struct Name {
    /// The components of this name. e.g. ["std", "path", "Path"]
    pub(crate) parts: Vec<String>,
}

/// Splits a composite name into names. Each name is further split on "::". For example:
/// "core::ptr::drop_in_place<std::rt::lang_start<()>::{{closure}}>" would split into:
/// [
///   ["core", "ptr", "drop_in_place"],
///   ["std", "rt", "lang_start"],
///   ["{{closure}}"],
/// ]
/// "<alloc::string::String as std::fmt::Debug>::fmt" would split into:
/// [
///   ["alloc", "string", "String"],
///   ["std", "fmt", "Debug", "fmt"],
/// ]
pub(crate) fn split_names(composite: &str) -> Vec<Name> {
    let mut all_names: Vec<Name> = Vec::new();
    let mut part = String::new();
    let mut parts = Vec::new();
    let mut chars = composite.chars();
    // True if we encountered " as ". When we subsequently encounter '>', we'll ignore it so
    // that the subsequent name part gets added to whatever part came after the " as ".
    let mut as_active = false;
    while let Some(ch) = chars.next() {
        if ch == '(' || ch == ')' {
            // Ignore parenthesis.
        } else if ch == '<' || ch == '>' {
            if as_active {
                as_active = false;
            } else {
                if !part.is_empty() {
                    parts.push(std::mem::take(&mut part));
                }
                if !parts.is_empty() {
                    all_names.push(Name {
                        parts: std::mem::take(&mut parts),
                    });
                }
            }
        } else if ch == ':' {
            if !part.is_empty() {
                parts.push(std::mem::take(&mut part));
            }
        } else if ch == ' ' {
            let mut ahead = chars.clone();
            if let (Some('a'), Some('s'), Some(' ')) = (ahead.next(), ahead.next(), ahead.next()) {
                chars = ahead;
                as_active = true;
                if !part.is_empty() {
                    parts.push(std::mem::take(&mut part));
                }
                if !parts.is_empty() {
                    all_names.push(Name {
                        parts: std::mem::take(&mut parts),
                    });
                }
            } else {
                part.push(ch);
            }
        } else {
            part.push(ch);
        }
    }
    if !part.is_empty() {
        parts.push(std::mem::take(&mut part));
    }
    if !parts.is_empty() {
        all_names.push(Name {
            parts: std::mem::take(&mut parts),
        });
    }
    all_names
}

impl Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.parts.join("::"))
    }
}

impl Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Name({})", self.parts.join("::"))
    }
}

impl Name {
    pub(crate) fn starts_with(&self, ignored_name: &Name) -> bool {
        self.parts.starts_with(&ignored_name.parts)
    }
}

#[test]
fn test_split_names() {
    fn borrow(input: &[Name]) -> Vec<Vec<&str>> {
        input
            .iter()
            .map(|name| name.parts.iter().map(|s| s.as_str()).collect())
            .collect()
    }

    let composite = "core::ptr::drop_in_place<std::rt::lang_start<()>::{{closure}}>";
    assert_eq!(
        borrow(&split_names(composite)),
        vec![
            vec!["core", "ptr", "drop_in_place"],
            vec!["std", "rt", "lang_start"],
            vec!["{{closure}}"],
        ]
    );

    let composite = "<alloc::string::String as core::fmt::Debug>::fmt";
    assert_eq!(
        borrow(&split_names(composite)),
        vec![
            vec!["alloc", "string", "String"],
            vec!["core", "fmt", "Debug", "fmt"]
        ]
    );
}
