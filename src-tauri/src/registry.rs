//! The real Windows registry, behind the core's [`LayerRegistry`] trait.
//!
//! The core keeps this behind a trait so its reference counting and its
//! leave-somebody-else's-registration-alone rule can be tested without a
//! machine - and, just as importantly, so the test suite cannot modify the
//! developer's own Vulkan setup by accident. This is the other side.
//!
//! `HKCU` rather than `HKLM`: registering an implicit layer needs no
//! administrator there, and it scopes the change to the person who asked for
//! it rather than to everyone with an account on the machine.

use neuralswap_core::error::{fail, Code, Result};
use neuralswap_core::install::layer::{LayerRegistry, REGISTRY_KEY};

pub struct WindowsRegistry;

/// The Vulkan loader reads the value's data as a `DWORD` where zero means the
/// layer is enabled. Anything else disables it.
const ENABLED: u32 = 0;

#[cfg(windows)]
impl LayerRegistry for WindowsRegistry {
    fn values(&self) -> Result<Vec<String>> {
        // A key that does not exist is an empty list, not a failure: nothing
        // has ever registered an implicit layer on this account, which is the
        // normal state of a fresh machine.
        let Ok(key) = windows_registry::CURRENT_USER.open(REGISTRY_KEY) else {
            return Ok(Vec::new());
        };
        Ok(key
            .values()
            .map(|values| values.map(|(name, _)| name).collect())
            .unwrap_or_default())
    }

    fn add(&self, value: &str) -> Result<()> {
        let key = windows_registry::CURRENT_USER
            .create(REGISTRY_KEY)
            .map_err(|error| {
                neuralswap_core::error::Error::new(
                    Code::StateUnwritable,
                    format!("could not open {REGISTRY_KEY}: {error}"),
                )
            })?;
        key.set_u32(value, ENABLED).map_err(|error| {
            neuralswap_core::error::Error::new(
                Code::StateUnwritable,
                format!("could not register the Vulkan layer {value}: {error}"),
            )
        })
    }

    fn remove(&self, value: &str) -> Result<()> {
        let Ok(key) = windows_registry::CURRENT_USER.open(REGISTRY_KEY) else {
            // No key means no registration, which is the state we were asked
            // to reach.
            return Ok(());
        };
        match key.remove_value(value) {
            Ok(()) => Ok(()),
            // Already gone. Undo is idempotent on purpose: recovery runs it
            // again after a crash, and a second run must not fail.
            Err(error) if error.code().is_ok() => Ok(()),
            Err(error) => fail(
                Code::StateUnwritable,
                format!("could not remove the Vulkan layer {value}: {error}"),
            ),
        }
    }
}

#[cfg(not(windows))]
impl LayerRegistry for WindowsRegistry {
    fn values(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn add(&self, _value: &str) -> Result<()> {
        fail(Code::BadRequest, "Vulkan layers are registered on Windows")
    }
    fn remove(&self, _value: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn reading_the_layer_key_never_fails() {
        // Runs against this machine's real registry, and reads only. A fresh
        // account has no such key at all, which must read as "no layers"
        // rather than as an error - a scan has to survive it either way.
        //
        // Nothing here writes. The reference counting and the
        // do-not-touch-a-foreign-registration rule are tested against a fake
        // in the core, precisely so no test can alter a developer's own Vulkan
        // setup.
        let found = WindowsRegistry.values().expect("reading must not fail");
        for value in &found {
            assert!(!value.is_empty());
        }
    }
}
