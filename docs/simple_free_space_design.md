<!-- cspell:locale en-GB -->

# Free Space in L2 - Simple Design

Bjorn Toft Madsen · started 22 Nov 2026

This document is a proposal to replace the free list described in [new_architecture.md](./new_architecture.md) (L2: Reusing blocks, The free list is copy-on-write too). It is the simpler alternative to [free_space_design.md](./free_space_design.md), which stays as a possible upgrade (see Upgrade path). Once agreed, its contents move into new_architecture.md and this file is retired.

## Overview and goals

L3 asks L2 for runs of blocks, with an optional *goal*: a block index the run should ideally follow. L2 tries, in order, for one contiguous extent starting at the goal, one contiguous extent anywhere, and several extents, and grows the file only for what remains. A free list stored as an unsorted chain cannot answer those questions without reading all of it, and a copy-on-write chain cannot cheaply give up an extent from the middle.

This design reads the whole free list into memory at the start of each transaction, searches it there, and writes it out whole, fresh, at commit.

Goals:

- **Find space by location and by size without IO.** Goal and size searches are answered from memory.
- **Merge neighbouring free extents**, whichever commits released them, so the free list records real fragmentation and stays short.
- **No state outside transactions and snapshots.** L2 keeps nothing from one transaction to the next, as new_architecture.md requires (L1: Beginning a transaction, step 2).
- **Crash safe by the same rules as everything else.** Nothing reachable from the live header is changed in place.

Design principles:

- **The on-disk list is a record, not an index.** It holds the free extents, in block order. Searching is done on an in-memory map built from it.
- **The free list is expected to be short.** A trunk holds 511 extents with 4 KiB blocks, and extents are always merged, so most files need one or two trunks. Reading and writing the whole list costs about what the current design costs. If real files show long lists, the log in free_space_design.md removes that cost (see Upgrade path).
- **Only the writer uses free space.** Snapshots never allocate, so they never read the free list or build the map.

## What does not change

- **Block states.** Free, Private, Used and Pending, as in new_architecture.md (L2: L2 API). Only Free and Pending are recorded on disk.
- **The pending list.** Its layout (tagged entries, newest first), its rule that trunks never live in the blocks they describe, and the rules for when a pending block may be reused, are unchanged. Eligible entries become free extents in the map at transaction start, and are recorded by the next commit's free list.
- **The L2 header fields.** Free list head and pending list head, as now.
- **Growing and shortening the file.** `total blocks` is updated at commit; a writer shortens the file to it when it begins a transaction after another open's crash.
- **Allocation order.** Goal first, then one extent anywhere, then several, then growth.

## On-disk structure

The free list is a chain of *trunk* blocks, starting at the free list head in L2's header section (sentinel 0xFFFFFFFF = no free blocks). Each trunk holds the index of the next trunk (4 bytes, sentinel at the end of the chain) followed by 8-byte extents:

| Field  | Size (bytes) | Notes                                            |
| ------ | ------------ | ------------------------------------------------ |
| Start  | 4            | Little-endian u32; index of the first free block |
| Length | 4            | Little-endian u32; number of free blocks         |

With 4 KiB blocks a trunk holds (4096 - 4) / 8 = 511 extents.

- **Extents are in block order** along the chain, and neighbouring extents are always merged.
- **Every trunk except the last is full.**
- **Unused slots are all zeros**, at the end of the last trunk. No extent has a length of 0, so the first slot whose length is 0 ends the list. No count is stored.
- **The list is never changed after it is written.** Each commit that changes free space writes a new list.

This alters the on-disk format slightly: the current design fills the head trunk from the back and keeps every trunk except the head full. The L2 header layout version is increased.

```ASCII
L2 header              trunk 811                                      trunk 95
┌──────────────┐      ┌───────────────────────────────────────────┐    ┌────────────────────────────────────────────┐
│ Free: 811    │────► │ next: 95 │ 77+1 │ 96+12 │ 812+4 │ … │ 2900+3 │──► │ next: 0xFFFFFFFF │ 3001+250 │ 4410+8 │ 0+0 │
└──────────────┘      └───────────────────────────────────────────┘    └────────────────────────────────────────────┘
                       full, in block order                              last trunk; unused slots are zeros
```

## The free block map

The transaction builds an in-memory map of free extents, ordered by start (for goal searches and for merging neighbours on release) and by length then start (for size searches). It belongs to the transaction and is dropped when the transaction ends.

## Transactions

### Beginning

After the writer lock is taken and the live header read (new_architecture.md, L1: Beginning a transaction):

1. L2 reads every trunk of the free list once and builds the map.
2. L2 checks which pending entries are eligible (unchanged from new_architecture.md) and moves their extents into the transaction's *became free* set.

### Changes are kept beside the map

While a transaction runs, the map stays equal to the committed version's free space. The transaction keeps its changes beside it:

- **Private:** blocks it has allocated. A block it allocates and releases again leaves the private set and is free to allocate again.
- **Became free:** extents moved from the pending list, and blocks gained by growing the file.
- **Released:** committed blocks it releases go to the pending list at commit, as now.

Allocation searches the map and the became-free set, excluding the private set. How this overlay is implemented is left to the implementation, provided rollback leaves the map exactly as it was. Since the map is dropped at the end of the transaction anyway, this matters only for `commit_and_continue()` and for keeping commit simple.

### Allocating

