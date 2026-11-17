use derives::Stringify;

/// Dead, and the module's only user of `Stringify` — the import goes too.
#[derive(Stringify)]
pub enum ReportKind {
    Weekly,
}
