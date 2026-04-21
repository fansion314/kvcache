#[allow(dead_code)]
#[path = "../src/lib.rs"]
mod cache_impl;

use std::time::{Duration, Instant};

use cache_impl::{ClockCache, LruCache, StepClock, TtlClockCache, TtlLruCache};

const CAPACITY: usize = 4_096;
const OPS_PER_SAMPLE: usize = 100_000;
const HOT_KEYS: u64 = 2_048;
const COLD_KEYS: u64 = 65_536;
const CLOCK_STEP: Duration = Duration::from_micros(1);
const TTL_LONG: Duration = Duration::from_secs(1);
const TTL_STRESS: Duration = Duration::from_micros(64);

#[derive(Clone, Copy)]
enum HitRate {
    High,
    Low,
}

#[derive(Clone, Copy)]
enum Mix {
    ReadHeavy,
    Balanced,
}

#[derive(Clone, Copy)]
enum Operation {
    Get(u64),
    Put(u64, u64),
}

trait CacheLike {
    fn get(&mut self, key: &u64) -> Option<&u64>;
    fn put(&mut self, key: u64, value: u64) -> Option<u64>;
}

impl CacheLike for LruCache<u64, u64> {
    fn get(&mut self, key: &u64) -> Option<&u64> {
        LruCache::get(self, key)
    }

    fn put(&mut self, key: u64, value: u64) -> Option<u64> {
        LruCache::put(self, key, value)
    }
}

impl CacheLike for ClockCache<u64, u64> {
    fn get(&mut self, key: &u64) -> Option<&u64> {
        ClockCache::get(self, key)
    }

    fn put(&mut self, key: u64, value: u64) -> Option<u64> {
        ClockCache::put(self, key, value)
    }
}

impl<C> CacheLike for TtlLruCache<u64, u64, C>
where
    C: cache_impl::Clock,
{
    fn get(&mut self, key: &u64) -> Option<&u64> {
        TtlLruCache::get(self, key)
    }

    fn put(&mut self, key: u64, value: u64) -> Option<u64> {
        TtlLruCache::put(self, key, value)
    }
}

impl<C> CacheLike for TtlClockCache<u64, u64, C>
where
    C: cache_impl::Clock,
{
    fn get(&mut self, key: &u64) -> Option<&u64> {
        TtlClockCache::get(self, key)
    }

    fn put(&mut self, key: u64, value: u64) -> Option<u64> {
        TtlClockCache::put(self, key, value)
    }
}

fn next_random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state
}

fn choose_key(state: &mut u64, hit_rate: HitRate) -> u64 {
    let hot_probability = match hit_rate {
        HitRate::High => 95,
        HitRate::Low => 5,
    };

    if next_random(state) % 100 < hot_probability {
        next_random(state) % HOT_KEYS
    } else {
        HOT_KEYS + (next_random(state) % (COLD_KEYS - HOT_KEYS))
    }
}

fn generate_trace(hit_rate: HitRate, mix: Mix) -> Vec<Operation> {
    let write_probability = match mix {
        Mix::ReadHeavy => 5,
        Mix::Balanced => 50,
    };

    let mut trace = Vec::with_capacity(OPS_PER_SAMPLE);
    let mut state = 0xfeed_face_cafe_beefu64;

    for index in 0..OPS_PER_SAMPLE {
        let key = choose_key(&mut state, hit_rate);
        if next_random(&mut state) % 100 < write_probability {
            trace.push(Operation::Put(key, key ^ index as u64));
        } else {
            trace.push(Operation::Get(key));
        }
    }

    trace
}

fn scenario_name(hit_rate: HitRate, mix: Mix) -> String {
    let hit = match hit_rate {
        HitRate::High => "high-hit",
        HitRate::Low => "low-hit",
    };
    let rw = match mix {
        Mix::ReadHeavy => "95r/5w",
        Mix::Balanced => "50r/50w",
    };
    format!("{hit} {rw}")
}

fn run_plain<C>(cache: &mut C, trace: &[Operation]) -> u64
where
    C: CacheLike,
{
    let mut checksum = 0u64;

    for &op in trace {
        match op {
            Operation::Get(key) => {
                if let Some(value) = cache.get(&key) {
                    checksum ^= *value;
                }
            }
            Operation::Put(key, value) => {
                checksum ^= cache.put(key, value).unwrap_or(0);
            }
        }
    }

    checksum
}

