//! The HTTP half of [`neuralswap_core::components::store::Fetcher`].
//!
//! The core keeps the transport behind a trait so its rules can be tested
//! without a network. This is the other side: the part that actually talks to
//! the internet, and therefore the part that has to be suspicious.
//!
//! # What it refuses
//!
//! A download is the one place this application takes bytes from someone else
//! and puts them on the user's disk, so every limit here is deliberate:
//!
//! - **HTTPS only**, checked on the initial URL *and* after every redirect.
//!   The catalogue validator already refuses a plain-HTTP source, but a
//!   redirect is chosen by the server rather than by us, and a 302 to `http://`
//!   would otherwise downgrade the connection silently.
//! - **A redirect cap**, so a redirect loop ends in an error rather than a
//!   hang.
//! - **A size cap**, checked against `Content-Length` before the transfer and
//!   against the bytes actually read during it. A server that declares a small
//!   body and then sends gigabytes should not be able to fill a disk.
//! - **A connect timeout**, and a whole-request timeout on *metadata* only.
//!   Not on a download: the neural rendering runtime is around 160 MB, and a
//!   total timeout would cancel a healthy transfer on a slow line.
//!
//! What that leaves, stated rather than glossed: a download whose connection
//! goes quiet without closing will block until the user cancels it. The
//! blocking client in this version exposes no read timeout, and the read loop
//! cannot impose one on itself because the read is what blocks. The cancel
//! token is checked between chunks, so a stalled transfer is interruptible
//! from the UI - it is just not self-limiting.
//!
//! # What it does not do
//!
//! It does not verify digests. That belongs to
//! [`neuralswap_core::components::store`], which checks before unpacking and
//! records trust on first use - and which can be tested against a fake fetcher
//! serving a quietly-changed release, where this code cannot.
//!
//! The download lands on a temporary file beside the target and is renamed
//! only once the stream has completed, so an interrupted transfer never leaves
//! a short file wearing the name of a complete one.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use neuralswap_core::components::catalog::{Component, Source};
use neuralswap_core::components::store::{Fetcher, Progress, Resolved};
use neuralswap_core::error::{fail, Code, Result};
use neuralswap_core::jobs::Cancel;

/// GitHub rejects requests without one, and an honest agent string is how a
/// maintainer works out who is hitting their release page.
const USER_AGENT: &str = concat!(
    "NeuralSwap/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/LoganSMiller/NeuralSwap)"
);

/// Enough for the largest thing the catalogue points at, and far short of
/// anything that would matter to a disk.
///
/// The neural rendering runtime is about 160 MB, ReShade's installer a few,
/// and a branch archive of a shader pack tens. 512 MB leaves room for all of
/// it while still being a bound.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// A body large enough to be a mistake. Release metadata is a few kilobytes.
const MAX_METADATA: u64 = 4 * 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Applied to metadata requests only. A release document that has not arrived
/// in half a minute is not going to.
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 8;

pub struct HttpFetcher {
    client: reqwest::blocking::Client,
}

impl HttpFetcher {
    pub fn new() -> Result<HttpFetcher> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            // Redirects are followed by hand rather than by the client, so
            // each hop's scheme can be checked. A policy that allowed the
            // client to follow them would let a 302 to `http://` through
            // before we ever saw the URL.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| Error::network(format!("could not start the HTTP client: {error}")))?;
        Ok(HttpFetcher { client })
    }

    /// Fetch a URL, following redirects ourselves so every hop is checked.
    ///
    /// `timeout` bounds the whole request and is used for metadata. A download
    /// passes `None`: see the note at the top of the module.
    fn open(&self, url: &str, timeout: Option<Duration>) -> Result<reqwest::blocking::Response> {
        let mut current = url.to_owned();
        for _ in 0..MAX_REDIRECTS {
            require_https(&current)?;
            let mut request = self.client.get(&current);
            if let Some(limit) = timeout {
                request = request.timeout(limit);
            }
            let response = request
                .send()
                .map_err(|error| Error::network(format!("{current}: {error}")))?;

            let status = response.status();
            if status.is_redirection() {
                let Some(location) = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                else {
                    return fail(
                        Code::DownloadRejected,
                        format!("{current} answered {status} with no destination"),
                    );
                };
                // A relative redirect is legal and common.
                current = match reqwest::Url::parse(&current).and_then(|base| base.join(location)) {
                    Ok(joined) => joined.to_string(),
                    Err(error) => {
                        return fail(
                            Code::DownloadRejected,
                            format!("{current} redirected somewhere unreadable: {error}"),
                        )
                    }
                };
                continue;
            }

            if !status.is_success() {
                return fail(
                    Code::DownloadRejected,
                    format!("{current} answered {status}"),
                );
            }
            return Ok(response);
        }
        fail(
            Code::DownloadRejected,
            format!("{url} redirected more than {MAX_REDIRECTS} times"),
        )
    }

    /// A small body, read whole. Used for release metadata.
    fn read_text(&self, url: &str) -> Result<String> {
        let mut response = self.open(url, Some(METADATA_TIMEOUT))?;
        if let Some(declared) = response.content_length() {
            if declared > MAX_METADATA {
                return fail(
                    Code::DownloadRejected,
                    format!("{url} declared {declared} bytes of metadata"),
                );
            }
        }
        let mut body = String::new();
        response
            .by_ref()
            .take(MAX_METADATA)
            .read_to_string(&mut body)
            .map_err(|error| Error::network(format!("{url}: {error}")))?;
        Ok(body)
    }
}

