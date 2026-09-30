//! The crate's error: a message for a person and a [`Kind`] for a program.
//!
//! The kind is decided where the error is raised, by the code that knows
//! what went wrong, and travels with the message from there. It used to be
//! recovered at the C ABI by searching the message for keywords ("not
//! found", "already exists", "full", ...), and many messages quote a name
//! the caller chose -- `parent '{path}' is not a directory` -- so the name
//! could decide the errno: creating `x` under a regular file called
//! `already exists` reported EEXIST, not ENOTDIR (#382).
//!
//! Wrapping an error in more context ([`Error::context`],
//! [`Error::map_message`]) keeps its kind. A failure whose kind is not one
//! of the specific ones below is [`Kind::Io`], which is also what a plain
//! `String` converts to: the block-device callbacks and the other string
//! errors from outside this crate carry no more than that.
//!
//! The message is what the `String` errors used to be, character for
//! character; [`Error`] derefs to `str`, compares equal to a `&str`, and
//! converts into a `String`, so code that reads the message reads it as
//! before.

use std::fmt;
use std::ops::Deref;
use std::os::raw::c_int;

/// What went wrong, as far as a caller that dispatches on it needs to know.
/// [`Kind::errno`] is the `<errno.h>` value for the platform the crate was
/// built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Kind {
    /// Anything without a more specific kind: an I/O failure, a volume
    /// whose metadata does not parse, a feature this driver does not have.
    Io,
    /// The name, attribute or stream asked for is not there. `ENOENT`.
    NotFound,
    /// The name asked for is already taken. `EEXIST`.
    Exists,
    /// No room: in the volume, the MFT, a record or an index. `ENOSPC`.
    NoSpace,
    /// The caller's argument is unacceptable: a NULL pointer, a path that
    /// is not UTF-8, a name Windows cannot hold. `EINVAL`.
    Invalid,
    /// A component that has to be a directory is not one. `ENOTDIR`.
    NotDirectory,
    /// The operation wants a file and was given a directory. `EISDIR`.
    IsDirectory,
    /// The directory to remove or replace still holds entries. `ENOTEMPTY`.
    NotEmpty,
    /// The driver refuses the operation on this volume or this file.
    /// `EPERM`.
    Refused,
    /// A caller's buffer is too small for the answer. `ERANGE`.
    Range,
}

impl Kind {
    /// The `<errno.h>` value of this kind on the platform the crate was
    /// built for.
    pub fn errno(self) -> c_int {
        match self {
            Kind::Io => libc::EIO,
            Kind::NotFound => libc::ENOENT,
            Kind::Exists => libc::EEXIST,
            Kind::NoSpace => libc::ENOSPC,
            Kind::Invalid => libc::EINVAL,
            Kind::NotDirectory => libc::ENOTDIR,
            Kind::IsDirectory => libc::EISDIR,
            Kind::NotEmpty => libc::ENOTEMPTY,
            Kind::Refused => libc::EPERM,
            Kind::Range => libc::ERANGE,
        }
    }
}

/// An error raised by this crate: a [`Kind`] and a message.
#[derive(Clone, PartialEq, Eq)]
pub struct Error {
    kind: Kind,
    message: String,
}

impl Error {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
        }
    }

    /// [`Kind::Io`].
    pub fn io(message: impl Into<String>) -> Self {
        Self::new(Kind::Io, message)
    }

    /// [`Kind::NotFound`].
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Kind::NotFound, message)
    }

    /// [`Kind::Exists`].
    pub fn exists(message: impl Into<String>) -> Self {
        Self::new(Kind::Exists, message)
    }

    /// [`Kind::NoSpace`].
    pub fn no_space(message: impl Into<String>) -> Self {
        Self::new(Kind::NoSpace, message)
    }

    /// [`Kind::Invalid`].
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Kind::Invalid, message)
    }

    /// [`Kind::NotDirectory`].
    pub fn not_directory(message: impl Into<String>) -> Self {
        Self::new(Kind::NotDirectory, message)
    }

    /// [`Kind::IsDirectory`].
    pub fn is_directory(message: impl Into<String>) -> Self {
        Self::new(Kind::IsDirectory, message)
    }

    /// [`Kind::NotEmpty`].
    pub fn not_empty(message: impl Into<String>) -> Self {
        Self::new(Kind::NotEmpty, message)
    }

    /// [`Kind::Refused`].
    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(Kind::Refused, message)
    }

    /// [`Kind::Range`].
    pub fn range(message: impl Into<String>) -> Self {
        Self::new(Kind::Range, message)
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Shorthand for `self.kind().errno()`.
    pub fn errno(&self) -> c_int {
        self.kind.errno()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// The same error with `context: ` in front of its message.
    pub fn context(self, context: impl fmt::Display) -> Self {
        self.map_message(|m| format!("{context}: {m}"))
    }

    /// The same error with its message rewritten by `f`, for context that
    /// does not fit [`Error::context`]'s `context: message` shape.
    pub fn map_message(self, f: impl FnOnce(&str) -> String) -> Self {
        let message = f(&self.message);
        Error {
            kind: self.kind,
            message,
        }
    }

    /// The same message under a different kind, for a caller that knows
    /// better than the code that raised it what the failure means here.
    pub fn with_kind(self, kind: Kind) -> Self {
        Error { kind, ..self }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Formats as the message alone, quoted, as the `String` errors did, so an
/// `unwrap` on a failure reads the same.
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.message, f)
    }
}

