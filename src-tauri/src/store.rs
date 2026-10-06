//! The set of known lights and the rules for folding network results back into it.
//!
//! Lights are keyed by serial number once they have reported one, and by IP address until
//! then, so a light that DHCP moves to a new address stays a single entry. Network work
//! happens on a copy without the lock held; these functions merge the outcome so that a
//! light forgotten meanwhile stays forgotten and a newer write is never overwritten by an
//! older read.

use crate::lights::Light;
use std::collections::BTreeMap;

pub type Lights = BTreeMap<String, Light>;

fn key_of(l: &Light) -> String {
    if l.serial.is_empty() {
        l.ip.clone()
    } else {
        l.serial.clone()
    }
}

fn put(map: &mut Lights, mut l: Light) {
    l.id = key_of(&l);
    map.insert(l.id.clone(), l);
}

/// Adds a light read from the saved file; nothing is known to be reachable yet.
pub fn load(map: &mut Lights, mut l: Light) {
    l.reachable = false;
    put(map, l);
}

/// Merges a re-probe of a known light; `probed.id` is its key from when the probe began.
/// `since` is the write counter at that moment.
pub fn merge_known(map: &mut Lights, probed: Light, since: u64) {
    let Some(cur) = map.get(&probed.id) else {
        return; // forgotten while the probe ran
    };
    if cur.rev > since {
        return; // written meanwhile; that answer is newer than this one
    }
    if key_of(&probed) == probed.id {
        map.insert(probed.id.clone(), probed);
    } else {
        // It has just reported its serial: re-key, joining any entry that already has it.
        map.remove(&probed.id);
        upsert(map, probed, since);
    }
}

/// Records a light found at `l.ip`, by discovery or by the user typing its address.
pub fn upsert(map: &mut Lights, mut l: Light, since: u64) {
    if l.serial.is_empty() {
        // Unknown identity: treat it as whatever is already known at this address.
        match map.values_mut().find(|o| o.ip == l.ip) {
            Some(cur) => absorb(cur, l, since),
            None => put(map, l),
        }
        return;
    }
    // An entry keyed by this address was this light before its serial was known.
    if map.get(&l.ip).is_some_and(|p| p.serial.is_empty()) {
        if let Some(p) = map.remove(&l.ip) {
            l.manual |= p.manual;
        }
    }
    // Only one light answers at an address; any other entry still claiming it is stale.
    for o in map.values_mut() {
        if o.ip == l.ip && o.serial != l.serial {
            o.reachable = false;
        }
    }
    match map.get_mut(&l.serial) {
        Some(cur) => absorb(cur, l, since),
        None => put(map, l),
    }
}