fn run_ttl<C>(cache: &mut C, trace: &[Operation], clock: &StepClock) -> u64
where
    C: CacheLike,
{
    let mut checksum = 0u64;

    for &op in trace {
        clock.advance(CLOCK_STEP);
        match op {
            Operation::Get(key) => {
                if let Some(value) = cache.get(&key) {
                    checksum ^= *value;
                }
            }
            Operation::Put(key, value) => {
                checksum ^= cache.put(key, value).unwrap_or(0);
            }
        }
    }

    checksum
}

fn benchmark_variant<F>(mut run_once: F) -> (Duration, u64)
where
    F: FnMut() -> u64,
{
    let _ = run_once();
    let start = Instant::now();
    let checksum = run_once();
    (start.elapsed(), checksum)
}

fn print_result(label: &str, elapsed: Duration, checksum: u64) {
    let ops_per_sec = OPS_PER_SAMPLE as f64 / elapsed.as_secs_f64();
    println!(
        "{label:<18} {:>10.2} ms {:>14.0} ops/s checksum={checksum}",
        elapsed.as_secs_f64() * 1_000.0,
        ops_per_sec
    );
}

fn print_replacement(trace: &[Operation]) {
    let (elapsed, checksum) = benchmark_variant(|| {
        let mut cache = LruCache::new(CAPACITY);
        run_plain(&mut cache, trace)
    });
    print_result("lru", elapsed, checksum);

    let (elapsed, checksum) = benchmark_variant(|| {
        let mut cache = ClockCache::new(CAPACITY);
        run_plain(&mut cache, trace)
    });
    print_result("clock", elapsed, checksum);
}

fn print_ttl_overhead(trace: &[Operation]) {
    let (elapsed, checksum) = benchmark_variant(|| {
        let mut cache = LruCache::new(CAPACITY);
        run_plain(&mut cache, trace)
    });
    print_result("lru_plain", elapsed, checksum);

    let (elapsed, checksum) = benchmark_variant(|| {
        let clock = StepClock::new();
        let mut cache = TtlLruCache::with_clock(CAPACITY, TTL_LONG, clock.clone());
        run_ttl(&mut cache, trace, &clock)
    });
    print_result("lru_ttl_long", elapsed, checksum);

    let (elapsed, checksum) = benchmark_variant(|| {
        let mut cache = ClockCache::new(CAPACITY);
        run_plain(&mut cache, trace)
    });
    print_result("clock_plain", elapsed, checksum);

    let (elapsed, checksum) = benchmark_variant(|| {
        let clock = StepClock::new();
        let mut cache = TtlClockCache::with_clock(CAPACITY, TTL_LONG, clock.clone());
        run_ttl(&mut cache, trace, &clock)
    });
    print_result("clock_ttl_long", elapsed, checksum);
}

fn print_expiration_stress(trace: &[Operation]) {
    let (elapsed, checksum) = benchmark_variant(|| {
        let clock = StepClock::new();
        let mut cache = TtlLruCache::with_clock(CAPACITY, TTL_STRESS, clock.clone());
        run_ttl(&mut cache, trace, &clock)
    });
    print_result("ttl_lru_short", elapsed, checksum);

    let (elapsed, checksum) = benchmark_variant(|| {
        let clock = StepClock::new();
        let mut cache = TtlClockCache::with_clock(CAPACITY, TTL_STRESS, clock.clone());
        run_ttl(&mut cache, trace, &clock)
    });
    print_result("ttl_clock_short", elapsed, checksum);
}

fn main() {
    for &(hit_rate, mix) in &[
        (HitRate::High, Mix::ReadHeavy),
        (HitRate::High, Mix::Balanced),
        (HitRate::Low, Mix::ReadHeavy),
        (HitRate::Low, Mix::Balanced),
    ] {
        let trace = generate_trace(hit_rate, mix);
        let scenario = scenario_name(hit_rate, mix);

        println!("\nReplacement policy: {scenario}");
        print_replacement(&trace);

        println!("\nTTL overhead (long TTL, no intended expiration): {scenario}");
        print_ttl_overhead(&trace);

        println!("\nExpiration stress (short TTL): {scenario}");
        print_expiration_stress(&trace);
    }
}