/// Constructors for the two error shapes this module raises, so the mapping
/// from a transport failure to a code is made in one place.
struct Error;

impl Error {
    fn network(detail: String) -> neuralswap_core::error::Error {
        neuralswap_core::error::Error::new(Code::NetworkFailed, detail)
    }
}

fn require_https(url: &str) -> Result<()> {
    if url
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty())
    {
        return Ok(());
    }
    fail(
        Code::DownloadRejected,
        format!("refusing to fetch over anything but HTTPS: {url}"),
    )
}

impl Fetcher for HttpFetcher {
    fn resolve(&self, component: &Component) -> Result<Resolved> {
        match &component.source {
            // Nothing to fetch, and each for a different reason the user needs
            // to hear rather than a generic failure.
            Source::LocalOnly { hint } => fail(
                Code::SourceNotFetchable,
                format!("{} is already on your machine: {hint}", component.name),
            ),
            Source::UserObtained { from, .. } => fail(
                Code::SourceNotFetchable,
                format!(
                    "{} has to be downloaded by hand from {from}",
                    component.name
                ),
            ),
            Source::Bundled { .. } => fail(
                Code::SourceNotFetchable,
                format!("{} ships inside NeuralSwap", component.name),
            ),

            Source::Pinned { url, .. } => Ok(Resolved {
                url: url.clone(),
                // A pin has no version of its own in the catalogue, so the
                // file name is the honest answer - it is what a person would
                // call this download.
                version: file_name_of(url),
            }),

            Source::GitHubBranch { repo, branch } => Ok(Resolved {
                url: format!("https://codeload.github.com/{repo}/zip/refs/heads/{branch}"),
                version: branch.clone(),
            }),

            Source::Official { template, known } => {
                let Some(newest) = known.first() else {
                    return fail(
                        Code::SourceNotFetchable,
                        format!("no known version of {} to fetch", component.name),
                    );
                };
                Ok(Resolved {
                    url: template.replace("{version}", newest),
                    version: newest.clone(),
                })
            }

            Source::GitHubLatest { repo, asset_suffix } => {
                let url = format!("https://api.github.com/repos/{repo}/releases/latest");
                let body = self.read_text(&url)?;
                pick_release_asset(&body, asset_suffix, repo)
            }
        }
    }

    fn download(
        &self,
        url: &str,
        into: &Path,
        cancel: &Cancel,
        progress: &Progress<'_>,
    ) -> Result<()> {
        let mut response = self.open(url, None)?;

        let declared = response.content_length();
        if let Some(total) = declared {
            if total > MAX_DOWNLOAD {
                return fail(
                    Code::DownloadRejected,
                    format!("{url} declared {total} bytes, over the {MAX_DOWNLOAD} byte limit"),
                );
            }
        }

        if let Some(parent) = into.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                neuralswap_core::error::Error::new(
                    Code::StateUnwritable,
                    format!("cannot create {}: {error}", parent.display()),
                )
            })?;
        }

        // Written beside the target and renamed at the end, so a transfer that
        // stops half way never leaves a short file under the real name - which
        // a later run would otherwise find, hash, and report as a mismatch it
        // could not explain.
        let partial = into.with_extension("partial");
        let outcome = stream_to(&mut response, &partial, url, declared, cancel, progress);
        if outcome.is_err() {
            let _ = std::fs::remove_file(&partial);
            return outcome;
        }

        std::fs::rename(&partial, into).map_err(|error| {
            let _ = std::fs::remove_file(&partial);
            neuralswap_core::error::Error::new(
                Code::StateUnwritable,
                format!("cannot put the download at {}: {error}", into.display()),
            )
        })
    }
}

