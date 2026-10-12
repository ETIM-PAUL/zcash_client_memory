use std::ops::{Deref, DerefMut, Range};

use zcash_client_backend::data_api::scanning::{
    ScanPriority, ScanRange, spanning_tree::SpanningTree,
};
use zcash_protocol::consensus::BlockHeight;

use crate::error::Error;

/// A queue of scanning ranges. Contains the start and end heights of each range, along with the
/// priority of scanning that range.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScanQueue(pub(crate) Vec<(BlockHeight, BlockHeight, ScanPriority)>);

impl ScanQueue {
    pub(crate) fn new() -> Self {
        ScanQueue(Vec::new())
    }

    pub(crate) fn suggest_scan_ranges(&self, min_priority: ScanPriority) -> Vec<ScanRange> {
        let mut priorities: Vec<_> = self
            .0
            .iter()
            .filter(|(_, _, p)| *p >= min_priority)
            .collect();
        priorities.sort_by(|(_, _, a), (_, _, b)| b.cmp(a));

        priorities
            .into_iter()
            .map(|(start, end, priority)| {
                let range = Range {
                    start: *start,
                    end: *end,
                };
                ScanRange::from_parts(range, *priority)
            })
            .collect()
    }
    fn insert_queue_entries<'a>(
        &mut self,
        entries: impl Iterator<Item = &'a ScanRange>,
    ) -> Result<(), Error> {
        for entry in entries {
            if entry.block_range().start >= entry.block_range().end {
                return Err(Error::InvalidScanRange(
                    entry.block_range().start,
                    entry.block_range().end,
                    "start must be less than end".to_string(),
                ));
            }

            for (start, end, _) in &self.0 {
                if *start == entry.block_range().start || *end == entry.block_range().end {
                    return Err(Error::InvalidScanRange(
                        entry.block_range().start,
                        entry.block_range().end,
                        "at least part of range is already covered by another range".to_string(),
                    ));
                }
            }

            self.0.push((
                entry.block_range().start,
                entry.block_range().end,
                entry.priority(),
            ));
        }
        Ok(())
    }
    pub(crate) fn replace_queue_entries(
        &mut self,
        query_range: &Range<BlockHeight>,
        entries: impl Iterator<Item = ScanRange>,
        force_rescans: bool,
    ) -> Result<(), Error> {
        let (to_create, to_delete_ends) = {
            let mut q_ranges: Vec<_> = self
                .0
                .iter()
                .filter(|(start, end, _)| {
                    //  Ignore ranges that do not overlap and are not adjacent to the query range.
                    !(start > &query_range.end || &query_range.start > end)
                })
                .collect();
            q_ranges.sort_by_key(|(_, end_a, _)| *end_a);

            // Iterate over the ranges in the scan queue that overlap the range that we have
            // identified as needing to be fully scanned. For each such range add it to the
            // spanning tree (these should all be nonoverlapping ranges, but we might coalesce
            // some in the process).
            let mut to_create: Option<SpanningTree> = None;
            let mut to_delete_ends: Vec<BlockHeight> = vec![];

            let q_ranges = q_ranges.into_iter();
            for (start, end, priority) in q_ranges {
                let entry = ScanRange::from_parts(
                    Range {
                        start: *start,
                        end: *end,
                    },
                    *priority,
                );
                to_delete_ends.push(entry.block_range().end);
                to_create = if let Some(cur) = to_create {
                    Some(cur.insert(entry, force_rescans))
                } else {
                    Some(SpanningTree::Leaf(entry))
                };
            }

            // Update the tree that we read from the database, or if we didn't find any ranges
            // start with the scanned range.
            for entry in entries {
                to_create = if let Some(cur) = to_create {
                    Some(cur.insert(entry, force_rescans))
                } else {
                    Some(SpanningTree::Leaf(entry))
                };
            }
            (to_create, to_delete_ends)
        };

        if let Some(tree) = to_create {
            self.0.retain(|(_, block_range_end, _)| {
                // if the block_range_end is equal to any in to_delete_ends, remove it
                !to_delete_ends.contains(block_range_end)
            });
            let scan_ranges = tree.into_vec();
            self.insert_queue_entries(scan_ranges.iter())?;
        }
        Ok(())
    }

    /// Drops the scan work queued below `height`, except where retained by
    /// `retain_with_priority`, and returns the number of queue entries removed or altered.
    ///
    /// Entries with the bookkeeping priorities `Scanned` and `Ignored` are always retained when
    /// `retain_with_priority` is `Some`, as are entries with that priority or greater. Pruned
    /// ranges are demoted to `Ignored` where needed to keep the queue contiguous above the lowest
    /// retained entry.
    pub(crate) fn prune_below(
        &mut self,
        height: BlockHeight,
        retain_with_priority: Option<ScanPriority>,
    ) -> u64 {
        let is_retained = |priority: ScanPriority| {
            retain_with_priority
                .is_some_and(|retain| priority <= ScanPriority::Scanned || priority >= retain)
        };

        // Every entry this operation can touch starts below `height`.
        let mut existing: Vec<ScanRange> = self
            .0
            .iter()
            .filter(|(start, _, _)| *start < height)
            .map(|(start, end, priority)| ScanRange::from_parts(*start..*end, *priority))
            .collect();
        existing.sort_by_key(|entry| entry.block_range().start);

        // The lowest retained entry is the floor of the region that must remain covered.
        let fill_from = existing
            .iter()
            .find(|entry| is_retained(entry.priority()))
            .map(|entry| entry.block_range().start);

        let replacement = existing
            .iter()
            .flat_map(|entry| {
                let range = entry.block_range();
                if is_retained(entry.priority()) {
                    vec![entry.clone()]
                } else {
                    // Only the part below `height` is pruned; any remainder keeps its priority.
                    let pruned = ScanRange::from_parts(
                        range.start..std::cmp::min(range.end, height),
                        ScanPriority::Ignored,
                    );
                    let kept = (range.end > height)
                        .then(|| ScanRange::from_parts(height..range.end, entry.priority()));

                    match fill_from {
                        Some(floor) if pruned.block_range().end > floor => {
                            Some(pruned).into_iter().chain(kept).collect()
                        }
                        _ => kept.into_iter().collect(),
                    }
                }
            })
            .fold(Vec::<ScanRange>::new(), |mut acc, entry| {
                match acc.last() {
                    Some(prev)
                        if prev.priority() == entry.priority()
                            && prev.block_range().end == entry.block_range().start =>
                    {
                        let merged = ScanRange::from_parts(
                            prev.block_range().start..entry.block_range().end,
                            prev.priority(),
                        );
                        acc.pop();
                        acc.push(merged);
                    }
                    _ => acc.push(entry),
                }
                acc
            });

        if replacement == existing {
            return 0;
        }

        let changed = existing
            .iter()
            .filter(|entry| !replacement.contains(entry))
            .count() as u64;

        self.0.retain(|(start, _, _)| *start >= height);
        self.0.extend(replacement.iter().map(|entry| {
            let range = entry.block_range();
            (range.start, range.end, entry.priority())
        }));
        changed
    }

    pub fn delete_starts_greater_than_equal_to(&mut self, height: BlockHeight) {
        self.0.retain(|(start, _, _)| *start < height);
    }

    pub fn truncate_ends_to(&mut self, height: BlockHeight) {
        self.0.iter_mut().for_each(|(_, end, _)| {
            if *end > height {
                *end = height;
            }
        });
    }
}

