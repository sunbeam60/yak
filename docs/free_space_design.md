<!-- cspell:locale en-GB -->

# Free Space in L2 - Design

Bjorn Toft Madsen · started 22 Nov 2026

This document is a proposal to replace the free list described in [new_architecture.md](./new_architecture.md) (L2: Reusing blocks, The free list is copy-on-write too). It covers how L2 records free blocks on disk, how it finds free space quickly, and how it keeps both in step across transactions and processes. Once agreed, its contents move into new_architecture.md and this file is retired.

## Overview and goals

L3 asks L2 for runs of blocks, with an optional *goal*: a block index the run should ideally follow. L2 tries, in order, for one contiguous extent starting at the goal, one contiguous extent anywhere, and several extents, and grows the file only for what remains. A free list stored as an unsorted chain cannot answer those questions without reading all of it, and a copy-on-write chain cannot cheaply give up an extent from the middle.

Goals:

- **Find space by location and by size without IO.** Goal and size searches are answered from memory.
- **Few pages written per commit.** A commit writes its changes to free space as a short record, usually into one trunk block, however much free space the file has.
- **No reads in the allocation loop.** The free space is read once per open, and afterwards only what other opens have committed is read, once per transaction.
- **Crash safe by the same rules as everything else.** Nothing reachable from the live header is changed in place.

Design principles:

- **The on-disk structures are a record, not an index.** They hold what is needed to rebuild the free space, in whatever order is cheapest to write. Searching is done on an in-memory map rebuilt from them.
- **One truth.** The map is derived from the on-disk record and is always checked against the live header before it is used. It never holds anything the record does not.
- **Only the writer uses free space.** Snapshots never allocate, so they never read the free space structures or build the map.

## What does not change

- **Block states.** Free, Private, Used and Pending, as in new_architecture.md (L2: L2 API). Only Free and Pending are recorded on disk.
- **The pending list.** Its layout (tagged entries, newest first), its rule that trunks never live in the blocks they describe, and the rules for when a pending block may be reused, are unchanged. Only where its blocks go once eligible changes: they become free records in the free block log (see Committing).
- **Growing and shortening the file.** `total blocks` is updated at commit; a writer shortens the file to it when it begins a transaction after another open's crash.
- **Allocation order.** Goal first, then one extent anywhere, then several, then growth.

## On-disk structures

### L2 header fields

The free list head is replaced by three fields; the pending list head stays:

| Field                      | Size (bytes) | Notes                                                                                           |
| -------------------------- | ------------ | ----------------------------------------------------------------------------------------------- |
| Free block extent head     | 4            | Little-endian u32; first trunk of the free block extent list (sentinel = empty)                 |
| Free block extent version  | 8            | Little-endian u64; the version created by the commit that wrote the free block extent list      |
| Free block log head        | 4            | Little-endian u32; first trunk of the free block log (sentinel = no changes since the extent list) |
| Pending list head          | 4            | Little-endian u32; unchanged                                                                    |

This alters the on-disk format: the L2 header layout version is increased.

The free block extent version is in the header, not in the extent list's first trunk, because it is the one fact a reader needs before deciding whether to read anything. It is a version and not just the block index of the head trunk, because trunk blocks are reused: a new extent list can start in the same block an old one did.

### Trunks

All three lists (free block extents, free block log, pending list) are chains of *trunk* blocks with the same shape:

- A trunk holds the index of the next trunk (4 bytes, sentinel 0xFFFFFFFF at the end of the chain), followed by 8-byte slots.
- **New trunks are added at the head of the chain.**
- **The head trunk fills from the back.** The unused part of the head trunk is the gap between the next pointer and its first used slot.
- **Every trunk except the head is full.**
- **Empty slots are all zeros.** No used slot is all zeros (see each list), so the first slot that is not zero is the head trunk's first used slot. No count of used slots is stored.

With 4 KiB blocks a trunk holds (4096 - 4) / 8 = 511 slots.

### Free block extent list

The free space as of one version, written as plain extents, one per slot:

| Field  | Size (bytes) | Notes                                            |
| ------ | ------------ | ------------------------------------------------ |
| Start  | 4            | Little-endian u32; index of the first free block |
| Length | 4            | Little-endian u32; number of free blocks         |

