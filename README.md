# kvcache

`kvcache` is a small Rust cache library focused on hardware-friendly implementations:

- `HashMap<K, usize>` for key lookup
- `Vec`-backed slot storage for better locality
- explicit replacement policies instead of pointer-heavy linked containers

It currently provides four cache variants:

- `LruCache<K, V>`: plain LRU, no TTL
- `TtlLruCache<K, V>`: LRU + TTL expiration
- `ClockCache<K, V>`: Clock / Second-Chance, no TTL
- `TtlClockCache<K, V>`: Clock / Second-Chance + TTL expiration

## API Overview

All cache types require `K: Eq + Hash + Clone`.

Shared API:

- `new(...)`
- `get(&mut self, key: &K) -> Option<&V>`
- `put(&mut self, key: K, value: V) -> Option<V>`
- `invalidate(&mut self, key: &K) -> Option<V>`

TTL-only API:

- `put_with_ttl(&mut self, key: K, value: V, ttl: Duration) -> Option<V>`
- `get_and_refresh_expiry(&mut self, key: &K) -> Option<&V>`

Behavior summary:

- `put` returns `Some(old_value)` only when updating the same key.
- `invalidate` returns the removed live value.
- In TTL variants, expired entries behave as absent.
- On insert, TTL variants prefer reusing expired entries before evicting a live one.

## Usage

### Plain LRU

```rust
use kvcache::LruCache;

let mut cache = LruCache::new(2);

assert_eq!(cache.put("a", 1), None);
assert_eq!(cache.put("b", 2), None);
assert_eq!(cache.get(&"a"), Some(&1));

cache.put("c", 3);

assert_eq!(cache.get(&"b"), None);
assert_eq!(cache.get(&"c"), Some(&3));
assert_eq!(cache.invalidate(&"a"), Some(1));
```

### TTL + Clock

```rust
use kvcache::TtlClockCache;
use std::time::Duration;

let mut cache = TtlClockCache::new(2, Duration::from_secs(30));

assert_eq!(cache.put("a", 1), None);
assert_eq!(cache.put_with_ttl("b", 2, Duration::from_secs(5)), None);
assert_eq!(cache.get(&"a"), Some(&1));
assert_eq!(cache.get_and_refresh_expiry(&"b"), Some(&2));
```

## Running Tests

```bash
cargo test
```

## Mini-Redis

This workspace also includes a small Redis-like TCP cache service built on top of
`TtlLruCache<String, String>` with Tokio networking:

- package: `miniredis`
- location: `apps/miniredis`
- server: `cargo run -p miniredis --bin miniredis-server -- --addr 127.0.0.1:6379`
- client: `cargo run -p miniredis --bin miniredis-client -- --addr 127.0.0.1:6379`
- storage: `TtlLruCache<String, String>`
- persistence: none; restarting the server clears all keys

Default server configuration:

- `--addr 127.0.0.1:6379`
- `--capacity 1024`
- `--default-ttl-secs 60`

Supported commands:

- `PING`
- `GET key`
- `SET key value`
- `SETEX key seconds value`
- `GETEX key`
- `DEL key`
- `QUIT`

Client REPL example:

```text
miniredis> SET greeting "hello world"
OK
miniredis> GET greeting
hello world
miniredis> SETEX token 5 abc123
OK
miniredis> GETEX token
abc123
miniredis> DEL greeting
1
miniredis> QUIT
OK
```

## Shortlink Service

This workspace also includes a memory-only shortlink service built with `axum`:

- package: `shortlink-service`
- location: `apps/shortlink-service`
- run: `cargo run -p shortlink-service`
- storage: `TtlLruCache<String, Arc<LinkRecord>>`
- persistence: none; restarting the process clears all short links

Default runtime configuration:

- `HOST=0.0.0.0`
- `PORT=3000`
- `CACHE_CAPACITY=10000`
- `DEFAULT_TTL_SECS=86400`
- `PUBLIC_BASE_URL=http://127.0.0.1:3000`

Main HTTP endpoints:

- `GET /`
- `GET /healthz`
- `POST /api/links`
- `GET /api/links/:alias`
- `DELETE /api/links/:alias`
- `GET /:alias`

## Running Benchmarks

Formal Criterion benchmark:

```bash
cargo bench --bench cache_policies
```

Quick local comparison runner:

```bash
cargo run --example cache_bench
```

Benchmark workload matrix:

