package signaling

import (
	"sync"
	"testing"
)

func TestPinAllocatorClaimAndFree(t *testing.T) {
	allocator := NewPinAllocator()
	const pin = uint32(123456)

	if !allocator.Claim(pin) {
		t.Fatal("expected initial claim to succeed")
	}
	if allocator.Claim(pin) {
		t.Fatal("expected second claim to fail")
	}

	allocator.Free(pin)

	if !allocator.Claim(pin) {
		t.Fatal("expected claim after free to succeed")
	}
}

func TestPinAllocatorConcurrentUniqueness(t *testing.T) {
	allocator := NewPinAllocator()

	const (
		workers   = 16
		perWorker = 2000
		total     = workers * perWorker
	)

	pins := make(chan uint32, total)
	var wg sync.WaitGroup

	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < perWorker; j++ {
				pin, ok := allocator.Allocate()
				if !ok {
					t.Errorf("unexpected allocation failure")
					return
				}
				pins <- pin
			}
		}()
	}

	wg.Wait()
	close(pins)

	seen := make(map[uint32]struct{}, total)
	for pin := range pins {
		if _, exists := seen[pin]; exists {
			t.Fatalf("duplicate pin allocated: %d", pin)
		}
		seen[pin] = struct{}{}
	}

	if len(seen) != total {
		t.Fatalf("expected %d pins, got %d", total, len(seen))
	}
}
