//! The locations weakly referring to each object, as the weak table in
//! `arc` keeps them (under its shard's lock): usually one, which needs no
//! allocation; a few, in a list that is searched; or many, in an
//! open-addressed table in the same vector, so storing or destroying one
//! costs the same however many there are. A delegate, an observed object
//! or a cache's owner can have thousands, each destroyed on its own.
//!
//! A list holds at most `FEW` locations, and a table has more than `FEW`
//! slots, so a vector's length says which it is: telling them apart costs
//! the few-locations path one comparison of a length it reads anyway.
//! Locations are aligned pointers, never zero, which marks an empty slot.

use std::collections::hash_map::Entry;

use crate::util::AddrMap;

/// The locations weakly referring to one object.
pub(crate) enum Locations {
    One(usize),
    /// A list of up to `FEW` locations, or a table (`Table`).
    Many(Vec<usize>),
}

/// The most locations a list holds: searching this many costs about what
/// finding one in a table does. A table shrinking to `FEW / 4` becomes a
/// list again, so an object whose count hovers near the limit doesn't
/// convert on every change.
const FEW: usize = 16;

/// A table's fewest slots: room for the list it replaces and as many again
/// at most half full.
const MIN_SLOTS: usize = 4 * FEW;

impl Locations {
    /// Every location, in no particular order.
    #[inline]
    pub(crate) fn for_each(self, mut f: impl FnMut(usize)) {
        match self {
            Locations::One(location) => f(location),
            Locations::Many(list) if list.len() <= FEW => list.into_iter().for_each(f),
            Locations::Many(table) => Table::slots(&table).iter().filter(|&&l| l != 0).for_each(|&l| f(l)),
        }
    }
}

/// Adds `location`, which isn't among them, to `obj`'s locations.
#[inline]
pub(crate) fn register(table: &mut AddrMap<Locations>, obj: usize, location: usize) {
    match table.entry(obj) {
        Entry::Vacant(entry) => {
            entry.insert(Locations::One(location));
        }
        Entry::Occupied(mut entry) => match entry.get_mut() {
            Locations::One(first) => {
                let first = *first;
                entry.insert(Locations::Many(vec![first, location]));
            }
            Locations::Many(list) if list.len() < FEW => list.push(location),
            Locations::Many(many) => Table::insert(many, location),
        },
    }
}

/// Removes `location` from `obj`'s locations, if it is there.
#[inline]
pub(crate) fn unregister(table: &mut AddrMap<Locations>, obj: usize, location: usize) {
    let Entry::Occupied(mut entry) = table.entry(obj) else { return };
    let emptied = match entry.get_mut() {
        Locations::One(only) => *only == location,
        Locations::Many(list) if list.len() <= FEW => {
            if let Some(i) = list.iter().position(|&l| l == location) {
                list.swap_remove(i);
            }
            list.is_empty()
        }
        Locations::Many(table) => {
            Table::remove(table, location);
            false
        }
    };
    if emptied {
        entry.remove();
    }
}

/// A table in a vector: the number of locations it holds, then a power of
/// two (at least `MIN_SLOTS`) of slots, each a location or zero, probed
/// linearly from a location's hash. At most half the slots are used.
struct Table;

impl Table {
    fn slots(table: &[usize]) -> &[usize] {
        &table[1..]
    }

    /// Where probing for `location` starts among `slots` slots: the top
    /// bits of the location fully mixed (murmur3's 64-bit finalizer), so
    /// each bit of it moves every bit of the hash. One multiply (Fibonacci
    /// hashing) isn't enough: it clusters locations a fixed distance
    /// apart, as weak ivars of objects of one size allocated in turn are,
    /// for some sizes into runs hundreds of slots long.
    fn home(location: usize, slots: usize) -> usize {
        let mut h = location as u64;
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
        h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        h ^= h >> 33;
        (h >> (u64::BITS - slots.trailing_zeros())) as usize
    }

    /// A table of `slots` slots holding `locations`.
    fn build(locations: impl IntoIterator<Item = usize>, slots: usize) -> Vec<usize> {
        let mut table = vec![0; 1 + slots];
        for location in locations {
            Table::put(&mut table, location);
        }
        table
    }

    /// Adds `location` to a table with room for it.
    fn put(table: &mut [usize], location: usize) {
        let mask = table.len() - 2;
        let mut i = Table::home(location, mask + 1);
        loop {
            match table[1 + i] {
                0 => break,
                l if l == location => return,
                _ => i = (i + 1) & mask,
            }
        }
        table[1 + i] = location;
        table[0] += 1;
    }

