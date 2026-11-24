# Makefile for appmgr (Rust)

PREFIX ?= /usr/local
USER_PREFIX ?= $(HOME)/.local
CARGO ?= cargo

.PHONY: all build install install-user test clean completions

all: build

build:
	$(CARGO) build --release --locked

test:
	$(CARGO) test
	$(CARGO) clippy --all-targets -- -D warnings

completions: build
	mkdir -p completions/bash completions/zsh completions/fish
	target/release/appmgr completions bash > completions/bash/appmgr
	target/release/appmgr completions zsh > completions/zsh/_appmgr
	target/release/appmgr completions fish > completions/fish/appmgr.fish

install-user: build completions
	install -d $(USER_PREFIX)/bin
	install -d $(USER_PREFIX)/share/bash-completion/completions
	install -d $(USER_PREFIX)/share/zsh/site-functions
	install -d $(USER_PREFIX)/share/fish/vendor_completions.d
	install -d $(USER_PREFIX)/share/man/man1

	install -m 755 target/release/appmgr $(USER_PREFIX)/bin/appmgr
	install -m 644 completions/bash/appmgr $(USER_PREFIX)/share/bash-completion/completions/appmgr
	install -m 644 completions/zsh/_appmgr $(USER_PREFIX)/share/zsh/site-functions/_appmgr
	install -m 644 completions/fish/appmgr.fish $(USER_PREFIX)/share/fish/vendor_completions.d/appmgr.fish
	if [ -f man/man1/appmgr.1 ]; then install -m 644 man/man1/appmgr.1 $(USER_PREFIX)/share/man/man1/appmgr.1; fi
	@echo "appmgr v2.2.1 installed successfully to $(USER_PREFIX)/bin/appmgr"

install: build completions
	install -d $(DESTDIR)$(PREFIX)/bin
	install -d $(DESTDIR)$(PREFIX)/share/bash-completion/completions
	install -d $(DESTDIR)$(PREFIX)/share/zsh/site-functions
	install -d $(DESTDIR)$(PREFIX)/share/fish/vendor_completions.d
	install -d $(DESTDIR)$(PREFIX)/share/man/man1

	install -m 755 target/release/appmgr $(DESTDIR)$(PREFIX)/bin/appmgr
	install -m 644 completions/bash/appmgr $(DESTDIR)$(PREFIX)/share/bash-completion/completions/appmgr
	install -m 644 completions/zsh/_appmgr $(DESTDIR)$(PREFIX)/share/zsh/site-functions/_appmgr
	install -m 644 completions/fish/appmgr.fish $(DESTDIR)$(PREFIX)/share/fish/vendor_completions.d/appmgr.fish
	if [ -f man/man1/appmgr.1 ]; then install -m 644 man/man1/appmgr.1 $(DESTDIR)$(PREFIX)/share/man/man1/appmgr.1; fi

clean:
	$(CARGO) clean