No extent has a length of 0, so no used slot is all zeros. The list is written whole by a condense (see Condensing), in block order, and is never changed afterwards: the next condense writes a new one.

### Free block log

Every commit's change to free space, since the extent list was written, newest first. Each commit adds one *batch*:

| Record        | Slots | Fields                                                                                     |
| ------------- | ----- | ------------------------------------------------------------------------------------------ |
| Batch marker  | 2     | Version (u64; the version created by the commit) · Alloc count (u32) · Free count (u32)    |
| Alloc extents | 1 each | Start (u32) · Length (u32); blocks that were free before this commit and are not after it |
| Free extents  | 1 each | Start (u32) · Length (u32); blocks that were not free before this commit and are after it |

- **A batch is the commit's net change.** No block appears in both its alloc and its free extents, so the order of records within a batch carries no meaning. This is what allows records to be grouped by kind and identified by position, with no type field. L2 gets the net change for free from its Private state: a block the transaction allocates and releases again simply leaves its private set.
- **Records are laid out in reading order.** A reader starting at the head meets a batch's marker, then its alloc extents, then its free extents, then the previous batch's marker. A batch that does not fit in the head trunk continues in the next trunk; the marker is in the newer one.
- **The marker's version slot comes first.** A version is never 0, so the head trunk's first used slot is always a version. The marker's second slot can be all zeros (a batch with nothing freed), which is why the version must lead.
- **Every commit writes a batch.** Each commit rewrites the head trunk, so it always allocates at least that block (see Trunk blocks are free space like any other). The newest version in the log is therefore always the first thing in the head trunk.

The marker costs 16 bytes per commit; extent records stay 8 bytes, as in the old free list.

### Trunk blocks are free space like any other

The trunks of all three lists are taken from free space, and their allocation and release are recorded as ordinary records in the commit's batch. The map is then exactly the replay of the extent list and the log, with no blocks excluded by special rules.

- **Allocating trunks.** A commit needs trunks to hold its batch, and taking them adds records to the batch. The commit settles this with a small loop: work out how many trunks are needed, allocate them, add their alloc records, and check again. With 511 slots per trunk this settles within one or two rounds. A record may describe the block it is stored in.
- **Releasing trunks.** A trunk the new version no longer uses (a rewritten head trunk, a rewritten pending trunk, every trunk of a condensed list) is recorded as freed in the same commit's batch. It is not available to the commit that frees it, since a crash before that commit's header is ordered falls back to the previous version, which still uses it. No snapshot reads free space structures, so no reader can hold it back, and the next transaction may reuse it once the commit has completed, by the same ordering rule as pending blocks (new_architecture.md, L2: Pending blocks).

## The free block map

### Ownership

L2 keeps one free block map per open. It is the only L2 state that belongs to the open rather than to a transaction or a snapshot, as the writer mutex and reader map are for L1 (new_architecture.md, Thread & process safety in L1).

- **Built lazily**, when the open first begins a transaction. An open that only takes snapshots never builds it.
- **Lent to the transaction.** At `begin_transaction()` the transaction takes the map from the open; at `commit()` or `rollback()` it hands it back. The writer mutex already ensures one transaction per open, so taking it never waits, and allocation works on a map the transaction owns outright, without locking.
- **Dropped when the open closes.** Closing writes nothing; it is not a transaction.

### Contents

- The free extents, ordered by start (for goal searches and for merging neighbours on release) and by length then start (for size searches). Neighbouring extents are always merged.
- The committed version the map reflects.
- The free block extent version the map was built from.

### Bringing the map up to date

At the start of each transaction, after the writer lock is taken and the live header read:

1. **No map yet, or the free block extent version in the header differs from the map's:** load in full. Read the extent list, then read the log, and apply its batches oldest first.
2. **The header's version equals the map's version:** use the map as it is. This is always the case when the last commit was made by this open.
3. **Otherwise:** catch up. Read the log from the head, a batch at a time, until reaching a batch whose version is at or below the map's version. Apply the batches read, oldest first.

The log is read newest first but applied oldest first, because a block freed by one commit may be allocated by a later one. Within a batch, order does not matter (see Free block log).

Catching up goes further than new_architecture.md currently allows (Beginning a transaction, step 2: a layer may keep state across transactions only while the live header is the one this open last committed). The map is kept even when another open has committed, because the log says exactly what changed.

