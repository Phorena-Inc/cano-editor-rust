PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
HELP_DIR ?= $(PREFIX)/share/cano/help
CARGO ?= cargo

.PHONY: all release debug test smoke clean install uninstall

all: release

release:
	CANO_HELP_DIR="$(HELP_DIR)" $(CARGO) build --release --locked
	mkdir -p build
	cp target/release/cano build/cano

debug:
	CANO_HELP_DIR="$(HELP_DIR)" $(CARGO) build --locked
	mkdir -p build
	cp target/debug/cano build/cano-debug

test:
	$(CARGO) test --locked --all-targets

smoke: release
	python3 tests/pty_smoke.py target/release/cano

clean:
	$(CARGO) clean
	rm -rf build

install: release
	install -Dm755 target/release/cano "$(DESTDIR)$(BINDIR)/cano"
	install -d "$(DESTDIR)$(HELP_DIR)"
	install -m644 docs/help/* "$(DESTDIR)$(HELP_DIR)/"

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/cano"
	rm -rf "$(DESTDIR)$(HELP_DIR)"