/// Copies what a probe learned onto an existing entry of the same light.
fn absorb(cur: &mut Light, l: Light, since: u64) {
    if !l.reachable && cur.ip != l.ip {
        return; // silence at another address says nothing about this light
    }
    if cur.rev <= since {
        cur.reachable = l.reachable;
        cur.on = l.on;
        cur.brightness = l.brightness;
        cur.temperature = l.temperature;
    }
    cur.ip = l.ip;
    cur.port = l.port;
    if !l.name.trim().is_empty() {
        cur.name = l.name;
    }
    if !l.product.trim().is_empty() {
        cur.product = l.product;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(ip: &str, serial: &str, name: &str) -> Light {
        let mut l = Light::new(ip.into(), 9123, name.into(), false);
        l.serial = serial.into();
        l.reachable = true;
        l
    }

    fn with(entries: Vec<Light>) -> Lights {
        let mut map = Lights::new();
        for l in entries {
            put(&mut map, l);
        }
        map
    }

    #[test]
    fn load_keys_by_serial_or_ip_and_marks_offline() {
        let mut map = Lights::new();
        load(&mut map, light("10.0.0.5", "SER1", "A"));
        load(&mut map, light("10.0.0.6", "", "B"));
        assert_eq!(map.keys().collect::<Vec<_>>(), ["10.0.0.6", "SER1"]);
        assert!(map.values().all(|l| !l.reachable));
        assert_eq!(map["SER1"].id, "SER1");
    }

    #[test]
    fn merge_known_updates_state() {
        let mut map = with(vec![light("10.0.0.5", "SER1", "A")]);
        let mut p = map["SER1"].clone();
        p.on = true;
        p.brightness = 80;
        merge_known(&mut map, p, 0);
        assert!(map["SER1"].on);
        assert_eq!(map["SER1"].brightness, 80);
    }

    #[test]
    fn merge_known_does_not_resurrect_forgotten_light() {
        let mut map = with(vec![light("10.0.0.5", "SER1", "A")]);
        let p = map["SER1"].clone();
        map.remove("SER1");
        merge_known(&mut map, p, 0);
        assert!(map.is_empty());
    }

    #[test]
    fn merge_known_keeps_newer_write() {
        let mut map = with(vec![light("10.0.0.5", "SER1", "A")]);
        let mut p = map["SER1"].clone();
        p.brightness = 10; // stale read
        let cur = map.get_mut("SER1").unwrap();
        cur.brightness = 90; // write that landed after the probe began
        cur.rev = 5;
        merge_known(&mut map, p, 4);
        assert_eq!(map["SER1"].brightness, 90);
    }

    #[test]
    fn merge_known_rekeys_when_serial_learned() {
        let mut map = with(vec![light("10.0.0.5", "", "A")]);
        let mut p = map["10.0.0.5"].clone();
        p.serial = "SER1".into();
        merge_known(&mut map, p, 0);
        assert_eq!(map.keys().collect::<Vec<_>>(), ["SER1"]);
        assert_eq!(map["SER1"].id, "SER1");
    }

    #[test]
    fn merge_known_rekey_joins_existing_entry_at_old_ip() {
        // Saved light SER1 at its old address, plus a placeholder at its new one.
        let mut old = light("10.0.0.5", "SER1", "Desk");
        old.reachable = false;
        let mut map = with(vec![old, light("10.0.0.9", "", "")]);
        let mut p = map["10.0.0.9"].clone();
        p.serial = "SER1".into();
        p.on = true;
        merge_known(&mut map, p, 0);
        assert_eq!(map.len(), 1);
        let l = &map["SER1"];
        assert_eq!(l.ip, "10.0.0.9");
        assert_eq!(l.name, "Desk");
        assert!(l.reachable && l.on);
    }

    #[test]
    fn upsert_moves_known_light_to_new_ip() {
        let mut old = light("10.0.0.5", "SER1", "Desk");
        old.reachable = false;
        old.manual = true;
        let mut map = with(vec![old]);
        upsert(&mut map, light("10.0.0.9", "SER1", ""), 0);
        assert_eq!(map.len(), 1);
        let l = &map["SER1"];
        assert_eq!(l.ip, "10.0.0.9");
        assert_eq!(l.name, "Desk");
        assert!(l.reachable && l.manual);
    }

    #[test]
    fn upsert_new_light_at_taken_ip_marks_old_entry_stale() {
        let mut map = with(vec![light("10.0.0.5", "SER1", "A")]);
        upsert(&mut map, light("10.0.0.5", "SER2", "B"), 0);
        assert_eq!(map.len(), 2);
        assert!(!map["SER1"].reachable);
        assert!(map["SER2"].reachable);
    }

    #[test]
    fn upsert_replaces_ip_placeholder_and_keeps_manual() {
        let mut ph = light("10.0.0.5", "", "");
        ph.manual = true;
        let mut map = with(vec![ph]);
        upsert(&mut map, light("10.0.0.5", "SER1", "A"), 0);
        assert_eq!(map.keys().collect::<Vec<_>>(), ["SER1"]);
        assert!(map["SER1"].manual);
    }

    #[test]
    fn upsert_without_serial_folds_into_entry_at_same_ip() {
        let mut map = with(vec![light("10.0.0.5", "SER1", "A")]);
        let mut l = light("10.0.0.5", "", "");
        l.reachable = false;
        upsert(&mut map, l, 0);
        assert_eq!(map.len(), 1);
        assert!(!map["SER1"].reachable);
        assert_eq!(map["SER1"].name, "A");
    }

    #[test]
    fn upsert_respects_newer_write() {
        let mut cur = light("10.0.0.5", "SER1", "A");
        cur.brightness = 90;
        cur.rev = 7;
        let mut map = with(vec![cur]);
        let mut l = light("10.0.0.5", "SER1", "A");
        l.brightness = 10;
        upsert(&mut map, l, 6);
        assert_eq!(map["SER1"].brightness, 90);
    }

    #[test]
    fn unreachable_probe_elsewhere_leaves_entry_alone() {
        let mut map = with(vec![light("10.0.0.5", "SER1", "A")]);
        let mut l = light("10.0.0.9", "SER1", "");
        l.reachable = false;
        upsert(&mut map, l, 0);
        assert_eq!(map["SER1"].ip, "10.0.0.5");
        assert!(map["SER1"].reachable);
    }
}