## Transactions

### Beginning

After the map is up to date, L2 checks which pending entries are eligible (unchanged from new_architecture.md) and moves their extents into the transaction's *became free* set. The map itself is not changed.

### Changes are kept beside the map

While a transaction runs, the map stays equal to the committed version's free space. The transaction keeps its changes beside it:

- **Private:** blocks it has allocated.
- **Became free:** extents moved from the pending list, and blocks gained by growing the file.
- **Released:** blocks it allocated and released again leave the private set; committed blocks it releases go to the pending list at commit, as now.

Allocation searches the map and the became-free set, excluding the private set. How this overlay is implemented is left to the implementation, provided rollback leaves the map exactly as it was.

### Allocating

1. **At the goal.** Look up, by start, the extent containing the block after the goal. If it holds the whole run, take it.
2. **One extent anywhere.** Look up, by length, the smallest extent that holds the whole run.
3. **Several extents.** Take the largest extents first until the run is covered, so the run is split as little as possible.
4. **Grow the file**, once, by what remains. If the free extent nearest the end of the file reaches `total blocks`, the run can take that extent and grow by the rest, keeping it contiguous.

Blocks gained by growing enter the became-free set before being allocated, so growth needs no special case in the batch: grown blocks that are allocated cancel out, and any left over are recorded as free.

### Committing

1. Work out the net change: alloc extents are the private set (less blocks gained by growing), free extents are the became-free set not allocated, plus trunks released by this commit.
2. If the pending list was cut, rewrite the pending trunks it keeps (unchanged from new_architecture.md); their allocation and the release of the old trunks join the batch.
3. Allocate trunks for the batch and settle the loop (see Trunk blocks are free space like any other).
4. Write the batch at the head of the free block log, rewriting the head trunk into a new block.
5. Merge the net change into the map, and set the map's version to the new version.
6. If the log has grown past the condense threshold, condense instead of steps 3-4 (see Condensing).

The header written by the commit records the new free block log head (and, after a condense, the new extent head and version), together with the pending list head and `total blocks`, as one.

### Rolling back

The transaction's changes are dropped and the map is handed back unchanged. Nothing was written to free space structures, so nothing on disk needs to change. Pending entries moved into the became-free set are still on the pending list on disk and are found eligible again by the next transaction.

### `commit_and_continue()`

The map is updated by the commit (step 5) and the transaction keeps it. Pending eligibility is checked again, as now.

## Condensing

The log only grows: every commit adds at least a marker and two extents (its new head trunk and the head trunk it replaced). A full load replays all of it, so L2 periodically replaces the extent list and log with a fresh extent list written from the map.

- **When.** At commit, when the number of slots used by the log exceeds a fixed multiple of the number of extents in the map. The map knows its own extent count, so the check costs nothing. The multiple is one named constant, with a floor so that a nearly empty map does not condense on every commit.
- **How.** Merge the commit's net change into the map. Allocate trunks for the new extent list from the map, settling the loop as for log trunks. Add every trunk of the old extent list and old log to the map as free (they are not available to this commit, see Releasing trunks). Write the map's extents, in block order, as the new extent list. The header gets the new extent head, the new version as the free block extent version, and the sentinel as the free block log head.
- **Effect on other opens.** An open whose map was built from an older extent list sees a different free block extent version in the header and loads in full.

Condensing never happens at close: closing is not a transaction.

## Crash safety

- **Nothing reachable from the live header changes in place.** The extent list is never changed after it is written; the log only gains a new head trunk; rewritten pending trunks are new blocks.
- **A crash before the header is written** leaves the previous header live, whose extent list, log and pending list are untouched. The open that next writes rebuilds or catches up from them.
- **Released trunks are reused only after the commit that released them completes**, so a crash can always fall back to a version whose free space structures are intact.
- **Pending blocks moved to free are recorded by the next commit**, as now. A crash before then leaves them pending, to be found eligible again: a crash can delay reuse, never lose a block.

## Integrity checks

`verify()` rebuilds the map from the extent list and log, and checks:

- That every alloc extent in a batch was free before that batch, and every free extent was not.
- That no block is both free and reachable from the committed version, or both free and pending.
- That every block below `total blocks` is reachable, free or pending.
- That the log's batch versions are in descending order and none is at or below the free block extent version.

