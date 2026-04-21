use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::Hash;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::rc::Rc;

pub struct LruCache<K, V> {
    inner: CacheCore<K, V, SystemClock>,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash + Clone,
{
    pub fn new(capacity: usize, default_ttl: Duration) -> Self {
        Self {
            inner: CacheCore::new(capacity, default_ttl, SystemClock),
        }
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }

    pub fn get_and_refresh_expiry(&mut self, key: &K) -> Option<&V> {
        self.inner.get_and_refresh_expiry(key)
    }

    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        self.inner.put(key, value)
    }

    pub fn put_with_ttl(&mut self, key: K, value: V, ttl: Duration) -> Option<V> {
        self.inner.put_with_ttl(key, value, ttl)
    }

    pub fn invalidate(&mut self, key: &K) -> Option<V> {
        self.inner.invalidate(key)
    }
}

trait Clock {
    fn now(&self) -> Instant;
}

#[derive(Clone, Copy, Debug, Default)]
struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct CacheCore<K, V, C> {
    capacity: usize,
    default_ttl: Duration,
    slots: Vec<Slot<K, V>>,
    index: HashMap<K, usize>,
    head: Option<usize>,
    tail: Option<usize>,
    free: Vec<usize>,
    expiries: BinaryHeap<Reverse<ExpiryRecord>>,
    clock: C,
}

