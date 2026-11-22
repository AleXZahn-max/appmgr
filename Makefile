# Makefile for appmgr

PREFIX ?= /usr/local
USER_PREFIX ?= $(HOME)/.local
PYTHON ?= python3

all:
	@echo "appmgr is ready. Run 'make install-user' or 'sudo make install'."

install-user:
	install -d $(USER_PREFIX)/bin
	install -d $(USER_PREFIX)/share/appmgr/appmgr
	install -d $(USER_PREFIX)/share/bash-completion/completions
	install -d $(USER_PREFIX)/share/zsh/site-functions
	install -d $(USER_PREFIX)/share/fish/vendor_completions.d
	install -d $(USER_PREFIX)/share/man/man1

	# Install Python library modules
	cp -r appmgr/* $(USER_PREFIX)/share/appmgr/appmgr/

	# Install binary launcher
	sed 's|project_root = .*|project_root = Path("$(USER_PREFIX)/share/appmgr")|' bin/appmgr > $(USER_PREFIX)/bin/appmgr
	chmod 755 $(USER_PREFIX)/bin/appmgr

	# Install completions
	install -m 644 completions/bash/appmgr $(USER_PREFIX)/share/bash-completion/completions/appmgr
	install -m 644 completions/zsh/_appmgr $(USER_PREFIX)/share/zsh/site-functions/_appmgr
	install -m 644 completions/fish/appmgr.fish $(USER_PREFIX)/share/fish/vendor_completions.d/appmgr.fish

	# Install man page
	install -m 644 man/man1/appmgr.1 $(USER_PREFIX)/share/man/man1/appmgr.1
	@echo "appmgr installed successfully to $(USER_PREFIX)/bin/appmgr"

install:
	install -d $(DESTDIR)$(PREFIX)/bin
	install -d $(DESTDIR)$(PREFIX)/share/appmgr/appmgr
	install -d $(DESTDIR)$(PREFIX)/share/bash-completion/completions
	install -d $(DESTDIR)$(PREFIX)/share/zsh/site-functions
	install -d $(DESTDIR)$(PREFIX)/share/fish/vendor_completions.d
	install -d $(DESTDIR)$(PREFIX)/share/man/man1

	# Install Python library modules
	cp -r appmgr/* $(DESTDIR)$(PREFIX)/share/appmgr/appmgr/

	# Install binary launcher
	sed 's|project_root = .*|project_root = Path("$(PREFIX)/share/appmgr")|' bin/appmgr > $(DESTDIR)$(PREFIX)/bin/appmgr
	chmod 755 $(DESTDIR)$(PREFIX)/bin/appmgr

	# Install completions
	install -m 644 completions/bash/appmgr $(DESTDIR)$(PREFIX)/share/bash-completion/completions/appmgr
	install -m 644 completions/zsh/_appmgr $(DESTDIR)$(PREFIX)/share/zsh/site-functions/_appmgr
	install -m 644 completions/fish/appmgr.fish $(DESTDIR)$(PREFIX)/share/fish/vendor_completions.d/appmgr.fish

	# Install man page
	install -m 644 man/man1/appmgr.1 $(DESTDIR)$(PREFIX)/share/man/man1/appmgr.1
	@echo "appmgr installed successfully to $(PREFIX)/bin/appmgr"

uninstall:
	rm -rf $(DESTDIR)$(PREFIX)/bin/appmgr
	rm -rf $(DESTDIR)$(PREFIX)/share/appmgr
	rm -f $(DESTDIR)$(PREFIX)/share/bash-completion/completions/appmgr
	rm -f $(DESTDIR)$(PREFIX)/share/zsh/site-functions/_appmgr
	rm -f $(DESTDIR)$(PREFIX)/share/fish/vendor_completions.d/appmgr.fish
	rm -f $(DESTDIR)$(PREFIX)/share/man/man1/appmgr.1

package:
	makepkg -f

clean:
	rm -rf build/ dist/ *.egg-info/ *.pkg.tar.zst src/ pkg/

.PHONY: all install install-user uninstall package clean
