CARGO ?= cargo
GATEWAY_PROVIDERS := $(CURDIR)/packages/gateway-providers
REGISTRY ?= $(CURDIR)/registry.json
CATALOG ?= $(CURDIR)/marketplace.json
# The marketplace tool for the UI plugins' catalog.
PLUGIN_KIT_REV := a63cfcf8ef1bcf3c868b75439a5600db740fa369
TOOLS_ROOT := $(CURDIR)/.build/tools/$(PLUGIN_KIT_REV)
NORA_PLUGIN := $(TOOLS_ROOT)/bin/nora-plugin

.PHONY: build test clippy check fmt fmt-check release marketplace marketplace-check release-entry

# The gateway provider plugins: a Cargo workspace of their own.
build:
	$(CARGO) build --manifest-path "$(GATEWAY_PROVIDERS)/Cargo.toml" --workspace

test:
	$(CARGO) test --manifest-path "$(GATEWAY_PROVIDERS)/Cargo.toml" --workspace

clippy:
	$(CARGO) clippy --manifest-path "$(GATEWAY_PROVIDERS)/Cargo.toml" --workspace --all-targets -- -D warnings

fmt:
	$(CARGO) fmt --manifest-path "$(GATEWAY_PROVIDERS)/Cargo.toml" --all

fmt-check:
	$(CARGO) fmt --manifest-path "$(GATEWAY_PROVIDERS)/Cargo.toml" --all --check

check: fmt-check clippy test

# Packages every gateway provider plugin and rewrites gateway-providers.json;
# pass TAG=gateway-providers-N, and PUBLISH=1 to upload to that release.
release:
	python3 "$(GATEWAY_PROVIDERS)/release.py" --tag "$(TAG)" $(if $(PUBLISH),--publish)

$(NORA_PLUGIN):
	$(CARGO) install --locked --quiet --git https://github.com/nora-release/plugin-kit --rev "$(PLUGIN_KIT_REV)" --root "$(TOOLS_ROOT)" nora-plugin

marketplace: $(NORA_PLUGIN)
	"$(NORA_PLUGIN)" generate --registry "$(REGISTRY)" --output "$(CATALOG)"

marketplace-check: $(NORA_PLUGIN)
	"$(NORA_PLUGIN)" validate --registry "$(REGISTRY)" --catalog "$(CATALOG)" --verify-downloads

# Describes a UI plugin release archive; leave TEAM empty for an ad-hoc signed release.
release-entry: $(NORA_PLUGIN)
	"$(NORA_PLUGIN)" release-entry --manifest "$(MANIFEST)" --archive "$(ARCHIVE)" \
		--repository "$(REPOSITORY)" --tag "$(TAG)" --output "$(OUTPUT)" $(if $(TEAM),--team "$(TEAM)")
