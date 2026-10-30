CARGO ?= cargo
# Set PROVIDER_ID to test/build/package one plugin; an empty value selects all.
PROVIDER_ID ?=
export PROVIDER_ID
PROVIDERS_OUTPUT ?= $(CURDIR)/.build/providers
PACKAGES_OUTPUT ?= $(CURDIR)/.build/packages
REGISTRY ?= $(CURDIR)/registry.json
CATALOG ?= $(CURDIR)/marketplace.json
# The marketplace tool comes from the same plugin-kit revision as the SDK.
PLUGIN_KIT_REV := $(shell sed -n 's/^nora-plugin-manifest = .*rev = "\([0-9a-f]*\)".*/\1/p' $(CURDIR)/Cargo.toml)
TOOLS_ROOT := $(CURDIR)/.build/tools/$(PLUGIN_KIT_REV)
NORA_PLUGIN := $(TOOLS_ROOT)/bin/nora-plugin

.PHONY: build package test test-scripts check fmt fmt-check marketplace marketplace-check

build:
	CARGO="$(CARGO)" bash scripts/build-providers.sh build "$(PROVIDERS_OUTPUT)"

package:
	CARGO="$(CARGO)" bash scripts/package-providers.sh "$(PACKAGES_OUTPUT)"

test:
	CARGO="$(CARGO)" bash scripts/build-providers.sh --test

test-scripts:
	bash -n scripts/*.sh
	PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p 'test_*.py'

check: fmt-check test-scripts test

fmt:
	CARGO="$(CARGO)" bash scripts/build-providers.sh --fmt

fmt-check:
	CARGO="$(CARGO)" bash scripts/build-providers.sh --fmt-check

$(NORA_PLUGIN):
	$(CARGO) install --locked --quiet --git https://github.com/nora-release/plugin-kit --rev "$(PLUGIN_KIT_REV)" --root "$(TOOLS_ROOT)" nora-plugin

marketplace: $(NORA_PLUGIN)
	"$(NORA_PLUGIN)" generate --registry "$(REGISTRY)" --output "$(CATALOG)"

marketplace-check: $(NORA_PLUGIN)
	"$(NORA_PLUGIN)" validate --registry "$(REGISTRY)" --catalog "$(CATALOG)" --verify-downloads
