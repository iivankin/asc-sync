use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Developer,
    Release,
}

impl Scope {
    pub const ALL: [Self; 2] = [Self::Developer, Self::Release];

    pub fn bundle_segment(self) -> &'static str {
        match self {
            Self::Developer => "developer",
            Self::Release => "release",
        }
    }

    pub fn from_bundle_segment(segment: &str) -> Option<Self> {
        match segment {
            "developer" => Some(Self::Developer),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    pub fn owns_bundle_ids(self) -> bool {
        matches!(self, Self::Developer)
    }

    pub fn owns_devices(self) -> bool {
        matches!(self, Self::Developer)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Developer => write!(f, "developer"),
            Self::Release => write!(f, "release"),
        }
    }
}
