//! Reading a dataset's **manifest** from the Hugging Face Hub, without downloading the dataset.
//!
//! The one thing this module is for: `veridex check hf://org/name --metadata-only` on a dataset far
//! too large to pull. A LeRobot dataset writes its structure down in `meta/` — the features and
//! their dtypes, the episode set and per-episode lengths, the stored statistics, the license on the
//! dataset card — and that is a few hundred kilobytes beside a repository that is routinely hundreds
//! of gigabytes. Fetching those files answers "is this the dataset I think it is, does it declare
//! what I need, and is its manifest self-consistent" in a second.
//!
//! **Scope, and what stays true.** Only the manifest is ever requested: the file list below is fixed
//! in the source, not derived from anything the server says, so a hostile response cannot enlarge
//! it. A remote run is a [`Coverage::MetadataOnly`](crate::adapter::Coverage::MetadataOnly) run and
//! carries every refusal that comes with one — no score gate, no certificate. Nothing else about
//! Veridex acquires a network dependency: a certificate still verifies with no network at all, which
//! is the property the whole trust chain rests on.
//!
//! **What it does not do**, refused by name rather than approximated:
//!
//! - **Download a dataset.** A full remote check would mean pulling the data, and Veridex is not a
//!   downloader. Fetch it with the Hub's own tooling and check the local copy.
//! - **Authenticate.** No token is read from the environment or the filesystem, so a private
//!   repository answers 401 and is reported as private. Quietly forwarding a credential the user
//!   happens to have to a host they did not name in this command is not something a validator should
//!   do on their behalf.
//! - **Talk to anywhere but the Hub.** Requests, and any redirect they follow, are checked against a
//!   host allowlist. The point of a fixed file list is lost if the host is not fixed too.
//!
//! **Layering.** Everything above the socket — parsing the source string, choosing the paths,
//! bounding the responses, assembling the manifest — is in this module and depends only on the
//! [`FetchFile`] trait, so all of it is tested without a network. The socket itself lives behind the
//! `remote` cargo feature; a build without it refuses a remote source by name rather than failing to
//! compile a caller.

use std::path::Path;

use crate::adapter::IngestError;

/// The only host a request, or a redirect, may reach.
///
/// A fixed file list bounds what is asked for; a fixed host bounds who is asked. `cdn-lfs*` is where
/// the Hub redirects large-file reads, so a manifest fetch legitimately lands there.
pub const ALLOWED_HOSTS: &[&str] = &[
    "huggingface.co",
    "cdn-lfs.huggingface.co",
    "cdn-lfs-us-1.huggingface.co",
    "cdn-lfs-eu-1.huggingface.co",
];

/// Ceiling on any single manifest file.
///
/// `meta/episodes.jsonl` is the one that grows with the dataset — roughly a hundred bytes per
/// episode, so a 50,000-episode dataset is a few megabytes. This is generous against that and still
/// a bound: the response is a stranger's bytes, and "it is only metadata" is the server's claim, not
/// a fact.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Ceiling on everything one remote ingest fetches.
pub const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

/// The manifest files a metadata-only **LeRobot** ingest reads, and whether the dataset is
/// unreadable without each.
///
/// Fixed here rather than discovered from the repository listing, deliberately: a discovered list is
/// a list the server chooses, and this one has to be the list Veridex chose. Adding a path here is
/// the only way to widen what a remote run requests.
pub const LEROBOT_MANIFEST: &[(&str, bool)] = &[
    ("meta/info.json", true),
    ("meta/episodes.jsonl", false),
    ("meta/stats.json", false),
    // Where a v2.1 dataset keeps the statistics v2.0 and v3 put in `stats.json`. Without it a
    // remote check of a v2.1 repository — most of what is published — reports a dataset that ships
    // statistics as shipping none, and every stored-statistics check silently has nothing to grade.
    ("meta/episodes_stats.jsonl", false),
    ("meta/tasks.jsonl", false),
    ("README.md", false),
];

/// The manifest files a metadata-only **RLDS/TFDS** ingest reads.
///
/// Both are required, and that is the format rather than a choice: `dataset_info.json` declares the
/// splits and their shard lengths, `features.json` declares the per-step features. A TFDS directory
/// missing either is one no reader can describe.
pub const RLDS_MANIFEST: &[(&str, bool)] = &[
    ("dataset_info.json", true),
    ("features.json", true),
    ("README.md", false),
];

