PYTHON ?= python3
CARGO_MANIFEST := rust/Cargo.toml
PYTEST := $(PYTHON) -m pytest

.PHONY: help check build legacy comparator unit harness differential knowledge security integration verify demo proxy

help:
	@printf '%s\n' \
	  'make check        format, clippy, Rust tests, comparator tests, knowledge validation' \
	  'make integration  build both servers and run C-only plus differential tests' \
	  'make security     run cargo-audit and cargo-deny (must be installed)' \
	  'make verify       run check, security, and integration' \
	  'make demo         run the short interview demonstration'

check: comparator unit knowledge
	cargo fmt --manifest-path $(CARGO_MANIFEST) --all -- --check
	cargo clippy --manifest-path $(CARGO_MANIFEST) --workspace --all-targets -- -D warnings

build:
	cargo build --manifest-path $(CARGO_MANIFEST) --workspace --release

legacy:
	bash pipeline/build_legacy.sh

comparator:
	$(PYTEST) harness/test_diff_engine.py -q

unit:
	cargo test --manifest-path $(CARGO_MANIFEST) --workspace

harness: build legacy
	$(PYTEST) harness/tests/ --ignore=harness/tests/test_differential.py --ignore=harness/tests/test_proxy.py -q --timeout=30 --timeout-method=thread

differential: build legacy
	$(PYTEST) harness/tests/test_differential.py -q --timeout=120 --timeout-method=thread

proxy: build legacy
	$(PYTEST) harness/tests/test_proxy.py -q --timeout=60 --timeout-method=thread

knowledge:
	$(PYTHON) pipeline/validate_knowledge.py

security:
	@command -v cargo-audit >/dev/null || { echo 'cargo-audit is required: cargo install cargo-audit --locked'; exit 1; }
	@command -v cargo-deny >/dev/null || { echo 'cargo-deny is required: cargo install cargo-deny --locked'; exit 1; }
	@command -v cargo-geiger >/dev/null || { echo 'cargo-geiger is required: cargo install cargo-geiger --locked'; exit 1; }
	cargo audit --file rust/Cargo.lock
	cargo deny --manifest-path rust/Cargo.toml check
	bash pipeline/audit_unsafe.sh

integration: harness differential proxy

verify: check security integration

demo: build legacy
	bash scripts/demo.sh