// We deref to slice so that we can reuse the slice impls
impl Deref for ScanQueue {
    type Target = [(BlockHeight, BlockHeight, ScanPriority)];

    fn deref(&self) -> &Self::Target {
        &self.0[..]
    }
}
impl DerefMut for ScanQueue {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0[..]
    }
}

mod serialization {
    use super::*;
    use crate::proto::memwallet as proto;

    impl From<(BlockHeight, BlockHeight, ScanPriority)> for proto::ScanQueueRecord {
        fn from(
            (start_height, end_height, priority): (BlockHeight, BlockHeight, ScanPriority),
        ) -> Self {
            Self {
                start_height: start_height.into(),
                end_height: end_height.into(),
                priority: match priority {
                    ScanPriority::Ignored => proto::ScanPriority::Ignored as i32,
                    ScanPriority::Scanned => proto::ScanPriority::Scanned as i32,
                    ScanPriority::Historic => proto::ScanPriority::Historic as i32,
                    ScanPriority::OpenAdjacent => proto::ScanPriority::OpenAdjacent as i32,
                    ScanPriority::FoundNote => proto::ScanPriority::FoundNote as i32,
                    ScanPriority::ChainTip => proto::ScanPriority::ChainTip as i32,
                    ScanPriority::Verify => proto::ScanPriority::Verify as i32,
                },
            }
        }
    }