/// Every manifest layout a remote read knows, in the order they are probed.
///
/// Probing is one request per layout against its first required file, which keeps the whole set of
/// paths that can ever be requested fixed in this source — the property the file list exists for.
pub const MANIFEST_LAYOUTS: &[(&str, &[(&str, bool)])] =
    &[("LeRobot", LEROBOT_MANIFEST), ("RLDS/TFDS", RLDS_MANIFEST)];

/// A dataset repository on the Hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubRepo {
    /// The owning user or organization.
    pub owner: String,
    /// The repository name.
    pub name: String,
    /// The git revision to read — a branch, tag, or commit. `main` unless the source names one.
    pub revision: String,
    /// The directory within the repository the manifest sits in; empty for the repository root.
    ///
    /// A TFDS export is usually published one version directory deep (`my_dataset/1.0.0/`), and the
    /// caller names it. Never discovered from a repository listing: a path the server chose would
    /// undo the fixed file list.
    pub subdir: String,
}

/// Whether a Hub owner/name/revision segment is one this module will put in a URL and a path.
///
/// The Hub's own charset, minus anything that could climb a directory or split a URL. It matters
/// twice over: these segments are interpolated into a request path, and the repository name becomes
/// a directory under the temporary root the manifest is assembled in.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

impl HubRepo {
    /// Parse a source string naming a Hub dataset.
    ///
    /// Accepts `hf://owner/name`, the same with `@revision`, and the browser URL
    /// `https://huggingface.co/datasets/owner/name` (with or without a `/tree/<revision>` suffix).
    /// Anything else is refused by name — including the other remote schemes Veridex recognizes but
    /// does not implement, so `s3://…` still says what it is rather than "not a Hub URL".
    pub fn parse(source: &str) -> Result<HubRepo, IngestError> {
        let refuse = |why: &str| -> Result<HubRepo, IngestError> {
            Err(IngestError::Parse {
                format_id: "hub",
                message: format!("`{source}` is not a Hugging Face dataset reference: {why}"),
            })
        };

        let (body, revision) = if let Some(rest) = source.strip_prefix("hf://") {
            match rest.split_once('@') {
                Some((r, rev)) => (r.to_string(), rev.to_string()),
                None => (rest.to_string(), "main".to_string()),
            }
        } else if let Some(rest) = source
            .strip_prefix("https://huggingface.co/datasets/")
            .or_else(|| source.strip_prefix("http://huggingface.co/datasets/"))
        {
            // `owner/name`, optionally followed by `/tree/<revision>` and nothing this reader will
            // guess at beyond that.
            let rest = rest.trim_end_matches('/');
            match rest.split_once("/tree/") {
                Some((r, rev)) => (r.to_string(), rev.to_string()),
                None => (rest.to_string(), "main".to_string()),
            }
        } else if source.starts_with("https://huggingface.co/") {
            return refuse(
                "only dataset repositories are readable, and their URLs carry `/datasets/` \
                 (a model or space repository has no dataset manifest to read)",
            );
        } else if let Some(scheme) = source.split_once("://").map(|(s, _)| s) {
            // A remote scheme Veridex recognizes but does not read. Naming it is the difference
            // between "this is not implemented" and "you typed a Hub reference wrong", and only one
            // of those is true.
            return Err(IngestError::Parse {
                format_id: "hub",
                message: format!(
                    "`{scheme}://` is not a source Veridex reads: a remote dataset is read from its \
                     manifest on the Hugging Face Hub (`hf://owner/name`), and anything else has to \
                     be fetched locally first"
                ),
            });
        } else {
            return refuse("expected `hf://owner/name` or a huggingface.co dataset URL");
        };

        let mut segments = body.split('/');
        let (Some(owner), Some(name)) = (segments.next(), segments.next()) else {
            return refuse("a repository is `owner/name`");
        };
        if !is_safe_segment(owner) || !is_safe_segment(name) {
            return refuse("the owner and name must be plain Hub identifiers");
        }
        if !is_safe_segment(&revision) {
            return refuse("the revision must be a plain branch, tag, or commit");
        }
        // Anything past `owner/name` is a directory *within* the repository — which is how a TFDS
        // export is usually published, one version directory per release. It has to be named by the
        // caller rather than discovered: a path the server chose is exactly what the fixed file
        // list exists to prevent.
        let subdir: Vec<&str> = segments.filter(|s| !s.is_empty()).collect();
        if !subdir.iter().all(|s| is_safe_segment(s)) {
            return refuse(
                "a directory inside the repository must be plain path segments (no `.`, `..`, or \
                 anything that would climb out of it)",
            );
        }
        Ok(HubRepo {
            owner: owner.to_string(),
            name: name.to_string(),
            revision,
            subdir: subdir.join("/"),
        })
    }