impl<K, V, C> CacheCore<K, V, C>
where
    K: Eq + Hash + Clone,
    C: Clock,
{
    fn new(capacity: usize, default_ttl: Duration, clock: C) -> Self {
        Self {
            capacity,
            default_ttl,
            slots: Vec::with_capacity(capacity),
            index: HashMap::with_capacity(capacity),
            head: None,
            tail: None,
            free: Vec::with_capacity(capacity),
            expiries: BinaryHeap::with_capacity(capacity),
            clock,
        }
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        self.get_internal(key, false)
    }

    fn get_and_refresh_expiry(&mut self, key: &K) -> Option<&V> {
        self.get_internal(key, true)
    }

    fn put(&mut self, key: K, value: V) -> Option<V> {
        self.put_with_ttl(key, value, self.default_ttl)
    }

    fn put_with_ttl(&mut self, key: K, value: V, ttl: Duration) -> Option<V> {
        if self.capacity == 0 {
            return None;
        }

        self.reap_expired();

        if let Some(&idx) = self.index.get(&key) {
            return Some(self.update_existing(idx, value, ttl));
        }

        let idx = if let Some(idx) = self.free.pop() {
            idx
        } else if self.slots.len() < self.capacity {
            let idx = self.slots.len();
            self.slots.push(Slot::vacant());
            idx
        } else {
            let tail = self.tail.expect("tail must exist when cache is full");
            let _ = self.remove_slot(tail).expect("tail slot must be occupied");
            self.free
                .pop()
                .expect("removing the tail should produce a free slot")
        };

        self.insert_at_slot(idx, key, value, ttl);
        None
    }

    fn invalidate(&mut self, key: &K) -> Option<V> {
        let idx = *self.index.get(key)?;

        if self.is_expired(idx, self.clock.now()) {
            let _ = self.remove_slot(idx);
            return None;
        }

        self.remove_slot(idx)
    }

    fn get_internal(&mut self, key: &K, refresh_expiry: bool) -> Option<&V> {
        let idx = *self.index.get(key)?;
        let now = self.clock.now();

        if self.is_expired(idx, now) {
            let _ = self.remove_slot(idx);
            return None;
        }

        if refresh_expiry {
            self.refresh_expiry(idx, now);
        }

        self.move_to_head(idx);
        self.value_ref(idx)
    }

    fn update_existing(&mut self, idx: usize, value: V, ttl: Duration) -> V {
        let now = self.clock.now();
        let next_generation = self.bump_generation(idx);
        let expires_at = checked_deadline(now, ttl);

        let entry = self.entry_mut(idx).expect("indexed slot must be occupied");
        let old_value = std::mem::replace(&mut entry.value, value);
        entry.ttl = ttl;
        entry.expires_at = expires_at;

        self.expiries.push(Reverse(ExpiryRecord {
            expires_at,
            index: idx,
            generation: next_generation,
        }));
        self.move_to_head(idx);

        old_value
    }

    fn insert_at_slot(&mut self, idx: usize, key: K, value: V, ttl: Duration) {
        let expires_at = checked_deadline(self.clock.now(), ttl);
        let generation = self.slots[idx].generation;

        self.slots[idx].entry = Some(Entry {
            key: key.clone(),
            value,
            ttl,
            expires_at,
        });
        self.slots[idx].prev = None;
        self.slots[idx].next = None;

        self.index.insert(key, idx);
        self.attach_to_head(idx);
        self.expiries.push(Reverse(ExpiryRecord {
            expires_at,
            index: idx,
            generation,
        }));
    }

    fn refresh_expiry(&mut self, idx: usize, now: Instant) {
        let ttl = self.entry(idx).expect("occupied slot must have entry").ttl;
        let expires_at = checked_deadline(now, ttl);
        let generation = self.bump_generation(idx);

        let entry = self.entry_mut(idx).expect("occupied slot must have entry");
        entry.expires_at = expires_at;

        self.expiries.push(Reverse(ExpiryRecord {
            expires_at,
            index: idx,
            generation,
        }));
    }

    fn reap_expired(&mut self) {
        let now = self.clock.now();

        while let Some(record) = self.expiries.peek().copied() {
            if record.0.expires_at > now {
                break;
            }

            let record = self.expiries.pop().expect("peeked record must exist").0;
            if !self.is_live_generation(record.index, record.generation) {
                continue;
            }

            if self.is_expired(record.index, now) {
                let _ = self.remove_slot(record.index);
            }
        }
    }

    fn remove_slot(&mut self, idx: usize) -> Option<V> {
        let _ = self.bump_generation(idx);
        let slot = self.slots.get(idx)?;
        if slot.entry.is_none() {
            return None;
        }

        self.detach(idx);

        let slot = &mut self.slots[idx];
        let entry = slot.entry.take().expect("slot was checked as occupied");
        slot.prev = None;
        slot.next = None;

        self.index.remove(&entry.key);
        self.free.push(idx);

        Some(entry.value)
    }

    fn is_expired(&self, idx: usize, now: Instant) -> bool {
        self.entry(idx)
            .map(|entry| entry.expires_at <= now)
            .unwrap_or(false)
    }

    fn is_live_generation(&self, idx: usize, generation: u64) -> bool {
        self.slots
            .get(idx)
            .map(|slot| slot.entry.is_some() && slot.generation == generation)
            .unwrap_or(false)
    }

    fn bump_generation(&mut self, idx: usize) -> u64 {
        let next = self.slots[idx].generation.wrapping_add(1);
        self.slots[idx].generation = next;
        next
    }

    fn move_to_head(&mut self, idx: usize) {
        if self.head == Some(idx) {
            return;
        }

        self.detach(idx);
        self.attach_to_head(idx);
    }

    fn detach(&mut self, idx: usize) {
        if self.slots[idx].entry.is_none() {
            return;
        }

        let prev = self.slots[idx].prev;
        let next = self.slots[idx].next;

        if let Some(prev_idx) = prev {
            self.slots[prev_idx].next = next;
        } else {
            self.head = next;
        }

        if let Some(next_idx) = next {
            self.slots[next_idx].prev = prev;
        } else {
            self.tail = prev;
        }

        self.slots[idx].prev = None;
        self.slots[idx].next = None;
    }

    fn attach_to_head(&mut self, idx: usize) {
        self.slots[idx].prev = None;
        self.slots[idx].next = self.head;

        if let Some(old_head) = self.head {
            self.slots[old_head].prev = Some(idx);
        } else {
            self.tail = Some(idx);
        }

        self.head = Some(idx);
    }

    fn value_ref(&self, idx: usize) -> Option<&V> {
        self.entry(idx).map(|entry| &entry.value)
    }

    fn entry(&self, idx: usize) -> Option<&Entry<K, V>> {
        self.slots.get(idx)?.entry.as_ref()
    }

    fn entry_mut(&mut self, idx: usize) -> Option<&mut Entry<K, V>> {
        self.slots.get_mut(idx)?.entry.as_mut()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.index.len()
    }

    #[cfg(test)]
    fn lru_keys(&self) -> Vec<K> {
        let mut keys = Vec::with_capacity(self.index.len());
        let mut cursor = self.head;
        while let Some(idx) = cursor {
            let entry = self
                .entry(idx)
                .expect("lru chain must only contain live entries");
            keys.push(entry.key.clone());
            cursor = self.slots[idx].next;
        }
        keys
    }
}

