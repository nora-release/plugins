CARGO ?= cargo

.PHONY: build test check fmt fmt-check

build:
	$(CARGO) build --workspace --locked

test:
	$(CARGO) test --workspace --locked

check: fmt-check test

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check