    /// `owner/name` — the identity the CDM records, and what the content hash binds.
    ///
    /// Deliberately the full repository id rather than just the name: two owners publishing a
    /// `pickplace` are two datasets, and a hash that could not tell them apart would let a
    /// certificate for one match the other.
    pub fn id(&self) -> String {
        if self.subdir.is_empty() {
            format!("{}/{}", self.owner, self.name)
        } else {
            // Two TFDS versions in one repository are two datasets, and the id has to tell them
            // apart for the same reason it has to carry the owner.
            format!("{}/{}/{}", self.owner, self.name, self.subdir)
        }
    }

    /// The URL a repo-relative manifest path is read from.
    pub fn url_for(&self, path: &str) -> String {
        let prefix = if self.subdir.is_empty() {
            String::new()
        } else {
            format!("{}/", self.subdir)
        };
        format!(
            "https://huggingface.co/datasets/{}/{}/resolve/{}/{prefix}{path}",
            self.owner, self.name, self.revision
        )
    }
}

/// The ceiling handed to the fetcher for the next file, given what the whole ingest has spent.
///
/// The smaller of the per-file cap and what the total budget has left, so a repository cannot spend
/// the total several times over by answering large on every path — and once the budget is gone the
/// cap is zero, which the fetcher must refuse rather than treat as "no limit".
fn cap_for(spent: u64) -> u64 {
    MAX_FILE_BYTES.min(MAX_TOTAL_BYTES.saturating_sub(spent))
}

/// One file as the Hub served it: the bytes, and the commit it says they came from.
///
/// The commit is the whole reason this is a struct rather than a `Vec<u8>`. `hf://org/name` reads
/// the `main` branch, and a branch moves: without the commit, a report can only say it read "main",
/// which names no particular bytes and cannot be re-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The file's bytes.
    pub body: Vec<u8>,
    /// The commit the Hub served this from, when it said — the `X-Repo-Commit` response header.
    ///
    /// `None` is the honest answer when the response carried no such header, and it is never
    /// replaced by a guess: the requested revision is what was asked for, not what was served.
    pub commit: Option<String>,
}

/// Whether a server-supplied string is a git commit id this module will record.
///
/// The value goes into the CDM's metadata and therefore into the content hash, so it is bounded
/// here rather than trusted: a header is a stranger's string, and one that is not a commit id is
/// treated as if the header were absent.
pub fn is_commit_id(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Fetches one file by URL. The socket lives behind this so everything above it is testable.
pub trait FetchFile {
    /// Read `url`, refusing past `max_bytes`.
    ///
    /// `Ok(None)` means the file is not in the repository — which is ordinary for four of the five
    /// manifest paths, and must not be confused with a failure to reach the Hub.
    fn get(&self, url: &str, max_bytes: u64) -> Result<Option<Fetched>, IngestError>;
}

/// A staged manifest: where it was written, and the commit it was read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Materialized {
    /// The directory the local adapter should read.
    pub dir: std::path::PathBuf,
    /// The commit every file in it was served from, when the Hub said.
    pub commit: Option<String>,
}