    /// Adds `location` to a full list, making it a table, or to a table,
    /// doubling it first if it would be more than half full. (Cold, like
    /// `remove`, to keep it out of the few locations' path; for a table,
    /// the call costs little next to the probing.)
    #[cold]
    fn insert(many: &mut Vec<usize>, location: usize) {
        if many.len() <= FEW {
            *many = Table::build(many.drain(..), MIN_SLOTS);
        } else if 2 * (many[0] + 1) > many.len() - 1 {
            let slots = 2 * (many.len() - 1);
            *many = Table::build(Table::slots(many).iter().copied().filter(|&l| l != 0), slots);
        }
        Table::put(many, location);
    }

    /// Removes `location`, if it is there, moving the locations probed past
    /// its slot back so that none is left behind an empty one. A table left
    /// with a few becomes a list; one left mostly empty is halved.
    #[cold]
    fn remove(table: &mut Vec<usize>, location: usize) {
        let mask = table.len() - 2;
        let mut hole = Table::home(location, mask + 1);
        loop {
            match table[1 + hole] {
                0 => return,
                l if l == location => break,
                _ => hole = (hole + 1) & mask,
            }
        }
        // Each location after the hole, up to the next empty slot, moves
        // into it if the hole lies between its home and its slot.
        let mut i = hole;
        loop {
            i = (i + 1) & mask;
            let l = table[1 + i];
            if l == 0 {
                break;
            }
            let home = Table::home(l, mask + 1);
            if (i.wrapping_sub(home) & mask) >= (i.wrapping_sub(hole) & mask) {
                table[1 + hole] = l;
                hole = i;
            }
        }
        table[1 + hole] = 0;
        table[0] -= 1;
        let (count, slots) = (table[0], mask + 1);
        if count <= FEW / 4 {
            *table = Table::slots(table).iter().copied().filter(|&l| l != 0).collect();
        } else if slots > MIN_SLOTS && 8 * count < slots {
            // Give back what a crowd of locations, now gone, needed:
            // halving costs as much as the removals since the table last
            // grew or shrank.
            *table = Table::build(Table::slots(table).iter().copied().filter(|&l| l != 0), slots / 2);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{FEW, Locations, MIN_SLOTS, Table, register, unregister};
    use crate::util::{AddrHash, AddrMap};

    fn held(table: &AddrMap<Locations>, obj: usize) -> BTreeSet<usize> {
        let mut all = BTreeSet::new();
        if let Some(Locations::Many(many)) = table.get(&obj)
            && many.len() > FEW
        {
            assert_eq!(many[0], Table::slots(many).iter().filter(|&&l| l != 0).count(), "the table's count");
        }
        match table.get(&obj) {
            None => {}
            Some(Locations::One(location)) => {
                all.insert(*location);
            }
            Some(Locations::Many(many)) => {
                let copy = Locations::Many(many.clone());
                copy.for_each(|l| assert!(all.insert(l), "{l:#x} held twice"));
            }
        }
        all
    }

    fn form(table: &AddrMap<Locations>, obj: usize) -> &'static str {
        match table.get(&obj) {
            None => "none",
            Some(Locations::One(_)) => "one",
            Some(Locations::Many(list)) if list.len() <= FEW => "list",
            Some(Locations::Many(_)) => "table",
        }
    }

    /// Locations as `objc2`'s boxed weak references have them: 16 bytes
    /// apart.
    fn at(i: usize) -> usize {
        0x7f00_0000_0000 + 16 * i
    }

    /// An object's locations as a list while few, a table past `FEW`, a
    /// list again at `FEW / 4`, and gone when none is left; always exactly
    /// the ones registered.
    #[test]
    fn locations_switch_between_list_and_table() {
        let mut table = AddrMap::with_hasher(AddrHash);
        let obj = 0x1000;
        let mut model = BTreeSet::new();
        for i in 0..=FEW {
            register(&mut table, obj, at(i));
            model.insert(at(i));
            assert_eq!(form(&table, obj), [if i == 0 { "one" } else { "list" }, "table"][usize::from(i == FEW)]);
            assert_eq!(held(&table, obj), model);
        }
        for i in FEW + 1..1000 {
            register(&mut table, obj, at(i));
            model.insert(at(i));
        }
        assert_eq!(held(&table, obj), model);
        // Removed in a scattered order down to a few: a list again.
        let mut order: Vec<usize> = (0..1000).map(|i| (i * 617) % 1000).collect();
        let last = order.split_off(1000 - FEW / 4);
        for (n, i) in order.into_iter().enumerate() {
            assert_eq!(form(&table, obj), "table");
            unregister(&mut table, obj, at(i));
            model.remove(&at(i));
            if n % 50 == 0 {
                assert_eq!(held(&table, obj), model);
            }
        }
        assert_eq!(form(&table, obj), "list");
        assert_eq!(held(&table, obj), model);
        // Unknown locations change nothing.
        unregister(&mut table, obj, at(5000));
        assert_eq!(held(&table, obj), model);
        for i in last {
            unregister(&mut table, obj, at(i));
            model.remove(&at(i));
            assert_eq!(held(&table, obj), model);
        }
        assert_eq!(form(&table, obj), "none");
    }

    /// Random registrations and removals, some of locations that aren't
    /// there, against a model, with the count crossing the list's limit
    /// both ways again and again and probe sequences wrapping around.
    #[test]
    fn random_changes_match_a_model() {
        let mut table = AddrMap::with_hasher(AddrHash);
        let obj = 0x2000;
        let mut model = BTreeSet::new();
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        for step in 0..200_000 {
            // Phases that favor adding, then removing, so the count sweeps
            // from a few to a few hundred and back.
            let adding = (step / 5000) % 2 == 0;
            let location = at(next(400));
            // As objc_storeWeak does, a location is registered only when
            // it isn't: it first leaves the object it held.
            if next(100) < if adding { 70 } else { 1 } && !model.contains(&location) {
                register(&mut table, obj, location);
                model.insert(location);
            } else {
                unregister(&mut table, obj, location);
                model.remove(&location);
            }
            if step % 97 == 0 {
                assert_eq!(held(&table, obj), model, "step {step}");
            }
        }
        assert_eq!(held(&table, obj), model);
    }

    /// A table grows as locations arrive and shrinks as they go, rather
    /// than keeping what its peak needed.
    #[test]
    fn a_table_grows_and_shrinks() {
        let mut table = AddrMap::with_hasher(AddrHash);
        let obj = 0x3000;
        for i in 0..100_000 {
            register(&mut table, obj, at(i));
        }
        let slots = |table: &AddrMap<Locations>| match table.get(&obj) {
            Some(Locations::Many(many)) => many.len() - 1,
            _ => 0,
        };
        assert_eq!(slots(&table), 262_144);
        for i in 100..100_000 {
            unregister(&mut table, obj, at(i));
        }
        assert_eq!(form(&table, obj), "table");
        assert!((MIN_SLOTS..=2048).contains(&slots(&table)), "{} slots", slots(&table));
        assert_eq!(held(&table, obj), (0..100).map(at).collect());
    }

    /// Locations a fixed distance apart, as the weak ivars of same-sized
    /// objects allocated one after another are, spread over the table:
    /// finding any of them takes a few probes, whatever the distance.
    /// (With the top bits of one multiply by the golden ratio, 608 bytes
    /// apart averaged 10 probes, 5168 bytes 81, and 7896 bytes left one
    /// location 999 slots from its home.)
    #[test]
    fn strided_locations_need_few_probes() {
        for stride in [16, 48, 608, 712, 904, 4096, 5168, 7896] {
            for n in [100, 1000, 5000] {
                let mut table = AddrMap::with_hasher(AddrHash);
                let obj = 0x4000;
                for i in 0..n {
                    register(&mut table, obj, 0x7f00_0000_1000 + stride * i);
                }
                let Some(Locations::Many(many)) = table.get(&obj) else { panic!("no table") };
                let slots = Table::slots(many);
                let mask = slots.len() - 1;
                // A location's probes: its slot's distance from its home,
                // plus the slot itself.
                let probes: Vec<usize> = (0..slots.len())
                    .filter(|&i| slots[i] != 0)
                    .map(|i| (i.wrapping_sub(Table::home(slots[i], slots.len())) & mask) + 1)
                    .collect();
                assert_eq!(probes.len(), n);
                let average = probes.iter().sum::<usize>() as f64 / n as f64;
                let most = probes.iter().copied().max().unwrap_or(0);
                assert!(
                    average < 3.0 && most <= 64,
                    "{stride} bytes apart, {n} locations: {average:.1} probes on average, {most} at most"
                );
            }
        }
    }
}
