# Pooling: the episodes two datasets both contain

Every check in the catalog looks inside one dataset. A corpus is assembled from several, and the
failure that costs a training run is not inside any of them:

> The same recording, exported twice, arrives through two sources and is trained on twice.

Nothing in either dataset is wrong. Each one's report is clean, each one's certificate verifies, and
the pooled corpus is a **smaller dataset than its episode count says**, weighted toward whatever was
duplicated. That is the redundancy the README's opening claim is about, and no per-dataset verdict
can see it.

```sh
veridex overlap source-a/ source-b/ source-c/
```

```
Veridex overlap
  Verdict:  SHARED EPISODES
  source-a/: 420 episode(s)
  source-b/: 180 episode(s)
  12 episode(s) in 6 group(s) are the same recording in more than one dataset:
    source-a/ episode 7 = source-b/ episode 102
    …
```

It exits **0** when nothing is shared and **20** when anything is, so the gate a pipeline actually
wants is one line: *this new source is not already in the corpus it is about to join.* `--json`
prints the same answer as `{schema, shares_anything, overlaps, shares_frames, shared_episodes,
uncomparable_episodes, datasets, shared, near, frames_uncounted}`, and `veridex.overlap([paths])`
returns that document byte-identically from Python.

## Two questions, because one answer is stronger than the other

**Shared episodes** are exact: the two are byte-for-byte the same recording. **Shared frames** are a
re-export: the same recording arrived with its statistics recomputed, its video re-encoded or its
tail trimmed, so no episode is identical and most of the frames are the same bytes.

```
  Episodes that share most of their frames without being identical:
    source-a/ episode 1 and source-b/ episode 44 share 94% of their frames (31 of 33)
```

The threshold is `near_duplicate_fraction` — the same one `structural.near-duplicate-episode`
applies within a dataset, over the same denominator (the smaller episode, so an episode contained in
a longer one counts as a full match), so a near-match means the same thing on either side of a
dataset boundary. A pair an exact group already speaks for is left to it, so the two halves of the
report never say the same thing twice.

Counted per episode, and only over the fingerprints that **identify** one episode of their own
dataset. Both rules are there to keep the number honest. A fingerprint held by more than one of its
own episodes — a padded frame, a frozen camera, a zeroed feature — names no particular recording, and
counting it would make two unrelated datasets that both contain a flat frame look like they shared
one. And counting per dataset rather than per episode turns a single coincidental frame into a
double-digit percentage: two datasets whose first frame happens to hold the same value share one
fingerprint, which is a few percent of an episode and no evidence at all.

## What it proves, and what it does not

**An episode group is an exact match.** Two episodes are grouped when they share the content signature
`structural.duplicate-episode` already uses — the same one, so the within-dataset check and this one
agree by construction. That signature covers every frame's content hash together with the streams'
declared shapes, clocks and stored statistics, and it excludes the episode's index and the dataset it
arrived in. So a group means *byte-for-byte the same recording, wherever it came from*.

A re-export that recomputed its summary statistics, re-encoded its video, or trimmed a tail is the
same recording and is **not** grouped — it appears in the shared-frame rows above instead, which is
why both halves exist. Veridex reports what it can prove on each side and says in the output which
claim is which, rather than leaving the stronger one to be assumed.

**An episode nobody can fingerprint is excluded, counted, and named.** A stream whose frames carry
no content hash cannot establish identity, and two episodes nobody can fingerprint are not thereby
the same recording — which is the finding this comparison would otherwise invent on any format that
stores its pixels out of band. The report states how many episodes were in that position, because a
silent exclusion is indistinguishable from a clean result.

**Duplication inside one dataset is not pooling.** That is `structural.duplicate-episode`'s question
and every `veridex check` answers it; repeating it here would mean a corpus report in which one
dataset's internal duplication reads as overlap with another. Where a group *does* span two
datasets, every member is listed — including any that sit in the same one, because that is the shape
of the group a reader has to act on.

**A dataset with too many fingerprints to hold at once is named, not reported as zero.** The
shared-frame half builds a set of fingerprints per dataset, and a frame count is a product of
numbers the files choose; past two million distinct fingerprints a dataset's rows are withheld and
the report says so, because "too many to compare" and "nothing shared" are opposite answers.

**No `--metadata-only`, and no sampling.** A content fingerprint comes from a frame, so a run that
opened no payload would report every episode as uncomparable; a sampled read would report the
episodes two samples happened to share. Both are refused by name rather than answered misleadingly.

## What to do with a group

Veridex does not edit data, here or anywhere. A shared group is a decision:

- Drop the duplicate from one source before pooling, and keep the dataset that carries the better
  provenance — `veridex inspect` prints each one's coverage, and the certificate says what it was
  issued over.
- Or keep both deliberately, as weighting, and record that it was deliberate. The problem is a
  corpus that cannot tell which of its recordings it holds twice, not a corpus that holds one twice
  on purpose.
