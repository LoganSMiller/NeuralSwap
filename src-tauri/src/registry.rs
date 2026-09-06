//! The real Windows registry, behind the core's [`LayerRegistry`] trait.
//!
//! The core keeps this behind a trait so its reference counting and its
//! leave-somebody-else's-registration-alone rule can be tested without a
//! machine - and, just as importantly, so the test suite cannot modify the
//! developer's own Vulkan setup by accident. This is the other side.
//!
//! Writes go to `HKCU`: registering an implicit layer needs no administrator
//! there, and it scopes the change to the person who asked for it rather than
//! to everyone with an account on the machine.
//!
//! Reads come from **both** hives. ReShade's own installer writes `HKLM` when
//! it is run elevated, so a reader that looks only at `HKCU` misses a
//! registration the user already has - and then adds a second one beside it.

use neuralswap_core::error::{fail, Code, Result};
use neuralswap_core::install::layer::{LayerRegistry, Registration, REGISTRY_KEY};

pub struct WindowsRegistry;

/// The Vulkan loader reads the value's data as a `DWORD` where zero means the
/// layer is enabled. Anything else disables it.
const ENABLED: u32 = 0;

#[cfg(windows)]
impl LayerRegistry for WindowsRegistry {
    fn values(&self) -> Result<Vec<Registration>> {
        // **Both hives.** `HKCU` is where this application registers, but
        // ReShade's own installer writes `HKLM` when it is run elevated - so
        // a reader that looks only at `HKCU` misses a registration the user
        // already has, stands down for nothing, and then adds a second one
        // beside it.
        //
        // A key that does not exist is an empty list rather than a failure:
        // nothing has ever registered an implicit layer, which is the normal
        // state of a fresh machine.
        let mut found = Vec::new();
        for (root, machine_wide) in [
            (windows_registry::CURRENT_USER, false),
            (windows_registry::LOCAL_MACHINE, true),
        ] {
            let Ok(key) = root.open(REGISTRY_KEY) else {
                continue;
            };
            let Ok(values) = key.values() else {
                continue;
            };
            for (name, _) in values {
                // The loader reads the data as a `DWORD` and **only zero means
                // enabled**. Anything else is registered but switched off, and
                // treating one of those as working is how an install finishes,
                // reports success, and leaves the game with no injector.
                let enabled = key.get_u32(&name).is_ok_and(|value| value == 0);
                found.push(Registration {
                    value: name,
                    enabled,
                    machine_wide,
                });
            }
        }
        Ok(found)
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
    fn values(&self) -> Result<Vec<Registration>> {
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
            assert!(!value.value.is_empty());
        }
    }
}