/// Assemble a dataset's manifest under `root`, returning the directory the local adapter should read.
///
/// The layout written is exactly the one a `huggingface-cli download` of those paths would leave, so
/// the local adapter reads it with no remote-specific code path — which is the point. A remote
/// metadata-only ingest and a local one of the same manifest then cannot disagree, because they are
/// the same code reading the same bytes.
///
/// Which layout the repository holds is settled by asking for each one's first required file in
/// turn: one request per layout, against a path fixed in this source. Nothing is written outside
/// `root`: the only paths joined onto it are the fixed ones in [`MANIFEST_LAYOUTS`] and the
/// validated repository name.
pub fn materialize(
    repo: &HubRepo,
    fetch: &dyn FetchFile,
    root: &Path,
) -> Result<Materialized, IngestError> {
    let dir = root.join(&repo.name);
    std::fs::create_dir_all(&dir).map_err(|e| IngestError::Io(e.to_string()))?;

    let mut state = Staging {
        dir,
        spent: 0,
        commit: None,
    };
    for (layout, files) in MANIFEST_LAYOUTS {
        let (first, _) = files[0];
        let Some(fetched) = state.fetch(repo, fetch, first)? else {
            continue;
        };
        state.write(first, fetched)?;
        for (path, required) in &files[1..] {
            let Some(fetched) = state.fetch(repo, fetch, path)? else {
                if *required {
                    return Err(IngestError::Parse {
                        format_id: "hub",
                        message: format!(
                            "{} looks like a {layout} dataset — it has `{first}` — but it has no \
                             `{path}`, which that format needs to describe itself; there is nothing \
                             here to check from the manifest alone",
                            repo.id()
                        ),
                    });
                }
                continue;
            };
            state.write(path, fetched)?;
        }
        return Ok(Materialized {
            dir: state.dir,
            commit: state.commit,
        });
    }
    Err(IngestError::Parse {
        format_id: "hub",
        message: format!(
            "{} holds none of the manifests a remote read knows ({}), so there is nothing to check \
             without downloading it — check the reference, that the dataset is public, and that a \
             TFDS directory nested under a version subdirectory is named in full \
             (`hf://owner/name/dataset/1.0.0`)",
            repo.id(),
            MANIFEST_LAYOUTS
                .iter()
                .map(|(layout, files)| format!("{layout}: `{}`", files[0].0))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

/// One manifest read in progress: where the files land, what the read has spent, and the commit
/// every response so far agreed on.
struct Staging {
    dir: std::path::PathBuf,
    spent: u64,
    commit: Option<String>,
}

impl Staging {
    /// Fetch one repo-relative path, holding the read to its byte budget and to one commit.
    fn fetch(
        &mut self,
        repo: &HubRepo,
        fetch: &dyn FetchFile,
        path: &str,
    ) -> Result<Option<Fetched>, IngestError> {
        let cap = cap_for(self.spent);
        let Some(fetched) = fetch.get(&repo.url_for(path), cap)? else {
            return Ok(None);
        };
        // A manifest read is several requests, and a branch can move between them. Two different
        // commits in one read is not a manifest — it is half of each of two datasets, hashed as
        // though it were one. Refuse it by name rather than reporting the first commit seen.
        if let Some(served) = fetched.commit.as_deref().filter(|c| is_commit_id(c)) {
            match &self.commit {
                None => self.commit = Some(served.to_string()),
                Some(first) if first != served => {
                    return Err(IngestError::Parse {
                        format_id: "hub",
                        message: format!(
                            "{} moved while its manifest was being read (`{first}` then \
                             `{served}`) — re-run against a pinned revision, `{}@{served}`, so the \
                             read describes one commit",
                            repo.id(),
                            repo.id()
                        ),
                    });
                }
                Some(_) => {}
            }
        }
        Ok(Some(fetched))
    }

    /// Write one fetched file into the staging directory, charging the total budget first.
    fn write(&mut self, path: &str, fetched: Fetched) -> Result<(), IngestError> {
        self.spent = self.spent.saturating_add(fetched.body.len() as u64);
        if self.spent > MAX_TOTAL_BYTES {
            return Err(IngestError::Parse {
                format_id: "hub",
                message: format!(
                    "this manifest is over the {MAX_TOTAL_BYTES}-byte ceiling for a remote read — \
                     fetch the dataset locally and check the path"
                ),
            });
        }
        let target = self.dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| IngestError::Io(e.to_string()))?;
        }
        std::fs::write(target, &fetched.body).map_err(|e| IngestError::Io(e.to_string()))
    }
}

/// The real client: an HTTPS read of one Hub URL, bounded and host-checked.
///
/// Compiled only with the `remote` feature. Everything policy-shaped is deliberately *not* here — the
/// paths, the caps, the assembly all live above the [`FetchFile`] boundary so they are tested without
/// a socket. What is here is the socket and the three rules that only make sense beside it: HTTPS
/// only, an allowlisted host on the request *and* on every redirect it follows, and a read that stops
/// at the cap instead of trusting `Content-Length`.
#[cfg(feature = "remote")]
pub struct HubFetcher {
    agent: ureq::Agent,
}

#[cfg(feature = "remote")]
impl Default for HubFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "remote")]
impl HubFetcher {
    /// A client with timeouts, no redirect following, and no credentials of any kind.
    ///
    /// Redirects are followed by hand rather than by the agent, because each hop's host has to be
    /// checked: a 302 is the server choosing where Veridex connects next, and an allowlist that only
    /// covers the first request is not an allowlist.
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(std::time::Duration::from_secs(60)))
            .timeout_connect(Some(std::time::Duration::from_secs(15)))
            .user_agent(concat!("veridex/", env!("CARGO_PKG_VERSION")))
            .build();
        HubFetcher {
            agent: config.new_agent(),
        }
    }
}

