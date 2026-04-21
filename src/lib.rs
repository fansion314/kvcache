//! ```compile_fail
//! use kvcache::LruCache;
//! use std::time::Duration;
//!
//! let mut cache = LruCache::<i32, i32>::new(2);
//! cache.put_with_ttl(1, 1, Duration::from_secs(1));
//! ```
//!
//! ```compile_fail
//! use kvcache::ClockCache;
//! use std::time::Duration;
//!
//! let mut cache = ClockCache::<i32, i32>::new(2);
//! cache.get_and_refresh_expiry(&1);
//! let _ = Duration::from_secs(1);
//! ```

use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::Hash;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub struct LruCache<K, V>
where
    K: Eq + Hash + Clone,
{
    inner: CacheCore<K, V, LruPolicy, PlainEntry<K, V>, SystemClock>,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash + Clone,
{
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: CacheCore::new(capacity, (), SystemClock),
        }
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }

    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        self.inner.put(key, value)
    }

    pub fn invalidate(&mut self, key: &K) -> Option<V> {
        self.inner.invalidate(key)
    }
}

pub struct TtlLruCache<K, V, C = SystemClock>
where
    K: Eq + Hash + Clone,
    C: Clock,
{
    inner: CacheCore<K, V, LruPolicy, TtlEntry<K, V>, C>,
}

impl<K, V> TtlLruCache<K, V, SystemClock>
where
    K: Eq + Hash + Clone,
{
    pub fn new(capacity: usize, default_ttl: Duration) -> Self {
        Self::with_clock(capacity, default_ttl, SystemClock)
    }
}

impl<K, V, C> TtlLruCache<K, V, C>
where
    K: Eq + Hash + Clone,
    C: Clock,
{
    #[allow(dead_code)]
    pub(crate) fn with_clock(capacity: usize, default_ttl: Duration, clock: C) -> Self {
        Self {
            inner: CacheCore::new(capacity, default_ttl, clock),
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
        self.inner.put_with_arg(key, value, ttl)
    }

    pub fn invalidate(&mut self, key: &K) -> Option<V> {
        self.inner.invalidate(key)
    }
}

pub struct ClockCache<K, V>
where
    K: Eq + Hash + Clone,
{
    inner: CacheCore<K, V, ClockPolicy, PlainEntry<K, V>, SystemClock>,
}

impl<K, V> ClockCache<K, V>
where
    K: Eq + Hash + Clone,
{
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: CacheCore::new(capacity, (), SystemClock),
        }
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }

    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        self.inner.put(key, value)
    }

    pub fn invalidate(&mut self, key: &K) -> Option<V> {
        self.inner.invalidate(key)
    }
}

pub struct TtlClockCache<K, V, C = SystemClock>
where
    K: Eq + Hash + Clone,
    C: Clock,
{
    inner: CacheCore<K, V, ClockPolicy, TtlEntry<K, V>, C>,
}

impl<K, V> TtlClockCache<K, V, SystemClock>
where
    K: Eq + Hash + Clone,
{
    pub fn new(capacity: usize, default_ttl: Duration) -> Self {
        Self::with_clock(capacity, default_ttl, SystemClock)
    }
}

impl<K, V, C> TtlClockCache<K, V, C>
where
    K: Eq + Hash + Clone,
    C: Clock,
{
    #[allow(dead_code)]
    pub(crate) fn with_clock(capacity: usize, default_ttl: Duration, clock: C) -> Self {
        Self {
            inner: CacheCore::new(capacity, default_ttl, clock),
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
        self.inner.put_with_arg(key, value, ttl)
    }

    pub fn invalidate(&mut self, key: &K) -> Option<V> {
        self.inner.invalidate(key)
    }
}

pub trait Clock {
    fn now(&self) -> Instant;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Clone)]
pub(crate) struct StepClock {
    now: Rc<Cell<Instant>>,
}

