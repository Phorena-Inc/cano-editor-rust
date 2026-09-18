//! Fresh Cano implementation derived exclusively from the migration documents.

pub mod app;
pub mod autoformat;
pub mod backup;
pub mod buffer;
pub mod cli;
pub mod command;
pub mod comment;
pub mod config;
pub mod editor;
pub mod explorer;
pub mod history;
pub mod io;
pub mod jump;
pub mod listchars;
pub mod markdown;
pub mod process;
pub mod recent;
pub mod render;
pub mod substitute;
pub mod syntax;
pub mod terminal;
pub mod textobject;

/// A scratch directory for filesystem tests, removed again on drop.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    pub(crate) struct Fixture {
        pub(crate) root: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new(label: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should follow the Unix epoch")
                .as_nanos();
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "cano-fresh-{label}-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&root).expect("create test fixture");
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