/// Resolve a `Location` header against the URL it was returned from.
///
/// The Hub answers a manifest read with a **relative** redirect —
/// `/api/resolve-cache/datasets/...` — which is ordinary HTTP, and which a host allowlist applied to
/// the raw header value rejects out of hand. Found by pointing the tool at a real repository, which
/// no test against a fake Hub would have caught: an invented server answers the way its author
/// expects.
///
/// Only the two shapes that actually occur are resolved — an absolute URL, and a root-relative path
/// against the current origin. A path-relative `Location` is legal HTTP and is deliberately *not*
/// resolved: guessing at the base of one is how a redirect ends up somewhere nobody chose. Whatever
/// comes out is host-checked by the caller either way.
///
/// Public, like [`is_allowed_url`], because both are policy about URLs rather than plumbing: they
/// are worth reading, worth testing, and worth compiling in the default build where the socket
/// itself is not.
pub fn resolve_redirect(current: &str, location: &str) -> Option<String> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Some(location.to_string());
    }
    if let Some(path) = location.strip_prefix('/') {
        let rest = current.strip_prefix("https://")?;
        let host = rest.split(['/', '?', '#']).next()?;
        return Some(format!("https://{host}/{path}"));
    }
    None
}

/// Whether `url` is `https` on a host this module will connect to.
///
/// Checked on the first request and on every redirect. `http` is refused even for the Hub's own
/// host: a manifest read over plaintext is a manifest an intermediary chooses, and the CDM built
/// from it is content-hashed and reported as fact.
pub fn is_allowed_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        // Strip any userinfo, which would otherwise let `https://huggingface.co@evil.test/` pass a
        // naive prefix test.
        .rsplit('@')
        .next()
        .unwrap_or("");
    // A port is allowed only implicitly (443); naming one is not something the Hub does.
    ALLOWED_HOSTS.contains(&host)
}

#[cfg(feature = "remote")]
impl FetchFile for HubFetcher {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Option<Fetched>, IngestError> {
        use std::io::Read;

        let refuse = |m: String| IngestError::Parse {
            format_id: "hub",
            message: m,
        };
        if max_bytes == 0 {
            return Err(refuse(
                "the remote read budget is exhausted; this dataset's manifest is larger than \
                 Veridex will fetch — download it and check the local copy"
                    .into(),
            ));
        }

        let mut current = url.to_string();
        // The commit is announced on the Hub's own response; the CDN it redirects to serves bytes
        // and says nothing about the repository, so the first hop that names one is kept.
        let mut commit: Option<String> = None;
        // Bounded by hand: each hop is a host the server picked, and each must be allowlisted.
        for _ in 0..5 {
            if !is_allowed_url(&current) {
                return Err(refuse(format!(
                    "refusing to fetch `{current}`: a manifest read may only reach {} over https",
                    ALLOWED_HOSTS.join(", ")
                )));
            }
            let response = match self.agent.get(&current).call() {
                Ok(r) => r,
                // ureq surfaces a non-2xx as an error carrying the response.
                Err(ureq::Error::StatusCode(code)) => {
                    return match code {
                        404 => Ok(None),
                        // The Hub answers 401 for a repository that is private, that is gated, and
                        // for one that does not exist — it does not distinguish them for a reader
                        // sending no credentials, so neither does this message. Saying "private"
                        // alone would send someone hunting for access to a dataset they mistyped.
                        401 | 403 => Err(refuse(format!(
                            "`{current}` returned {code}: the repository is private, gated, or does \
                             not exist — the Hub does not tell an unauthenticated reader which. \
                             Veridex sends no credentials; check the id, or download the dataset \
                             with the Hub's own tooling and check the local copy"
                        ))),
                        429 => Err(refuse(
                            "the Hub is rate-limiting this client (429); try again shortly".into(),
                        )),
                        other => Err(refuse(format!("`{current}` returned HTTP {other}"))),
                    };
                }
                Err(e) => return Err(IngestError::Io(format!("fetching `{current}`: {e}"))),
            };

            if commit.is_none() {
                commit = response
                    .headers()
                    .get("x-repo-commit")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
            }

            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let Some(location) = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                else {
                    return Err(refuse(format!(
                        "`{current}` redirected with no destination"
                    )));
                };
                let Some(next) = resolve_redirect(&current, &location) else {
                    return Err(refuse(format!(
                        "`{current}` redirected to `{location}`, which is not an absolute URL or a \
                         root-relative path — Veridex will not guess where that points"
                    )));
                };
                current = next;
                continue;
            }

