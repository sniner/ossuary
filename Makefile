# Every directory with a src/main.rs holds a program. The directory name is
# the package name, which install-git, install-crates.io and uninstall pass
# to cargo.
BINS := $(patsubst %/src/main.rs,%,$(wildcard */src/main.rs))
# Packages with publish = false are not on crates.io; install-crates.io
# leaves them out.
UNPUBLISHED := $(patsubst %/Cargo.toml,%,$(shell grep -l '^publish *= *false' $(BINS:=/Cargo.toml)))
PUBLISHED := $(filter-out $(UNPUBLISHED),$(BINS))
GIT_URL := https://github.com/sniner/ossuary

.PHONY: help install install-git install-crates.io uninstall

help:
	@echo 'make install            install all programs from this clone'
	@echo 'make install-git        install all programs from the main branch on GitHub'
	@echo 'make install-crates.io  install the latest release from crates.io, without ossuary-fix'
	@echo 'make uninstall          uninstall all programs, whatever they were installed from'

# cargo install takes one package per --path and does not accept the
# virtual manifest at the root, so each program is installed on its own.
# The builds share target/, the dependencies are compiled only once.
install:
	@for p in $(BINS); do cargo install --locked --path $$p || exit 1; done

install-git:
	@cargo install --locked --git $(GIT_URL) $(BINS)

install-crates.io:
	@cargo install --locked $(PUBLISHED)

# cargo uninstall exits with an error for a package that is not installed,
# such as ossuary-fix after install-crates.io, so only the installed ones
# are passed.
uninstall:
	@pkgs=$$(cargo install --list | sed -n 's/^\([^ ]*\) .*:$$/\1/p' | grep -Fx $(addprefix -e ,$(BINS))); \
	if [ -n "$$pkgs" ]; then cargo uninstall $$pkgs; fi
