GO_BUILD_PARALLELISM ?= 1

.PHONY: all framework vm proto go-test

all: proto vm framework

ifeq ($(DEBUG),1)
GCFLAGS := -gcflags=all="-N -l"
endif

# Generate protobuf code
proto:
	protoc --go_out=. --go_opt=paths=source_relative \
		--go-grpc_out=. --go-grpc_opt=paths=source_relative \
		internal/platform/vm/vmrpc/vmrpc.proto

go-test:
	go test -p $(GO_BUILD_PARALLELISM) ./cmd/vmd/... ./sdk/apple/... ./internal/... ./pkg/...

vm:
	./scripts/kbuild.sh
	./scripts/build-vm.sh

framework:
	./scripts/build-framework.sh