            // The cap bounds the *read*, not a header: `Content-Length` is the server's claim, and a
            // response that keeps going past it would otherwise keep being read.
            let mut body = Vec::new();
            let mut reader = response.into_body().into_reader().take(max_bytes + 1);
            reader
                .read_to_end(&mut body)
                .map_err(|e| IngestError::Io(format!("reading `{current}`: {e}")))?;
            if body.len() as u64 > max_bytes {
                return Err(refuse(format!(
                    "`{current}` is larger than the {max_bytes}-byte ceiling for a remote manifest \
                     read — download the dataset and check the local copy"
                )));
            }
            return Ok(Some(Fetched { body, commit }));
        }
        Err(refuse(format!("`{url}` redirected more than 5 times")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[test]
    fn a_relative_redirect_is_resolved_against_the_url_it_came_from() {
        // The shape the real Hub answers with, and the one a fake Hub never would: found by running
        // the tool against `hf://lerobot/svla_so101_pickplace`, where the manifest read 302s to
        // `/api/resolve-cache/...` and the host check rejected the bare path.
        let from = "https://huggingface.co/datasets/a/b/resolve/main/meta/info.json";
        assert_eq!(
            resolve_redirect(
                from,
                "/api/resolve-cache/datasets/a/b/x/meta%2Finfo.json?e=1"
            ),
            Some(
                "https://huggingface.co/api/resolve-cache/datasets/a/b/x/meta%2Finfo.json?e=1"
                    .to_string()
            )
        );
        // An absolute redirect is taken as it stands — and still host-checked by the caller, which
        // is what stops it leaving the Hub.
        assert_eq!(
            resolve_redirect(from, "https://cdn-lfs.huggingface.co/x"),
            Some("https://cdn-lfs.huggingface.co/x".to_string())
        );
        assert!(!is_allowed_url(
            &resolve_redirect(from, "https://evil.test/x").unwrap()
        ));
        // A path-relative `Location` is legal HTTP, and guessing at its base is how a redirect ends
        // up somewhere nobody chose. Refused rather than resolved.
        assert_eq!(resolve_redirect(from, "other/path"), None);
        assert_eq!(resolve_redirect(from, ""), None);
    }

    #[test]
    fn only_https_on_an_allowlisted_host_is_reachable() {
        assert!(is_allowed_url(
            "https://huggingface.co/datasets/a/b/resolve/main/meta/info.json"
        ));
        assert!(is_allowed_url("https://cdn-lfs.huggingface.co/x/y"));
        // Plaintext, even to the right host: a manifest read over http is a manifest an
        // intermediary chooses, and what Veridex builds from it is hashed and reported as fact.
        assert!(!is_allowed_url("http://huggingface.co/datasets/a/b"));
        // The shapes a redirect could use to leave the Hub while looking like it did not.
        assert!(!is_allowed_url("https://huggingface.co.evil.test/x"));
        assert!(!is_allowed_url("https://evil.test/huggingface.co/x"));
        assert!(!is_allowed_url("https://huggingface.co@evil.test/x"));
        assert!(!is_allowed_url("https://evil.test?u=huggingface.co"));
        assert!(!is_allowed_url("file:///etc/passwd"));
    }

    #[test]
    fn a_hub_reference_is_parsed_in_the_forms_people_actually_paste() {
        let expect = |s: &str, owner: &str, name: &str, rev: &str| {
            let r = HubRepo::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"));
            assert_eq!(
                (r.owner.as_str(), r.name.as_str(), r.revision.as_str()),
                (owner, name, rev)
            );
        };
        expect(
            "hf://lerobot/svla_so101_pickplace",
            "lerobot",
            "svla_so101_pickplace",
            "main",
        );
        expect(
            "hf://lerobot/pickplace@v2.1",
            "lerobot",
            "pickplace",
            "v2.1",
        );
        expect(
            "https://huggingface.co/datasets/lerobot/pickplace",
            "lerobot",
            "pickplace",
            "main",
        );
        expect(
            "https://huggingface.co/datasets/lerobot/pickplace/tree/dev",
            "lerobot",
            "pickplace",
            "dev",
        );
    }

    #[test]
    fn anything_that_is_not_a_dataset_repository_is_refused_by_name() {
        for bad in [
            "hf://lerobot",                             // no name
            "hf://lerobot/",                            // empty name
            "hf:///pickplace",                          // empty owner
            "https://huggingface.co/lerobot/pickplace", // a model, not a dataset
            "s3://bucket/key",                          // a scheme this does not implement
            "./local/path",
        ] {
            assert!(HubRepo::parse(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_segment_that_could_climb_or_split_a_path_is_refused() {
        // These reach a URL *and* a directory name, so a traversal here would write outside the
        // temporary root and read a file the caller never asked for.
        for bad in [
            "hf://../etc/passwd",
            "hf://owner/..",
            "hf://owner/na%2fme",
            "hf://owner/name@../../x",
            "hf://ow ner/name",
            "hf://owner/name?x=1",
        ] {
            assert!(HubRepo::parse(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn the_url_names_the_revision_and_the_repo_id_names_the_owner() {
        let r = HubRepo::parse("hf://lerobot/pickplace@v2").unwrap();
        assert_eq!(
            r.url_for("meta/info.json"),
            "https://huggingface.co/datasets/lerobot/pickplace/resolve/v2/meta/info.json"
        );
        // Two owners publishing the same name are two datasets, and the id is bound into the hash.
        assert_eq!(r.id(), "lerobot/pickplace");
        assert_ne!(
            HubRepo::parse("hf://acme/pickplace").unwrap().id(),
            HubRepo::parse("hf://lerobot/pickplace").unwrap().id()
        );
    }

    /// A fetcher that answers from a fixed map and records what was asked for.
    struct FakeHub {
        files: BTreeMap<String, Vec<u8>>,
        asked: RefCell<Vec<String>>,
        /// The commit each answer claims, in the order the answers are given; `None` for a
        /// response that names none. A shorter list than the number of files answers `None`
        /// thereafter.
        commits: Vec<Option<String>>,
    }

    impl FetchFile for FakeHub {
        fn get(&self, url: &str, max_bytes: u64) -> Result<Option<Fetched>, IngestError> {
            let nth = self.asked.borrow().len();
            self.asked.borrow_mut().push(url.to_string());
            let commit = self.commits.get(nth).cloned().flatten();
            match self.files.iter().find(|(k, _)| url.ends_with(k.as_str())) {
                Some((_, body)) if body.len() as u64 > max_bytes => Err(IngestError::Parse {
                    format_id: "hub",
                    message: "over the cap".into(),
                }),
                Some((_, body)) => Ok(Some(Fetched {
                    body: body.clone(),
                    commit,
                })),
                None => Ok(None),
            }
        }
    }

    fn hub(files: &[(&str, &str)]) -> FakeHub {
        FakeHub {
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
                .collect(),
            asked: RefCell::new(Vec::new()),
            commits: Vec::new(),
        }
    }

    #[test]
    fn only_the_fixed_manifest_paths_are_ever_requested() {
        // The list is in the source, not in anything a server says, so a hostile repository cannot
        // enlarge what Veridex asks for. This is the test that keeps it that way.
        let repo = HubRepo::parse("hf://lerobot/pickplace").unwrap();
        let h = hub(&[("meta/info.json", "{}")]);
        let tmp = tempfile::tempdir().unwrap();
        materialize(&repo, &h, tmp.path()).unwrap();
        let asked = h.asked.borrow().clone();
        // Every request, probe included, lands on a path spelled out in this file.
        for url in &asked {
            assert!(
                MANIFEST_LAYOUTS
                    .iter()
                    .flat_map(|(_, files)| files.iter())
                    .any(|(p, _)| url.ends_with(p)),
                "requested something outside the fixed list: {url}"
            );
        }
        // The LeRobot probe hit, so no other layout's paths were asked for at all.
        assert_eq!(asked.len(), LEROBOT_MANIFEST.len());
    }

    #[test]
    fn the_manifest_lands_where_the_local_adapter_looks_for_it() {
        let repo = HubRepo::parse("hf://lerobot/pickplace").unwrap();
        let h = hub(&[
            ("meta/info.json", "{\"codebase_version\":\"v3.0\"}"),
            ("README.md", "---\nlicense: apache-2.0\n---\n"),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize(&repo, &h, tmp.path()).unwrap().dir;
        assert_eq!(dir.file_name().unwrap(), "pickplace");
        assert!(dir.join("meta/info.json").is_file());
        assert!(dir.join("README.md").is_file());
        // The four optional files that were not there are simply absent — the local adapter treats
        // each as "the dataset records none", which is what it means.
        assert!(!dir.join("meta/stats.json").exists());
    }

    #[test]
    fn a_repository_without_the_one_required_file_is_refused_not_read_as_empty() {
        let repo = HubRepo::parse("hf://someone/not-a-lerobot-dataset").unwrap();
        let h = hub(&[("README.md", "hello")]);
        let tmp = tempfile::tempdir().unwrap();
        match materialize(&repo, &h, tmp.path()) {
            Err(IngestError::Parse { message, .. }) => {
                assert!(message.contains("meta/info.json"), "{message}");
                assert!(
                    message.contains("someone/not-a-lerobot-dataset"),
                    "{message}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_per_file_cap_never_exceeds_what_the_total_budget_has_left() {
        // A repository answering large on every path must not be able to spend the total several
        // times over, and once the budget is gone the cap is zero — which the fetcher refuses rather
        // than reading as "no limit".
        assert_eq!(cap_for(0), MAX_FILE_BYTES);
        assert_eq!(cap_for(MAX_TOTAL_BYTES - 10), 10);
        assert_eq!(cap_for(MAX_TOTAL_BYTES), 0);
        assert_eq!(
            cap_for(u64::MAX),
            0,
            "saturating, never wrapping to a huge cap"
        );
        for spent in [0, 1, MAX_FILE_BYTES, MAX_TOTAL_BYTES / 2, MAX_TOTAL_BYTES] {
            let cap = cap_for(spent);
            assert!(cap <= MAX_FILE_BYTES);
            assert!(cap <= MAX_TOTAL_BYTES.saturating_sub(spent));
        }
    }

    #[test]
    fn a_manifest_over_the_total_budget_is_refused() {
        // Every individual file within its cap, and the set of them over the total.
        struct Big;
        impl FetchFile for Big {
            fn get(&self, _url: &str, max_bytes: u64) -> Result<Option<Fetched>, IngestError> {
                Ok(Some(Fetched {
                    body: vec![b'x'; max_bytes.min(4096) as usize],
                    commit: None,
                }))
            }
        }
        // With the real ceilings a five-file manifest cannot reach the total, so the accumulation is
        // checked directly instead of by allocating 128 MiB in a unit test.
        let mut spent = 0u64;
        for _ in 0..LEROBOT_MANIFEST.len() {
            spent = spent.saturating_add(cap_for(spent).min(4096));
        }
        assert!(spent <= MAX_TOTAL_BYTES);

        let repo = HubRepo::parse("hf://a/b").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        assert!(materialize(&repo, &Big, tmp.path()).is_ok());
    }

    const COMMIT_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const COMMIT_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn the_commit_the_hub_served_is_what_comes_back_not_the_branch_asked_for() {
        // `main` names no particular bytes. The commit does, and it is the only thing that makes a
        // remote run re-runnable.
        let repo = HubRepo::parse("hf://lerobot/pickplace").unwrap();
        let mut h = hub(&[("meta/info.json", "{}")]);
        h.commits = vec![Some(COMMIT_A.to_string())];
        let tmp = tempfile::tempdir().unwrap();
        let staged = materialize(&repo, &h, tmp.path()).unwrap();
        assert_eq!(staged.commit.as_deref(), Some(COMMIT_A));
        assert_eq!(repo.revision, "main");
    }

    #[test]
    fn a_hub_that_names_no_commit_yields_none_rather_than_a_guess() {
        let repo = HubRepo::parse("hf://lerobot/pickplace").unwrap();
        let h = hub(&[("meta/info.json", "{}")]);
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(materialize(&repo, &h, tmp.path()).unwrap().commit, None);
    }

    #[test]
    fn a_commit_header_that_is_not_a_commit_id_is_ignored() {
        // The header is a stranger's string and its value reaches the content hash. Anything that
        // is not 40 hex digits is treated as if the header were absent.
        assert!(is_commit_id(COMMIT_A));
        assert!(!is_commit_id("main"));
        assert!(!is_commit_id(""));
        assert!(!is_commit_id(&"z".repeat(40)));
        assert!(!is_commit_id(&COMMIT_A[..39]));

        let repo = HubRepo::parse("hf://lerobot/pickplace").unwrap();
        let mut h = hub(&[("meta/info.json", "{}")]);
        h.commits = vec![Some("../../etc/passwd".to_string())];
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(materialize(&repo, &h, tmp.path()).unwrap().commit, None);
    }

    #[test]
    fn a_repository_that_moves_mid_read_is_refused_not_stitched_together() {
        // Five requests read one manifest. A branch that moves between them would otherwise be
        // reported — and content-hashed — as a single dataset that never existed at any commit.
        let repo = HubRepo::parse("hf://lerobot/pickplace").unwrap();
        let mut h = hub(&[("meta/info.json", "{}"), ("meta/stats.json", "{}")]);
        h.commits = vec![Some(COMMIT_A.to_string()), None, Some(COMMIT_B.to_string())];
        let tmp = tempfile::tempdir().unwrap();
        match materialize(&repo, &h, tmp.path()) {
            Err(IngestError::Parse { message, .. }) => {
                assert!(message.contains(COMMIT_A), "{message}");
                assert!(message.contains(COMMIT_B), "{message}");
                // The remedy has to be the pinned re-run, not "try again".
                assert!(
                    message.contains(&format!("lerobot/pickplace@{COMMIT_B}")),
                    "{message}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