## Cost model

| Operation                           | Reads                                       | Writes                                         |
| ----------------------------------- | ------------------------------------------- | ---------------------------------------------- |
| First transaction of an open        | Every extent list and log trunk, once       | -                                              |
| Transaction after own commit        | None                                        | -                                              |
| Transaction after another's commit  | The log trunks added since (usually one)    | -                                              |
| Allocation and release              | None                                        | -                                              |
| Commit                              | None                                        | Usually one log trunk (two when the head fills) |
| Condense (occasional)               | None                                        | One trunk per 511 extents in the map           |

Memory: the map holds each free extent twice (by start, and by length). A file with 100,000 free extents needs a few megabytes; a healthy file needs far less.

## Decision log

| Decision                                  | Choice                                                                   | Why                                                                                                                                 |
| ----------------------------------------- | ------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| Where free space is searched              | In-memory map, by start and by length                                    | Goal and size searches without IO; neighbours merged on release                                                                     |
| Rejected: sorted copy-on-write chain      | Not used (the current design)                                            | No search beyond the head trunks without reading the whole chain; taking an extent from deep in the chain rewrites every trunk above it; neighbours released by different commits are never merged |
| Rejected: copy-on-write B+ trees on disk  | Not used (XFS style, one tree by start and one by length)                | Exact searches without a full load, but every allocation and release copies a path in both trees each commit                        |
| On-disk record                            | Extent list plus append-only log of per-commit batches                   | A commit writes one short batch; the list is rebuilt into the map, so it needs no order                                             |
| Rejected: extent list and log in one chain | Not used                                                                | Whether a condense happened could only be found by walking the chain; with two, the header says so                                  |
| Batch contents                            | Net change: allocs and frees never overlap                               | Order within a batch carries no meaning, so records need no type field; L2's Private state yields the net change for free           |
| Rejected: type field per record           | Not used                                                                 | Doubles every extent record to carry a flag that position already gives                                                             |
| Batch marker                              | Version, alloc count, free count; two slots, version first               | Catch-up finds where its knowledge ends; first used slot is never zero                                                              |
| Rejected: catch-up by trunk index         | Not used                                                                 | The head trunk is rewritten by every commit, so a remembered trunk may have left the chain                                          |
| Trunk blocks                              | Recorded as ordinary allocs and frees                                    | The map is exactly the replay of the record; catch-up, load and verify follow one rule                                              |
| Rejected: trunks implicit by reachability | Not used                                                                 | Catch-up would have to find trunks that left the chain by comparing chains; releases end up logged anyway                           |
| Released trunks                           | Freed in the releasing commit's batch, reusable once it completes        | No snapshot reads free space structures; every open learns of them from the log                                                     |
| Map ownership                             | Per open, lent to the transaction                                        | Survives transactions, which is what makes it worth having; never shared at the same time, so no locking during allocation          |
| Transaction changes                       | Kept beside the map; merged at commit, dropped at rollback               | Rollback cannot leave pending blocks counted as free twice                                                                          |
| Extent list version                       | In the L2 header                                                         | Decides whether to read anything; a block index alone suffers from reuse                                                            |
| Condense trigger                          | Log slots exceed a multiple of map extents, at commit                    | Bounds full-load work relative to the minimum                                                                                       |
| Rejected: condense at close               | Not used                                                                 | Closing is not a transaction                                                                                                        |
| Pending list                              | Unchanged                                                                | Tags and newest-first order are what make the reuse check cheap                                                                     |

**Open questions**

- The condense multiple and its floor. 2× bounds a full load at about three times the minimum work; to be settled by profiling.
- Whether "several extents" should prefer extents near the goal over the largest ones, for streams whose blocks are read together.

**Changes needed in new_architecture.md once agreed**

- L2: Reusing blocks and The free list is copy-on-write too are replaced by this design.
- L2: Pending blocks: eligible entries become free records in the next batch, rather than extents added to the free list.
- L2 header table: the free list head is replaced by the three free block fields.
- Thread safety in L2: the free block map is the one piece of L2 state owned by the open.
- L1: Beginning a transaction, step 2: L2's map may be kept, and caught up, across other opens' commits.
- Verifying a Yak file: as in Integrity checks above.
