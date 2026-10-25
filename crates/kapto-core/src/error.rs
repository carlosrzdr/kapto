// SPDX-License-Identifier: Apache-2.0

//! Errors that users see: every failure carries a cause code, a message and a
//! remedy.
//!
//! A cause code ([`ErrorCode`]) is a stable, descriptive identifier such as
//! `config.unknown_key`. It names the kind of failure, so it can be searched
//! for in logs and documentation without a lookup table. Each code has a
//! one-line summary and a remedy telling the user what to do about it.
//!
//! A [`KaptoError`] is one occurrence of a failure: its code, optionally a
//! detail about this specific case, key/value context (a file path, a port)
//! and the underlying error that caused it.

use std::error::Error;
use std::fmt;

/// Defines [`ErrorCode`] and its table of identifiers, summaries and remedies
/// from a single list, so the enum and its texts can't drift apart.
macro_rules! error_codes {
    ($(
        $(#[doc = $doc:literal])*
        $variant:ident = $id:literal {
            summary: $summary:literal,
            remedy: $remedy:literal $(,)?
        }
    )+) => {
        /// A stable, descriptive identifier for one kind of failure.
        ///
        /// Identifiers never change once released; new failures get new codes.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[non_exhaustive]
        pub enum ErrorCode {
            $( $(#[doc = $doc])* $variant, )+
        }

        impl ErrorCode {
            /// Every code, in the order they are defined.
            pub const ALL: &'static [ErrorCode] = &[ $(ErrorCode::$variant),+ ];

            /// The identifier shown to users, e.g. `config.unknown_key`.
            #[must_use]
            pub const fn id(self) -> &'static str {
                match self { $(ErrorCode::$variant => $id,)+ }
            }

            /// A one-line description of the failure, without specifics.
            #[must_use]
            pub const fn summary(self) -> &'static str {
                match self { $(ErrorCode::$variant => $summary,)+ }
            }

            /// What the user can do to fix it.
            #[must_use]
            pub const fn remedy(self) -> &'static str {
                match self { $(ErrorCode::$variant => $remedy,)+ }
            }
        }
    };
}

mod codes;

pub use codes::ErrorCode;

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// One occurrence of a failure, ready to be shown to a user.
#[derive(Debug)]
pub struct KaptoError {
    code: ErrorCode,
    detail: Option<String>,
    context: Vec<(&'static str, String)>,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl KaptoError {
    /// Creates an error with the given code and no further information.
    #[must_use]
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            detail: None,
            context: Vec::new(),
            source: None,
        }
    }

    /// Adds what is specific to this occurrence, e.g. the key that was unknown.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Adds a named value that helps locate the problem, e.g. `("file", path)`.
    #[must_use]
    pub fn context(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.context.push((key, value.into()));
        self
    }

    /// Records the underlying error that caused this one.
    #[must_use]
    pub fn source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// The cause code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The code's summary, followed by the detail if there is one.
    #[must_use]
    pub fn message(&self) -> String {
        match &self.detail {
            Some(detail) => format!("{}: {detail}", self.code.summary()),
            None => self.code.summary().to_owned(),
        }
    }

    /// The code's remedy.
    #[must_use]
    pub fn remedy(&self) -> &'static str {
        self.code.remedy()
    }

    /// The named values added with [`KaptoError::context`], in order.
    #[must_use]
    pub fn context_entries(&self) -> &[(&'static str, String)] {
        &self.context
    }

    /// A multi-line report for people: message, context, causes and remedy.
    ///
    /// ```
    /// use kapto_core::{ErrorCode, KaptoError};
    ///
    /// let error = KaptoError::new(ErrorCode::ConfigUnknownKey)
    ///     .detail("`prot` in [server]")
    ///     .context("file", "/config/kapto.toml");
    /// assert_eq!(
    ///     error.report(),
    ///     "error[config.unknown_key]: Unknown configuration key: `prot` in [server]\n  \
    ///      file: /config/kapto.toml\n  \
    ///      remedy: Remove the key or correct its spelling. \
    ///      `kapto config show` lists every valid key.",
    /// );
    /// ```
    #[must_use]
    pub fn report(&self) -> String {
        Report(self, "error").to_string()
    }

    /// The same report for a problem that doesn't stop what was asked, such
    /// as a smaller receive buffer than asked for: `warning[...]` instead of
    /// `error[...]`.
    #[must_use]
    pub fn warning_report(&self) -> String {
        Report(self, "warning").to_string()
    }
}

/// Formats [`KaptoError::report`] under its label: an error or a warning.
struct Report<'a>(&'a KaptoError, &'static str);

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(error, label) = *self;
        write!(f, "{label}[{}]: {}", error.code.id(), error.message())?;
        for (key, value) in &error.context {
            write!(f, "\n  {key}: {value}")?;
        }
        let mut cause = Error::source(error);
        while let Some(source) = cause {
            write!(f, "\n  caused by: {source}")?;
            cause = source.source();
        }
        write!(f, "\n  remedy: {}", error.remedy())
    }
}

