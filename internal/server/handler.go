package server

import (
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"time"

	"databeam/internal/signaling"
	"github.com/gorilla/websocket"
)

type Handler struct {
	logger  *slog.Logger
	manager *signaling.Manager

	static   http.Handler
	upgrader websocket.Upgrader
}

func New(logger *slog.Logger, manager *signaling.Manager, staticDir string) *Handler {
	if logger == nil {
		logger = slog.Default()
	}
	if staticDir == "" {
		staticDir = "web"
	}

	return &Handler{
		logger:  logger,
		manager: manager,
		static:  http.FileServer(http.Dir(staticDir)),
		upgrader: websocket.Upgrader{
			ReadBufferSize:  4096,
			WriteBufferSize: 4096,
			CheckOrigin:     func(*http.Request) bool { return true },
		},
	}
}

func (h *Handler) Routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /pin", h.handlePIN)
	mux.HandleFunc("GET /ws/{pin}", h.handleWebSocket)
	mux.HandleFunc("/", h.handleStatic)

	return loggingMiddleware(h.logger, corsMiddleware(mux))
}

func (h *Handler) handlePIN(w http.ResponseWriter, _ *http.Request) {
	pin, err := h.manager.AllocateRoom()
	if err != nil {
		if errors.Is(err, signaling.ErrNoPins) {
			http.Error(w, "No PINs available", http.StatusServiceUnavailable)
			return
		}
		h.logger.Error("failed to allocate room", "error", err)
		http.Error(w, "Internal server error", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(struct {
		Pin string `json:"pin"`
	}{Pin: pin})
}

func (h *Handler) handleWebSocket(w http.ResponseWriter, r *http.Request) {
	pin := r.PathValue("pin")
	if !isValidPIN(pin) {
		http.Error(w, "Invalid PIN: must be exactly 6 digits", http.StatusBadRequest)
		return
	}

	if !h.manager.RoomExists(pin) {
		http.Error(w, "PIN not found or expired", http.StatusNotFound)
		return
	}

	conn, err := h.upgrader.Upgrade(w, r, nil)
	if err != nil {
		h.logger.Debug("websocket upgrade failed", "pin", pin, "error", err)
		return
	}

	if err := h.manager.Connect(pin, conn); err != nil {
		if errors.Is(err, signaling.ErrRoomNotFound) {
			_ = conn.WriteControl(
				websocket.CloseMessage,
				websocket.FormatCloseMessage(websocket.CloseTryAgainLater, "PIN expired"),
				time.Now().Add(time.Second),
			)
			_ = conn.Close()
			return
		}
		h.logger.Error("failed to attach websocket", "pin", pin, "error", err)
		_ = conn.Close()
	}
}

func (h *Handler) handleStatic(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}
	h.static.ServeHTTP(w, r)
}

func isValidPIN(pin string) bool {
	if len(pin) != 6 {
		return false
	}
	for i := 0; i < len(pin); i++ {
		if pin[i] < '0' || pin[i] > '9' {
			return false
		}
	}
	return true
}

type statusWriter struct {
	http.ResponseWriter
	status int
	size   int
}

func (w *statusWriter) WriteHeader(code int) {
	w.status = code
	w.ResponseWriter.WriteHeader(code)
}

func (w *statusWriter) Write(b []byte) (int, error) {
	if w.status == 0 {
		w.status = http.StatusOK
	}
	n, err := w.ResponseWriter.Write(b)
	w.size += n
	return n, err
}

func corsMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Access-Control-Allow-Headers", "*")
		w.Header().Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")

		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}

		next.ServeHTTP(w, r)
	})
}

func loggingMiddleware(logger *slog.Logger, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		sw := &statusWriter{ResponseWriter: w}
		next.ServeHTTP(sw, r)

		status := sw.status
		if status == 0 {
			status = http.StatusOK
		}

		logger.Info(
			"http_request",
			"method", r.Method,
			"path", r.URL.Path,
			"status", status,
			"bytes", sw.size,
			"duration_ms", time.Since(start).Milliseconds(),
		)
	})
}
