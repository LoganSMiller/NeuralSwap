//! The local component cache: fetch once, verify, extract fresh every time.
//!
//! The transport is a trait rather than an HTTP client. Two reasons, and the
//! second is the one that matters: this crate is host-agnostic by design, and
//! network I/O with progress and cancellation is a host concern - but more
//! usefully, every rule worth having here can then be tested without a
//! network. A fake fetcher can serve a changed release, a truncated download
//! or a hostile archive on demand, and none of those are reproducible against
//! a real server.
//!
//! The discipline, which is upstream's and worth copying exactly:
//!
//! 1. Verify the archive **before** unpacking it. A digest checked afterwards
//!    has already let the bytes through.
//! 2. **Re-extract on every install**, into a directory emptied first. Never
//!    trust loose files left in a cache - they are not covered by the digest,
//!    and something else may have touched them since.
//! 3. Cache the *archive*, not the extraction. The archive is the thing the
//!    digest describes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::components::catalog::{Component, Source, Trust};
use crate::components::trust::{TrustStore, Verdict};
use crate::error::{fail, Code, Result};
use crate::jobs::Cancel;

/// What a source resolved to. A moving target has to be asked before it can be
/// fetched, and the version it answers with is what gets recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    pub url: String,
    pub version: String,
}

/// Bytes read so far, and the total when the server declared one.
pub type Progress<'a> = dyn Fn(u64, Option<u64>) + 'a;

/// Everything this module needs from the outside world.
pub trait Fetcher {
    /// Ask a source what it currently offers. For a pinned source this is a
    /// formality; for "the latest release" it is a network call.
    fn resolve(&self, component: &Component) -> Result<Resolved>;

    /// Download `url` to `into`, reporting progress and honouring `cancel`.
    fn download(
        &self,
        url: &str,
        into: &Path,
        cancel: &Cancel,
        progress: &Progress<'_>,
    ) -> Result<()>;
}

/// A component ready to install from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    pub component: String,
    pub version: String,
    /// Where the files are, freshly extracted.
    pub dir: PathBuf,
    pub sha256: String,
    pub trust: Verdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Outcome {
    Ready(Ready),
    /// The publisher is serving different bytes than last time. Stopped
    /// deliberately, with everything a person needs to decide.
    ChangedUpstream {
        component: String,
        version: String,
        url: String,
        sha256: String,
        size: u64,
        trust: Verdict,
        explanation: String,
    },
    /// Nothing to fetch. Bundled with us, or supplied by the user.
    NotFetched {
        component: String,
        reason: String,
    },
}