struct Slot<K, V> {
    entry: Option<Entry<K, V>>,
    prev: Option<usize>,
    next: Option<usize>,
    generation: u64,
}

impl<K, V> Slot<K, V> {
    fn vacant() -> Self {
        Self {
            entry: None,
            prev: None,
            next: None,
            generation: 0,
        }
    }
}

struct Entry<K, V> {
    key: K,
    value: V,
    ttl: Duration,
    expires_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ExpiryRecord {
    expires_at: Instant,
    index: usize,
    generation: u64,
}

fn checked_deadline(now: Instant, ttl: Duration) -> Instant {
    now.checked_add(ttl)
        .expect("ttl overflowed the supported Instant range")
}

#[cfg(test)]
#[derive(Clone)]
struct TestClock {
    now: Rc<Cell<Instant>>,
}

#[cfg(test)]
impl TestClock {
    fn new() -> Self {
        Self {
            now: Rc::new(Cell::new(Instant::now())),
        }
    }

    fn advance(&self, delta: Duration) {
        let next = self
            .now
            .get()
            .checked_add(delta)
            .expect("test clock overflowed");
        self.now.set(next);
    }
}

#[cfg(test)]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        self.now.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with_clock<K, V>(
        capacity: usize,
        default_ttl: Duration,
    ) -> (CacheCore<K, V, TestClock>, TestClock)
    where
        K: Eq + Hash + Clone,
    {
        let clock = TestClock::new();
        (CacheCore::new(capacity, default_ttl, clock.clone()), clock)
    }

    #[test]
    fn basic_put_get_and_lru_ordering() {
        let (mut cache, _) = cache_with_clock::<i32, i32>(2, Duration::from_secs(30));

        assert_eq!(cache.put(1, 10), None);
        assert_eq!(cache.put(2, 20), None);
        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.lru_keys(), vec![1, 2]);
        assert_eq!(cache.get(&2), Some(&20));
        assert_eq!(cache.lru_keys(), vec![2, 1]);
    }

    #[test]
    fn evicts_lru_when_no_entries_are_expired() {
        let (mut cache, _) = cache_with_clock::<i32, i32>(2, Duration::from_secs(30));

        cache.put(1, 10);
        cache.put(2, 20);
        assert_eq!(cache.get(&1), Some(&10));

        cache.put(3, 30);

        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&3), Some(&30));
    }

    #[test]
    fn expired_entry_returns_none_on_get() {
        let (mut cache, clock) = cache_with_clock::<i32, i32>(2, Duration::from_secs(5));

        cache.put(1, 10);
        clock.advance(Duration::from_secs(5));

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn put_reuses_expired_slot_before_evicting_live_lru() {
        let (mut cache, clock) = cache_with_clock::<i32, i32>(2, Duration::from_secs(10));

        cache.put_with_ttl(1, 10, Duration::from_secs(2));
        cache.put_with_ttl(2, 20, Duration::from_secs(20));
        clock.advance(Duration::from_secs(3));

        cache.put(3, 30);

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&20));
        assert_eq!(cache.get(&3), Some(&30));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn invalidate_returns_removed_value_for_live_entry_only() {
        let (mut cache, clock) = cache_with_clock::<i32, i32>(2, Duration::from_secs(10));

        cache.put(1, 10);
        assert_eq!(cache.invalidate(&1), Some(10));
        assert_eq!(cache.invalidate(&1), None);

        cache.put_with_ttl(2, 20, Duration::from_secs(1));
        clock.advance(Duration::from_secs(1));

        assert_eq!(cache.invalidate(&2), None);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn invalidated_entry_is_removed_from_lru_and_expiry_tracking() {
        let (mut cache, clock) = cache_with_clock::<i32, i32>(2, Duration::from_secs(10));

        cache.put_with_ttl(1, 10, Duration::from_secs(2));
        cache.put(2, 20);
        assert_eq!(cache.invalidate(&1), Some(10));
        clock.advance(Duration::from_secs(3));

        cache.put(3, 30);

        assert_eq!(cache.get(&2), Some(&20));
        assert_eq!(cache.get(&3), Some(&30));
        assert_eq!(cache.lru_keys(), vec![3, 2]);
    }

    #[test]
    fn updating_existing_key_refreshes_value_ttl_and_recency() {
        let (mut cache, clock) = cache_with_clock::<i32, i32>(2, Duration::from_secs(5));

        cache.put(1, 10);
        clock.advance(Duration::from_secs(2));
        assert_eq!(cache.put_with_ttl(1, 15, Duration::from_secs(4)), Some(10));

        clock.advance(Duration::from_secs(3));
        assert_eq!(cache.get(&1), Some(&15));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.lru_keys(), vec![1]);
    }

    #[test]
    fn get_and_refresh_expiry_differs_from_plain_get() {
        let (mut cache, clock) = cache_with_clock::<i32, &'static str>(2, Duration::from_secs(3));

        cache.put(1, "one");
        clock.advance(Duration::from_secs(2));
        assert_eq!(cache.get(&1), Some(&"one"));
        clock.advance(Duration::from_secs(1));
        assert_eq!(cache.get(&1), None);

        cache.put(2, "two");
        clock.advance(Duration::from_secs(2));
        assert_eq!(cache.get_and_refresh_expiry(&2), Some(&"two"));
        clock.advance(Duration::from_secs(2));
        assert_eq!(cache.get(&2), Some(&"two"));
    }

    #[test]
    fn stale_expiry_records_do_not_remove_live_entries() {
        let (mut cache, clock) = cache_with_clock::<i32, i32>(2, Duration::from_secs(5));

        cache.put(1, 10);
        clock.advance(Duration::from_secs(1));
        cache.put(2, 20);
        clock.advance(Duration::from_secs(1));
        assert_eq!(cache.get_and_refresh_expiry(&1), Some(&10));
        clock.advance(Duration::from_secs(3));

        cache.put(3, 30);

        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&3), Some(&30));
    }

    #[test]
    fn slots_are_reused_after_invalidation_and_expiration() {
        let (mut cache, clock) = cache_with_clock::<i32, i32>(2, Duration::from_secs(2));

        cache.put(1, 10);
        cache.put(2, 20);
        assert_eq!(cache.invalidate(&1), Some(10));
        cache.put_with_ttl(3, 30, Duration::from_secs(10));

        let first_layout = cache.slots.len();
        clock.advance(Duration::from_secs(2));
        cache.put(4, 40);

        assert_eq!(cache.slots.len(), first_layout);
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&3), Some(&30));
        assert_eq!(cache.get(&4), Some(&40));
    }

    #[test]
    fn zero_capacity_cache_is_always_empty() {
        let (mut cache, _) = cache_with_clock::<i32, i32>(0, Duration::from_secs(1));

        assert_eq!(cache.put(1, 10), None);
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.invalidate(&1), None);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn public_wrapper_matches_core_behavior() {
        let mut cache = LruCache::new(2, Duration::from_secs(60));

        assert_eq!(cache.put("a", 1), None);
        assert_eq!(cache.put("b", 2), None);
        assert_eq!(cache.get(&"a"), Some(&1));
        assert_eq!(cache.put("c", 3), None);
        assert_eq!(cache.get(&"b"), None);
        assert_eq!(cache.invalidate(&"a"), Some(1));
        assert_eq!(cache.get(&"c"), Some(&3));
    }
}
