package apiproxy

import (
	"context"
	"strings"
)

type callerAgentContextKey struct{}

func withCallerAgentID(ctx context.Context, callerAgentID string) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	return context.WithValue(ctx, callerAgentContextKey{}, strings.TrimSpace(callerAgentID))
}

// CallerAgentIDFromContext extracts caller agent id from request context.
func CallerAgentIDFromContext(ctx context.Context) (string, bool) {
	if ctx == nil {
		return "", false
	}
	callerAgentID, ok := ctx.Value(callerAgentContextKey{}).(string)
	if !ok {
		return "", false
	}
	callerAgentID = strings.TrimSpace(callerAgentID)
	return callerAgentID, callerAgentID != ""
}