fn stream_to(
    response: &mut reqwest::blocking::Response,
    partial: &Path,
    url: &str,
    declared: Option<u64>,
    cancel: &Cancel,
    progress: &Progress<'_>,
) -> Result<()> {
    let mut file = std::fs::File::create(partial).map_err(|error| {
        neuralswap_core::error::Error::new(
            Code::StateUnwritable,
            format!("cannot write {}: {error}", partial.display()),
        )
    })?;

    // 64 KiB: large enough that the syscall overhead disappears, small enough
    // that a cancel is acted on within a few milliseconds rather than at the
    // end of the file.
    let mut buffer = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;
    progress(0, declared);

    loop {
        if cancel.is_cancelled() {
            return fail(
                Code::JobCancelled,
                format!("cancelled while fetching {url}"),
            );
        }

        let read = response
            .read(&mut buffer)
            .map_err(|error| Error::network(format!("{url}: {error}")))?;
        if read == 0 {
            break;
        }

        written += read as u64;
        // Checked against what actually arrives, not only what was declared:
        // a server is free to send more than its `Content-Length` said, and
        // the point of a cap is to hold when the other side is misbehaving.
        if written > MAX_DOWNLOAD {
            return fail(
                Code::DownloadRejected,
                format!("{url} sent more than the {MAX_DOWNLOAD} byte limit"),
            );
        }

        file.write_all(&buffer[..read]).map_err(|error| {
            neuralswap_core::error::Error::new(
                Code::StateUnwritable,
                format!("cannot write {}: {error}", partial.display()),
            )
        })?;
        progress(written, declared);
    }

    // A truncated transfer that ends cleanly at the socket looks exactly like
    // a complete one to the loop above. The declared length is the only thing
    // that distinguishes them.
    if let Some(total) = declared {
        if written != total {
            return fail(
                Code::DownloadRejected,
                format!("{url} sent {written} bytes of a declared {total}"),
            );
        }
    }

    file.flush().map_err(|error| {
        neuralswap_core::error::Error::new(
            Code::StateUnwritable,
            format!("cannot finish writing {}: {error}", partial.display()),
        )
    })
}

fn file_name_of(url: &str) -> String {
    url.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(url)
        .to_owned()
}

