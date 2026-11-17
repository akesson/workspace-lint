use derives::Stringify;

/// Dead: only derives, one of them custom — deleted with them.
#[derive(Debug, Clone, Stringify)]
pub enum Size {
    Small,
    Large,
}

/// Dead: a custom derive plus an empty hand-written impl — both go.
#[derive(Stringify)]
pub enum ReportType {
    Daily,
}

impl ReportType {}

/// Used by `app` — stays, and keeps the `derives` import alive.
#[derive(Stringify)]
pub enum FileType {
    Pdf,
}
