use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

/// A path argument that has already been checked for shape.
///
/// The TypeScript build carried a table of validator functions and a
/// `registerHandlers` wrapper that applied them before each handler ran.
/// Tauri deserialises command arguments with serde, so the check belongs in
/// the type instead: a command that takes an `AbsolutePath` cannot be called
/// with anything that failed validation, and no wrapper has to remember to
/// apply it.
///
/// This proves the value is a path shape we are willing to reason about. It
/// does **not** prove the user chose it - for anything destructive that means
/// checking the path against the library, which is the caller's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsolutePath(PathBuf);

impl AbsolutePath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consume into an owned path, for handing to a blocking thread.
    pub fn into_inner(self) -> PathBuf {
        self.0
    }

    fn check(text: &str) -> Result<Self, String> {
        if text.is_empty() || text.len() > 32_767 {
            return Err("implausible path length".to_owned());
        }
        if text.contains('\0') {
            return Err("NUL byte in path".to_owned());
        }
        // \\?\ and \\.\ reach devices and bypass Win32 path normalisation.
        let bytes = text.as_bytes();
        if bytes.len() >= 4
            && bytes.first() == Some(&b'\\')
            && bytes.get(1) == Some(&b'\\')
            && matches!(bytes.get(2), Some(b'?') | Some(b'.'))
            && bytes.get(3) == Some(&b'\\')
        {
            return Err("device-namespace path".to_owned());
        }
        let path = PathBuf::from(text);
        if !path.is_absolute() {
            return Err("expected an absolute path".to_owned());
        }
        Ok(Self(path))
    }
}

impl<'de> Deserialize<'de> for AbsolutePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::check(&text).map_err(serde::de::Error::custom)
    }
}

/// A relative directory inside a game folder - where an install writes.
///
/// The core refuses an unsafe relative path anyway, and does so against the
/// filesystem where a junction is visible. This is the boundary's own check:
/// the same principle as `AbsolutePath`, so a handler cannot be handed a shape
/// nobody looked at. The empty string is valid and means the game folder
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeDir(String);

impl RelativeDir {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn check(text: &str) -> Result<Self, String> {
        if text.len() > 4_096 {
            return Err("implausible path length".to_owned());
        }
        if text.contains('\0') {
            return Err("NUL byte in path".to_owned());
        }
        // A colon is a drive letter or an alternate data stream; neither is a
        // relative directory.
        if text.contains(':') {
            return Err("colon in relative path".to_owned());
        }
        if text.starts_with('/') || text.starts_with('\\') {
            return Err("rooted or UNC path".to_owned());
        }
        for segment in text.split(['/', '\\']) {
            if segment == ".." {
                return Err("path escapes the game folder".to_owned());
            }
        }
        Ok(Self(text.to_owned()))
    }
}

impl<'de> Deserialize<'de> for RelativeDir {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::check(&text).map_err(serde::de::Error::custom)
    }
}

/// Bounded free text. Anything reaching the DOM or a catalogue lookup has a
/// length limit rather than being taken as arbitrary input.
#[derive(Debug, Clone)]
pub struct LanguageTag(String);

impl LanguageTag {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn check(text: &str) -> Result<Self, String> {
        if text.len() > 35 {
            return Err("language tag is too long".to_owned());
        }
        let mut parts = text.split('-');
        let primary = parts.next().unwrap_or_default();
        if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err("not a language tag".to_owned());
        }
        if !parts
            .all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return Err("not a language tag".to_owned());
        }
        Ok(Self(text.to_owned()))
    }
}

impl<'de> Deserialize<'de> for LanguageTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::check(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> Result<AbsolutePath, String> {
        AbsolutePath::check(value)
    }

    #[test]
    fn relative_install_directories_are_checked_at_the_boundary_too() {
        assert!(RelativeDir::check("bin/x64").is_ok());
        assert!(RelativeDir::check("bin\\x64").is_ok());
        // The game folder itself.
        assert!(RelativeDir::check("").is_ok());

        assert!(RelativeDir::check("../escape").is_err());
        assert!(RelativeDir::check("bin/../../escape").is_err());
        assert!(RelativeDir::check("/etc").is_err());
        assert!(RelativeDir::check("\\\\server\\share").is_err());
        assert!(RelativeDir::check("C:\\Windows").is_err());
        assert!(RelativeDir::check("bin:stream").is_err());
        assert!(RelativeDir::check("bin/\0x").is_err());
    }

    #[test]
    fn accepts_real_paths_and_refuses_the_dangerous_shapes() {
        assert!(path(if cfg!(windows) {
            r"C:\Games\Skyrim"
        } else {
            "/games/skyrim"
        })
        .is_ok());
        assert!(path("Games/Skyrim").is_err());
        assert!(path("").is_err());
        assert!(path("C:\\a\0b").is_err());
        assert!(path(r"\\?\C:\Windows").is_err());
        assert!(path(r"\\.\PhysicalDrive0").is_err());
    }

    #[test]
    fn language_tags_are_shape_checked() {
        assert!(LanguageTag::check("en").is_ok());
        assert!(LanguageTag::check("pt-BR").is_ok());
        assert!(LanguageTag::check("zh-Hans-CN").is_ok());
        assert!(LanguageTag::check("Not A Tag").is_err());
        assert!(LanguageTag::check("e").is_err());
        assert!(LanguageTag::check(&"x".repeat(40)).is_err());
    }
}
