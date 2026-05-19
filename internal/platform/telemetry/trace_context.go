package telemetry

import (
	"context"
	"strings"
)

const (
	traceparentMetadataKey = "otel.traceparent"
	tracestateMetadataKey  = "otel.tracestate"

	traceparentEnvKey = "CUE_TRACEPARENT"
	tracestateEnvKey  = "CUE_TRACESTATE"
)

type TraceContext struct {
	Traceparent string
	Tracestate  string
}

func TraceContextFromStrings(traceparent, tracestate string) TraceContext {
	return TraceContext{
		Traceparent: strings.TrimSpace(traceparent),
		Tracestate:  strings.TrimSpace(tracestate),
	}
}

func (tc TraceContext) IsZero() bool {
	return tc.Traceparent == "" && tc.Tracestate == ""
}

func (tc TraceContext) InjectEnv(env map[string]string) map[string]string {
	if tc.IsZero() {
		return env
	}
	if env == nil {
		env = make(map[string]string)
	}
	if tc.Traceparent != "" {
		env[traceparentEnvKey] = tc.Traceparent
	}
	if tc.Tracestate != "" {
		env[tracestateEnvKey] = tc.Tracestate
	}
	return env
}

func (tc TraceContext) ApplyToMetadata(metadata map[string]interface{}) {
	if tc.IsZero() {
		return
	}
	if metadata == nil {
		return
	}
	if tc.Traceparent != "" {
		metadata[traceparentMetadataKey] = tc.Traceparent
	}
	if tc.Tracestate != "" {
		metadata[tracestateMetadataKey] = tc.Tracestate
	}
}

func TraceContextFromMetadata(metadata map[string]interface{}) TraceContext {
	if metadata == nil {
		return TraceContext{}
	}
	var tc TraceContext
	if raw, ok := metadata[traceparentMetadataKey]; ok {
		if value, ok := raw.(string); ok {
			tc.Traceparent = strings.TrimSpace(value)
		}
	}
	if raw, ok := metadata[tracestateMetadataKey]; ok {
		if value, ok := raw.(string); ok {
			tc.Tracestate = strings.TrimSpace(value)
		}
	}
	return tc
}

type traceContextKey struct{}

func WithTraceContext(ctx context.Context, tc TraceContext) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	if tc.IsZero() {
		return ctx
	}
	return context.WithValue(ctx, traceContextKey{}, tc)
}

func TraceContextFromContext(ctx context.Context) TraceContext {
	if ctx == nil {
		return TraceContext{}
	}
	tc, _ := ctx.Value(traceContextKey{}).(TraceContext)
	return tc
}
