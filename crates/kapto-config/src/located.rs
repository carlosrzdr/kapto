// SPDX-License-Identifier: Apache-2.0

//! Where in a configuration file a problem is: every error about a file
//! names the file and the line, so it can be found and fixed.

use kapto_core::{ErrorCode, KaptoError};
use toml_edit::TableLike;

use crate::file::ConfigFile;

/// A file that isn't valid TOML, with the position of the problem.
pub(crate) fn syntax_error(file: &ConfigFile, error: &toml_edit::TomlError) -> KaptoError {
    let mut report = KaptoError::new(ErrorCode::ConfigInvalidSyntax)
        .detail(error.message().trim_end().to_owned())
        .context("file", file.path.display().to_string());
    if let Some(span) = error.span() {
        let (line, column) = line_and_column(&file.contents, span.start);
        report = report
            .context("line", line.to_string())
            .context("column", column.to_string());
    }
    report
}

/// An error with `code` for the key `name`, which isn't one, suggesting
/// the closest that is if there is one.
pub(crate) fn unknown_key(code: ErrorCode, name: &str, suggestion: Option<&str>) -> KaptoError {
    let detail = match suggestion {
        Some(suggestion) => format!("`{name}` (did you mean `{suggestion}`?)"),
        None => format!("`{name}`"),
    };
    KaptoError::new(code).detail(detail)
}

/// `error`, found on `line` of `file`.
pub(crate) fn at_line(error: KaptoError, file: &ConfigFile, line: usize) -> KaptoError {
    error
        .context("file", file.path.display().to_string())
        .context("line", line.to_string())
}

/// `error`, found at the key `name` of `table` in `file`.
pub(crate) fn at_key(
    error: KaptoError,
    file: &ConfigFile,
    table: &dyn TableLike,
    name: &str,
) -> KaptoError {
    at_line(error, file, line_of(file, table, name))
}

/// The line a key is on, falling back to its value's line, then to line 1.
pub(crate) fn line_of(file: &ConfigFile, table: &dyn TableLike, name: &str) -> usize {
    let span = table
        .get_key_value(name)
        .and_then(|(key, item)| key.span().or_else(|| item.span()));
    span.map_or(1, |span| line_and_column(&file.contents, span.start).0)
}

/// 1-based line and column of a byte offset.
pub(crate) fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map_or(before.len(), |newline| before.len() - newline - 1)
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_and_column_are_one_based() {
        assert_eq!(line_and_column("ab\ncd", 0), (1, 1));
        assert_eq!(line_and_column("ab\ncd", 4), (2, 2));
    }
}
