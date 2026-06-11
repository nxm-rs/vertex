//! Randomness facade backed by the operating system CSPRNG.
//!
//! Every accessor here seeds from `rand`'s `OsRng`, which routes to `getrandom`
//! and works under the `getrandom_backend="wasm_js"` configuration in the
//! browser. Nothing here touches `rand::rng()` or a thread-local generator, so
//! the facade is safe on `wasm32` where thread-local entropy sourcing is
//! fragile.
//!
//! Pick the right accessor for the job:
//!
//! - [`fill_bytes`] and [`crypto_rng`] are cryptographically secure. Use them
//!   for key material, identifiers, nonces, and anything an adversary must not
//!   predict.
//! - [`non_crypto_rng`] is a fast, non-cryptographic PRNG. Use it for shuffles,
//!   jitter, and sampling where predictability is not a security concern. Do
//!   not use it for secrets.
//!
//! The infallible helpers panic only if the operating system entropy source
//! fails, which on a healthy host does not happen. Use the `try_*` variants
//! when a caller must handle that failure instead of aborting.

use rand::{
    CryptoRng, RngCore, SeedableRng, TryRngCore,
    rngs::{OsRng, SmallRng},
};
use rand_chacha::ChaCha20Rng;

/// Error returned when the operating system entropy source cannot be read.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum RngError {
    /// The OS CSPRNG (`getrandom`) failed to produce entropy.
    #[error("operating system entropy source unavailable: {0}")]
    OsEntropyUnavailable(#[from] rand::rand_core::OsError),
}

/// Fills `dst` with cryptographically secure random bytes.
///
/// Use this for key and identifier material. Panics if the operating system
/// entropy source fails; call [`try_fill_bytes`] to handle that case instead.
#[inline]
pub fn fill_bytes(dst: &mut [u8]) {
    OsRng.unwrap_err().fill_bytes(dst);
}

/// Fills `dst` with cryptographically secure random bytes, surfacing entropy
/// failures as a [`RngError`] rather than panicking.
#[inline]
pub fn try_fill_bytes(dst: &mut [u8]) -> Result<(), RngError> {
    OsRng.try_fill_bytes(dst)?;
    Ok(())
}

/// Returns a fresh cryptographically secure RNG seeded from OS entropy.
///
/// The returned generator implements [`CryptoRng`], which requires [`RngCore`],
/// so it is suitable for key generation and other secret material. Panics if
/// the operating system entropy source fails; use [`try_crypto_rng`] to handle
/// that case instead.
#[inline]
pub fn crypto_rng() -> impl CryptoRng {
    ChaCha20Rng::from_rng(&mut OsRng.unwrap_err())
}

/// Returns a fresh cryptographically secure RNG seeded from OS entropy,
/// surfacing entropy failures as a [`RngError`] rather than panicking.
#[inline]
pub fn try_crypto_rng() -> Result<impl CryptoRng, RngError> {
    Ok(ChaCha20Rng::try_from_rng(&mut OsRng)?)
}

/// Returns a fresh fast, non-cryptographic RNG seeded from OS entropy.
///
/// Use this for shuffles, jitter, and sampling. The output is not
/// cryptographically secure and must not seed secrets. Panics if the operating
/// system entropy source fails.
#[inline]
pub fn non_crypto_rng() -> impl RngCore {
    SmallRng::from_rng(&mut OsRng.unwrap_err())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_bytes_changes_buffer() {
        let mut a = [0u8; 32];
        fill_bytes(&mut a);
        assert_ne!(a, [0u8; 32], "32 zero bytes from the CSPRNG is implausible");
    }

    #[test]
    fn fill_bytes_is_not_constant() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        fill_bytes(&mut a);
        fill_bytes(&mut b);
        assert_ne!(a, b, "two CSPRNG draws should differ");
    }

    #[test]
    fn try_fill_bytes_succeeds() {
        let mut a = [0u8; 16];
        try_fill_bytes(&mut a).expect("OS entropy should be available in tests");
        assert_ne!(a, [0u8; 16]);
    }

    #[test]
    fn crypto_rng_produces_distinct_streams() {
        let mut a = crypto_rng();
        let mut b = crypto_rng();
        assert_ne!(
            a.next_u64(),
            b.next_u64(),
            "independently seeded crypto RNGs should diverge"
        );
    }

    #[test]
    fn try_crypto_rng_succeeds() {
        let mut rng = try_crypto_rng().expect("OS entropy should be available in tests");
        // Exercise the generator so the accessor is not optimised away.
        let _ = rng.next_u64();
    }

    #[test]
    fn non_crypto_rng_produces_distinct_streams() {
        let mut a = non_crypto_rng();
        let mut b = non_crypto_rng();
        assert_ne!(
            a.next_u64(),
            b.next_u64(),
            "independently seeded non-crypto RNGs should diverge"
        );
    }
}
