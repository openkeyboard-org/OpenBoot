# Repository entry point. The firmware and host tool keep their native build
# systems; this file only orchestrates the common workflows.

.DEFAULT_GOAL := all

CARGO ?= cargo

.PHONY: all firmware tool test check clean

all: firmware tool

firmware:
	$(MAKE) -C firmware all

tool:
	$(CARGO) build --release --manifest-path tools/Cargo.toml

test:
	$(MAKE) -C firmware test
	$(CARGO) test --manifest-path tools/Cargo.toml

check:
	$(MAKE) -C firmware matrix-report
	$(MAKE) -C firmware test
	$(CARGO) test --manifest-path tools/Cargo.toml

clean:
	$(MAKE) -C firmware clean
	$(CARGO) clean --manifest-path tools/Cargo.toml