    impl From<proto::ScanQueueRecord> for (BlockHeight, BlockHeight, ScanPriority) {
        fn from(record: proto::ScanQueueRecord) -> Self {
            (
                record.start_height.into(),
                record.end_height.into(),
                match record.priority() {
                    proto::ScanPriority::Ignored => ScanPriority::Ignored,
                    proto::ScanPriority::Scanned => ScanPriority::Scanned,
                    proto::ScanPriority::Historic => ScanPriority::Historic,
                    proto::ScanPriority::OpenAdjacent => ScanPriority::OpenAdjacent,
                    proto::ScanPriority::FoundNote => ScanPriority::FoundNote,
                    proto::ScanPriority::ChainTip => ScanPriority::ChainTip,
                    proto::ScanPriority::Verify => ScanPriority::Verify,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use zcash_client_backend::data_api::scanning::ScanPriority;
    use zcash_protocol::consensus::BlockHeight;

    use super::ScanQueue;

    fn h(n: u32) -> BlockHeight {
        BlockHeight::from_u32(n)
    }

    fn queue(entries: &[(u32, u32, ScanPriority)]) -> ScanQueue {
        ScanQueue(entries.iter().map(|(s, e, p)| (h(*s), h(*e), *p)).collect())
    }

    fn sorted(q: &ScanQueue) -> Vec<(u32, u32, ScanPriority)> {
        let mut v: Vec<_> =
            q.0.iter()
                .map(|(s, e, p)| (u32::from(*s), u32::from(*e), *p))
                .collect();
        v.sort_by_key(|(s, _, _)| *s);
        v
    }

    #[test]
    fn prune_below_retains_at_and_above_priority() {
        use ScanPriority::*;
        let mut q = queue(&[
            (419_200, 450_000, Ignored),   // bookkeeping: kept
            (450_000, 460_000, FoundNote), // retained priority: kept
            (460_000, 500_000, Scanned),   // bookkeeping: kept
            (500_000, 700_000, Historic),  // straddler: split at the height
            (700_000, 710_000, ChainTip),  // above the height: untouched
        ]);

        let changed = q.prune_below(h(663_150), Some(FoundNote));

        assert_eq!(changed, 1);
        assert_eq!(
            sorted(&q),
            vec![
                (419_200, 450_000, Ignored),
                (450_000, 460_000, FoundNote),
                (460_000, 500_000, Scanned),
                // The pruned part is demoted to `Ignored`, preserving contiguity above the lowest
                // retained entry, and the remainder keeps its priority.
                (500_000, 663_150, Ignored),
                (663_150, 700_000, Historic),
                (700_000, 710_000, ChainTip),
            ]
        );
    }

    #[test]
    fn prune_below_without_retention_drops_everything_below() {
        use ScanPriority::*;
        let mut q = queue(&[(0, 10, Historic), (10, 30, Historic), (30, 40, ChainTip)]);

        let changed = q.prune_below(h(20), None);

        assert_eq!(changed, 2);
        assert_eq!(sorted(&q), vec![(20, 30, Historic), (30, 40, ChainTip)]);
    }

    #[test]
    fn prune_below_is_a_noop_when_nothing_is_prunable() {
        use ScanPriority::*;
        let entries = [(0, 10, Scanned), (10, 20, Ignored), (20, 30, Historic)];
        let mut q = queue(&entries);

        // Everything below the height is bookkeeping, so there is nothing to prune.
        assert_eq!(q.prune_below(h(20), Some(Historic)), 0);
        assert_eq!(sorted(&q), entries.to_vec());
    }
}
