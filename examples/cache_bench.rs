#[allow(dead_code)]
#[path = "../src/lib.rs"]
mod cache_impl;

use std::collections::HashMap;
use std::env;
use std::time::{Duration, Instant};

use cache_impl::{ClockCache, LruCache, StepClock, TtlClockCache, TtlLruCache};

const CAPACITY: usize = 4_096;
const OPS_PER_SAMPLE: usize = 100_000;
const HOT_KEYS: u64 = 2_048;
const COLD_KEYS: u64 = 65_536;
const TRACE_WINDOW: usize = 256;
const POLICY_SENSITIVE_CAPACITY: usize = 1_024;
const POLICY_SENSITIVE_DB_MS: f64 = 0.0;
const CLOCK_STEP: Duration = Duration::from_micros(1);
const TTL_LONG: Duration = Duration::from_secs(1);
const TTL_STRESS: Duration = Duration::from_micros(64);
const DEFAULT_DB_READ_MS: f64 = 5.0;
const DEFAULT_DB_WRITE_MS: f64 = 5.0;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Get,
    Put,
}

#[derive(Clone, Copy)]
struct BenchmarkConfig {
    db_read_penalty: Duration,
    db_write_penalty: Duration,
}

#[derive(Clone, Copy, Default)]
struct RunStats {
    checksum: u64,
    gets: u64,
    puts: u64,
    hits: u64,
    misses: u64,
    db_reads: u64,
    db_writes: u64,
}

impl RunStats {
    fn hit_rate(self) -> f64 {
        if self.gets == 0 {
            0.0
        } else {
            self.hits as f64 / self.gets as f64
        }
    }
}

struct Database {
    data: HashMap<u64, u64>,
}

impl Database {
    fn new() -> Self {
        Self {
            data: HashMap::with_capacity(COLD_KEYS as usize),
        }
    }

    fn read(&mut self, key: u64) -> u64 {
        *self.data.entry(key).or_insert_with(|| default_db_value(key))
    }