impl StepClock {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self {
            now: Rc::new(Cell::new(Instant::now())),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn advance(&self, delta: Duration) {
        let next = self.now.get().checked_add(delta).expect("clock overflowed");
        self.now.set(next);
    }
}

impl Clock for StepClock {
    fn now(&self) -> Instant {
        self.now.get()
    }
}

trait EntryFlavor<K, V> {
    type InsertArg: Copy;

    fn uses_expiry() -> bool;
    fn new(key: K, value: V, insert: Self::InsertArg, now: Instant) -> Self;
    fn key(&self) -> &K;
    fn value(&self) -> &V;
    fn replace_value(&mut self, value: V, insert: Self::InsertArg, now: Instant) -> V;
    fn is_expired(&self, now: Instant) -> bool;
    fn refresh_expiry(&mut self, now: Instant);
    fn expires_at(&self) -> Option<Instant>;
}

struct PlainEntry<K, V> {
    key: K,
    value: V,
}

impl<K, V> EntryFlavor<K, V> for PlainEntry<K, V> {
    type InsertArg = ();

    fn uses_expiry() -> bool {
        false
    }

    fn new(key: K, value: V, _insert: Self::InsertArg, _now: Instant) -> Self {
        Self { key, value }
    }

    fn key(&self) -> &K {
        &self.key
    }

    fn value(&self) -> &V {
        &self.value
    }

    fn replace_value(&mut self, value: V, _insert: Self::InsertArg, _now: Instant) -> V {
        std::mem::replace(&mut self.value, value)
    }

    fn is_expired(&self, _now: Instant) -> bool {
        false
    }

    fn refresh_expiry(&mut self, _now: Instant) {}

    fn expires_at(&self) -> Option<Instant> {
        None
    }
}

struct TtlEntry<K, V> {
    key: K,
    value: V,
    ttl: Duration,
    expires_at: Instant,
}

impl<K, V> EntryFlavor<K, V> for TtlEntry<K, V> {
    type InsertArg = Duration;

    fn uses_expiry() -> bool {
        true
    }

    fn new(key: K, value: V, ttl: Self::InsertArg, now: Instant) -> Self {
        Self {
            key,
            value,
            ttl,
            expires_at: checked_deadline(now, ttl),
        }
    }

    fn key(&self) -> &K {
        &self.key
    }

    fn value(&self) -> &V {
        &self.value
    }

    fn replace_value(&mut self, value: V, ttl: Self::InsertArg, now: Instant) -> V {
        let old_value = std::mem::replace(&mut self.value, value);
        self.ttl = ttl;
        self.expires_at = checked_deadline(now, ttl);
        old_value
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at <= now
    }

    fn refresh_expiry(&mut self, now: Instant) {
        self.expires_at = checked_deadline(now, self.ttl);
    }

    fn expires_at(&self) -> Option<Instant> {
        Some(self.expires_at)
    }
}

trait Policy<E> {
    type Meta: Default;
    type State;

