//! Host-native test program for the nightly workflow fixture.

use std::{collections::BTreeMap, process::ExitCode};

const PAGE_MASK: u64 = 4095;

#[derive(Default)]
struct PageTable(BTreeMap<u64, u64>);

impl PageTable {
    fn map(&mut self, virtual_page: u64, physical_page: u64) -> bool {
        if (((virtual_page << 25) as i64 >> 25) as u64) != virtual_page
            || (virtual_page | physical_page) & PAGE_MASK != 0
            || physical_page >= 1 << 56
        {
            return false;
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = self.0.entry(virtual_page) {
            entry.insert(physical_page);
            true
        } else {
            false
        }
    }

    fn translate(&self, address: u64) -> Option<u64> {
        let base = self.0.get(&(address & !PAGE_MASK))?;
        Some(base + (address & PAGE_MASK))
    }
}

fn main() -> ExitCode {
    let mut table = PageTable::default();
    let expected = if cfg!(inject_page_offset_failure) {
        0x9005
    } else {
        0x8005
    };
    let checks = [
        ("map a page", table.map(0x4000, 0x8000)),
        (
            "preserve page offset",
            table.translate(0x4005) == Some(expected),
        ),
        ("reject duplicate mapping", !table.map(0x4000, 0x9000)),
        (
            "retain original mapping",
            table.translate(0x4000) == Some(0x8000),
        ),
        ("reject unmapped page", table.translate(0x5000).is_none()),
        ("reject unaligned address", !table.map(0x6000, 0x8010)),
        ("reject noncanonical address", !table.map(1 << 39, 0x8000)),
        (
            "allow high canonical half",
            table.map(0xffff_ffc0_0000_0000, 0x9000),
        ),
    ];
    let mut passed = true;
    for (check, result) in checks {
        println!("{} {check}", if result { "ok" } else { "FAIL" });
        passed &= result;
    }
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
