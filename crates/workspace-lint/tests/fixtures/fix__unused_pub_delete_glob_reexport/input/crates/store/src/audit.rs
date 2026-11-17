/// Dead, but kept by its hand-written trait impl — and so it keeps the glob
/// non-empty.
pub struct Audited;

impl core::fmt::Display for Audited {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("audited")
    }
}

/// Dead — deleted: the kept `Audited` still sits behind the glob.
pub struct Stale;
