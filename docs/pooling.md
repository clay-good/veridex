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

It exits **0** when nothing is shared and **20** when something is, so the gate a pipeline actually
wants is one line: *this new source is not already in the corpus it is about to join.* `--json`
prints the same answer as `{schema, overlaps, shared_episodes, uncomparable_episodes, datasets,
shared}`, and `veridex.overlap([paths])` returns that document byte-identically from Python.

## What it proves, and what it does not

**Exact matches only.** Two episodes are reported when they share the content signature
`structural.duplicate-episode` already uses — the same one, so the within-dataset check and this one
agree by construction. That signature covers every frame's content hash together with the streams'
declared shapes, clocks and stored statistics, and it excludes the episode's index and the dataset it
arrived in. So a group means *byte-for-byte the same recording, wherever it came from*.

A re-export that recomputed its summary statistics, re-encoded its video, or trimmed a tail is the
same recording and is **not** reported. Within one dataset `structural.near-duplicate-episode`
answers that weaker question by comparing frame hashes directly and tolerating a shortfall; across
datasets Veridex reports what it can prove and says so in the output rather than leaving the
stronger claim to be assumed.

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
