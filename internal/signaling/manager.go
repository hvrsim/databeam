package signaling

import (
	"errors"
	"fmt"
	"log/slog"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"
)

const (
	roomIdleTTL = 15 * time.Minute

	clientSendBuffer = 256

	writeWait      = 10 * time.Second
	pongWait       = 60 * time.Second
	pingPeriod     = (pongWait * 9) / 10
	maxMessageSize = 1 << 20 // 1 MiB text message cap
)

var (
	ErrNoPins       = errors.New("no pins available")
	ErrRoomNotFound = errors.New("room not found")
)

// Manager coordinates room creation, connection lifecycle, and PIN ownership.
type Manager struct {
	logger *slog.Logger
	pins   *PinAllocator

	rooms  sync.Map // map[string]*room
	peerID atomic.Uint64
}

func NewManager(logger *slog.Logger) *Manager {
	if logger == nil {
		logger = slog.Default()
	}
	return &Manager{
		logger: logger,
		pins:   NewPinAllocator(),
	}
}

// AllocateRoom reserves a unique six-digit PIN and prepares an empty room.
func (m *Manager) AllocateRoom() (string, error) {
	for attempts := 0; attempts < 16; attempts++ {
		pin, ok := m.pins.Allocate()
		if !ok {
			return "", ErrNoPins
		}

		code := fmt.Sprintf("%06d", pin)
		room := newRoom(code, m)
		actual, loaded := m.rooms.LoadOrStore(code, room)
		if loaded {
			_ = actual
			continue
		}

		go room.run()
		m.logger.Debug("allocated room", "pin", code)
		return code, nil
	}

	return "", ErrNoPins
}

func (m *Manager) RoomExists(pin string) bool {
	_, ok := m.rooms.Load(pin)
	return ok
}

// Connect registers a websocket connection with an existing room.
func (m *Manager) Connect(pin string, conn *websocket.Conn) error {
	roomValue, ok := m.rooms.Load(pin)
	if !ok {
		return ErrRoomNotFound
	}

	r := roomValue.(*room)
	peerID := m.peerID.Add(1)
	c := &client{
		id:   peerID,
		conn: conn,
		room: r,
		send: make(chan []byte, clientSendBuffer),
		log:  m.logger.With("pin", pin, "peer_id", peerID),
	}

	if !r.addClient(c) {
		return ErrRoomNotFound
	}

	go c.writePump()
	go c.readPump()
	return nil
}

func (m *Manager) releaseRoom(pin string, target *room) {
	value, ok := m.rooms.Load(pin)
	if !ok || value != target {
		return
	}

	m.rooms.Delete(pin)
	if pinNum, err := strconv.Atoi(pin); err == nil {
		m.pins.Free(uint32(pinNum))
	}
	m.logger.Debug("released room", "pin", pin)
}

type room struct {
	pin     string
	manager *Manager

	join      chan *client
	leave     chan *client
	broadcast chan []byte
	done      chan struct{}

	clients map[*client]struct{}
	idle    *time.Timer
}

func newRoom(pin string, manager *Manager) *room {
	return &room{
		pin:       pin,
		manager:   manager,
		join:      make(chan *client),
		leave:     make(chan *client),
		broadcast: make(chan []byte),
		done:      make(chan struct{}),
		clients:   make(map[*client]struct{}),
		idle:      time.NewTimer(roomIdleTTL),
	}
}

func (r *room) run() {
	defer close(r.done)
	defer r.idle.Stop()

	for {
		select {
		case c := <-r.join:
			r.clients[c] = struct{}{}
			if len(r.clients) == 1 {
				stopTimer(r.idle)
			}
			c.log.Debug("joined room")

		case c := <-r.leave:
			r.removeClient(c)
			if len(r.clients) == 0 {
				resetTimer(r.idle, roomIdleTTL)
			}

		case msg := <-r.broadcast:
			for c := range r.clients {
				select {
				case c.send <- msg:
				default:
					c.log.Warn("dropping slow client")
					r.removeClient(c)
				}
			}

			if len(r.clients) == 0 {
				resetTimer(r.idle, roomIdleTTL)
			}

		case <-r.idle.C:
			if len(r.clients) == 0 {
				r.manager.releaseRoom(r.pin, r)
				return
			}
		}
	}
}

func (r *room) addClient(c *client) bool {
	select {
	case <-r.done:
		return false
	case r.join <- c:
		return true
	}
}

func (r *room) removeClient(c *client) {
	if _, exists := r.clients[c]; !exists {
		return
	}

	delete(r.clients, c)
	close(c.send)
	_ = c.conn.Close()
	c.log.Debug("left room")
}

func (r *room) removeClientAsync(c *client) {
	select {
	case <-r.done:
	case r.leave <- c:
	}
}

func (r *room) publish(msg []byte) bool {
	select {
	case <-r.done:
		return false
	case r.broadcast <- msg:
		return true
	}
}

type client struct {
	id   uint64
	conn *websocket.Conn
	room *room
	send chan []byte
	log  *slog.Logger
}

func (c *client) readPump() {
	defer c.room.removeClientAsync(c)

	c.conn.SetReadLimit(maxMessageSize)
	_ = c.conn.SetReadDeadline(time.Now().Add(pongWait))
	c.conn.SetPongHandler(func(string) error {
		return c.conn.SetReadDeadline(time.Now().Add(pongWait))
	})

	for {
		messageType, payload, err := c.conn.ReadMessage()
		if err != nil {
			if websocket.IsUnexpectedCloseError(err, websocket.CloseGoingAway, websocket.CloseAbnormalClosure) {
				c.log.Debug("read error", "error", err)
			}
			return
		}

		if messageType != websocket.TextMessage {
			continue
		}

		if !c.room.publish(payload) {
			return
		}
	}
}

func (c *client) writePump() {
	ticker := time.NewTicker(pingPeriod)
	defer ticker.Stop()
	defer c.room.removeClientAsync(c)

	for {
		select {
		case msg, ok := <-c.send:
			_ = c.conn.SetWriteDeadline(time.Now().Add(writeWait))
			if !ok {
				_ = c.conn.WriteMessage(websocket.CloseMessage, []byte{})
				return
			}
			if err := c.conn.WriteMessage(websocket.TextMessage, msg); err != nil {
				return
			}

		case <-ticker.C:
			_ = c.conn.SetWriteDeadline(time.Now().Add(writeWait))
			if err := c.conn.WriteMessage(websocket.PingMessage, nil); err != nil {
				return
			}
		}
	}
}

func stopTimer(timer *time.Timer) {
	if !timer.Stop() {
		select {
		case <-timer.C:
		default:
		}
	}
}

func resetTimer(timer *time.Timer, d time.Duration) {
	stopTimer(timer)
	timer.Reset(d)
}