    fn new_state(capacity: usize) -> Self::State;
    fn on_insert(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State, idx: usize);
    fn on_access(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State, idx: usize);
    fn on_remove(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State, idx: usize);
    fn select_victim(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State) -> usize;
}

struct CacheCore<K, V, P, E, C>
where
    K: Eq + Hash + Clone,
    P: Policy<E>,
    E: EntryFlavor<K, V> + IntoValue<V>,
    C: Clock,
{
    capacity: usize,
    default_insert: E::InsertArg,
    slots: Vec<Slot<E, P::Meta>>,
    index: HashMap<K, usize>,
    free: Vec<usize>,
    expiries: BinaryHeap<Reverse<ExpiryRecord>>,
    policy_state: P::State,
    clock: C,
    _policy: PhantomData<P>,
    _value: PhantomData<V>,
}

impl<K, V, P, E, C> CacheCore<K, V, P, E, C>
where
    K: Eq + Hash + Clone,
    P: Policy<E>,
    E: EntryFlavor<K, V> + IntoValue<V>,
    C: Clock,
{
    fn new(capacity: usize, default_insert: E::InsertArg, clock: C) -> Self {
        Self {
            capacity,
            default_insert,
            slots: Vec::with_capacity(capacity),
            index: HashMap::with_capacity(capacity),
            free: Vec::with_capacity(capacity),
            expiries: BinaryHeap::with_capacity(capacity),
            policy_state: P::new_state(capacity),
            clock,
            _policy: PhantomData,
            _value: PhantomData,
        }
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        self.get_internal(key, false)
    }

    fn get_and_refresh_expiry(&mut self, key: &K) -> Option<&V> {
        self.get_internal(key, true)
    }

    fn put(&mut self, key: K, value: V) -> Option<V> {
        self.put_with_arg(key, value, self.default_insert)
    }

    fn put_with_arg(&mut self, key: K, value: V, insert: E::InsertArg) -> Option<V> {
        if self.capacity == 0 {
            return None;
        }

        if E::uses_expiry() {
            self.reap_expired();
        }

        if let Some(&idx) = self.index.get(&key) {
            return Some(self.update_existing(idx, value, insert));
        }

        let idx = self.acquire_slot();
        self.insert_at_slot(idx, key, value, insert);
        None
    }

    fn invalidate(&mut self, key: &K) -> Option<V> {
        let idx = *self.index.get(key)?;

        if E::uses_expiry() {
            let now = self.clock.now();
            if self.is_expired(idx, now) {
                let _ = self.remove_slot(idx);
                return None;
            }
        }

        self.remove_slot(idx)
    }

    fn get_internal(&mut self, key: &K, refresh_expiry: bool) -> Option<&V> {
        let idx = *self.index.get(key)?;

        if E::uses_expiry() {
            let now = self.clock.now();
            if self.is_expired(idx, now) {
                let _ = self.remove_slot(idx);
                return None;
            }

            if refresh_expiry {
                self.refresh_expiry(idx, now);
            }
        }

        P::on_access(&mut self.slots, &mut self.policy_state, idx);
        self.value_ref(idx)
    }

    fn acquire_slot(&mut self) -> usize {
        if let Some(idx) = self.free.pop() {
            return idx;
        }

        if self.slots.len() < self.capacity {
            let idx = self.slots.len();
            self.slots.push(Slot::vacant());
            return idx;
        }

        let victim = P::select_victim(&mut self.slots, &mut self.policy_state);
        let _ = self
            .remove_slot(victim)
            .expect("victim selection must choose a live slot");
        self.free
            .pop()
            .expect("removing a victim should release a reusable slot")
    }

    fn update_existing(&mut self, idx: usize, value: V, insert: E::InsertArg) -> V {
        let now = self.clock.now();
        let generation = self.bump_generation(idx);
        let old_value;
        let expires_at;

        {
            let entry = self.entry_mut(idx).expect("indexed slot must be occupied");
            old_value = entry.replace_value(value, insert, now);
            expires_at = entry.expires_at();
        }

        if let Some(expires_at) = expires_at {
            self.expiries.push(Reverse(ExpiryRecord {
                expires_at,
                index: idx,
                generation,
            }));
        }

        P::on_access(&mut self.slots, &mut self.policy_state, idx);
        old_value
    }

    fn insert_at_slot(&mut self, idx: usize, key: K, value: V, insert: E::InsertArg) {
        let now = self.clock.now();
        let entry = E::new(key.clone(), value, insert, now);
        let expires_at = entry.expires_at();
        let generation = self.slots[idx].generation;

        self.slots[idx].entry = Some(entry);
        self.index.insert(key, idx);
        P::on_insert(&mut self.slots, &mut self.policy_state, idx);

        if let Some(expires_at) = expires_at {
            self.expiries.push(Reverse(ExpiryRecord {
                expires_at,
                index: idx,
                generation,
            }));
        }
    }

    fn refresh_expiry(&mut self, idx: usize, now: Instant) {
        if !E::uses_expiry() {
            return;
        }

        if self.entry(idx).and_then(EntryFlavor::expires_at).is_none() {
            return;
        }

        let generation = self.bump_generation(idx);
        let expires_at = {
            let entry = self.entry_mut(idx).expect("indexed slot must be occupied");
            entry.refresh_expiry(now);
            entry.expires_at().expect("refreshed expiry must exist")
        };

        self.expiries.push(Reverse(ExpiryRecord {
            expires_at,
            index: idx,
            generation,
        }));
    }

    fn reap_expired(&mut self) {
        if !E::uses_expiry() {
            return;
        }

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

        P::on_remove(&mut self.slots, &mut self.policy_state, idx);

        let slot = &mut self.slots[idx];
        let entry = slot.entry.take().expect("slot was checked as occupied");
        self.index.remove(entry.key());
        self.free.push(idx);

        Some(entry.into_value())
    }

    fn is_expired(&self, idx: usize, now: Instant) -> bool {
        self.entry(idx)
            .map(|entry| entry.is_expired(now))
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

    fn value_ref(&self, idx: usize) -> Option<&V> {
        self.entry(idx).map(EntryFlavor::value)
    }

    fn entry(&self, idx: usize) -> Option<&E> {
        self.slots.get(idx)?.entry.as_ref()
    }

    fn entry_mut(&mut self, idx: usize) -> Option<&mut E> {
        self.slots.get_mut(idx)?.entry.as_mut()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.index.len()
    }
}

trait IntoValue<V> {
    fn into_value(self) -> V;
}

impl<K, V> IntoValue<V> for PlainEntry<K, V> {
    fn into_value(self) -> V {
        self.value
    }
}

impl<K, V> IntoValue<V> for TtlEntry<K, V> {
    fn into_value(self) -> V {
        self.value
    }
}

struct Slot<E, M> {
    entry: Option<E>,
    meta: M,
    generation: u64,
}

impl<E, M> Slot<E, M>
where
    M: Default,
{
    fn vacant() -> Self {
        Self {
            entry: None,
            meta: M::default(),
            generation: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct LruMeta {
    prev: Option<usize>,
    next: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default)]
struct LruState {
    head: Option<usize>,
    tail: Option<usize>,
}

struct LruPolicy;

impl<E> Policy<E> for LruPolicy {
    type Meta = LruMeta;
    type State = LruState;

    fn new_state(_capacity: usize) -> Self::State {
        LruState::default()
    }

    fn on_insert(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State, idx: usize) {
        lru_attach_to_head(slots, state, idx);
    }

    fn on_access(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State, idx: usize) {
        if state.head == Some(idx) {
            return;
        }

        lru_detach(slots, state, idx);
        lru_attach_to_head(slots, state, idx);
    }

    fn on_remove(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State, idx: usize) {
        lru_detach(slots, state, idx);
    }

    fn select_victim(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State) -> usize {
        let _ = slots;
        state.tail.expect("tail must exist when the cache is full")
    }
}

fn lru_detach<E>(slots: &mut [Slot<E, LruMeta>], state: &mut LruState, idx: usize) {
    let prev = slots[idx].meta.prev;
    let next = slots[idx].meta.next;

    if let Some(prev_idx) = prev {
        slots[prev_idx].meta.next = next;
    } else {
        state.head = next;
    }

    if let Some(next_idx) = next {
        slots[next_idx].meta.prev = prev;
    } else {
        state.tail = prev;
    }

    slots[idx].meta.prev = None;
    slots[idx].meta.next = None;
}

fn lru_attach_to_head<E>(slots: &mut [Slot<E, LruMeta>], state: &mut LruState, idx: usize) {
    slots[idx].meta.prev = None;
    slots[idx].meta.next = state.head;

    if let Some(old_head) = state.head {
        slots[old_head].meta.prev = Some(idx);
    } else {
        state.tail = Some(idx);
    }

    state.head = Some(idx);
}

#[derive(Clone, Copy, Debug, Default)]
struct ClockMeta {
    referenced: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct ClockState {
    hand: usize,
}

struct ClockPolicy;

impl<E> Policy<E> for ClockPolicy {
    type Meta = ClockMeta;
    type State = ClockState;

    fn new_state(_capacity: usize) -> Self::State {
        ClockState::default()
    }

    fn on_insert(slots: &mut [Slot<E, Self::Meta>], _state: &mut Self::State, idx: usize) {
        slots[idx].meta.referenced = true;
    }

    fn on_access(slots: &mut [Slot<E, Self::Meta>], _state: &mut Self::State, idx: usize) {
        slots[idx].meta.referenced = true;
    }

    fn on_remove(slots: &mut [Slot<E, Self::Meta>], _state: &mut Self::State, idx: usize) {
        slots[idx].meta.referenced = false;
    }

    fn select_victim(slots: &mut [Slot<E, Self::Meta>], state: &mut Self::State) -> usize {
        assert!(
            !slots.is_empty(),
            "clock eviction requires at least one slot"
        );

        loop {
            let idx = state.hand % slots.len();
            state.hand = (state.hand + 1) % slots.len();

            if slots[idx].entry.is_none() {
                continue;
            }

            if slots[idx].meta.referenced {
                slots[idx].meta.referenced = false;
                continue;
            }

            return idx;
        }
    }
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
mod tests {
    use super::*;

    fn ttl_lru_with_clock<K, V>(
        capacity: usize,
        default_ttl: Duration,
    ) -> (TtlLruCache<K, V, StepClock>, StepClock)
    where
        K: Eq + Hash + Clone,
    {
        let clock = StepClock::new();
        (
            TtlLruCache::with_clock(capacity, default_ttl, clock.clone()),
            clock,
        )
    }

    fn ttl_clock_with_clock<K, V>(
        capacity: usize,
        default_ttl: Duration,
    ) -> (TtlClockCache<K, V, StepClock>, StepClock)
    where
        K: Eq + Hash + Clone,
    {
        let clock = StepClock::new();
        (
            TtlClockCache::with_clock(capacity, default_ttl, clock.clone()),
            clock,
        )
    }

    #[test]
    fn lru_without_ttl_supports_basic_get_put_invalidate() {
        let mut cache = LruCache::new(2);

        assert_eq!(cache.put(1, 10), None);
        assert_eq!(cache.put(2, 20), None);
        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.invalidate(&1), Some(10));
        assert_eq!(cache.get(&1), None);
    }

    #[test]
    fn lru_without_ttl_evicts_tail() {
        let mut cache = LruCache::new(2);

        cache.put(1, 10);
        cache.put(2, 20);
        assert_eq!(cache.get(&1), Some(&10));
        cache.put(3, 30);

        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&3), Some(&30));
    }

    #[test]
    fn ttl_lru_expires_entries_on_read() {
        let (mut cache, clock) = ttl_lru_with_clock::<i32, i32>(2, Duration::from_secs(5));

        cache.put(1, 10);
        clock.advance(Duration::from_secs(5));

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.inner.len(), 0);
    }

    #[test]
    fn ttl_lru_reuses_expired_slot_before_live_lru() {
        let (mut cache, clock) = ttl_lru_with_clock::<i32, i32>(2, Duration::from_secs(10));

        cache.put_with_ttl(1, 10, Duration::from_secs(2));
        cache.put_with_ttl(2, 20, Duration::from_secs(20));
        clock.advance(Duration::from_secs(3));

        cache.put(3, 30);

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&20));
        assert_eq!(cache.get(&3), Some(&30));
        assert_eq!(cache.inner.len(), 2);
    }