/// Whether a cached archive may be reused.
///
/// This is a real decision rather than an optimisation, and leaving it
/// implicit hid a hole: the archive is cached under its resolved version, so
/// reusing it always means the trust comparison only ever runs on the very
/// first download. That is safe - the bytes cannot change under us - but it
/// also means a release quietly replaced upstream is never noticed, which is
/// the one thing the trust record exists to do.
///
/// So the default reuses the cache and stays cheap, and [`Freshness::Recheck`]
/// downloads again specifically to compare. A "verify my components" action
/// uses the latter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Freshness {
    /// Reuse an archive already on disk when it matches what was recorded.
    UseCache,
    /// Fetch again even when cached, and compare against the record.
    Recheck,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Where the verified archive for one component version lives.
    fn archive_path(&self, id: &str, version: &str) -> PathBuf {
        // The version is part of the filename rather than a directory, so a
        // stale extraction cannot be mistaken for a current one.
        self.root
            .join("archives")
            .join(format!("{}-{}.bin", safe(id), safe(version)))
    }

    /// Where its contents are unpacked. Emptied and rewritten every time.
    pub fn contents_path(&self, id: &str, version: &str) -> PathBuf {
        self.root
            .join("components")
            .join(safe(id))
            .join(safe(version))
    }

    /// Make a component available, fetching it if necessary.
    pub fn ensure(
        &self,
        component: &Component,
        fetcher: &dyn Fetcher,
        cancel: &Cancel,
        freshness: Freshness,
        progress: &Progress<'_>,
    ) -> Result<Outcome> {
        match &component.source {
            Source::Bundled { rel } => {
                return Ok(Outcome::NotFetched {
                    component: component.id.clone(),
                    reason: format!("shipped with NeuralSwap, at {rel}"),
                })
            }
            Source::LocalOnly { hint } => {
                return Ok(Outcome::NotFetched {
                    component: component.id.clone(),
                    reason: format!("found on your machine rather than downloaded: {hint}"),
                })
            }
            _ => {}
        }

        let resolved = fetcher.resolve(component)?;
        if !resolved.url.starts_with("https://") {
            return fail(
                Code::BadRequest,
                format!(
                    "{} resolved to something other than HTTPS: {}",
                    component.id, resolved.url
                ),
            );
        }

        let archive = self.archive_path(&component.id, &resolved.version);
        let mut trust = TrustStore::load(&self.root)?;

        // A cached archive is reusable only if it still matches what is on
        // record. Anything else - a truncated download, a file something else
        // wrote over - is discarded and fetched again.
        let cached = match freshness {
            Freshness::Recheck => None,
            Freshness::UseCache => match crate::hash::hash_file(&archive) {
                Ok(digest) if self.cached_is_usable(component, &trust, &resolved, &digest) => {
                    Some(digest)
                }
                _ => None,
            },
        };

        let digest = match cached {
            Some(digest) => digest,
            None => {
                if cancel.is_cancelled() {
                    return fail(Code::JobCancelled, "cancelled before downloading");
                }
                let parent = archive.parent().unwrap_or(&self.root);
                std::fs::create_dir_all(parent).map_err(|error| {
                    crate::Error::new(
                        Code::StateUnwritable,
                        format!("could not create {}: {error}", parent.display()),
                    )
                })?;

                // Downloaded to a temporary name and renamed only once it has
                // been hashed, so a cancelled or failed transfer never leaves
                // something that looks like a cached archive.
                let partial = archive.with_extension("part");
                let _ = std::fs::remove_file(&partial);
                fetcher.download(&resolved.url, &partial, cancel, progress)?;

                let digest = crate::hash::hash_file(&partial)?;
                if let Source::Pinned { sha256, .. } = &component.source {
                    // A pin is checked here and nowhere else matters: if this
                    // fails the bytes are simply wrong and there is no
                    // judgement to make.
                    if let Err(error) = crate::hash::verify(&partial, &digest, sha256) {
                        let _ = std::fs::remove_file(&partial);
                        return Err(error);
                    }
                }
                std::fs::rename(&partial, &archive).map_err(|error| {
                    crate::Error::new(
                        Code::StateUnwritable,
                        format!("could not store {}: {error}", archive.display()),
                    )
                })?;
                digest
            }
        };

        let size = std::fs::metadata(&archive)
            .map(|meta| meta.len())
            .unwrap_or(0);

        // A pinned source is already settled. A moving target is compared
        // against what was recorded, and a change stops here rather than being
        // waved through or silently refused.
        let verdict = match component.source.trust() {
            Trust::Pinned => Verdict::Pinned,
            _ => trust.check(&component.id, &resolved.version, &digest),
        };
        if !verdict.is_acceptable() {
            return Ok(Outcome::ChangedUpstream {
                component: component.id.clone(),
                version: resolved.version,
                url: resolved.url,
                explanation: verdict.explain(),
                sha256: digest,
                size,
                trust: verdict,
            });
        }
        if component.source.trust() != Trust::Pinned {
            trust.remember(
                &component.id,
                &resolved.version,
                &digest,
                &resolved.url,
                size,
                now_millis(),
            )?;
            trust.save(&self.root)?;
        }

        let dir = self.unpack(component, &resolved.version, &archive)?;
        Ok(Outcome::Ready(Ready {
            component: component.id.clone(),
            version: resolved.version,
            dir,
            sha256: digest,
            trust: verdict,
        }))
    }

    /// Whether a digest already on disk is one we are willing to reuse.
    fn cached_is_usable(
        &self,
        component: &Component,
        trust: &TrustStore,
        resolved: &Resolved,
        digest: &str,
    ) -> bool {
        match &component.source {
            Source::Pinned { sha256, .. } => crate::hash::matches(digest, sha256),
            _ => trust
                .get(&component.id, &resolved.version)
                .is_some_and(|record| crate::hash::matches(&record.sha256, digest)),
        }
    }

    /// Empty the contents directory and unpack the verified archive into it.
    ///
    /// Emptied first, always. A cached extraction is not covered by the digest
    /// and may have been altered since it was written, so the only safe copy
    /// is the one made from bytes just verified.
    fn unpack(&self, component: &Component, version: &str, archive: &Path) -> Result<PathBuf> {
        let dir = self.contents_path(&component.id, version);
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return fail(
                    Code::StateUnwritable,
                    format!("could not clear {}: {error}", dir.display()),
                );
            }
        }
        std::fs::create_dir_all(&dir).map_err(|error| {
            crate::Error::new(
                Code::StateUnwritable,
                format!("could not create {}: {error}", dir.display()),
            )
        })?;

        // Not everything is an archive. An add-on is a single DLL and a shader
        // header is a single text file, and treating those as ZIPs would
        // simply fail. The URL's own extension decides.
        if looks_like_an_archive(&component.source) {
            crate::zip::extract::extract_zip(
                archive,
                &dir,
                crate::zip::extract::Limits::default(),
            )?;
        } else {
            let name = single_file_name(&component.source)
                .unwrap_or_else(|| format!("{}.bin", component.id));
            // Through the path validator, because the name came off a URL.
            let target = crate::fsx::paths::assert_safe_relative(&name, &dir)?;
            std::fs::copy(archive, &target).map_err(|error| {
                crate::Error::new(
                    Code::StateUnwritable,
                    format!("could not place {}: {error}", target.display()),
                )
            })?;
        }
        Ok(dir)
    }
}