impl std::error::Error for Error {}

impl Deref for Error {
    type Target = str;
    fn deref(&self) -> &str {
        &self.message
    }
}

impl AsRef<str> for Error {
    fn as_ref(&self) -> &str {
        &self.message
    }
}

/// A string error from outside the crate -- a block-device callback, a
/// caller's `BlockIo` -- says nothing more specific than that I/O failed.
impl From<String> for Error {
    fn from(message: String) -> Self {
        Error::io(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Error::io(message)
    }
}

impl From<Error> for String {
    fn from(e: Error) -> String {
        e.message
    }
}

impl PartialEq<str> for Error {
    fn eq(&self, other: &str) -> bool {
        self.message == other
    }
}

impl PartialEq<&str> for Error {
    fn eq(&self, other: &&str) -> bool {
        self.message == *other
    }
}

impl PartialEq<String> for Error {
    fn eq(&self, other: &String) -> bool {
        &self.message == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_is_its_platform_errno() {
        assert_eq!(Kind::Io.errno(), libc::EIO);
        assert_eq!(Kind::NotFound.errno(), libc::ENOENT);
        assert_eq!(Kind::Exists.errno(), libc::EEXIST);
        assert_eq!(Kind::NoSpace.errno(), libc::ENOSPC);
        assert_eq!(Kind::Invalid.errno(), libc::EINVAL);
        assert_eq!(Kind::NotDirectory.errno(), libc::ENOTDIR);
        assert_eq!(Kind::IsDirectory.errno(), libc::EISDIR);
        assert_eq!(Kind::NotEmpty.errno(), libc::ENOTEMPTY);
        assert_eq!(Kind::Refused.errno(), libc::EPERM);
        assert_eq!(Kind::Range.errno(), libc::ERANGE);
    }

    #[test]
    fn context_keeps_the_kind() {
        let e = Error::not_directory("parent '/full' is not a directory").context("create_file");
        assert_eq!(e.kind(), Kind::NotDirectory);
        assert_eq!(e, "create_file: parent '/full' is not a directory");
    }

    #[test]
    fn map_message_keeps_the_kind() {
        let e = Error::exists("'a' already exists").map_message(|m| format!("[{m}]"));
        assert_eq!(e.kind(), Kind::Exists);
        assert_eq!(e.message(), "['a' already exists]");
    }

    #[test]
    fn with_kind_keeps_the_message() {
        let e = Error::io("bad").with_kind(Kind::Refused);
        assert_eq!(e.kind(), Kind::Refused);
        assert_eq!(e.message(), "bad");
    }

    #[test]
    fn a_string_is_an_io_error_whatever_it_says() {
        for s in ["x not found", "already exists", "volume is full", "invalid"] {
            assert_eq!(Error::from(s.to_string()).kind(), Kind::Io, "{s}");
            assert_eq!(Error::from(s).kind(), Kind::Io, "{s}");
        }
    }

    #[test]
    fn it_reads_as_its_message() {
        let e = Error::not_found("'a' not found");
        assert_eq!(e.to_string(), "'a' not found");
        assert_eq!(format!("{e:?}"), "\"'a' not found\"");
        assert!(e.contains("not found"));
        assert_eq!(String::from(e.clone()), "'a' not found");
        assert_eq!(e, "'a' not found".to_string());
    }
}