/// Choose the asset from a GitHub `releases/latest` body.
///
/// Split out from the request so the awkward parts - no assets, several
/// matches, a draft with no tag - are testable without a network, which is the
/// same reasoning that put a trait in front of this module in the first place.
fn pick_release_asset(body: &str, asset_suffix: &str, repo: &str) -> Result<Resolved> {
    let parsed: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        neuralswap_core::error::Error::new(
            Code::DownloadRejected,
            format!("{repo}: release metadata was not JSON: {error}"),
        )
    })?;

    let Some(tag) = parsed["tag_name"].as_str() else {
        return fail(
            Code::DownloadRejected,
            format!("{repo}: the latest release has no tag"),
        );
    };

    let assets = parsed["assets"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let matching: Vec<&serde_json::Value> = assets
        .iter()
        .filter(|asset| {
            asset["name"]
                .as_str()
                .is_some_and(|name| name.ends_with(asset_suffix))
        })
        .collect();

    match matching.as_slice() {
        [] => fail(
            Code::DownloadRejected,
            format!("{repo} {tag} has no asset ending in {asset_suffix}"),
        ),
        [only] => {
            let Some(url) = only["browser_download_url"].as_str() else {
                return fail(
                    Code::DownloadRejected,
                    format!("{repo} {tag}: the matching asset has no download URL"),
                );
            };
            require_https(url)?;
            Ok(Resolved {
                url: url.to_owned(),
                version: tag.to_owned(),
            })
        }
        // Ambiguity is refused rather than resolved by picking the first.
        // Which asset a suffix matches decides what gets installed into
        // somebody's game, and "the one that happened to be listed first" is
        // not a rule anybody agreed to.
        many => {
            let names: Vec<&str> = many
                .iter()
                .filter_map(|asset| asset["name"].as_str())
                .collect();
            fail(
                Code::DownloadRejected,
                format!(
                    "{repo} {tag} has {} assets ending in {asset_suffix} ({}), so the catalogue \
                     entry does not identify one",
                    many.len(),
                    names.join(", ")
                ),
            )
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn only_https_is_accepted() {
        assert!(require_https("https://example.com/a.zip").is_ok());
        for bad in [
            "http://example.com/a.zip",
            "ftp://example.com/a.zip",
            "file:///C:/evil.dll",
            "//example.com/a.zip",
            "https://",
            "",
        ] {
            let refused = require_https(bad).expect_err(bad);
            assert_eq!(refused.code, Code::DownloadRejected, "{bad}");
        }
    }

    fn release(tag: &str, assets: &[(&str, &str)]) -> String {
        let listed: Vec<String> = assets
            .iter()
            .map(|(name, url)| format!(r#"{{"name":"{name}","browser_download_url":"{url}"}}"#))
            .collect();
        format!(r#"{{"tag_name":"{tag}","assets":[{}]}}"#, listed.join(","))
    }

    #[test]
    fn one_matching_asset_resolves_to_its_url_and_tag() {
        let body = release(
            "v6.8.0",
            &[
                ("ReShade_Setup_6.8.0.exe", "https://example.com/plain.exe"),
                (
                    "ReShade_Setup_6.8.0_Addon.exe",
                    "https://example.com/addon.exe",
                ),
            ],
        );
        let found = pick_release_asset(&body, "_Addon.exe", "org/repo").expect("resolved");
        assert_eq!(found.url, "https://example.com/addon.exe");
        assert_eq!(found.version, "v6.8.0");
    }

    #[test]
    fn an_ambiguous_suffix_is_refused_rather_than_guessed() {
        // The failure this prevents is quiet: two assets match, the first is
        // installed, and it is the wrong architecture or the wrong variant.
        let body = release(
            "v1.0",
            &[
                ("tool-win64.zip", "https://example.com/a.zip"),
                ("tool-win32.zip", "https://example.com/b.zip"),
            ],
        );
        let refused = pick_release_asset(&body, ".zip", "org/repo").expect_err("ambiguous");
        assert_eq!(refused.code, Code::DownloadRejected);
        assert!(refused.detail.contains("tool-win64.zip"), "{refused:?}");
        assert!(refused.detail.contains("tool-win32.zip"), "{refused:?}");
    }

    #[test]
    fn a_release_with_no_matching_asset_says_so() {
        let body = release("v1.0", &[("notes.txt", "https://example.com/notes.txt")]);
        let refused = pick_release_asset(&body, ".zip", "org/repo").expect_err("no match");
        assert_eq!(refused.code, Code::DownloadRejected);
        assert!(refused.detail.contains(".zip"), "{refused:?}");
    }

    #[test]
    fn an_asset_url_that_is_not_https_is_refused() {
        // GitHub serves HTTPS, but the URL comes from a remote document and is
        // used to fetch bytes onto a user's disk. It gets the same check as
        // anything else that arrives over the network.
        let body = release("v1.0", &[("tool.zip", "http://example.com/tool.zip")]);
        let refused = pick_release_asset(&body, ".zip", "org/repo").expect_err("plain http");
        assert_eq!(refused.code, Code::DownloadRejected);
    }

    #[test]
    fn malformed_metadata_is_a_rejection_rather_than_a_panic() {
        for body in ["", "not json", "{}", r#"{"tag_name":"v1"}"#] {
            let refused = pick_release_asset(body, ".zip", "org/repo").expect_err(body);
            assert_eq!(refused.code, Code::DownloadRejected, "{body}");
        }
    }

    #[test]
    fn a_pinned_source_is_named_by_its_file() {
        assert_eq!(
            file_name_of("https://example.com/downloads/ReShade_6.8.0.exe"),
            "ReShade_6.8.0.exe"
        );
        // A URL ending in a slash has no file name to take, and the whole URL
        // is a better answer than an empty string.
        assert_eq!(file_name_of("https://example.com/"), "https://example.com/");
    }
}
