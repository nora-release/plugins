CARGO ?= cargo
PROVIDERS_OUTPUT ?= $(CURDIR)/.build/providers
PACKAGES_OUTPUT ?= $(CURDIR)/.build/packages

.PHONY: build package test check fmt fmt-check

build:
	CARGO="$(CARGO)" bash scripts/build-providers.sh build "$(PROVIDERS_OUTPUT)"

package:
	CARGO="$(CARGO)" bash scripts/package-providers.sh "$(PACKAGES_OUTPUT)"

test:
	CARGO="$(CARGO)" bash scripts/build-providers.sh --test

check: fmt-check test

fmt:
	CARGO="$(CARGO)" bash scripts/build-providers.sh --fmt

fmt-check:
	CARGO="$(CARGO)" bash scripts/build-providers.sh --fmt-check
