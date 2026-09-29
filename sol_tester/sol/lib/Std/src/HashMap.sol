pub struct HashMap<K: Hash, V> {
    MAX_LOAD_FACTOR :: 0.75_f64
    MIN_CAP :: 8_uint
    struct Entry {
        key: K
        value: V
        hash: u64
    }

    mut entries: []?Entry
    mut len: uint = 0
    mut cap: uint = MIN_CAP

    pub This.() => withCapacity(MIN_CAP)

    pub withCapacity(capacity: uint) {
        cap := capacity.max(MIN_CAP)
        entries := new[?Entry: for cap => null]
        This{entries, cap, ..}
    }

    pub insert(&mut this, key: K, value: V): ?V {
        if this.loadFactor() > MAX_LOAD_FACTOR {
            this.grow()
        }

        hash := key.hash()
        mut index := this.index(hash)
        index = this.findEntry(index).null{
            this.entries[index] = Entry{key, value, hash}
            this.len += 1
            return null
        }

        Std.Mem.replace(
            &mut this.entries[index],
            Entry{key, value, hash}
        )
    }

    pub get(&this, key: &K): ?&V {
        hash := key.hash()
        mut index := this.index(hash)
        index = this.findEntry(index).pass
        &this.entries[index]
    }

    pub getMut(&mut this, key: &K): ?&mut V {
        hash := key.hash()
        mut index := this.index(hash)
        index = this.findEntry(index).pass
        &mut this.entries[index]
    }

    pub remove(&mut this, key: &K): ?V {
        hash := key.hash()
        mut index := this.index(hash)
        index = this.findEntry(index).pass
        entry := this.entries[index].take().null{panic}
        this.len -= 1

        this.reinsertFrom(index)
        entry.value
    }

    reinsertFrom(&mut this, start: uint) {
        mut index = (start + 1) % capacity
        for type !null(entry) = this.entries[index].take() {
            this.len -= 1
            this.insert(entry.k, entry.v)
            index = (index + 1) % this.capacity
        }
    }

    findEntry(&this, mut index: uint): ?uint {
        for {
            entry := this.entries[index].pass

            if entry.hash == hash && entry.key == entry.key {
                return index
            }

            index := (index + 1) % this.cap
        }
    }

    grow(&mut this) {
        this.cap *= 2
        newEntries := new[?Entry: for this.cap => null]
        oldEntries := Std.Mem.replace(&mut this.entries, newEntries)
        this.len = 0

        for el in oldEntries {
            entry := el.null{continue}
            hash := entry.hash
            mut index := uint.(hash) % this.cap
            for {
                if this.entries[index] == null {
                    this.entries[index] = entry
                    this.len += 1
                    break
                }

                index = (index + 1) % this.cap
            }
        }
    }

    loadFactor(&this): f64 => f64.(this.len) / f64.(this.cap)
    index(&this, hash: u64): uint => uint.(hash) % this.cap
}
