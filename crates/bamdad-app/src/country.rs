//! Zero-copy reader for the MehrNet country database (`country.mncc`).
//!
//! The format is documented in the bgp-api repository (`docs/country-database.md`):
//! little-endian integers, LEB128 varints, quantized IPv4/IPv6 slots.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;

const MAGIC: &[u8; 4] = b"MNCC";
const FORMAT: u8 = 1;
const MAX_V6_BITS: u8 = 62;

static BUNDLED: OnceLock<Option<CountryDb>> = OnceLock::new();

pub fn bundled() -> Option<&'static CountryDb> {
    BUNDLED
        .get_or_init(|| CountryDb::open(include_bytes!("../assets/country.mncc")))
        .as_ref()
}

pub fn lookup(ip: IpAddr) -> Option<&'static str> {
    bundled()?.lookup(ip)
}

pub struct CountryDb {
    v4_bits: u8,
    v6_bits: u8,
    countries: &'static [u8],
    v4: Family,
    v6: Family,
}

struct Family {
    bits: u8,
    entries: usize,
    index: &'static [u8],
    data: &'static [u8],
}

impl CountryDb {
    pub fn open(data: &'static [u8]) -> Option<Self> {
        if data.len() < 10 || &data[..4] != MAGIC || data[4] != FORMAT {
            return None;
        }
        let v4_bits = data[5];
        let v6_bits = data[6];
        let block_size = data[7];
        if !(1..=32).contains(&v4_bits) || !(1..=MAX_V6_BITS).contains(&v6_bits) || block_size == 0
        {
            return None;
        }
        let count = u16::from_le_bytes([data[8], data[9]]) as usize;
        let mut pos = 10;
        let countries = data.get(pos..pos + count * 2)?;
        pos += count * 2;

        let (v4, next) = Family::open(data, pos, v4_bits, block_size)?;
        let (v6, _) = Family::open(data, next, v6_bits, block_size)?;
        Some(Self {
            v4_bits,
            v6_bits,
            countries,
            v4,
            v6,
        })
    }

    pub fn country_count(&self) -> usize {
        self.countries.len() / 2
    }

    pub fn lookup(&self, ip: IpAddr) -> Option<&'static str> {
        match ip {
            IpAddr::V4(ip) => self.lookup_v4(ip),
            IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
                Some(ip) => self.lookup_v4(ip),
                None => self.lookup_v6(ip),
            },
        }
    }

    fn lookup_v4(&self, ip: Ipv4Addr) -> Option<&'static str> {
        let key = (u32::from_be_bytes(ip.octets()) as u64) >> (32 - self.v4_bits);
        self.country_name(self.v4.lookup(key)?)
    }

    fn lookup_v6(&self, ip: Ipv6Addr) -> Option<&'static str> {
        let octets = ip.octets();
        let high = u64::from_be_bytes(octets[..8].try_into().ok()?);
        let key = high >> (64 - self.v6_bits);
        self.country_name(self.v6.lookup(key)?)
    }

    fn country_name(&self, index: u16) -> Option<&'static str> {
        let index = index as usize;
        if index == 0 || index > self.country_count() {
            return None;
        }
        let start = (index - 1) * 2;
        std::str::from_utf8(&self.countries[start..start + 2]).ok()
    }
}

impl Family {
    fn open(data: &'static [u8], pos: usize, bits: u8, block_size: u8) -> Option<(Self, usize)> {
        let entries = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as usize;
        let data_len = u32::from_le_bytes(data.get(pos + 4..pos + 8)?.try_into().ok()?) as usize;
        let mut pos = pos + 8;
        if entries == 0 {
            return None;
        }
        let key_bytes = (bits as usize + 7) / 8;
        let blocks = entries.div_ceil(block_size as usize);
        let index_len = blocks.checked_mul(key_bytes + 4)?;
        let index = data.get(pos..pos.checked_add(index_len)?)?;
        pos += index_len;
        let body = data.get(pos..pos.checked_add(data_len)?)?;
        Some((
            Self {
                bits,
                entries,
                index,
                data: body,
            },
            pos + data_len,
        ))
    }

    fn key_at(&self, block: usize) -> Option<u64> {
        let key_bytes = (self.bits as usize + 7) / 8;
        let stride = key_bytes + 4;
        let start = block.checked_mul(stride)?;
        let bytes = self.index.get(start..start + key_bytes)?;
        let mut key = 0u64;
        for byte in bytes {
            key = (key << 8) | *byte as u64;
        }
        Some(key)
    }

    fn offset_at(&self, block: usize) -> Option<usize> {
        let key_bytes = (self.bits as usize + 7) / 8;
        let stride = key_bytes + 4;
        let start = block.checked_mul(stride)? + key_bytes;
        Some(u32::from_le_bytes(self.index.get(start..start + 4)?.try_into().ok()?) as usize)
    }

    fn lookup(&self, key: u64) -> Option<u16> {
        let blocks = self.index.len() / ((self.bits as usize + 7) / 8 + 4);
        if blocks == 0 {
            return None;
        }
        // Last block whose first slot is <= key.
        let mut low = 0usize;
        let mut high = blocks;
        while low < high {
            let mid = (low + high) / 2;
            if self.key_at(mid)? <= key {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let block = low.checked_sub(1)?;
        let mut slot = self.key_at(block)?;
        let start = self.offset_at(block)?;
        let end = if block + 1 < blocks {
            self.offset_at(block + 1)?
        } else {
            self.data.len()
        };
        if start > end || end > self.data.len() {
            return None;
        }

        let mut pos = start;
        let mut country = 0u16;
        while pos < end {
            let (delta, used) = uvarint(self.data.get(pos..end)?)?;
            pos = pos.checked_add(used)?;
            let (index, used) = uvarint(self.data.get(pos..end)?)?;
            pos = pos.checked_add(used)?;
            slot = slot.checked_add(delta)?;
            if slot > key {
                break;
            }
            country = index as u16;
        }
        Some(country)
    }
}

fn uvarint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    for (index, byte) in bytes.iter().enumerate() {
        let part = (byte & 0x7f) as u64;
        value |= part.checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
        shift = shift.checked_add(7)?;
        if shift >= 64 {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_database_parses() {
        let db = bundled().expect("bundled country database");
        assert!(db.country_count() > 200);
        assert!(db.v4.entries > 100_000);
        assert!(db.v6.entries > 100_000);
    }

    #[test]
    fn looks_up_a_known_country() {
        let db = bundled().expect("bundled country database");
        assert_eq!(db.lookup("185.51.200.1".parse().unwrap()), Some("IR"));
        assert_eq!(
            db.lookup("::ffff:185.51.200.1".parse().unwrap()),
            Some("IR")
        );
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(CountryDb::open(b"nope").is_none());
    }
}
