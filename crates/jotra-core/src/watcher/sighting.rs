//! What a watcher has already looked at, so it does not look twice.
//!
//! A watcher classifies a delivered event by reading the file it names. On
//! Linux that read is itself a watched event: `notify` registers `IN_OPEN` on
//! every directory it watches, so opening the file raises another event for
//! the same path, whose classification opens the file again. The loop never
//! settles, and every turn of it announces the same change: one rename-over
//! inside the notes folder arrived as eleven identical `BufferExternal`
//! events on CI. macOS and Windows never showed it because neither backend
//! reports a read.
//!
//! The watchers drop read events where the backend delivers them, before they
//! are debounced, so the read never comes back. [`LastSeen`] bounds what is
//! left, the same change delivered more than once, by asking from the file's
//! metadata alone whether the event says anything the previous one did not.
//! Metadata is read without opening the file, so the question costs nothing
//! and raises nothing. A file whose length and modification time have not
//! moved since the watcher last looked has nothing new to say, and the event
//! is dropped before any read happens.
//!
//! That answer holds only where the modification time can tell two writes
//! apart ([`crate::notes::guard::is_fine_grained_mtime`]). A volume that keeps
//! whole seconds or two (HFS+, FAT, exFAT) gives two saves of the same length
//! inside one tick the same sighting, so a file with a coarse or missing time
//! is always news and its bytes decide.
//!
//! Time is passed in rather than read, so the decision is testable without a
//! clock or a filesystem.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long one look is remembered.
///
/// The floor is the round trip a watcher's own read takes to come back as an
/// event: one debounce window plus scheduling slack. The ceiling is the width
/// of the coarse-timestamp case in the module docs, so this stays as short as
/// it can be while still covering the round trip. It matches
/// [`super::ignore::DEFAULT_IGNORE_TTL`], which is sized the same way.
pub const DEFAULT_SIGHTING_TTL: Duration = Duration::from_secs(5);

/// What a file is, as its metadata describes it.
///
/// Deliberately not the inode: a file's identity is read over candidate paths
/// when it matters (ADR-033), and there is no portable equivalent to compare
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSighting {
    /// The file's length in bytes.
    pub len: u64,
    /// The file's modification time, where the filesystem reports one.
    pub modified: Option<SystemTime>,
}

/// One recorded look at a path. `sighting` is `None` for a path with no file
/// at it, which is a state like any other: a file that is still gone is not
/// news a second time.
#[derive(Debug, Clone, Copy)]
struct Sighted {
    sighting: Option<FileSighting>,
    at: Instant,
}

/// What each watched path was last seen to hold.
///
/// One of these lives for as long as a watcher thread does, across delivered
/// batches: the echo of a read arrives in a later batch than the change that
/// caused it, so a record scoped to one batch would not see it.
#[derive(Debug, Default)]
pub struct LastSeen {
    inner: HashMap<PathBuf, Sighted>,
}

impl LastSeen {
    /// An empty record.
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Records `sighting` for `path`, and answers whether it differs from the
    /// last one recorded within `ttl`.
    ///
    /// `true` for a path never looked at, for a look older than `ttl`, for a
    /// file whose length or modification time has moved, and for a file whose
    /// modification time is too coarse to tell two writes apart. `false` only
    /// when the file is exactly as this watcher last found it and its time
    /// says no write landed since, or when the path is still empty.
    pub fn is_news(
        &mut self,
        path: &Path,
        sighting: Option<FileSighting>,
        now: Instant,
        ttl: Duration,
    ) -> bool {
        self.forget_older_than(now, ttl);
        let previous = self
            .inner
            .insert(path.to_path_buf(), Sighted { sighting, at: now });
        match previous {
            Some(previous) => {
                previous.sighting != sighting
                    || !tells_writes_apart(sighting)
                    || now.saturating_duration_since(previous.at) > ttl
            }
            None => true,
        }
    }

    /// How many paths are remembered. The map is bounded by how many files
    /// changed in the last `ttl`, not by how many the folder holds.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn forget_older_than(&mut self, now: Instant, ttl: Duration) {
        self.inner
            .retain(|_, seen| now.saturating_duration_since(seen.at) <= ttl);
    }
}

