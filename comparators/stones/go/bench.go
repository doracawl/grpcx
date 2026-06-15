// Cross-language comparator — Go stdlib.
//
// Same methodology as the Rust + C harnesses:
// per (op, size): ~100 ms warmup, then 5 batches of
// >= 10 ms each; report the best (minimum) ns/op.
//
// Output: CSV lines  impl,op,size,ns_per_op,gib_s
//
// Build + run:  go run bench.go
package main

import (
	"crypto/sha1"
	"encoding/base64"
	"fmt"
	"time"
)

// Global sink defeats dead-code elimination.
var sink byte

func runOne(opName string, fn func() byte, reportedSize int) {
	// Warmup ~100 ms.
	t0 := time.Now()
	for time.Since(t0) < 100*time.Millisecond {
		sink = fn()
	}

	// Pick K so one batch is >= ~10 ms.
	t0 = time.Now()
	sink = fn()
	once := time.Since(t0)
	k := 1024
	if once > 0 {
		k = int(10*time.Millisecond/once) + 1
	}

	best := float64(1e30)
	for b := 0; b < 5; b++ {
		t0 = time.Now()
		for i := 0; i < k; i++ {
			sink = fn()
		}
		per := float64(time.Since(t0).Nanoseconds()) / float64(k)
		if per < best {
			best = per
		}
	}

	gibS := (float64(reportedSize) / 1073741824.0) / (best / 1e9)
	fmt.Printf("go-stdlib,%s,%d,%.1f,%.4f\n", opName, reportedSize, best, gibS)
}

func main() {
	for _, n := range []int{1024, 65536, 1048576} {
		buf := make([]byte, n)
		for i := range buf {
			buf[i] = byte(i * 7)
		}

		runOne("sha1", func() byte {
			d := sha1.Sum(buf)
			return d[0]
		}, n)

		enc := base64.StdEncoding.EncodeToString(buf)
		runOne("b64-encode", func() byte {
			s := base64.StdEncoding.EncodeToString(buf)
			return s[0]
		}, n)

		runOne("b64-decode", func() byte {
			d, err := base64.StdEncoding.DecodeString(enc)
			if err != nil {
				panic(err)
			}
			return d[0]
		}, n)
	}
}
