package signaling

import (
	"math/bits"
	"sync/atomic"
)

const (
	// MaxPins is the total number of six-digit PINs (000000-999999).
	MaxPins = 1_000_000

	randomProbeAttempts = 256
)

// PinAllocator tracks PIN allocations using an atomic bitmap.
//
// Each bit represents one PIN where 0 means free and 1 means allocated.
// This stays lock-free for the common allocation path.
type PinAllocator struct {
	chunks []atomic.Uint64

	probeCounter atomic.Uint64
	scanCursor   atomic.Uint64
}

func NewPinAllocator() *PinAllocator {
	chunkCount := (MaxPins + 63) / 64
	return &PinAllocator{chunks: make([]atomic.Uint64, chunkCount)}
}

// Allocate reserves a free PIN.
func (p *PinAllocator) Allocate() (uint32, bool) {
	for i := 0; i < randomProbeAttempts; i++ {
		candidate := int(splitMix64(p.probeCounter.Add(1)) % MaxPins)
		if p.tryClaim(candidate) {
			return uint32(candidate), true
		}
	}

	chunkCount := len(p.chunks)
	start := int(p.scanCursor.Add(1) % uint64(chunkCount))

	for offset := 0; offset < chunkCount; offset++ {
		chunkIdx := (start + offset) % chunkCount
		chunk := &p.chunks[chunkIdx]
		current := chunk.Load()

		for current != ^uint64(0) {
			freeBit := bits.TrailingZeros64(^current)
			if freeBit == 64 {
				break
			}

			pin := chunkIdx*64 + freeBit
			if pin >= MaxPins {
				break
			}

			next := current | (uint64(1) << freeBit)
			if chunk.CompareAndSwap(current, next) {
				return uint32(pin), true
			}
			current = chunk.Load()
		}
	}

	return 0, false
}

// Claim reserves a specific PIN if available.
func (p *PinAllocator) Claim(pin uint32) bool {
	if pin >= MaxPins {
		return false
	}
	return p.tryClaim(int(pin))
}

// Free releases a PIN back to the pool. Releasing an already free PIN is safe.
func (p *PinAllocator) Free(pin uint32) {
	if pin >= MaxPins {
		return
	}

	idx := int(pin)
	chunkIdx := idx / 64
	bitIdx := idx % 64
	mask := uint64(1) << bitIdx
	chunk := &p.chunks[chunkIdx]

	for {
		current := chunk.Load()
		if current&mask == 0 {
			return
		}

		next := current &^ mask
		if chunk.CompareAndSwap(current, next) {
			return
		}
	}
}

func (p *PinAllocator) tryClaim(pin int) bool {
	chunkIdx := pin / 64
	bitIdx := pin % 64
	mask := uint64(1) << bitIdx
	chunk := &p.chunks[chunkIdx]

	for {
		current := chunk.Load()
		if current&mask != 0 {
			return false
		}
		next := current | mask
		if chunk.CompareAndSwap(current, next) {
			return true
		}
	}
}

// splitMix64 provides a fast, high-quality integer mix suitable for probe generation.
func splitMix64(x uint64) uint64 {
	x += 0x9e3779b97f4a7c15
	x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9
	x = (x ^ (x >> 27)) * 0x94d049bb133111eb
	return x ^ (x >> 31)
}
