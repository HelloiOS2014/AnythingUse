//! macOS layer over the shared AnythingUse core.
//!
//! All platform-neutral contracts live in [`anything_core`] and are re-exported
//! here unchanged, so every historical `lcu_core::` path keeps working with
//! zero import churn. This crate adds the **macOS evidence guard**
//! ([`StaticEffectGuard`]) and the macOS security-set tests. Other endpoints
//! (`lau` on Android) must not depend on this crate; they implement their own
//! evidence layer on `anything_core` (see `docs/lau-android-plan.md` §7).

pub use anything_core::*;

pub mod effect_guard;
pub mod security_set;

pub use effect_guard::StaticEffectGuard;
