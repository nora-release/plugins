.DEFAULT_GOAL := build
SWIFT ?= swift
PLUGIN_TAG ?= v$(shell python3 -c "import json; print(json.load(open('nora-plugin.json'))['version'])")
.PHONY: install build test package

install:
	$(SWIFT) package resolve

build:
	bash script/build-app.sh

test:
	$(SWIFT) test

package: build
	mkdir -p dist
	ditto -c -k --keepParent .build/NoraGitHub.app "dist/com.nora.github-$(PLUGIN_TAG)-macos.zip"
