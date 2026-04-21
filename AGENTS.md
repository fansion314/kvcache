# AGENTS

## Project Intent
- This crate provides hardware-friendly in-memory caches using `HashMap + Vec` slot storage rather than `LinkedList`.
- Public API should stay simple and concrete: expose explicit cache types instead of policy-heavy public generics.

## Public Cache Types
- Keep these four concrete cache types as the primary public surface:
  - `LruCache<K, V>`
  - `TtlLruCache<K, V>`
  - `ClockCache<K, V>`
  - `TtlClockCache<K, V>`
- Non-TTL variants must not expose TTL-only APIs.
- TTL variants should provide `put_with_ttl` and `get_and_refresh_expiry`.

## Behavioral Contracts
- `get` returns `Option<&V>`.
- `invalidate` returns `Option<V>`.
- `put` and `put_with_ttl` return the previous value only when updating the same key.
- TTL semantics must stay identical across LRU and Clock:
  - expired reads return `None`
  - expired entries are removed lazily
  - insertion should reuse expired entries before evicting a live one
  - `get_and_refresh_expiry` extends expiry using the entry's stored TTL

## Implementation Preferences
- Keep library code `std`-only.
- Dev-only benchmark dependencies are allowed; currently `criterion` is used.
- Preserve slot reuse and generation/version tracking to protect against stale expiry records after updates or slot reuse.
- Prefer compact per-slot metadata:
  - LRU uses intrusive index-linked metadata
  - Clock uses a `referenced` bit and `hand` pointer

## Benchmark Expectations
- Maintain both benchmark entry points:
  - `cargo bench --bench cache_policies`
  - `cargo run --example cache_bench`
- Benchmark suite should stay split into three benchmark families:
  - replacement policy: compare `LruCache` vs `ClockCache`
  - TTL overhead: compare long-TTL variants against the matching non-TTL policy
  - expiration stress: compare `TtlLruCache` vs `TtlClockCache` under short TTL
- Each family should continue covering:
  - high-hit and low-hit workloads
  - 95/5 and 50/50 read/write mixes
- TTL benchmarks should use deterministic synthetic time progression rather than `sleep`.
