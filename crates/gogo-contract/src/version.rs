//! The contract version (design 26 §8).

use std::fmt;
use std::str::FromStr;

/// A contract version, `MAJOR.MINOR`. A skin's `contract "1.2"` means "needs 1.2 or
/// newer, before 2.0".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ContractVersion {
    pub major: u16,
    pub minor: u16,
}

impl ContractVersion {
    /// The version this app implements.
    pub const CURRENT: ContractVersion = ContractVersion { major: 1, minor: 0 };

    /// Until the first GUI release the contract is "1.0 (draft)" and may still change
    /// (§8, "The draft label").
    pub const DRAFT: bool = true;

    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Whether an app implementing `self` can load a skin that declares `skin`: the same
    /// major, and a minor no newer than ours (minor versions only add). Skins of the
    /// previous major go through an adapter, which doesn't exist before 2.0.
    pub fn accepts(self, skin: ContractVersion) -> bool {
        skin.major == self.major && skin.minor <= self.minor
    }
}

impl fmt::Display for ContractVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A contract version string that isn't `MAJOR.MINOR`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("contract version `{input}` is not MAJOR.MINOR (for example \"1.0\")")]
pub struct VersionError {
    pub input: String,
}

impl FromStr for ContractVersion {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || VersionError {
            input: s.to_owned(),
        };
        let (major, minor) = s.split_once('.').ok_or_else(err)?;
        let num = |p: &str| {
            if p.is_empty()
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
            {
                return None;
            }
            p.parse::<u16>().ok()
        };
        match (num(major), num(minor)) {
            (Some(major), Some(minor)) => Ok(Self { major, minor }),
            _ => Err(err()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_major_minor() {
        assert_eq!("1.0".parse(), Ok(ContractVersion::new(1, 0)));
        assert_eq!("1.12".parse(), Ok(ContractVersion::new(1, 12)));
        for bad in [
            "1", "1.", ".1", "1.0.0", "v1.0", "1.-1", "01.0", "1.0 ", "", "1.x",
        ] {
            assert!(bad.parse::<ContractVersion>().is_err(), "{bad}");
        }
    }

    #[test]
    fn accepts_same_major_up_to_our_minor() {
        let app = ContractVersion::new(1, 2);
        assert!(app.accepts(ContractVersion::new(1, 0)));
        assert!(app.accepts(ContractVersion::new(1, 2)));
        assert!(!app.accepts(ContractVersion::new(1, 3)));
        assert!(!app.accepts(ContractVersion::new(2, 0)));
        assert!(!app.accepts(ContractVersion::new(0, 9)));
    }

    #[test]
    fn current_is_the_1_0_draft() {
        assert_eq!(ContractVersion::CURRENT.to_string(), "1.0");
        const { assert!(ContractVersion::DRAFT) };
    }
}
