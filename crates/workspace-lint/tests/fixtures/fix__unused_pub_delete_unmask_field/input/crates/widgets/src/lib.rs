pub struct Panel {
    open: bool,
}

impl Panel {
    pub fn new() -> Self {
        Self { open: true }
    }
}

/// Unused, but it holds the last READ of `Panel.open` — deleting it would
/// trip `dead_code` on the surviving field. The unmask veto keeps it.
pub fn open_state(panel: &Panel) -> bool {
    panel.open
}

/// Mentioned nowhere, no unmask consequence — deleted.
pub fn dead_helper() -> i32 {
    0
}
