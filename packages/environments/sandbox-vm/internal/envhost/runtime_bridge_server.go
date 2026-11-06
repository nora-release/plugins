package envhost

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"strings"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
)

// RuntimeBridgeHTTPServer exposes loopback HTTP endpoints for runtime callbacks.
// It translates local HTTP requests into envhost runtime bridge messages.
type RuntimeBridgeHTTPServer struct {
	listener net.Listener
	server   *http.Server
	baseURL  string
}

func NewRuntimeBridgeHTTPServer(bridge RuntimeBridge) (*RuntimeBridgeHTTPServer, error) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, fmt.Errorf("listen runtime bridge: %w", err)
	}

	return NewRuntimeBridgeHTTPServerWithListener(listener, "http://"+listener.Addr().String(), bridge)
}

func NewRuntimeBridgeHTTPServerWithListener(listener net.Listener, baseURL string, bridge RuntimeBridge) (*RuntimeBridgeHTTPServer, error) {
	if listener == nil {
		return nil, fmt.Errorf("runtime bridge listener is required")
	}
	server := &RuntimeBridgeHTTPServer{
		listener: listener,
		baseURL:  strings.TrimRight(strings.TrimSpace(baseURL), "/"),
	}
	server.server = &http.Server{
		Handler:           server.handler(bridge),
		ReadHeaderTimeout: 10 * time.Second,
		IdleTimeout:       90 * time.Second,
	}

	go func() {
		err := server.server.Serve(listener)
		if err != nil && err != http.ErrServerClosed {
			log.Printf("runtime bridge server exited: %v", err)
			_ = listener.Close()
		}
	}()

	return server, nil
}

func NewRuntimeBridgeHandler(bridge RuntimeBridge) http.Handler {
	server := &RuntimeBridgeHTTPServer{}
	return server.handler(bridge)
}

func (s *RuntimeBridgeHTTPServer) BaseURL() string {
	if s == nil {
		return ""
	}
	return s.baseURL
}

func (s *RuntimeBridgeHTTPServer) Close() error {
	if s == nil || s.server == nil {
		return nil
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	err := s.server.Shutdown(ctx)
	if err == nil {
		return nil
	}
	closeErr := s.server.Close()
	if closeErr != nil && !errors.Is(closeErr, http.ErrServerClosed) {
		return errors.Join(err, closeErr)
	}
	return err
}

func (s *RuntimeBridgeHTTPServer) handler(bridge RuntimeBridge) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		token, routeType, routePath, ok := parseRuntimeBridgePath(req.URL.Path)
		if !ok {
			http.NotFound(w, req)
			return
		}

		switch routeType {
		case "tool":
			s.serveTool(w, req, bridge, token, routePath)
		case "api":
			s.serveAPI(w, req, bridge, token, routePath)
		default:
			http.NotFound(w, req)
		}
	})
}

func (s *RuntimeBridgeHTTPServer) serveTool(w http.ResponseWriter, req *http.Request, bridge RuntimeBridge, token string, routePath string) {
	if bridge == nil {
		writeRuntimeBridgeJSONError(w, NewProtocolError(ErrCodeCapabilityNotSupported, "runtime tool bridge is unavailable"))
		return
	}
	if strings.ToUpper(strings.TrimSpace(req.Method)) != http.MethodPost {
		writeRuntimeBridgeJSON(w, http.StatusMethodNotAllowed, map[string]string{"error": "method not allowed"})
		return
	}

	toolName := strings.Trim(strings.TrimSpace(routePath), "/")
	if toolName == "" {
		http.NotFound(w, req)
		return
	}

	body, err := io.ReadAll(req.Body)
	if err != nil {
		writeRuntimeBridgeJSONError(w, NewProtocolError(ErrCodeInvalidRequest, fmt.Sprintf("read request body: %v", err)))
		return
	}

	ctx := otel.GetTextMapPropagator().Extract(req.Context(), propagation.HeaderCarrier(req.Header))

	result, err := bridge.CallTool(ctx, &RuntimeToolRequest{
		CapabilityToken: token,
		Tool:            toolName,
		Input:           normalizeRuntimeToolInput(body),
		Headers:         cloneRuntimeBridgeHeaders(req.Header),
	})
	if err != nil {
		writeRuntimeBridgeJSONError(w, err)
		return
	}
	writeRuntimeBridgeJSONRaw(w, http.StatusOK, result.Result)
}

func (s *RuntimeBridgeHTTPServer) serveAPI(w http.ResponseWriter, req *http.Request, bridge RuntimeBridge, token string, routePath string) {
	if bridge == nil {
		writeRuntimeBridgePlainError(w, NewProtocolError(ErrCodeCapabilityNotSupported, "runtime http bridge is unavailable"))
		return
	}

	body, err := io.ReadAll(req.Body)
	if err != nil {
		writeRuntimeBridgePlainError(w, NewProtocolError(ErrCodeInvalidRequest, fmt.Sprintf("read request body: %v", err)))
		return
	}

	ctx := otel.GetTextMapPropagator().Extract(req.Context(), propagation.HeaderCarrier(req.Header))

	result, err := bridge.DoHTTP(ctx, &RuntimeHTTPRequest{
		CapabilityToken: token,
		Method:          req.Method,
		Path:            routePath,
		Query:           req.URL.RawQuery,
		Headers:         cloneRuntimeBridgeHeaders(req.Header),
		Body:            base64.StdEncoding.EncodeToString(body),
		BodyEncoding:    "base64",
	})
	if err != nil {
		writeRuntimeBridgePlainError(w, err)
		return
	}

	responseBody, err := decodeRuntimeBridgeBody(result.Body, result.BodyEncoding)
	if err != nil {
		writeRuntimeBridgePlainError(w, err)
		return
	}

	copyRuntimeBridgeHeaders(w.Header(), result.Headers)
	if result.StatusCode == 0 {
		result.StatusCode = http.StatusOK
	}
	w.WriteHeader(result.StatusCode)
	if len(responseBody) > 0 {
		_, _ = w.Write(responseBody)
	}
}

