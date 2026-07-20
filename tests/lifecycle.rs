use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cano-fresh-lifecycle-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn run(&self, args: &[&Path]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cano"))
            .args(args)
            .current_dir(&self.0)
            .env("HOME", self.path("home"))
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn startup_errors_are_reported_before_a_terminal_is_required() {
    let fixture = Fixture::new();
    let missing_value = fixture.run(&[Path::new("--config")]);
    assert_eq!(missing_value.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing_value.stderr).contains("<init.lua>"));

    let unexpected = fixture.run(&[Path::new("--unknown")]);
    assert_eq!(unexpected.status.code(), Some(1));
    assert_eq!(unexpected.stderr, b"Unexpected flag\n");

    // A nonexistent file is a fresh buffer, not an error, so an unreadable
    // path (a directory) stands in as the unopenable file.
    let config = fixture.path("init.lua");
    std::fs::write(&config, b"setup({})").unwrap();
    let directory = fixture.path("subdir");
    std::fs::create_dir(&directory).unwrap();
    let output = fixture.run(&[Path::new("--config"), &config, &directory]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("Could not open "));
}

#[test]
fn lua_exit_propagates_without_initializing_the_terminal() {
    let fixture = Fixture::new();
    let file = fixture.path("file.txt");
    let config = fixture.path("exit.lua");
    std::fs::write(&file, b"text\n").unwrap();
    std::fs::write(
        &config,
        br#"local cano = setup({}); cano.exit(42, "fresh exit")"#,
    )
    .unwrap();

    let output = fixture.run(&[Path::new("--config"), &config, &file]);
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(
        output.stdout,
        b"Exiting as specified in the configuration, message: fresh exit\n"
    );
    assert!(output.stderr.is_empty());
}
