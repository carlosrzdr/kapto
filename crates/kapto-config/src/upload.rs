// SPDX-License-Identifier: Apache-2.0

//! How episodes are uploaded, from the `[upload]` section of `kapto.toml`:
//! when, how fast, how persistently, how they are verified, and what
//! happens to the local copies. Each is a deployment setting like any other,
//! overridden by its environment variable; a destination may override them
//! for itself.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use crate::source::Effective;

/// Which episodes are uploaded by themselves (`upload.when`). Any episode
/// can still be uploaded by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadWhen {
    /// `never`: only by hand.
    Never,
    /// `finalised`: every finished episode, with warnings or not.
    Finalised,
    /// `finalised_clean`: finished with nothing lost, dropped or recovered,
    /// and pre-flight passed.
    FinalisedClean,
    /// `accepted`: no quality gate failed.
    Accepted,
}

/// What happens to an episode's local copy once it is uploaded
/// (`upload.delete_local`). One not verified on enough destinations is never
/// deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteLocal {
    /// `never`: kept.
    Never,
    /// `when_disk_low`: the oldest verified go first when the disk runs low.
    WhenDiskLow,
    /// `after_upload`: deleted as soon as verified.
    AfterUpload,
}

/// How an uploaded file is checked to be the one sent (`upload.verify`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verification {
    /// `server_checksum`: the store checks each file's SHA-256 itself;
    /// where it can't, the file is read back.
    ServerChecksum,
    /// `read_back`: each file is read back and hashed again.
    ReadBack,
    /// `size_only`: only the size is compared.
    SizeOnly,
}

macro_rules! words {
    ($type:ty { $($variant:ident = $word:literal),+ $(,)? }) => {
        impl $type {
            /// Every word accepted, in documentation order.
            pub const WORDS: &'static [&'static str] = &[$($word),+];
        }

        impl FromStr for $type {
            type Err = ();

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                match text {
                    $($word => Ok(Self::$variant),)+
                    _ => Err(()),
                }
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self {
                    $(Self::$variant => $word,)+
                })
            }
        }
    };
}

words!(UploadWhen {
    Never = "never",
    Finalised = "finalised",
    FinalisedClean = "finalised_clean",
    Accepted = "accepted",
});
words!(DeleteLocal {
    Never = "never",
    WhenDiskLow = "when_disk_low",
    AfterUpload = "after_upload",
});
words!(Verification {
    ServerChecksum = "server_checksum",
    ReadBack = "read_back",
    SizeOnly = "size_only",
});

/// The `[upload]` settings, with the source of every value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadSettings {
    /// Which episodes are uploaded by themselves.
    pub when: Effective<UploadWhen>,
    /// Whether uploads wait while an episode is recorded.
    pub pause_while_recording: Effective<bool>,
    /// What happens to the local copies.
    pub delete_local: Effective<DeleteLocal>,
    /// On how many destinations an episode is verified before its local
    /// copy may go.
    pub min_verified_destinations: Effective<u32>,
    /// How uploaded files are checked.
    pub verify: Effective<Verification>,
    /// Whether the completeness marker is written.
    pub marker: Effective<bool>,
    /// The size of a file's parts, in MiB.
    pub part_size_mib: Effective<u64>,
    /// How many parts of a file go up at once.
    pub parallel_parts: Effective<u32>,
    /// How many files go up at once.
    pub parallel_files: Effective<u32>,
    /// The most requests in flight at once.
    pub max_in_flight: Effective<u32>,
    /// The bandwidth cap in MiB/s; 0 for none.
    pub bandwidth_mib: Effective<u32>,
    /// How many times a file is tried before its episode fails.
    pub max_attempts: Effective<u32>,
    /// How long before a failed episode is tried again.
    pub retry_failed_after: Effective<Duration>,
    /// The first wait before a request is tried again.
    pub backoff_initial: Effective<Duration>,
    /// The longest wait before a request is tried again.
    pub backoff_max: Effective<Duration>,
    /// How long connecting may take.
    pub timeout_connect: Effective<Duration>,
    /// How long one request may take.
    pub timeout_request: Effective<Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_word_reads_back_as_written_and_others_are_refused() {
        for word in UploadWhen::WORDS {
            assert_eq!(word.parse::<UploadWhen>().unwrap().to_string(), *word);
        }
        for word in DeleteLocal::WORDS {
            assert_eq!(word.parse::<DeleteLocal>().unwrap().to_string(), *word);
        }
        for word in Verification::WORDS {
            assert_eq!(word.parse::<Verification>().unwrap().to_string(), *word);
        }
        assert!("finalized".parse::<UploadWhen>().is_err());
        assert!("server-checksum".parse::<Verification>().is_err());
    }
}
