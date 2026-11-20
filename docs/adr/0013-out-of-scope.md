# ADR-0013: Out of scope — offloading, audio mixing, bonding, X11

Status: Accepted · 2026-11-20

## Decision and reasons
- **Tablet RAM as swap/zRAM for the PC:** slower than the PC's own NVMe on both latency (0.3–1+ ms vs 50–100 µs) and bandwidth; swap over a network block device deadlocks under memory pressure on Linux; no supported path on Windows; Android's low-memory killer terminates apps holding gigabytes; unplugging the cable loses pages and corrupts processes.
- **Compute or LLM offloading to the tablet:** a different product; a Snapdragon 8 Gen 1 throttles and would steal thermal budget from the video decoder this product depends on. If ever pursued, it belongs in a separate repository.
- **Bidirectional audio mixing:** two independent clocks drift by tens of ppm and sit 20–40 ms apart, causing echo and comb filtering. Optional one-way PC→tablet audio may come later, Linux first (Windows needs a kernel-mode audio driver).
- **USB + Wi-Fi bonding:** adds reordering and jitter equal to the slower path ([ADR-0007](0007-usb-strategy.md)).
- **X11 host:** no clean virtual-monitor mechanism ([ADR-0002](0002-platform-scope.md)).

## Consequences
Pull requests that add any of these are closed with a link to this ADR unless a new ADR supersedes it.
