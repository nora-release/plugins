# Sandbox VM

This file provides guidance to coding agents working in `packages/sandbox/`, Nora's pure sandbox VM control library.

## What This Component Is

`sandbox-vm` owns VM and sandbox control only. Swift owns the agent SDK, model loop, tool definitions, product features, and environment routing.

Current product-facing responsibilities:

- local-vm runtime framework export for the macOS app
- local VM runtime ownership (`internal/localconnector`)
- sandbox execution host (`internal/envhost/sandbox`)
- execution-scoped runtime bridge (`internal/framework/runtimebridge`)
- VM lifecycle / vsock / guest daemon (`internal/platform/vm`)

## Current Architecture

```text
macos (SwiftUI)
  └─ local-vm runtime framework (sdk/apple/)
       ├─ internal/localconnector   local VM session runtime
       ├─ internal/envhost/         sandbox host + runtime bridge server
       ├─ internal/framework/
       │   └─ runtimebridge/        host callback + local runtime bridge
       └─ internal/platform/vm/     VM + vsock + guest daemon
```

## Directory Map

```text
packages/sandbox/
├── cmd/
│   └── vmd/
├── sdk/apple/               gomobile-facing runtime API
├── internal/envhost/        sandbox execution host + runtime bridge server
├── internal/framework/      runtime bridge
├── internal/localconnector/ local VM runtime ownership
├── internal/platform/vm/    VM lifecycle + guest daemon
└── internal/integrations/   local integration helpers
```

## Build and Test

Prefer root Makefile targets:

- `make install-go` download Go module dependencies
- `make build-go` build the sandbox framework target
- `make test-go` run Go tests

Use component targets when working directly inside `packages/sandbox`:

- `make framework` build the local-vm runtime artifact (current filename: `dist/SandboxVM.xcframework`)
- `make vm` build VM kernel/rootfs assets
- `make proto` regenerate `internal/platform/vm/vmrpc/*.pb.go`
- `make go-test` run Go tests

## Runtime Concepts

### Session and Local VM State

- runtime root defaults to `~/.nora/sandbox-vm`
- per-session local VM state lives under `~/.nora/sandbox-vm/sessions/<session-id>/`
- `localconnector` owns overlay restore / apply / discard

### Environment Model

- concrete execution host ownership now lives in `internal/envhost/sandbox`
- VM lifecycle ownership lives in `envhost`/`platform` layers, not in higher adapters

### Runtime Bridge

- capability URL generation and local callbacks live in `internal/framework/runtimebridge`
- host HTTP entrypoint lives in `internal/envhost/runtime_bridge_server.go`
- `sandbox` reuses the shared host-side runtime bridge server

## Guardrails

- Keep source-of-truth in current Go code; docs here must track implementation
- Do not reintroduce `agent-cli`, `slack-agentd`, `meetd`, `gcal-auth`, meeting joiner, prompt/history agent types, or old framework client/server paths
- Prefer product-facing runtime ownership in `localconnector`, `envhost`, `runtimebridge`, and `platform/vm`
- Avoid adding new tool/loop/LLM logic to `sandbox-vm`; the product agent now lives in `packages/agents` and the macOS app bridge