/// Whether this source serves an archive or a single file.
fn looks_like_an_archive(source: &Source) -> bool {
    match source {
        // A branch download is always a zip.
        Source::GitHubBranch { .. } => true,
        Source::GitHubLatest { asset_suffix, .. } => {
            asset_suffix.eq_ignore_ascii_case(".zip") || asset_suffix.eq_ignore_ascii_case(".7z")
        }
        Source::Pinned { url, .. } | Source::Official { template: url, .. } => {
            let lower = url.to_lowercase();
            // ReShade's installer is an executable with a ZIP appended, which
            // our reader handles - see `zip::read` and the self-extracting
            // tests.
            lower.ends_with(".zip") || lower.ends_with(".exe")
        }
        Source::Bundled { .. } | Source::LocalOnly { .. } => false,
    }
}

/// The filename to give a single-file download.
fn single_file_name(source: &Source) -> Option<String> {
    let url = match source {
        Source::Pinned { url, .. } => url.clone(),
        Source::GitHubLatest { asset_suffix, .. } => return Some(format!("asset{asset_suffix}")),
        Source::Official { template, .. } => template.clone(),
        _ => return None,
    };
    let name = url.rsplit('/').next()?;
    let clean: String = name
        .chars()
        .take_while(|character| *character != '?' && *character != '#')
        .collect();
    (!clean.is_empty()).then_some(clean)
}