    fn write(&mut self, key: u64, value: u64) {
        self.data.insert(key, value);
    }
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

fn default_db_value(key: u64) -> u64 {
    key.wrapping_mul(1_146_959_810_393_466_560).rotate_left(17)
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

fn burst_len(state: &mut u64, mix: Mix, kind: OpKind) -> usize {
    match (mix, kind) {
        (Mix::ReadHeavy, OpKind::Get) => 6 + (next_random(state) % 10) as usize,
        (Mix::Balanced, OpKind::Get) => 2 + (next_random(state) % 4) as usize,
        (Mix::ReadHeavy, OpKind::Put) => 1 + (next_random(state) % 2) as usize,
        (Mix::Balanced, OpKind::Put) => 1 + (next_random(state) % 3) as usize,
    }
}

fn write_count(mix: Mix) -> usize {
    match mix {
        Mix::ReadHeavy => OPS_PER_SAMPLE * 5 / 100,
        Mix::Balanced => OPS_PER_SAMPLE / 2,
    }
}

fn make_op_kinds(mix: Mix) -> Vec<OpKind> {
    let puts = write_count(mix);
    let gets = OPS_PER_SAMPLE - puts;

    let mut kinds = Vec::with_capacity(OPS_PER_SAMPLE);
    kinds.extend(std::iter::repeat_n(OpKind::Put, puts));
    kinds.extend(std::iter::repeat_n(OpKind::Get, gets));

    let mut state = 0x9e37_79b9_7f4a_7c15;
    for i in (1..kinds.len()).rev() {
        let j = (next_random(&mut state) as usize) % (i + 1);
        kinds.swap(i, j);
    }

    let mut reordered = Vec::with_capacity(kinds.len());
    for chunk in kinds.chunks(TRACE_WINDOW) {
        reordered.extend(chunk.iter().copied().filter(|kind| *kind == OpKind::Put));
        reordered.extend(chunk.iter().copied().filter(|kind| *kind == OpKind::Get));
    }

    reordered
}

fn generate_trace(hit_rate: HitRate, mix: Mix) -> Vec<Operation> {
    let mut trace = Vec::with_capacity(OPS_PER_SAMPLE);
    let mut state = 0xfeed_face_cafe_beef;
    let kinds = make_op_kinds(mix);
    let mut current_key = 0u64;
    let mut remaining_burst = 0usize;

    for (index, kind) in kinds.into_iter().enumerate() {
        if remaining_burst == 0 {
            current_key = choose_key(&mut state, hit_rate);
            remaining_burst = burst_len(&mut state, mix, kind);
        } else {
            remaining_burst -= 1;
        }

        let key = current_key;
        match kind {
            OpKind::Get => trace.push(Operation::Get(key)),
            OpKind::Put => {
                let value = key ^ ((index as u64).wrapping_mul(0x9e37_79b9));
                trace.push(Operation::Put(key, value));
            }
        }
    }

    trace
}

fn push_op(trace: &mut Vec<Operation>, op: Operation) -> bool {
    if trace.len() >= OPS_PER_SAMPLE {
        return false;
    }
    trace.push(op);
    true
}

fn generate_policy_sensitive_trace() -> Vec<Operation> {
    let mut trace = Vec::with_capacity(OPS_PER_SAMPLE);
    let mut round = 0u64;
    for key in 0..POLICY_SENSITIVE_CAPACITY as u64 {
        if !push_op(&mut trace, Operation::Put(key, default_db_value(key))) {
            return trace;
        }
    }

    while trace.len() < OPS_PER_SAMPLE {
        for key in 0..POLICY_SENSITIVE_CAPACITY as u64 {
            let value = default_db_value(key) ^ round.wrapping_mul(0x9e37_79b9);
            if !push_op(&mut trace, Operation::Put(key, value)) {
                return trace;
            }
            if !push_op(&mut trace, Operation::Get(key)) {
                return trace;
            }
        }
        round = round.wrapping_add(1);
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

fn modeled_duration(raw: Duration, stats: RunStats, config: BenchmarkConfig) -> Duration {
    raw + Duration::from_secs_f64(
        config.db_read_penalty.as_secs_f64() * stats.db_reads as f64
            + config.db_write_penalty.as_secs_f64() * stats.db_writes as f64,
    )
}

fn run_no_cache(trace: &[Operation]) -> RunStats {
    let mut db = Database::new();
    let mut stats = RunStats::default();

    for &op in trace {
        match op {
            Operation::Get(key) => {
                stats.gets += 1;
                stats.misses += 1;
                stats.db_reads += 1;
                stats.checksum ^= db.read(key);
            }
            Operation::Put(key, value) => {
                stats.puts += 1;
                stats.db_writes += 1;
                db.write(key, value);
            }
        }
    }

    stats
}

fn run_cache<C>(cache: &mut C, trace: &[Operation], maybe_clock: Option<&StepClock>) -> RunStats
where
    C: CacheLike,
{
    let mut db = Database::new();
    let mut stats = RunStats::default();

    for &op in trace {
        if let Some(clock) = maybe_clock {
            clock.advance(CLOCK_STEP);
        }

        match op {
            Operation::Get(key) => {
                stats.gets += 1;
                if let Some(value) = cache.get(&key) {
                    stats.hits += 1;
                    stats.checksum ^= *value;
                } else {
                    stats.misses += 1;
                    stats.db_reads += 1;
                    let loaded = db.read(key);
                    stats.checksum ^= loaded;
                    let _ = cache.put(key, loaded);
                }
            }
            Operation::Put(key, value) => {
                stats.puts += 1;
                stats.db_writes += 1;
                db.write(key, value);
                let _ = cache.put(key, value);
            }
        }
    }

    stats
}

fn benchmark_variant<F>(mut run_once: F) -> (Duration, RunStats)
where
    F: FnMut() -> RunStats,
{
    let _ = run_once();
    let start = Instant::now();
    let stats = run_once();
    (start.elapsed(), stats)
}

fn print_result(
    label: &str,
    raw_elapsed: Duration,
    stats: RunStats,
    no_cache_modeled_ops: f64,
    config: BenchmarkConfig,
) {
    let raw_ops_per_sec = OPS_PER_SAMPLE as f64 / raw_elapsed.as_secs_f64();
    let modeled_elapsed = modeled_duration(raw_elapsed, stats, config);
    let modeled_ops_per_sec = OPS_PER_SAMPLE as f64 / modeled_elapsed.as_secs_f64();

    println!(
        "{label:<18} raw={:>8.2} ms {:>10.0} ops/s  hit_rate={:>6.2}%  misses={:>6}  modeled={:>9.2} ms {:>8.0} ops/s  no_cache={:>8.0} ops/s  speedup={:>5.2}x  checksum={}",
        raw_elapsed.as_secs_f64() * 1_000.0,
        raw_ops_per_sec,
        stats.hit_rate() * 100.0,
        stats.misses,
        modeled_elapsed.as_secs_f64() * 1_000.0,
        modeled_ops_per_sec,
        no_cache_modeled_ops,
        modeled_ops_per_sec / no_cache_modeled_ops,
        stats.checksum
    );
}

fn print_baseline(trace: &[Operation], config: BenchmarkConfig) -> f64 {
    let (raw_elapsed, stats) = benchmark_variant(|| run_no_cache(trace));
    let modeled_elapsed = modeled_duration(raw_elapsed, stats, config);
    let modeled_ops = OPS_PER_SAMPLE as f64 / modeled_elapsed.as_secs_f64();

    println!(
        "{:<18} raw={:>8.2} ms {:>10.0} ops/s  hit_rate={:>6}  misses={:>6}  modeled={:>9.2} ms {:>8.0} ops/s  checksum={}",
        "no_cache",
        raw_elapsed.as_secs_f64() * 1_000.0,
        OPS_PER_SAMPLE as f64 / raw_elapsed.as_secs_f64(),
        "n/a",
        stats.misses,
        modeled_elapsed.as_secs_f64() * 1_000.0,
        modeled_ops,
        stats.checksum
    );

    modeled_ops
}

fn print_replacement(trace: &[Operation], capacity: usize, config: BenchmarkConfig) {
    let no_cache_modeled_ops = print_baseline(trace, config);

    let (elapsed, stats) = benchmark_variant(|| {
        let mut cache = LruCache::new(capacity);
        run_cache(&mut cache, trace, None)
    });
    print_result("lru", elapsed, stats, no_cache_modeled_ops, config);

    let (elapsed, stats) = benchmark_variant(|| {
        let mut cache = ClockCache::new(capacity);
        run_cache(&mut cache, trace, None)
    });
    print_result("clock", elapsed, stats, no_cache_modeled_ops, config);
}

fn print_ttl_overhead(trace: &[Operation], config: BenchmarkConfig) {
    let no_cache_modeled_ops = print_baseline(trace, config);

    let (elapsed, stats) = benchmark_variant(|| {
        let mut cache = LruCache::new(CAPACITY);
        run_cache(&mut cache, trace, None)
    });
    print_result("lru_plain", elapsed, stats, no_cache_modeled_ops, config);

    let (elapsed, stats) = benchmark_variant(|| {
        let clock = StepClock::new();
        let mut cache = TtlLruCache::with_clock(CAPACITY, TTL_LONG, clock.clone());
        run_cache(&mut cache, trace, Some(&clock))
    });
    print_result("lru_ttl_long", elapsed, stats, no_cache_modeled_ops, config);

    let (elapsed, stats) = benchmark_variant(|| {
        let mut cache = ClockCache::new(CAPACITY);
        run_cache(&mut cache, trace, None)
    });
    print_result("clock_plain", elapsed, stats, no_cache_modeled_ops, config);

    let (elapsed, stats) = benchmark_variant(|| {
        let clock = StepClock::new();
        let mut cache = TtlClockCache::with_clock(CAPACITY, TTL_LONG, clock.clone());
        run_cache(&mut cache, trace, Some(&clock))
    });
    print_result("clock_ttl_long", elapsed, stats, no_cache_modeled_ops, config);
}

fn print_expiration_stress(trace: &[Operation], config: BenchmarkConfig) {
    let no_cache_modeled_ops = print_baseline(trace, config);

    let (elapsed, stats) = benchmark_variant(|| {
        let clock = StepClock::new();
        let mut cache = TtlLruCache::with_clock(CAPACITY, TTL_STRESS, clock.clone());
        run_cache(&mut cache, trace, Some(&clock))
    });
    print_result("ttl_lru_short", elapsed, stats, no_cache_modeled_ops, config);

    let (elapsed, stats) = benchmark_variant(|| {
        let clock = StepClock::new();
        let mut cache = TtlClockCache::with_clock(CAPACITY, TTL_STRESS, clock.clone());
        run_cache(&mut cache, trace, Some(&clock))
    });
    print_result("ttl_clock_short", elapsed, stats, no_cache_modeled_ops, config);
}

fn parse_config() -> BenchmarkConfig {
    let mut db_read_ms = DEFAULT_DB_READ_MS;
    let mut db_write_ms = DEFAULT_DB_WRITE_MS;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db-read-ms" | "--db-miss-ms" => {
                let value = args
                    .next()
                    .expect("expected a number after --db-read-ms/--db-miss-ms");
                db_read_ms = value
                    .parse::<f64>()
                    .expect("--db-read-ms/--db-miss-ms must be a number");
            }
            "--db-write-ms" => {
                let value = args
                    .next()
                    .expect("expected a number after --db-write-ms");
                db_write_ms = value
                    .parse::<f64>()
                    .expect("--db-write-ms must be a number");
            }
            other => panic!("unknown argument: {other}"),
        }
    }

    BenchmarkConfig {
        db_read_penalty: Duration::from_secs_f64(db_read_ms / 1_000.0),
        db_write_penalty: Duration::from_secs_f64(db_write_ms / 1_000.0),
    }
}

fn main() {
    let config = parse_config();

    println!(
        "Modeled DB penalties: read={:.2} ms, write={:.2} ms\n",
        config.db_read_penalty.as_secs_f64() * 1_000.0,
        config.db_write_penalty.as_secs_f64() * 1_000.0
    );

    for &(hit_rate, mix) in &[
        (HitRate::High, Mix::ReadHeavy),
        (HitRate::High, Mix::Balanced),
        (HitRate::Low, Mix::ReadHeavy),
        (HitRate::Low, Mix::Balanced),
    ] {
        let trace = generate_trace(hit_rate, mix);
        let scenario = scenario_name(hit_rate, mix);

        println!("Replacement policy: {scenario}");
        print_replacement(&trace, CAPACITY, config);

        println!("\nTTL overhead (long TTL, no intended expiration): {scenario}");
        print_ttl_overhead(&trace, config);

        println!("\nExpiration stress (short TTL): {scenario}");
        print_expiration_stress(&trace, config);
        println!();
    }

    let policy_trace = generate_policy_sensitive_trace();
    let policy_config = BenchmarkConfig {
        db_read_penalty: Duration::from_secs_f64(POLICY_SENSITIVE_DB_MS / 1_000.0),
        db_write_penalty: Duration::from_secs_f64(POLICY_SENSITIVE_DB_MS / 1_000.0),
    };
    println!(
        "Policy-sensitive replacement: cache-core dominated (capacity={POLICY_SENSITIVE_CAPACITY}, working_set={POLICY_SENSITIVE_CAPACITY}, resident 50r/50w, db_read={POLICY_SENSITIVE_DB_MS:.2} ms, db_write={POLICY_SENSITIVE_DB_MS:.2} ms)"
    );
    print_replacement(&policy_trace, POLICY_SENSITIVE_CAPACITY, policy_config);
}
