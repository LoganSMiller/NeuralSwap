//! The component service: the catalogue, the on-disk cache, and the fetcher
//! that fills it.
//!
//! Kept beside [`crate::installer`] and for the same reason - the command
//! layer stays a thin validated boundary, and nothing in here knows about
//! Tauri.
//!
//! The catalogue is validated once, at construction. It is static data
//! compiled into the binary, so a licence rule it breaks is a bug in this
//! build rather than something a user can cause, and finding out at startup is
//! better than finding out during an install.

use std::path::{Path, PathBuf};

use neuralswap_core::components::catalog::{default_catalog, Catalog, Component};
use neuralswap_core::components::store::{Freshness, Outcome, Progress, Store};
use neuralswap_core::error::{fail, Code, Error, Result};
use neuralswap_core::jobs::{Cancel, KeyedLock};

use crate::fetch::HttpFetcher;

pub struct Components {
    catalog: Catalog,
    store: Store,
    fetcher: HttpFetcher,
    /// One fetch per component at a time. Two downloads of the same archive
    /// would race on the same cache path, and the second would be doing work
    /// the first is already doing.
    locks: KeyedLock,
}

impl Components {
    pub fn new(data_dir: &Path) -> Result<Components> {
        let catalog = default_catalog();
        // Static data, so a failure here is a build that should not have
        // shipped - but it is checked rather than assumed, because the whole
        // point of encoding licences as data is that something enforces them.
        catalog.validate()?;

        Ok(Components {
            catalog,
            store: Store::new(&Self::root(data_dir)),
            fetcher: HttpFetcher::new()?,
            locks: KeyedLock::new(),
        })
    }

    fn root(data_dir: &Path) -> PathBuf {
        data_dir.join("components")
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn get(&self, id: &str) -> Result<&Component> {
        self.catalog
            .get(id)
            .ok_or_else(|| Error::new(Code::BadRequest, format!("no component called {id}")))
    }

    /// Make a component available, fetching it if it is not already cached.
    ///
    /// Blocking, and holds that component's lock for the duration.
    pub fn ensure(
        &self,
        id: &str,
        cancel: &Cancel,
        freshness: Freshness,
        progress: &Progress<'_>,
    ) -> Result<Outcome> {
        let component = self.get(id)?;
        let Some(_guard) = self.locks.try_acquire(id) else {
            return fail(Code::JobBusy, format!("{id} is already being fetched"));
        };
        self.store
            .ensure(component, &self.fetcher, cancel, freshness, progress)
    }

    pub fn is_busy(&self, id: &str) -> bool {
        self.locks.is_busy(id)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_catalogue_is_valid() {
        // The licence rules are data, and this is the thing that enforces
        // them. If it ever fails, a build is about to redistribute something
        // it has no right to.
        let dir = tempfile::tempdir().expect("tempdir");
        Components::new(dir.path()).expect("the shipped catalogue validates");
    }

    #[test]
    fn an_unknown_component_is_a_bad_request_rather_than_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let components = Components::new(dir.path()).expect("catalogue");
        let refused = components.get("no-such-component").expect_err("unknown");
        assert_eq!(refused.code, Code::BadRequest);
    }

    #[test]
    fn a_component_that_is_not_downloadable_says_why_without_a_network() {
        // Bundled, local-only and user-obtained components resolve to an
        // explanation rather than an attempt, so this runs offline and must
        // never reach the fetcher.
        let dir = tempfile::tempdir().expect("tempdir");
        let components = Components::new(dir.path()).expect("catalogue");
        let cancel = Cancel::new();
        let quiet: &Progress<'_> = &|_, _| {};

        let mut explained = 0;
        for component in components.catalog().components.values() {
            if component.source.trust() != neuralswap_core::components::catalog::Trust::UserSupplied
                && !component.source.we_redistribute()
            {
                continue;
            }
            let outcome = components
                .ensure(&component.id, &cancel, Freshness::UseCache, quiet)
                .expect("an explanation, not an error");
            match outcome {
                Outcome::NotFetched { reason, .. } => {
                    assert!(!reason.is_empty(), "{}", component.id);
                    explained += 1;
                }
                other => panic!("{} tried to fetch: {other:?}", component.id),
            }
        }
        assert!(explained > 0, "no offline components in the catalogue");
    }
}
