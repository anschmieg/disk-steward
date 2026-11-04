//! Bounded allocation visualization metadata for one observed inventory page.

use super::{FolderInventoryEntry, NameCategory};
use disk_steward_core::InventoryItemId;
use serde::Serialize;

const MAX_PAGE_ENTRIES: usize = super::MAX_RAW_ENTRIES;
const MAX_TILES: usize = 64;
const MAX_INDIVIDUAL_TILES: usize = MAX_TILES - 1;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AllocationMap {
    pub known_allocated_bytes: String,
    pub tiles: Vec<AllocationTile>,
    pub unknown_allocation_entries: u32,
    pub shared_link_entries: u32,
    pub zero_allocation_entries: u32,
    pub unmapped_non_file_entries: u32,
    pub dataless_entries: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AllocationTile {
    pub entry_ids: Vec<InventoryItemId>,
    pub allocated_bytes: String,
    pub category: Option<NameCategory>,
}

struct PositiveAllocation {
    entry_id: InventoryItemId,
    bytes: u64,
    category: NameCategory,
}

fn parse_decimal_u64(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<u64>().ok())
        .flatten()
}

pub(super) fn aggregate(entries: &[FolderInventoryEntry]) -> Result<AllocationMap, &'static str> {
    if entries.len() > MAX_PAGE_ENTRIES {
        return Err("Allocation map exceeded the current page entry limit.");
    }

    let mut known_allocated_bytes = 0u64;
    let mut positive = Vec::new();
    let mut unknown_allocation_entries = 0u32;
    let mut shared_link_entries = 0u32;
    let mut zero_allocation_entries = 0u32;
    let mut unmapped_non_file_entries = 0u32;
    let mut dataless_entries = 0u32;

    for entry in entries {
        if entry.availability == "dataless" {
            dataless_entries += 1;
            continue;
        }
        if entry.kind != "file" {
            unmapped_non_file_entries += 1;
            continue;
        }
        let link_count = entry.link_count.as_deref().and_then(parse_decimal_u64);
        if entry.hard_linked || link_count.is_some_and(|count| count > 1) {
            shared_link_entries += 1;
            continue;
        }
        if link_count != Some(1) {
            unknown_allocation_entries += 1;
            continue;
        }
        let Some(bytes) = entry.allocated_bytes.as_deref().and_then(parse_decimal_u64) else {
            unknown_allocation_entries += 1;
            continue;
        };
        known_allocated_bytes = known_allocated_bytes
            .checked_add(bytes)
            .ok_or("Allocated byte totals exceeded the supported range.")?;
        if bytes == 0 {
            zero_allocation_entries += 1;
        } else {
            positive.push(PositiveAllocation {
                entry_id: entry.id,
                bytes,
                category: entry.name_category,
            });
        }
    }

    positive.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.entry_id.cmp(&right.entry_id))
    });
    let mut tiles = Vec::with_capacity(positive.len().min(MAX_TILES));
    if positive.len() <= MAX_TILES {
        tiles.extend(positive.into_iter().map(|item| AllocationTile {
            entry_ids: vec![item.entry_id],
            allocated_bytes: item.bytes.to_string(),
            category: Some(item.category),
        }));
    } else {
        let remainder = positive.split_off(MAX_INDIVIDUAL_TILES);
        tiles.extend(positive.into_iter().map(|item| AllocationTile {
            entry_ids: vec![item.entry_id],
            allocated_bytes: item.bytes.to_string(),
            category: Some(item.category),
        }));
        let aggregate_bytes = remainder.iter().try_fold(0u64, |total, item| {
            total
                .checked_add(item.bytes)
                .ok_or("Allocated byte totals exceeded the supported range.")
        })?;
        tiles.push(AllocationTile {
            entry_ids: remainder.iter().map(|item| item.entry_id).collect(),
            allocated_bytes: aggregate_bytes.to_string(),
            category: None,
        });
    }

    Ok(AllocationMap {
        known_allocated_bytes: known_allocated_bytes.to_string(),
        tiles,
        unknown_allocation_entries,
        shared_link_entries,
        zero_allocation_entries,
        unmapped_non_file_entries,
        dataless_entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        kind: &'static str,
        allocated_bytes: Option<&str>,
        link_count: Option<&str>,
        hard_linked: bool,
        availability: &'static str,
    ) -> FolderInventoryEntry {
        FolderInventoryEntry {
            id: InventoryItemId::new(),
            name: "metadata-fixture".to_owned(),
            kind,
            name_category: NameCategory::OtherFile,
            logical_bytes: None,
            allocated_bytes: allocated_bytes.map(str::to_owned),
            link_count: link_count.map(str::to_owned),
            availability,
            hard_linked,
            can_open: false,
            navigation_blocker: None,
        }
    }

    #[test]
    fn allocation_counts_are_exclusive_and_only_eligible_files_have_area() {
        let entries = vec![
            entry("file", Some("12"), Some("1"), false, "unknown"),
            entry("file", Some("0"), Some("1"), false, "unknown"),
            entry("file", Some("9"), Some("2"), true, "unknown"),
            entry("file", Some("3"), Some("2"), false, "unknown"),
            entry("file", Some("7"), None, false, "unknown"),
            entry("file", None, Some("1"), false, "unknown"),
            entry("file", Some("-1"), Some("1"), false, "unknown"),
            entry(
                "file",
                Some("18446744073709551616"),
                Some("1"),
                false,
                "unknown",
            ),
            entry("file", Some("8"), Some("1"), false, "dataless"),
            entry("folder", None, None, false, "unknown"),
            entry("symlink", None, None, false, "unknown"),
        ];
        let map = aggregate(&entries).unwrap();
        assert_eq!(map.known_allocated_bytes, "12");
        assert_eq!(map.unknown_allocation_entries, 4);
        assert_eq!(map.shared_link_entries, 2);
        assert_eq!(map.zero_allocation_entries, 1);
        assert_eq!(map.unmapped_non_file_entries, 2);
        assert_eq!(map.dataless_entries, 1);
        assert_eq!(map.tiles.len(), 1);
        assert_eq!(map.tiles[0].entry_ids, vec![entries[0].id]);
        assert_eq!(map.tiles[0].allocated_bytes, "12");
        assert_eq!(map.tiles[0].category, Some(NameCategory::OtherFile));
    }

    #[test]
    fn allocation_sum_overflow_fails_closed() {
        let max_allocated = u64::MAX.to_string();
        let entries = vec![
            entry("file", Some(&max_allocated), Some("1"), false, "unknown"),
            entry("file", Some("1"), Some("1"), false, "unknown"),
        ];
        assert_eq!(
            aggregate(&entries).unwrap_err(),
            "Allocated byte totals exceeded the supported range."
        );
    }

    #[test]
    fn capped_tiles_preserve_exact_area_and_each_entry_id_once() {
        let entries = (1..=MAX_PAGE_ENTRIES)
            .map(|bytes| {
                let bytes = bytes.to_string();
                entry("file", Some(&bytes), Some("1"), false, "unknown")
            })
            .collect::<Vec<_>>();
        let map = aggregate(&entries).unwrap();
        let expected_total = (MAX_PAGE_ENTRIES as u64 * (MAX_PAGE_ENTRIES as u64 + 1)) / 2;
        assert_eq!(map.known_allocated_bytes, expected_total.to_string());
        assert_eq!(map.tiles.len(), MAX_TILES);
        assert_eq!(map.tiles[0].allocated_bytes, "512");
        assert_eq!(map.tiles[MAX_INDIVIDUAL_TILES - 1].allocated_bytes, "450");
        let aggregate_tile = map.tiles.last().unwrap();
        assert_eq!(aggregate_tile.category, None);
        assert_eq!(
            aggregate_tile.entry_ids.len(),
            MAX_PAGE_ENTRIES - MAX_INDIVIDUAL_TILES
        );
        assert_eq!(aggregate_tile.allocated_bytes, "101025");
        let mut tiled_ids = map
            .tiles
            .iter()
            .flat_map(|tile| tile.entry_ids.iter().copied())
            .collect::<Vec<_>>();
        tiled_ids.sort();
        tiled_ids.dedup();
        let mut expected_ids = entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
        expected_ids.sort();
        assert_eq!(tiled_ids, expected_ids);
    }

    #[test]
    fn tie_order_is_stable_by_entry_id() {
        let mut entries = vec![
            entry("file", Some("4"), Some("1"), false, "unknown"),
            entry("file", Some("4"), Some("1"), false, "unknown"),
            entry("file", Some("4"), Some("1"), false, "unknown"),
        ];
        entries.sort_by_key(|entry| entry.id);
        let map = aggregate(&entries).unwrap();
        assert_eq!(
            map.tiles
                .iter()
                .map(|tile| tile.entry_ids[0])
                .collect::<Vec<_>>(),
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>()
        );
    }
}