    #[test]
    fn ttl_lru_refreshes_expiry_without_changing_value() {
        let (mut cache, clock) = ttl_lru_with_clock::<i32, &'static str>(2, Duration::from_secs(3));

        cache.put(1, "one");
        clock.advance(Duration::from_secs(2));
        assert_eq!(cache.get_and_refresh_expiry(&1), Some(&"one"));
        clock.advance(Duration::from_secs(2));

        assert_eq!(cache.get(&1), Some(&"one"));
    }

    #[test]
    fn ttl_lru_stale_expiry_records_do_not_remove_live_entries() {
        let (mut cache, clock) = ttl_lru_with_clock::<i32, i32>(2, Duration::from_secs(5));

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
    fn clock_without_ttl_uses_second_chance_eviction() {
        let mut cache = ClockCache::new(3);

        cache.put(1, 10);
        cache.put(2, 20);
        cache.put(3, 30);
        cache.put(4, 40);

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&20));
        assert_eq!(cache.get(&3), Some(&30));
        assert_eq!(cache.get(&4), Some(&40));
    }

    #[test]
    fn clock_reads_protect_entries_with_second_chance() {
        let mut cache = ClockCache::new(3);

        cache.put(1, 10);
        cache.put(2, 20);
        cache.put(3, 30);
        cache.put(4, 40);
        assert_eq!(cache.get(&2), Some(&20));
        cache.put(5, 50);

        assert_eq!(cache.get(&2), Some(&20));
        assert_eq!(cache.get(&3), None);
        assert_eq!(cache.get(&4), Some(&40));
        assert_eq!(cache.get(&5), Some(&50));
    }

    #[test]
    fn ttl_clock_reuses_expired_slot_before_scanning_for_victim() {
        let (mut cache, clock) = ttl_clock_with_clock::<i32, i32>(2, Duration::from_secs(10));

        cache.put_with_ttl(1, 10, Duration::from_secs(2));
        cache.put_with_ttl(2, 20, Duration::from_secs(20));
        clock.advance(Duration::from_secs(3));
        cache.put(3, 30);

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&20));
        assert_eq!(cache.get(&3), Some(&30));
    }

    #[test]
    fn ttl_clock_expires_entries_on_read() {
        let (mut cache, clock) = ttl_clock_with_clock::<i32, i32>(2, Duration::from_secs(4));

        cache.put(1, 10);
        clock.advance(Duration::from_secs(4));

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.inner.len(), 0);
    }

    #[test]
    fn ttl_clock_supports_refreshing_expiry() {
        let (mut cache, clock) =
            ttl_clock_with_clock::<i32, &'static str>(2, Duration::from_secs(3));

        cache.put(1, "one");
        clock.advance(Duration::from_secs(2));
        assert_eq!(cache.get_and_refresh_expiry(&1), Some(&"one"));
        clock.advance(Duration::from_secs(2));

        assert_eq!(cache.get(&1), Some(&"one"));
    }

    #[test]
    fn ttl_clock_invalidate_returns_none_for_expired_entries() {
        let (mut cache, clock) = ttl_clock_with_clock::<i32, i32>(2, Duration::from_secs(1));

        cache.put(1, 10);
        clock.advance(Duration::from_secs(1));

        assert_eq!(cache.invalidate(&1), None);
        assert_eq!(cache.inner.len(), 0);
    }

    #[test]
    fn all_variants_handle_zero_capacity() {
        let mut lru = LruCache::<i32, i32>::new(0);
        let mut clock = ClockCache::<i32, i32>::new(0);
        let (mut ttl_lru, _) = ttl_lru_with_clock::<i32, i32>(0, Duration::from_secs(1));
        let (mut ttl_clock, _) = ttl_clock_with_clock::<i32, i32>(0, Duration::from_secs(1));

        assert_eq!(lru.put(1, 1), None);
        assert_eq!(clock.put(1, 1), None);
        assert_eq!(ttl_lru.put(1, 1), None);
        assert_eq!(ttl_clock.put(1, 1), None);

        assert_eq!(lru.get(&1), None);
        assert_eq!(clock.get(&1), None);
        assert_eq!(ttl_lru.get(&1), None);
        assert_eq!(ttl_clock.get(&1), None);
    }
}