1. **At the goal.** Look up, by start, the extent containing the block after the goal. If it holds the whole run, take it.
2. **One extent anywhere.** Look up, by length, the smallest extent that holds the whole run.
3. **Several extents.** Take the largest extents first until the run is covered, so the run is split as little as possible.
4. **Grow the file**, once, by what remains. If the free extent nearest the end of the file reaches `total blocks`, the run can take that extent and grow by the rest, keeping it contiguous.

Blocks gained by growing enter the became-free set before being allocated; any left unallocated at commit are written to the free list.

### Committing

If the transaction changed free space (allocated, moved pending entries, or grew the file and left blocks unallocated):

1. If the pending list was cut, rewrite the pending trunks it keeps (unchanged from new_architecture.md). Their blocks are allocated from the map; the old pending trunks are released (see step 4).
2. Merge the transaction's changes into the map.
3. Allocate trunks for the new free list from the map. Taking them changes the extents, which can change how many trunks are needed, so the commit works out the count, allocates, and checks again. With 511 extents per trunk this settles within one or two rounds.
4. Add the old free list's trunks, and any old pending trunks, to the map as free. They are not available to this commit: a crash before its header is ordered falls back to the previous version, which still uses them. No snapshot reads free space structures, so no reader can hold them back, and the next transaction may reuse them once this commit has completed, by the same ordering rule as pending blocks (new_architecture.md, L2: Pending blocks).
5. Write the map's extents, in block order, into the new trunks.

The header written by the commit records the new free list head, together with the pending list head and `total blocks`, as one. A commit that changed no free space keeps the free list head as it was and writes no trunks.

### Rolling back

The map and the transaction's changes are dropped. Nothing was written to the free list, so nothing on disk needs to change. Pending entries moved into the became-free set are still on the pending list on disk and are found eligible again by the next transaction.

### `commit_and_continue()`

The transaction keeps the map as merged by the commit, including the trunks released by that commit, which it may now reuse. Pending eligibility is checked again, as now.

## Crash safety

- **Nothing reachable from the live header changes in place.** Each free list is written into new blocks and never changed afterwards.
- **A crash before the header is written** leaves the previous header live, whose free list and pending list are untouched.
- **Released trunks are reused only after the commit that released them completes**, so a crash can always fall back to a version whose free space structures are intact.
- **Pending blocks moved to free are recorded by the next commit**, as now. A crash before then leaves them pending, to be found eligible again: a crash can delay reuse, never lose a block.

## Integrity checks

`verify()` builds the map from the free list, and checks:

- That the extents are in block order, do not overlap, and no two are neighbours (they would have been merged).
- That every trunk except the last is full, and the last's unused slots are zeros.
- That no block is both free and reachable from the committed version, or both free and pending.
- That every block below `total blocks` is reachable, free or pending.

It also reports the number of free extents and trunks, so benchmarks show how long free lists get in practice (see Upgrade path).

## Cost model

| Operation                      | Reads                          | Writes                                               |
| ------------------------------ | ------------------------------ | ---------------------------------------------------- |
| Beginning a transaction        | Every free list trunk, once    | -                                                    |
| Allocation and release         | None                           | -                                                    |
| Commit that changes free space | None                           | Every free list trunk: one per 511 extents           |
| Commit that changes none       | None                           | -                                                    |

For a file with up to 511 free extents, which is expected to be most files, each transaction reads one trunk and each commit writes one, as the current design usually does. Memory: each transaction holds each free extent twice (by start, and by length).

## Upgrade path

The cost of this design grows with the length of the free list: a file with 5,000 free extents reads and writes ten trunks per transaction. If benchmarks (see Integrity checks) show such lists are common, free_space_design.md replaces the whole-list rewrite with a condensed extent list plus a log of per-commit changes, and keeps the map per open with catch-up. Allocation, the map and the pending list are the same in both designs, so the upgrade changes only how free space is recorded and when it is read.

## Decision log

| Decision                                 | Choice                                                     | Why                                                                                                                             |
| ---------------------------------------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Where free space is searched             | In-memory map, by start and by length                      | Goal and size searches without IO; neighbours merged on release                                                                 |
| Rejected: sorted copy-on-write chain     | Not used (the current design)                              | No search beyond the head trunks without reading the whole chain; taking an extent from deep in the chain rewrites every trunk above it; neighbours released by different commits are never merged |
| On-disk record                           | Whole free list, written fresh in block order at commit    | Simplest record that can be rebuilt; short because extents are always merged                                                    |
| Rejected for now: extent list plus log   | Not used (free_space_design.md)                            | Saves reads and writes only when free lists are long; costs a second list kind, batch markers, condensing, catch-up and state per open |
| Map ownership                            | Per transaction                                            | No state outside transactions and snapshots; nothing to keep in step with other opens                                           |
| Transaction changes                      | Kept beside the map; merged at commit, dropped at rollback | Rollback cannot leave pending blocks counted as free twice; commit merges in one place                                          |
| Trunk blocks                             | Allocated from the map; old trunks freed into the new list | The written list is exactly the free space of the new version                                                                   |
| Released trunks                          | Free in the new list, reusable once the commit completes   | No snapshot reads free space structures; the ordering rule is the one pending blocks use                                        |
| Pending list                             | Unchanged                                                  | Tags and newest-first order are what make the reuse check cheap                                                                 |

**Open questions**

- Whether "several extents" should prefer extents near the goal over the largest ones, for streams whose blocks are read together.

**Changes needed in new_architecture.md once agreed**

- L2: Reusing blocks and The free list is copy-on-write too are replaced by this design.
- L2: Pending blocks: eligible entries become free extents in the map, recorded by the next commit's free list.
- L2: Allocating blocks: the search order is answered from the map.
- Verifying a Yak file: as in Integrity checks above.
