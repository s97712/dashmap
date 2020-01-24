use super::element::*;
use crossbeam_epoch::{pin, Atomic, Guard, Owned, Shared};
use std::borrow::Borrow;
use std::hash::{BuildHasher, Hash, Hasher};
use std::iter;
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const PTR_SIZE_BITS: usize = mem::size_of::<usize>() * 8;
const REDIRECT_TAG: usize = 5;
const TOMBSTONE_TAG: usize = 7;

fn make_shift(x: usize) -> usize {
    debug_assert!(x.is_power_of_two());
    PTR_SIZE_BITS - x.trailing_zeros() as usize
}

fn make_buckets<K, V>(x: usize) -> Box<[Atomic<Element<K, V>>]> {
    iter::repeat(Atomic::null()).take(x).collect()
}

fn hash2idx(hash: u64, shift: usize) -> usize {
    hash as usize >> shift
}

fn do_hash(f: &impl BuildHasher, i: &(impl ?Sized + Hash)) -> u64 {
    let mut hasher = f.build_hasher();
    i.hash(&mut hasher);
    hasher.finish()
}

pub struct BucketArray<K, V, S> {
    remaining_cells: AtomicUsize,
    shift: usize,
    hash_builder: Arc<S>,
    buckets: Box<[Atomic<Element<K, V>>]>,
    next: Atomic<Self>,
}

impl<K: Eq + Hash, V, S: BuildHasher> BucketArray<K, V, S> {
    pub fn new(capacity: usize, hash_builder: Arc<S>) -> Self {
        let remaining_cells = AtomicUsize::new(capacity * 3 / 4);
        let shift = make_shift(capacity);
        let buckets = make_buckets(capacity);

        Self {
            remaining_cells,
            shift,
            hash_builder,
            buckets,
            next: Atomic::null(),
        }
    }

    fn get_next<'a>(&self, guard: &'a Guard) -> Option<&'a Self> {
        unsafe { self.next.load(Ordering::SeqCst, guard).as_ref() }
    }

    fn get_elem<'a, Q>(&'a self, guard: &'a Guard, key: &Q) -> Option<&'a Element<K, V>>
    where
        K: Borrow<Q>,
        Q: ?Sized + Eq + Hash,
    {
        let hash = do_hash(&*self.hash_builder, key);
        let mut idx = hash2idx(hash, self.shift);

        loop {
            let shared = unsafe { self.buckets[idx].load(Ordering::SeqCst, guard) };
            match shared.tag() {
                REDIRECT_TAG => {
                    return self.get_next(guard).unwrap().get_elem(guard, key);
                }

                TOMBSTONE_TAG => {
                    idx = hash2idx(idx as u64 + 1, self.shift);
                    continue;
                }

                _ => (),
            }
            if shared.is_null() {
                return None;
            }
            let elem = unsafe { shared.as_ref().unwrap() };
            if hash == elem.hash && key == elem.key.borrow() {
                return Some(elem);
            } else {
                idx = hash2idx(idx as u64 + 1, self.shift);
            }
        }
    }

    pub fn get<'a, Q>(&'a self, guard: &'a Guard, key: &Q) -> Option<ElementReadGuard<'a, K, V>>
    where
        K: Borrow<Q>,
        Q: ?Sized + Eq + Hash,
    {
        self.get_elem(guard, key).map(|e| e.read(pin()))
    }

    pub fn get_mut<'a, Q>(
        &'a self,
        guard: &'a Guard,
        key: &Q,
    ) -> Option<ElementWriteGuard<'a, K, V>>
    where
        K: Borrow<Q>,
        Q: ?Sized + Eq + Hash,
    {
        self.get_elem(guard, key).map(|e| e.write(pin()))
    }
}