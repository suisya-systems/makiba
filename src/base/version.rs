//! The one herdr-core release makiba runs against. The pin moves on purpose and rarely:
//! see docs/upgrading-the-base.md. `scripts/install-base.sh` reads `PINNED` from this file.

use super::{BaseError, BaseInfo};

/// The pinned release: the one the design was read against and the probes were run on.
pub const PINNED: &str = "0.1.22";
/// The supported range, as written in messages. One patch release (docs/upgrading-the-base.md).
pub const SUPPORTED: &str = ">=0.1.22,<0.1.23";

/// Parse `major.minor.patch` and nothing else: a pre-release or build suffix is refused.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().split('.').map(|p| {
        let digits = !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
        digits.then(|| p.parse::<u64>().ok()).flatten()
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// Accept `found` only if it is the pinned release.
pub fn check(found: &str) -> Result<(), BaseError> {
    if parse(found) == parse(PINNED) {
        return Ok(());
    }
    Err(BaseError::UnsupportedVersion {
        found: found.to_string(),
        supported: SUPPORTED.to_string(),
    })
}

/// How `found` compares with `PINNED`, or `None` when `found` is not plain `major.minor.patch`
/// (a pre-release or build suffix, or anything else `parse` refuses).
pub fn compare_with_pinned(found: &str) -> Option<std::cmp::Ordering> {
    Some(parse(found)?.cmp(&parse(PINNED)?))
}

/// Run `hello` and refuse its answer unless the version is supported. Callers use `?` on the
/// result, so no other call is made on an unsupported base. M0-3 wires `Base::hello()` to this.
pub fn checked_hello(
    hello: impl FnOnce() -> Result<BaseInfo, BaseError>,
) -> Result<BaseInfo, BaseError> {
    let info = hello()?;
    check(&info.version)?;
    Ok(info)
}
