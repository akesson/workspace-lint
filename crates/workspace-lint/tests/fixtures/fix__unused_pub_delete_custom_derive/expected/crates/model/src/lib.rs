mod report;

use derives::Stringify;

/// Used by `app` — stays, and keeps the `derives` import alive.
#[derive(Stringify)]
pub enum FileType {
    Pdf,
}
