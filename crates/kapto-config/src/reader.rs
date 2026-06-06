// SPDX-License-Identifier: Apache-2.0

//! Reading the keys of one small TOML file (`package.toml`, a profile, a
//! task), collecting every problem with the file and line it is on, so one
//! run reports them all.

use std::ops::RangeInclusive;

use kapto_core::{ErrorCode, KaptoError};
use toml_edit::TableLike;

use crate::file::ConfigFile;
use crate::located::{at_key, unknown_key};
use crate::suggest::closest;

/// One key of a file, for documentation and for checking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileKey {
    /// The key.
    pub name: &'static str,
    /// Whether every file has it.
    pub required: bool,
    /// What it says, and its default if it has one.
    pub help: &'static str,
}

/// Reads the keys of one file's top-level table.
pub(crate) struct Reader<'a> {
    pub(crate) file: &'a ConfigFile,
    pub(crate) root: &'a dyn TableLike,
    pub(crate) errors: Vec<KaptoError>,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(file: &'a ConfigFile, root: &'a dyn TableLike) -> Self {
        Self {
            file,
            root,
            errors: Vec::new(),
        }
    }

    /// Records `error` at the line of `key`.
    pub(crate) fn located(&mut self, error: KaptoError, key: &str) {
        self.errors.push(at_key(error, self.file, self.root, key));
    }

    /// Reports every key not in `keys` with `unknown`, suggesting the
    /// closest known one, and every required key that is missing.
    pub(crate) fn check_keys(&mut self, keys: &[FileKey], unknown: ErrorCode, what: &str) {
        let names: Vec<&str> = keys.iter().map(|key| key.name).collect();
        let present: Vec<String> = self.root.iter().map(|(key, _)| key.to_owned()).collect();
        for key in &present {
            if !names.contains(&key.as_str()) {
                let suggestion = closest(key, names.iter().copied());
                self.located(unknown_key(unknown, key, suggestion), key);
            }
        }
        for key in keys.iter().filter(|key| key.required) {
            if self.root.get(key.name).is_none() {
                let first = present.first().map_or(key.name, String::as_str);
                let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
                    .detail(format!("{what} needs `{}`", key.name));
                self.located(error, first);
            }
        }
    }

    /// The text at `key` if `valid` holds for it; `None` (and an error) if
    /// it doesn't, or `None` if it's absent.
    pub(crate) fn text(
        &mut self,
        key: &str,
        expected: &str,
        valid: fn(&str) -> bool,
    ) -> Option<String> {
        let item = self.root.get(key)?;
        let text = item.as_str().filter(|text| valid(text));
        if text.is_none() {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
                .detail(format!("`{key}` must be {expected}"));
            self.located(error, key);
        }
        text.map(str::to_owned)
    }

    /// The list of texts at `key`, each one for which `valid` holds; empty
    /// (and an error) if one doesn't or it isn't a list, or empty if it's
    /// absent.
    pub(crate) fn texts(
        &mut self,
        key: &str,
        expected: &str,
        valid: fn(&str) -> bool,
    ) -> Vec<String> {
        let Some(item) = self.root.get(key) else {
            return Vec::new();
        };
        let texts: Option<Vec<String>> = item.as_array().and_then(|array| {
            array
                .iter()
                .map(|value| value.as_str().filter(|text| valid(text)).map(str::to_owned))
                .collect()
        });
        if texts.is_none() {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
                .detail(format!("`{key}` must be a list of {expected}"));
            self.located(error, key);
        }
        texts.unwrap_or_default()
    }

    /// The switch at `key`, `true` or `false`; `None` (and an error) if it
    /// is something else, or `None` if it's absent.
    pub(crate) fn switch(&mut self, key: &str) -> Option<bool> {
        let item = self.root.get(key)?;
        let switch = item.as_bool();
        if switch.is_none() {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue)
                .detail(format!("`{key}` must be true or false"));
            self.located(error, key);
        }
        switch
    }

    /// The whole number at `key` if it is in `range`; `None` (and an error)
    /// if it isn't, or `None` if it's absent.
    pub(crate) fn whole(&mut self, key: &str, range: RangeInclusive<i64>) -> Option<i64> {
        let item = self.root.get(key)?;
        let whole = item.as_integer().filter(|whole| range.contains(whole));
        if whole.is_none() {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                "`{key}` must be a whole number from {} to {}",
                range.start(),
                range.end()
            ));
            self.located(error, key);
        }
        whole
    }

    /// The number at `key`, whole or not, if it is in `range`; `None` (and
    /// an error) if it isn't, or `None` if it's absent.
    pub(crate) fn number(&mut self, key: &str, range: RangeInclusive<f64>) -> Option<f64> {
        let item = self.root.get(key)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "any whole number in a sensible range is exact"
        )]
        let number = item
            .as_float()
            .or_else(|| item.as_integer().map(|whole| whole as f64))
            .filter(|number| range.contains(number));
        if number.is_none() {
            let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                "`{key}` must be a number from {} to {}",
                range.start(),
                range.end()
            ));
            self.located(error, key);
        }
        number
    }
}
