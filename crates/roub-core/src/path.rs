//! Repository paths that check out cleanly on both Linux and Windows.
//!
//! The server validates every path when a changeset is created, so a repository never
//! contains a path that one of the supported platforms cannot represent.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;

use unicode_normalization::is_nfc;

/// Longest allowed path component in bytes. ext4 allows 255 bytes, NTFS 255 UTF-16 units;
/// 255 UTF-8 bytes satisfies both.
const MAX_COMPONENT_BYTES: usize = 255;

/// Longest allowed path in bytes.
const MAX_PATH_BYTES: usize = 4096;

/// Directory name reserved for roub's own metadata inside a working copy.
const METADATA_DIR: &str = ".roub";

/// Device names Windows reserves in every directory, with or without an extension.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Why a string is not a valid repository path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is empty")]
    Empty,
    #[error("path is longer than {MAX_PATH_BYTES} bytes")]
    TooLong,
    #[error("path must not start or end with '/'")]
    LeadingOrTrailingSlash,
    #[error("path contains an empty component ('//')")]
    EmptyComponent,
    #[error("path component '{0}' is not allowed")]
    DotComponent(String),
    #[error("path is not in Unicode NFC form")]
    NotNfc,
    #[error("path component '{component}' contains the character {character:?}")]
    ForbiddenCharacter { component: String, character: char },
    #[error("path component '{0}' ends with a dot or a space, which Windows strips")]
    TrailingDotOrSpace(String),
    #[error("path component '{0}' is a reserved device name on Windows")]
    WindowsReservedName(String),
    #[error("path component '{0}' is longer than {MAX_COMPONENT_BYTES} bytes")]
    ComponentTooLong(String),
    #[error("path component '{METADATA_DIR}' is reserved for roub metadata")]
    MetadataDirectory,
}

/// A validated repository path: `/`-separated, relative, NFC, valid on Linux and Windows.
///
/// Paths order component by component, comparing each component's bytes, which is the order of a
/// depth-first walk of the tree: `a/b` sorts before `a.b`, although `/` is a larger byte than `.`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RepoPath(String);

impl RepoPath {
    /// Validates `path` and wraps it.
    pub fn new(path: impl Into<String>) -> Result<Self, PathError> {
        let path = path.into();
        validate(&path)?;
        Ok(Self(path))
    }

    /// The path as stored in trees and changesets.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path's components, from the root down.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// Whether `self` is a strict ancestor directory of `other`: `a/b` of `a/b/c`, not of `a/bc`.
    pub fn is_ancestor_of(&self, other: &RepoPath) -> bool {
        other.0.len() > self.0.len()
            && other.0.starts_with(&self.0)
            && other.0.as_bytes()[self.0.len()] == b'/'
    }
}

impl Ord for RepoPath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.components().cmp(other.components())
    }
}

impl PartialOrd for RepoPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RepoPath({:?})", self.0)
    }
}

fn validate(path: &str) -> Result<(), PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(PathError::TooLong);
    }
    if path.starts_with('/') || path.ends_with('/') {
        return Err(PathError::LeadingOrTrailingSlash);
    }
    if !is_nfc(path) {
        return Err(PathError::NotNfc);
    }
    path.split('/').try_for_each(validate_component)
}

/// Validates one path component: a file or directory name as stored in a tree entry.
pub(crate) fn validate_component(component: &str) -> Result<(), PathError> {
    if component.is_empty() {
        return Err(PathError::EmptyComponent);
    }
    if component == "." || component == ".." {
        return Err(PathError::DotComponent(component.to_owned()));
    }
    if component.eq_ignore_ascii_case(METADATA_DIR) {
        return Err(PathError::MetadataDirectory);
    }
    if component.len() > MAX_COMPONENT_BYTES {
        return Err(PathError::ComponentTooLong(component.to_owned()));
    }
    if let Some(character) = component.chars().find(|&c| is_forbidden(c)) {
        return Err(PathError::ForbiddenCharacter {
            component: component.to_owned(),
            character,
        });
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return Err(PathError::TrailingDotOrSpace(component.to_owned()));
    }
    if is_windows_reserved(component) {
        return Err(PathError::WindowsReservedName(component.to_owned()));
    }
    Ok(())
}

/// Control characters, the separator and the characters Windows forbids in file names. A path
/// never reaches this with a `/`, since it is split first; a tree entry name can.
fn is_forbidden(c: char) -> bool {
    c.is_control() || matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*')
}

/// `NUL`, `nul.txt` and `Com1.tar.gz` are all reserved: Windows compares the part before the
/// first dot, ignoring case.
fn is_windows_reserved(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    WINDOWS_RESERVED
        .iter()
        .any(|reserved| stem.to_uppercase() == *reserved)
}

/// The key under which two names collide on a case-insensitive file system.
///
/// Unicode lowercase mapping approximates the NTFS upcase table; the exact table is an open design
/// decision (see `docs/design/decisions.md`). Every case check goes through this function.
pub(crate) fn fold_case(name: &str) -> String {
    name.to_lowercase()
}