/// A path segment that cannot escape or collide.
fn safe(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '.' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::catalog::{Licence, Role};
    use std::cell::RefCell;

    /// A fetcher that serves whatever the test tells it to, so a changed
    /// release or a corrupt download is reproducible - neither of which can be
    /// arranged against a real server.
    struct Fake {
        version: String,
        body: RefCell<Vec<u8>>,
        calls: RefCell<u32>,
    }

    impl Fake {
        fn new(body: &[u8]) -> Self {
            Self {
                version: "1.0.0".to_owned(),
                body: RefCell::new(body.to_vec()),
                calls: RefCell::new(0),
            }
        }
        fn serve(&self, body: &[u8]) {
            *self.body.borrow_mut() = body.to_vec();
        }
        fn downloads(&self) -> u32 {
            *self.calls.borrow()
        }
    }

    impl Fetcher for Fake {
        fn resolve(&self, _component: &Component) -> Result<Resolved> {
            Ok(Resolved {
                url: "https://example.test/thing.addon64".to_owned(),
                version: self.version.clone(),
            })
        }
        fn download(
            &self,
            _url: &str,
            into: &Path,
            _cancel: &Cancel,
            progress: &Progress<'_>,
        ) -> Result<()> {
            *self.calls.borrow_mut() += 1;
            let body = self.body.borrow().clone();
            progress(body.len() as u64, Some(body.len() as u64));
            std::fs::write(into, &body).expect("fake download");
            Ok(())
        }
    }

    fn moving_component() -> Component {
        Component {
            id: "test-addon".to_owned(),
            name: "Test Add-on".to_owned(),
            summary: String::new(),
            role: Role::Addon,
            licence: Licence::Mit,
            homepage: "https://example.test".to_owned(),
            source: Source::GitHubLatest {
                repo: "someone/thing".to_owned(),
                asset_suffix: ".addon64".to_owned(),
            },
            requires: Vec::new(),
            experimental: false,
        }
    }

    fn nothing(_read: u64, _total: Option<u64>) {}

    /// A known-good archive, taken from the generated vectors rather than
    /// hand-assembled, so a failure here is about the store and not about a
    /// header somebody typed.
    fn real_archive() -> Vec<u8> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../spec/zip/benign.zip.bin")
            .canonicalize()
            .expect("spec/zip/benign.zip.bin - run `npm run vectors`");
        std::fs::read(fixture).expect("read the fixture")
    }

    #[test]
    fn a_first_fetch_downloads_verifies_and_unpacks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let fetcher = Fake::new(b"the add-on bytes");

        let outcome = store
            .ensure(
                &moving_component(),
                &fetcher,
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect("ensure");
        let ready = match outcome {
            Outcome::Ready(ready) => ready,
            other => panic!("expected Ready, got {other:?}"),
        };
        assert_eq!(ready.trust, Verdict::FirstSighting);
        assert_eq!(ready.sha256, crate::hash::hash_bytes(b"the add-on bytes"));
        // A single file, placed rather than extracted.
        assert!(ready.dir.join("asset.addon64").is_file());
    }

    #[test]
    fn a_second_fetch_reuses_the_cached_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let fetcher = Fake::new(b"same bytes");
        let component = moving_component();

        store
            .ensure(
                &component,
                &fetcher,
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect("first");
        let second = store
            .ensure(
                &component,
                &fetcher,
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect("second");

        assert_eq!(
            fetcher.downloads(),
            1,
            "the archive should not be refetched"
        );
        match second {
            Outcome::Ready(ready) => {
                assert_eq!(ready.trust, Verdict::Unchanged { confirmations: 0 })
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn a_publisher_serving_new_bytes_stops_the_install() {
        // The whole point of the trust record. The version has not changed, so
        // this is the same release serving different content.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let fetcher = Fake::new(b"the original release");
        let component = moving_component();

        store
            .ensure(
                &component,
                &fetcher,
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect("first");
        fetcher.serve(b"something else entirely");

        // `Recheck`, because that is what the comparison is for. With the
        // cache in play the second call never touches the network and the
        // change could not be seen - which is exactly the hole that writing
        // this test exposed.
        let outcome = store
            .ensure(
                &component,
                &fetcher,
                &Cancel::new(),
                Freshness::Recheck,
                &nothing,
            )
            .expect("ensure returns rather than raising");
        match outcome {
            Outcome::ChangedUpstream {
                explanation, trust, ..
            } => {
                assert!(!trust.is_acceptable());
                assert!(explanation.contains("different bytes"));
            }
            other => panic!("a changed release must stop, got {other:?}"),
        }
    }

    #[test]
    fn a_pinned_source_refuses_bytes_that_do_not_match_its_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let mut component = moving_component();
        component.source = Source::Pinned {
            url: "https://example.test/pinned.zip".to_owned(),
            sha256: crate::hash::hash_bytes(b"what we expect"),
            size: 14,
        };

        let wrong = Fake::new(b"not what we expect");
        let refused = store
            .ensure(
                &component,
                &wrong,
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect_err("a pin mismatch is simply wrong");
        assert_eq!(refused.code, Code::VerifyFailed);

        // Nothing was kept: a failed transfer must not leave something that
        // looks like a cached archive.
        let archive = store.archive_path(&component.id, "1.0.0");
        assert!(!archive.exists());
        assert!(!archive.with_extension("part").exists());
    }

    #[test]
    fn a_pinned_source_accepts_the_bytes_it_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let mut component = moving_component();
        let body = real_archive();
        component.source = Source::Pinned {
            url: "https://example.test/pinned.zip".to_owned(),
            sha256: crate::hash::hash_bytes(&body),
            size: body.len() as u64,
        };

        let outcome = store
            .ensure(
                &component,
                &Fake::new(&body),
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect("ensure");
        match outcome {
            Outcome::Ready(ready) => {
                assert_eq!(ready.trust, Verdict::Pinned);
                // A real archive, so it was extracted rather than placed.
                assert!(ready.dir.join("readme.txt").is_file());
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn the_contents_directory_is_emptied_before_every_unpack() {
        // A cached extraction is not covered by the digest, so anything left
        // in it must not survive.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let component = moving_component();
        let fetcher = Fake::new(b"bytes");

        store
            .ensure(
                &component,
                &fetcher,
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect("first");
        let contents = store.contents_path(&component.id, "1.0.0");
        std::fs::write(contents.join("smuggled.dll"), b"put here later").expect("write");

        store
            .ensure(
                &component,
                &fetcher,
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect("second");
        assert!(
            !contents.join("smuggled.dll").exists(),
            "anything not in the verified archive must be gone"
        );
        assert!(contents.join("asset.addon64").is_file());
    }

    #[test]
    fn a_bundled_or_local_component_is_not_fetched_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let fetcher = Fake::new(b"never used");

        for (source, expect) in [
            (
                Source::Bundled {
                    rel: "streamline".to_owned(),
                },
                "shipped with NeuralSwap",
            ),
            (
                Source::LocalOnly {
                    hint: "your games".to_owned(),
                },
                "found on your machine",
            ),
        ] {
            let mut component = moving_component();
            component.source = source;
            match store
                .ensure(
                    &component,
                    &fetcher,
                    &Cancel::new(),
                    Freshness::UseCache,
                    &nothing,
                )
                .expect("ensure")
            {
                Outcome::NotFetched { reason, .. } => assert!(reason.contains(expect)),
                other => panic!("expected NotFetched, got {other:?}"),
            }
        }
        assert_eq!(fetcher.downloads(), 0);
    }

    #[test]
    fn cancelling_before_the_download_starts_stops_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let cancel = Cancel::new();
        cancel.cancel();

        let refused = store
            .ensure(
                &moving_component(),
                &Fake::new(b"x"),
                &cancel,
                Freshness::UseCache,
                &nothing,
            )
            .expect_err("cancelled");
        assert_eq!(refused.code, Code::JobCancelled);
    }

    #[test]
    fn progress_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        let seen = RefCell::new(Vec::new());
        let record = |read: u64, total: Option<u64>| seen.borrow_mut().push((read, total));

        store
            .ensure(
                &moving_component(),
                &Fake::new(b"twelve bytes"),
                &Cancel::new(),
                Freshness::UseCache,
                &record,
            )
            .expect("ensure");
        assert_eq!(seen.into_inner(), vec![(12, Some(12))]);
    }

    #[test]
    fn cache_paths_cannot_escape_the_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path());
        // Neither an id nor a version reaches us from a trusted place: both can
        // come from a fetched catalogue or a release tag.
        let archive = store.archive_path("../../evil", "../../../etc");
        assert!(archive.starts_with(dir.path()), "{}", archive.display());
        let contents = store.contents_path("..\\..\\evil", "v1/../..");
        assert!(contents.starts_with(dir.path()), "{}", contents.display());
    }

    #[test]
    fn an_http_url_is_refused_even_if_a_source_somehow_offers_one() {
        struct Insecure;
        impl Fetcher for Insecure {
            fn resolve(&self, _component: &Component) -> Result<Resolved> {
                Ok(Resolved {
                    url: "http://example.test/thing".to_owned(),
                    version: "1.0.0".to_owned(),
                })
            }
            fn download(
                &self,
                _url: &str,
                _into: &Path,
                _cancel: &Cancel,
                _progress: &Progress<'_>,
            ) -> Result<()> {
                panic!("must not be reached");
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let refused = Store::new(dir.path())
            .ensure(
                &moving_component(),
                &Insecure,
                &Cancel::new(),
                Freshness::UseCache,
                &nothing,
            )
            .expect_err("plain HTTP must be refused");
        assert_eq!(refused.code, Code::BadRequest);
    }
}
