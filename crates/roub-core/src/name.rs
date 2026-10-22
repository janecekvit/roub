//! Names of repositories and refs.
//!
//! Both are `/`-separated ASCII names such as `tools/roub` or `release/2026.10`. The character set
//! is small enough that a name needs no escaping in a URL path, a log line, a metrics label or a
//! database column, and two names that differ only in letter case are still distinct.

use std::fmt;

/// Longest allowed name in bytes.
const MAX_NAME_BYTES: usize = 255;

/// Why a string is not a valid repository or ref name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name is empty")]
    Empty,
    #[error("name is longer than {MAX_NAME_BYTES} bytes")]
    TooLong,
    #[error("name has an empty segment (leading, trailing or double '/')")]
    EmptySegment,
    #[error("name segment '{0}' starts with a dot")]
    LeadingDot(String),
    #[error(
        "name contains the character {0:?}; only ASCII letters, digits, '-', '_', '.' and '/' are allowed"
    )]
    ForbiddenCharacter(char),
}

/// Checks the rules shared by repository and ref names: 1 to 255 bytes of ASCII letters, digits,
/// `-`, `_` and `.`, in `/`-separated segments that are not empty and do not start with a dot
/// (which also rules out `.` and `..`).
fn validate(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(NameError::TooLong);
    }
    if let Some(character) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')))
    {
        return Err(NameError::ForbiddenCharacter(character));
    }
    for segment in name.split('/') {
        if segment.is_empty() {
            return Err(NameError::EmptySegment);
        }
        if segment.starts_with('.') {
            return Err(NameError::LeadingDot(segment.to_owned()));
        }
    }
    Ok(())
}

macro_rules! name_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(name: impl Into<String>) -> Result<Self, NameError> {
                let name = name.into();
                validate(&name)?;
                Ok(Self(name))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({:?})", stringify!($name), self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = NameError;

            fn from_str(name: &str) -> Result<Self, NameError> {
                Self::new(name)
            }
        }
    };
}

name_type!(
    /// A validated repository name, such as `monorepo` or `tools/roub`.
    RepositoryName
);

name_type!(
    /// A validated ref name, such as `main` or `release/2026.10`.
    RefName
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_names() {
        for name in ["main", "release/2026.10", "tools/roub", "a-b_c.d", "A/b"] {
            assert!(RefName::new(name).is_ok(), "{name}");
            assert!(RepositoryName::new(name).is_ok(), "{name}");
        }
        assert!(RefName::new("x".repeat(MAX_NAME_BYTES)).is_ok());
    }

    #[test]
    fn rejects_invalid_names() {
        let cases = [
            ("", NameError::Empty),
            ("/main", NameError::EmptySegment),
            ("main/", NameError::EmptySegment),
            ("a//b", NameError::EmptySegment),
            ("..", NameError::LeadingDot("..".into())),
            ("a/./b", NameError::LeadingDot(".".into())),
            ("release/.hidden", NameError::LeadingDot(".hidden".into())),
            ("a b", NameError::ForbiddenCharacter(' ')),
            ("a\\b", NameError::ForbiddenCharacter('\\')),
            ("a:b", NameError::ForbiddenCharacter(':')),
            ("ref\n", NameError::ForbiddenCharacter('\n')),
            ("větev", NameError::ForbiddenCharacter('ě')),
        ];
        for (name, error) in cases {
            assert_eq!(RefName::new(name), Err(error), "{name:?}");
        }
        assert_eq!(
            RefName::new("x".repeat(MAX_NAME_BYTES + 1)),
            Err(NameError::TooLong)
        );
    }

    #[test]
    fn parses_and_prints() {
        let name: RepositoryName = "tools/roub".parse().unwrap();
        assert_eq!(name.to_string(), "tools/roub");
        assert_eq!(format!("{name:?}"), r#"RepositoryName("tools/roub")"#);
    }
}