impl fmt::Display for KaptoError {
    /// One line for logs: the message followed by the code.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.message(), self.code.id())
    }
}

impl Error for KaptoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Converts any error into a [`KaptoError`] at the point where its meaning for
/// the user is known.
pub trait ResultExt<T> {
    /// Replaces the error with a [`KaptoError`] carrying `code`; the original
    /// error is kept as its source.
    ///
    /// # Errors
    ///
    /// Returns the converted error if `self` is an error.
    fn or_code(self, code: ErrorCode) -> Result<T, KaptoError>;
}

impl<T, E> ResultExt<T> for Result<T, E>
where
    E: Error + Send + Sync + 'static,
{
    fn or_code(self, code: ErrorCode) -> Result<T, KaptoError> {
        self.map_err(|error| KaptoError::new(code).source(error))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io;

    use super::*;

    #[test]
    fn error_code_ids_are_unique() {
        let mut seen = HashSet::new();
        for code in ErrorCode::ALL {
            assert!(seen.insert(code.id()), "duplicate id {}", code.id());
        }
    }

    #[test]
    fn error_code_ids_are_lowercase_names_grouped_by_area() {
        for code in ErrorCode::ALL {
            let id = code.id();
            let segments: Vec<&str> = id.split('.').collect();
            assert!(segments.len() >= 2, "{id} has no area prefix");
            for segment in segments {
                assert!(
                    !segment.is_empty()
                        && segment
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "{id} must be lowercase words separated by dots and underscores"
                );
            }
        }
    }

    #[test]
    fn every_error_code_has_a_summary_and_a_remedy_sentence() {
        for code in ErrorCode::ALL {
            let summary = code.summary();
            assert!(
                summary.starts_with(|c: char| c.is_ascii_uppercase()) && !summary.ends_with('.'),
                "{code}: the summary is a capitalised phrase without a full stop"
            );
            let remedy = code.remedy();
            assert!(
                remedy.starts_with(|c: char| c.is_ascii_uppercase()) && remedy.ends_with('.'),
                "{code}: the remedy is a full sentence"
            );
        }
    }

    #[test]
    fn error_texts_avoid_generic_catch_all_phrases() {
        const GENERIC: &[&str] = &[
            "something went wrong",
            "an error occurred",
            "unknown error",
            "unexpected error",
            "internal error",
            "please try again",
            "contact support",
        ];
        for code in ErrorCode::ALL {
            let texts = format!("{} {}", code.summary(), code.remedy()).to_lowercase();
            for phrase in GENERIC {
                assert!(!texts.contains(phrase), "{code} uses \"{phrase}\"");
            }
        }
    }

    #[test]
    fn message_is_the_summary_when_there_is_no_detail() {
        let error = KaptoError::new(ErrorCode::ConfigFileNotFound);
        assert_eq!(error.message(), "Configuration file not found");
    }

    #[test]
    fn message_appends_the_detail_to_the_summary() {
        let error = KaptoError::new(ErrorCode::ConfigFileNotFound).detail("/config/kapto.toml");
        assert_eq!(
            error.message(),
            "Configuration file not found: /config/kapto.toml"
        );
    }

    #[test]
    fn display_is_one_line_with_the_code_at_the_end() {
        let error = KaptoError::new(ErrorCode::ConfigPortConflict).detail("7700");
        assert_eq!(
            error.to_string(),
            "Two listeners are configured on the same port: 7700 [config.port_conflict]"
        );
    }

    #[test]
    fn report_lists_context_every_cause_and_the_remedy() {
        let error = KaptoError::new(ErrorCode::ConfigInvalidSyntax)
            .context("file", "/config/kapto.toml")
            .context("line", "3")
            .source(io::Error::other("expected `=`"));
        assert_eq!(
            error.report(),
            "error[config.invalid_syntax]: Configuration file is not valid TOML\n  \
             file: /config/kapto.toml\n  \
             line: 3\n  \
             caused by: expected `=`\n  \
             remedy: Fix the syntax at the reported line. \
             Strings need quotes, and each key is written `key = value`."
        );
    }

    #[test]
    fn a_warning_is_reported_like_an_error_under_its_own_label() {
        let problem = KaptoError::new(ErrorCode::ConfigInvalidSyntax).context("line", "3");
        assert_eq!(
            problem.warning_report(),
            problem.report().replacen("error[", "warning[", 1)
        );
        assert!(
            problem
                .warning_report()
                .starts_with("warning[config.invalid_syntax]: ")
        );
    }

    #[test]
    fn or_code_keeps_the_original_error_as_the_source() {
        let result: Result<(), io::Error> = Err(io::Error::other("permission denied"));
        let error = result.or_code(ErrorCode::ConfigUnreadable).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ConfigUnreadable);
        assert_eq!(
            Error::source(&error).map(ToString::to_string),
            Some("permission denied".to_owned())
        );
    }

    #[test]
    fn or_code_leaves_success_untouched() {
        let result: Result<u16, io::Error> = Ok(7700);
        assert_eq!(result.or_code(ErrorCode::ConfigUnreadable).ok(), Some(7700));
    }
}