/// Whether a sighting equal to the last one means no write landed between
/// them: there is no file, or its modification time is fine grained.
fn tells_writes_apart(sighting: Option<FileSighting>) -> bool {
    sighting.is_none_or(|sighting| {
        sighting
            .modified
            .is_some_and(crate::notes::guard::is_fine_grained_mtime)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = DEFAULT_SIGHTING_TTL;

    fn at(len: u64, modified_secs: u64) -> Option<FileSighting> {
        Some(FileSighting {
            len,
            modified: Some(
                SystemTime::UNIX_EPOCH
                    + Duration::from_secs(modified_secs)
                    + Duration::from_nanos(123_456_789),
            ),
        })
    }

    fn at_coarse(len: u64, modified_secs: u64) -> Option<FileSighting> {
        Some(FileSighting {
            len,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_secs)),
        })
    }

    #[test]
    fn a_same_length_write_inside_one_coarse_tick_is_news() {
        let mut seen = LastSeen::new();
        let path = Path::new("/notes/one.md");
        let now = Instant::now();
        assert!(seen.is_news(path, at_coarse(29, 100), now, TTL));
        assert!(
            seen.is_news(path, at_coarse(29, 100), now + Duration::from_secs(1), TTL),
            "a coarse time cannot say the second write did not happen"
        );
    }

    #[test]
    fn a_file_with_no_modification_time_is_always_news() {
        let mut seen = LastSeen::new();
        let path = Path::new("/notes/one.md");
        let now = Instant::now();
        let untimed = Some(FileSighting {
            len: 29,
            modified: None,
        });
        assert!(seen.is_news(path, untimed, now, TTL));
        assert!(seen.is_news(path, untimed, now, TTL));
    }

    #[test]
    fn the_first_look_at_a_path_is_always_news() {
        let mut seen = LastSeen::new();
        assert!(seen.is_news(Path::new("/notes/one.md"), at(12, 100), Instant::now(), TTL));
    }

    #[test]
    fn a_burst_of_events_for_a_file_that_has_not_moved_is_news_once() {
        // One write, then eleven deliveries of the same unchanged file.
        let mut seen = LastSeen::new();
        let path = Path::new("/notes/one.md");
        let now = Instant::now();
        let news = (0..11)
            .filter(|_| seen.is_news(path, at(29, 100), now, TTL))
            .count();
        assert_eq!(
            news, 1,
            "the same file must be news once, not once per read"
        );
    }

    #[test]
    fn a_file_written_again_is_news_again() {
        let mut seen = LastSeen::new();
        let path = Path::new("/notes/one.md");
        let now = Instant::now();
        assert!(seen.is_news(path, at(29, 100), now, TTL));
        assert!(!seen.is_news(path, at(29, 100), now, TTL));
        assert!(seen.is_news(path, at(29, 101), now, TTL));
    }

    #[test]
    fn a_file_of_the_same_age_that_changed_length_is_news() {
        let mut seen = LastSeen::new();
        let path = Path::new("/notes/one.md");
        let now = Instant::now();
        assert!(seen.is_news(path, at(29, 100), now, TTL));
        assert!(seen.is_news(path, at(30, 100), now, TTL));
    }

    #[test]
    fn a_file_that_is_still_gone_is_news_once() {
        let mut seen = LastSeen::new();
        let path = Path::new("/notes/one.md");
        let now = Instant::now();
        assert!(seen.is_news(path, None, now, TTL));
        assert!(!seen.is_news(path, None, now, TTL));
    }

    #[test]
    fn a_file_that_came_back_is_news() {
        let mut seen = LastSeen::new();
        let path = Path::new("/notes/one.md");
        let now = Instant::now();
        assert!(seen.is_news(path, at(29, 100), now, TTL));
        assert!(seen.is_news(path, None, now, TTL));
        assert!(seen.is_news(path, at(29, 100), now, TTL));
    }

    #[test]
    fn two_paths_are_remembered_apart() {
        let mut seen = LastSeen::new();
        let now = Instant::now();
        assert!(seen.is_news(Path::new("/notes/one.md"), at(29, 100), now, TTL));
        assert!(seen.is_news(Path::new("/notes/two.md"), at(29, 100), now, TTL));
        assert!(!seen.is_news(Path::new("/notes/one.md"), at(29, 100), now, TTL));
    }

    #[test]
    fn a_look_older_than_the_ttl_is_news_again() {
        let mut seen = LastSeen::new();
        let path = Path::new("/notes/one.md");
        let now = Instant::now();
        assert!(seen.is_news(path, at(29, 100), now, TTL));
        let later = now + TTL + Duration::from_secs(1);
        assert!(seen.is_news(path, at(29, 100), later, TTL));
    }

    #[test]
    fn a_look_older_than_the_ttl_is_forgotten_rather_than_kept() {
        let mut seen = LastSeen::new();
        let now = Instant::now();
        for i in 0..50 {
            seen.is_news(
                &PathBuf::from(format!("/notes/{i}.md")),
                at(1, 100),
                now,
                TTL,
            );
        }
        assert_eq!(seen.len(), 50);
        let later = now + TTL + Duration::from_secs(1);
        seen.is_news(Path::new("/notes/last.md"), at(1, 100), later, TTL);
        assert_eq!(
            seen.len(),
            1,
            "a quiet folder must not be remembered forever"
        );
        assert!(!seen.is_empty());
    }
}