- 3 benchmark families:
  - replacement policy: `LruCache` vs `ClockCache`
  - TTL overhead: long-TTL variants vs the matching non-TTL policy
  - expiration stress: `TtlLruCache` vs `TtlClockCache` with short TTL
- 2 hit-rate regimes: high-hit and low-hit
- 2 read/write mixes: `95/5` and `50/50`
- Capacity: `4096`
- Operations per sample: `100_000`
- Key/value type: `u64`
- synthetic clock step per operation: `1us`
- long TTL for overhead measurement: `1s`
- short TTL for expiration stress: `64us`

TTL benchmarks use a deterministic synthetic clock instead of `sleep`. This is important because it lets us separate three different questions:

- Which replacement policy is faster when there is no expiration logic?
- How much overhead does TTL add when entries are not expected to expire?
- How do `TTL + LRU` and `TTL + Clock` behave when expiration pressure is intentionally high?

## Benchmark Results

The table below was collected on this local machine on 2026-04-21 with:

- macOS `26.4.1`
- architecture: `arm64`
- command: `cargo bench --bench cache_policies`
- Criterion default warm-up / sample settings

The numbers below use Criterion's reported throughput midpoint for each scenario.

### 1. Replacement Policy

This table isolates eviction-policy cost without TTL logic.

| Scenario | LRU | Clock |
| --- | ---: | ---: |
| High hit, 95% reads / 5% writes | 49.072 Melem/s | 57.500 Melem/s |
| High hit, 50% reads / 50% writes | 37.175 Melem/s | 42.898 Melem/s |
| Low hit, 95% reads / 5% writes | 83.716 Melem/s | 85.148 Melem/s |
| Low hit, 50% reads / 50% writes | 26.429 Melem/s | 24.096 Melem/s |

Takeaway:

- `Clock` wins clearly in the high-hit scenarios.
- In low-hit, write-heavier traffic, `LRU` can catch up or pull ahead because it evicts directly instead of scanning the ring.

### 2. TTL Overhead

This table keeps the same workload but uses a long TTL (`1s`) so entries are not intended to expire during the run. That makes it a better approximation of pure TTL bookkeeping overhead.

| Scenario | LRU Plain | LRU TTL Long | Clock Plain | Clock TTL Long |
| --- | ---: | ---: | ---: | ---: |
| High hit, 95% reads / 5% writes | 44.013 Melem/s | 37.904 Melem/s | 51.552 Melem/s | 41.364 Melem/s |
| High hit, 50% reads / 50% writes | 40.246 Melem/s | 37.191 Melem/s | 46.219 Melem/s | 38.624 Melem/s |
| Low hit, 95% reads / 5% writes | 99.382 Melem/s | 92.992 Melem/s | 100.670 Melem/s | 92.561 Melem/s |
| Low hit, 50% reads / 50% writes | 31.025 Melem/s | 31.270 Melem/s | 27.745 Melem/s | 29.177 Melem/s |

Takeaway:

- In three of the four scenarios, long-TTL variants are slower than the matching plain cache, which is what we would expect from extra expiry bookkeeping.
- The low-hit, 50/50 scenario came out nearly tied and slightly favored the TTL variants in this particular run. Treat that as workload-and-run specific rather than as a strong claim that TTL is "free."

### 3. Expiration Stress

This table intentionally uses a short TTL (`64us`) so expiration is part of the workload. It is meant to compare `TTL + LRU` against `TTL + Clock`, not TTL against non-TTL caches.

| Scenario | TTL LRU Short | TTL Clock Short |
| --- | ---: | ---: |
| High hit, 95% reads / 5% writes | 102.000 Melem/s | 100.500 Melem/s |
| High hit, 50% reads / 50% writes | 33.965 Melem/s | 34.616 Melem/s |
| Low hit, 95% reads / 5% writes | 102.430 Melem/s | 102.030 Melem/s |
| Low hit, 50% reads / 50% writes | 33.839 Melem/s | 34.934 Melem/s |

Takeaway:

- Under strong expiration pressure, the two TTL policies are very close.
- `Clock` has a small edge in the write-heavier short-TTL scenarios from this run, while read-heavy short-TTL scenarios are essentially tied.

Overall guidance:

- If you want the fastest plain cache on high-hit workloads, `ClockCache` looks attractive.
- If you want predictable plain-cache behavior under heavier churn, `LruCache` remains competitive.
- TTL support does have measurable bookkeeping cost in most scenarios, so it is worth benchmarking against your real workload before choosing a default.
