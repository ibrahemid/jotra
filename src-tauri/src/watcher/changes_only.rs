//! A watcher backend that delivers what changed a file and drops what only
//! read it.
//!
//! `notify` registers `IN_OPEN` on every folder it watches on Linux, so a
//! watcher that reads a file to classify its change hears its own read as the
//! next change, and the read that classifies that one raises another
//! ([`jotra_core::watcher::sighting`]). Dropping reads where the backend
//! delivers them, before the debouncer turns every event into a bare path,
//! ends that loop on every volume, the ones whose timestamps cannot tell two
//! writes apart included.

use std::path::Path;

use notify::event::{AccessKind, AccessMode};
use notify::{Config, Event, EventHandler, EventKind, PathsMut, RecursiveMode, Watcher};

/// Whether `kind` reports that a file was opened, read or closed after
/// reading, none of which changes it. A close after writing is a change.
pub fn is_read_access(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Access(
            AccessKind::Read
                | AccessKind::Open(_)
                | AccessKind::Close(AccessMode::Read | AccessMode::Execute)
        )
    )
}

/// Hands every event but a read to `inner`.
struct DropReads<F> {
    inner: F,
}

impl<F: EventHandler> EventHandler for DropReads<F> {
    fn handle_event(&mut self, event: notify::Result<Event>) {
        if event
            .as_ref()
            .is_ok_and(|event| is_read_access(&event.kind))
        {
            return;
        }
        self.inner.handle_event(event);
    }
}

/// `W`, delivering every event except the reads [`is_read_access`] names.
pub struct ChangesOnly<W> {
    inner: W,
}

impl<W: Watcher> Watcher for ChangesOnly<W> {
    fn new<F: EventHandler>(event_handler: F, config: Config) -> notify::Result<Self> {
        Ok(Self {
            inner: W::new(
                DropReads {
                    inner: event_handler,
                },
                config,
            )?,
        })
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        self.inner.watch(path, recursive_mode)
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        self.inner.unwatch(path)
    }

    fn paths_mut<'me>(&'me mut self) -> Box<dyn PathsMut + 'me> {
        self.inner.paths_mut()
    }

    fn configure(&mut self, option: Config) -> notify::Result<bool> {
        self.inner.configure(option)
    }

    fn kind() -> notify::WatcherKind {
        W::kind()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind};
    use std::path::PathBuf;
    use std::sync::mpsc;

    /// Every kind of event a backend can raise for one file, in order.
    fn every_kind() -> Vec<EventKind> {
        vec![
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Access(AccessKind::Open(AccessMode::Write)),
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Access(AccessKind::Close(AccessMode::Execute)),
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            EventKind::Create(CreateKind::File),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            EventKind::Remove(RemoveKind::File),
            EventKind::Any,
        ]
    }

    /// A backend that raises [`every_kind`] for a path when it is watched.
    struct EveryKind {
        handler: Box<dyn EventHandler>,
    }

    impl Watcher for EveryKind {
        fn new<F: EventHandler>(event_handler: F, _config: Config) -> notify::Result<Self> {
            Ok(Self {
                handler: Box::new(event_handler),
            })
        }

        fn watch(&mut self, path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
            for kind in every_kind() {
                self.handler
                    .handle_event(Ok(Event::new(kind).add_path(path.to_path_buf())));
            }
            self.handler
                .handle_event(Err(notify::Error::generic("lost")));
            Ok(())
        }

        fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
            Ok(())
        }

        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }

    #[test]
    fn a_read_is_dropped_and_every_change_is_delivered() {
        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher = ChangesOnly::<EveryKind>::new(tx, Config::default()).unwrap();
        watcher
            .watch(Path::new("/notes/one.md"), RecursiveMode::NonRecursive)
            .unwrap();

        let delivered: Vec<notify::Result<Event>> = rx.try_iter().collect();
        let kinds: Vec<EventKind> = delivered
            .iter()
            .filter_map(|event| event.as_ref().ok().map(|event| event.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::Access(AccessKind::Close(AccessMode::Write)),
                EventKind::Create(CreateKind::File),
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                EventKind::Remove(RemoveKind::File),
                EventKind::Any,
            ]
        );
        assert!(
            delivered
                .iter()
                .filter_map(|event| event.as_ref().ok())
                .all(|event| event.paths == vec![PathBuf::from("/notes/one.md")]),
            "a delivered event lost its path"
        );
        assert_eq!(
            delivered.iter().filter(|event| event.is_err()).count(),
            1,
            "a backend error must still reach the debouncer"
        );
    }

    #[test]
    fn the_backend_kind_is_the_wrapped_one() {
        assert_eq!(
            <ChangesOnly<EveryKind> as Watcher>::kind(),
            notify::WatcherKind::NullWatcher
        );
    }
}