func parseRuntimeBridgePath(path string) (token string, routeType string, routePath string, ok bool) {
	trimmed := strings.TrimPrefix(strings.TrimSpace(path), "/")
	if trimmed == "" {
		return "", "", "", false
	}

	parts := strings.SplitN(trimmed, "/", 3)
	if len(parts) < 2 {
		return "", "", "", false
	}

	token = strings.TrimSpace(parts[0])
	routeType = strings.TrimSpace(parts[1])
	if token == "" || routeType == "" {
		return "", "", "", false
	}

	routePath = "/"
	if len(parts) == 3 {
		routePath += parts[2]
	}
	if routeType == "api" && routePath == "/" {
		return token, routeType, routePath, true
	}
	if routeType == "tool" && routePath == "/" {
		return "", "", "", false
	}
	return token, routeType, routePath, true
}

func normalizeRuntimeToolInput(body []byte) json.RawMessage {
	body = bytesTrimRuntimeBridgeBody(body)
	if len(body) == 0 {
		return nil
	}
	if json.Valid(body) {
		return json.RawMessage(body)
	}

	encoded, err := json.Marshal(string(body))
	if err != nil {
		return json.RawMessage(`null`)
	}
	return json.RawMessage(encoded)
}

func bytesTrimRuntimeBridgeBody(body []byte) []byte {
	if len(body) == 0 {
		return nil
	}
	return []byte(strings.TrimSpace(string(body)))
}

func decodeRuntimeBridgeBody(body string, encoding string) ([]byte, error) {
	switch strings.TrimSpace(encoding) {
	case "", "base64":
		if body == "" {
			return nil, nil
		}
		decoded, err := base64.StdEncoding.DecodeString(body)
		if err != nil {
			return nil, NewProtocolError(ErrCodeInternalError, fmt.Sprintf("decode runtime response body: %v", err))
		}
		return decoded, nil
	default:
		return nil, NewProtocolError(ErrCodeInternalError, fmt.Sprintf("unsupported runtime response encoding %q", encoding))
	}
}

func cloneRuntimeBridgeHeaders(headers http.Header) map[string][]string {
	if len(headers) == 0 {
		return nil
	}
	cloned := make(map[string][]string, len(headers))
	for key, values := range headers {
		copied := make([]string, len(values))
		copy(copied, values)
		cloned[key] = copied
	}
	return cloned
}

func copyRuntimeBridgeHeaders(dst http.Header, src map[string][]string) {
	for key, values := range src {
		for _, value := range values {
			dst.Add(key, value)
		}
	}
}

func writeRuntimeBridgeJSON(w http.ResponseWriter, status int, value any) {
	body, err := json.Marshal(value)
	if err != nil {
		http.Error(w, `{"error":"failed to encode response"}`, http.StatusInternalServerError)
		return
	}
	writeRuntimeBridgeJSONBytes(w, status, body)
}

func writeRuntimeBridgeJSONRaw(w http.ResponseWriter, status int, body json.RawMessage) {
	if len(body) == 0 {
		body = json.RawMessage(`null`)
	}
	writeRuntimeBridgeJSONBytes(w, status, body)
}

func writeRuntimeBridgeJSONBytes(w http.ResponseWriter, status int, body []byte) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write(body)
}

func writeRuntimeBridgeJSONError(w http.ResponseWriter, err error) {
	status, message := RuntimeBridgeHTTPError(err)
	writeRuntimeBridgeJSON(w, status, map[string]string{"error": message})
}

func writeRuntimeBridgePlainError(w http.ResponseWriter, err error) {
	status, message := RuntimeBridgeHTTPError(err)
	http.Error(w, message, status)
}

func RuntimeBridgeHTTPError(err error) (status int, message string) {
	if err == nil {
		return http.StatusOK, ""
	}

	message = err.Error()
	if protocolErr, ok := err.(*ProtocolError); ok {
		message = protocolErr.Message
		switch protocolErr.Code {
		case ErrCodeInvalidRequest, ErrCodeEnvironmentNotFound, ErrCodeFileNotFound:
			return http.StatusBadRequest, message
		case ErrCodeUnauthorized, ErrCodeCapabilityInvalid:
			return http.StatusUnauthorized, message
		case ErrCodePermissionDenied:
			return http.StatusForbidden, message
		case ErrCodeCapabilityNotSupported, ErrCodeToolNotSupported:
			return http.StatusNotImplemented, message
		case ErrCodeHTTPUpstreamFailed:
			return http.StatusBadGateway, message
		case ErrCodeTimeout:
			return http.StatusGatewayTimeout, message
		default:
			return http.StatusInternalServerError, message
		}
	}
	return http.StatusInternalServerError, message
}
