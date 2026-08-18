#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Visibility {
    #[default]
    Hidden,
    Internal,
    Exposed,
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Visibility::Hidden => "hidden",
            Visibility::Internal => "internal",
            Visibility::Exposed => "exposed",
        })
    }
}
