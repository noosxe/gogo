//! The value types a skin sees (design 26 §2).

/// A type a source field can have, with the helpers a skin may call on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ValueType {
    /// A display string, already localised.
    Text,
    /// A byte count (u64).
    Bytes,
    /// An instant.
    Time,
    /// A span of time.
    Duration,
    /// 0.0–1.0.
    Fraction,
    /// A numeric GOG id. Opaque in skins: only passed to actions.
    GameId,
    /// Image handles (`cover`, `background`, `logo`) and two colours. Used by the `art`
    /// and `image` elements only; never a URL.
    Art,
    /// A [`Status`].
    Status,
    /// An [`Os`].
    Os,
    /// A value, or `•••` with `secret: true`. The clear value is never in the binding
    /// scope (S2).
    Masked,
    /// An ordered, virtualised list.
    List,
    /// A value that may be null.
    Option,
}

impl ValueType {
    pub const ALL: &[ValueType] = &[
        Self::Text,
        Self::Bytes,
        Self::Time,
        Self::Duration,
        Self::Fraction,
        Self::GameId,
        Self::Art,
        Self::Status,
        Self::Os,
        Self::Masked,
        Self::List,
        Self::Option,
    ];

    /// The name the doc and skin authors use.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Bytes => "Bytes",
            Self::Time => "Time",
            Self::Duration => "Duration",
            Self::Fraction => "Fraction",
            Self::GameId => "GameId",
            Self::Art => "Art",
            Self::Status => "Status",
            Self::Os => "Os",
            Self::Masked => "Masked<T>",
            Self::List => "List<T>",
            Self::Option => "Option<T>",
        }
    }

    /// The helpers and operators a skin may use on a value of this type, as written in
    /// expressions (`fmt.bytes(x)`, `?.`).
    pub const fn helpers(self) -> &'static [&'static str] {
        match self {
            Self::Text => &["upper", "lower", "trim n"],
            Self::Bytes => &["fmt.bytes(x)", "fmt.rate(x)"],
            Self::Time => &["fmt.date", "fmt.ago"],
            Self::Duration => &["fmt.hours", "fmt.eta"],
            Self::Fraction => &["fmt.pct"],
            Self::Status => &["status.word(s)", "status.tone(s)"],
            Self::List => &["where", "sort", "take", "len", "first"],
            Self::Option => &["?.", "else"],
            Self::GameId | Self::Art | Self::Os | Self::Masked => &[],
        }
    }
}

/// A game's status (design 26 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Status {
    Ready,
    Running,
    Downloading,
    SettingUp,
    UpdateAvailable,
    Failed,
    NotInstalled,
    Queued,
    Paused,
}

impl Status {
    pub const ALL: &[Status] = &[
        Self::Ready,
        Self::Running,
        Self::Downloading,
        Self::SettingUp,
        Self::UpdateAvailable,
        Self::Failed,
        Self::NotInstalled,
        Self::Queued,
        Self::Paused,
    ];

    /// The name in expressions and the doc.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Downloading => "downloading",
            Self::SettingUp => "setting_up",
            Self::UpdateAvailable => "update_available",
            Self::Failed => "failed",
            Self::NotInstalled => "not_installed",
            Self::Queued => "queued",
            Self::Paused => "paused",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.name() == name)
    }

    /// The word it shows as: what `status.word(s)` returns (design 26 §2).
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Running => "Running",
            Self::Downloading => "Downloading",
            Self::SettingUp => "Setting up",
            Self::UpdateAvailable => "Update available",
            Self::Failed => "Failed",
            Self::NotInstalled => "Not installed",
            Self::Queued => "Queued",
            Self::Paused => "Paused",
        }
    }

    /// Its tone: the colour token `status.tone(s)` returns (design 26 §6.1).
    pub const fn tone(self) -> &'static str {
        match self {
            Self::Ready => "tone.ready",
            Self::Running => "tone.running",
            Self::Downloading => "tone.downloading",
            Self::SettingUp => "tone.setting_up",
            Self::UpdateAvailable => "tone.update",
            Self::Failed => "tone.failed",
            Self::NotInstalled => "tone.not_installed",
            Self::Queued => "tone.queued",
            Self::Paused => "tone.paused",
        }
    }

    /// The token used when a skin doesn't define [`Status::tone`]: `text-lo` for queued
    /// and `warn` for paused. A skin must define the other tones.
    pub const fn tone_fallback(self) -> Option<&'static str> {
        match self {
            Self::Queued => Some("text-lo"),
            Self::Paused => Some("warn"),
            _ => None,
        }
    }
}

/// An operating system a game is built for (design 26 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    pub const ALL: &[Os] = &[Self::Windows, Self::Mac, Self::Linux];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Mac => "mac",
            Self::Linux => "linux",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|o| o.name() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for s in Status::ALL {
            assert_eq!(Status::from_name(s.name()), Some(*s));
        }
        for o in Os::ALL {
            assert_eq!(Os::from_name(o.name()), Some(*o));
        }
        assert_eq!(Status::from_name("installed"), None);
    }

    #[test]
    fn every_status_has_its_own_tone() {
        let tones: std::collections::HashSet<_> = Status::ALL.iter().map(|s| s.tone()).collect();
        assert_eq!(tones.len(), Status::ALL.len());
        for s in Status::ALL {
            assert!(crate::tokens::COLOURS.contains(&s.tone()), "{}", s.tone());
            if let Some(f) = s.tone_fallback() {
                assert!(crate::tokens::is_semantic(f), "{f}");
            }
        }
        let tone_tokens = crate::tokens::COLOURS
            .iter()
            .filter(|t| t.starts_with("tone."))
            .count();
        assert_eq!(
            tone_tokens,
            Status::ALL.len(),
            "a tone token without a status"
        );
    }
}