/// Finds paths that would land in the same file or directory on a case-insensitive file
/// system, such as `docs/Readme.md` and `Docs/readme.md`.
///
/// Returns each colliding pair once, as the two differently cased prefixes. Case folding uses
/// Unicode lowercase mapping, which approximates the NTFS upcase table; the exact table is an
/// open design decision (see `docs/design/decisions.md`).
pub fn find_case_collisions<'a>(
    paths: impl IntoIterator<Item = &'a RepoPath>,
) -> Vec<(String, String)> {
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut collisions = Vec::new();
    for path in paths {
        let mut prefix = String::new();
        for component in path.components() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let folded = fold_case(&prefix);
            match seen.get(&folded) {
                Some(existing) if *existing != prefix => {
                    let pair = (existing.clone(), prefix.clone());
                    if !collisions.contains(&pair) {
                        collisions.push(pair);
                    }
                }
                Some(_) => {}
                None => {
                    seen.insert(folded, prefix.clone());
                }
            }
        }
    }
    collisions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> RepoPath {
        RepoPath::new(s).unwrap()
    }

    #[test]
    fn accepts_ordinary_paths() {
        for ok in [
            "README.md",
            "src/main.rs",
            "docs/architecture/roub-architecture.drawio",
            ".github/workflows/ci.yml",
            "naïve/český soubor.txt",
            "CONSOLE.md",
            "nullable.rs",
        ] {
            assert!(RepoPath::new(ok).is_ok(), "{ok} should be valid");
        }
    }

    #[test]
    fn rejects_structural_problems() {
        assert_eq!(RepoPath::new(""), Err(PathError::Empty));
        assert_eq!(
            RepoPath::new("/abs"),
            Err(PathError::LeadingOrTrailingSlash)
        );
        assert_eq!(
            RepoPath::new("dir/"),
            Err(PathError::LeadingOrTrailingSlash)
        );
        assert_eq!(RepoPath::new("a//b"), Err(PathError::EmptyComponent));
        assert_eq!(
            RepoPath::new("a/../b"),
            Err(PathError::DotComponent("..".into()))
        );
        assert_eq!(
            RepoPath::new("./a"),
            Err(PathError::DotComponent(".".into()))
        );
    }

    #[test]
    fn rejects_windows_problems() {
        assert!(matches!(
            RepoPath::new(r"dir\file"),
            Err(PathError::ForbiddenCharacter {
                character: '\\',
                ..
            })
        ));
        assert!(matches!(
            RepoPath::new("what?.txt"),
            Err(PathError::ForbiddenCharacter { character: '?', .. })
        ));
        assert!(matches!(
            RepoPath::new("tab\there"),
            Err(PathError::ForbiddenCharacter {
                character: '\t',
                ..
            })
        ));
        assert_eq!(
            RepoPath::new("notes."),
            Err(PathError::TrailingDotOrSpace("notes.".into()))
        );
        assert_eq!(
            RepoPath::new("dir /file"),
            Err(PathError::TrailingDotOrSpace("dir ".into()))
        );
        for reserved in ["nul", "NUL.txt", "src/Com1.tar.gz", "lpt³", "conout$"] {
            assert!(
                matches!(
                    RepoPath::new(reserved),
                    Err(PathError::WindowsReservedName(_))
                ),
                "{reserved} should be reserved"
            );
        }
    }

    #[test]
    fn rejects_non_nfc() {
        // "e" followed by a combining acute accent is NFD; NFC is the single code point é.
        assert_eq!(RepoPath::new("cafe\u{301}.txt"), Err(PathError::NotNfc));
        assert!(RepoPath::new("caf\u{e9}.txt").is_ok());
    }

    #[test]
    fn rejects_long_components_and_metadata_dir() {
        let long = "a".repeat(MAX_COMPONENT_BYTES + 1);
        assert_eq!(
            RepoPath::new(long.clone()),
            Err(PathError::ComponentTooLong(long))
        );
        assert_eq!(
            RepoPath::new("sub/.Roub/config"),
            Err(PathError::MetadataDirectory)
        );
    }

    #[test]
    fn finds_file_and_directory_collisions() {
        let paths = [
            path("docs/Readme.md"),
            path("docs/readme.md"),
            path("src/lib.rs"),
            path("Src/main.rs"),
            path("src/other.rs"),
        ];
        let collisions = find_case_collisions(&paths);
        assert_eq!(
            collisions,
            vec![
                ("docs/Readme.md".to_owned(), "docs/readme.md".to_owned()),
                ("src".to_owned(), "Src".to_owned()),
            ]
        );
    }

    #[test]
    fn orders_like_a_tree_walk() {
        let mut paths = [
            path("a.b"),
            path("a/b"),
            path("a"),
            path("a-b/c"),
            path("b"),
        ];
        paths.sort();
        let sorted: Vec<&str> = paths.iter().map(RepoPath::as_str).collect();
        assert_eq!(sorted, ["a", "a/b", "a-b/c", "a.b", "b"]);
    }

    #[test]
    fn ancestor_is_component_wise() {
        assert!(path("a/b").is_ancestor_of(&path("a/b/c")));
        assert!(!path("a/b").is_ancestor_of(&path("a/bc")));
        assert!(!path("a/b").is_ancestor_of(&path("a/b")));
        assert!(!path("a/b/c").is_ancestor_of(&path("a/b")));
    }

    #[test]
    fn no_collisions_for_distinct_paths() {
        let paths = [path("a/b.txt"), path("a/c.txt"), path("b/b.txt")];
        assert!(find_case_collisions(&paths).is_empty());
    }
}
