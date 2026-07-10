enum Mode {
    Normal,
    Fast,
}

pub struct Engine {
    mode: Mode,
}

impl Engine {
    pub fn new() -> Self {
        Self { mode: Mode::Normal }
    }

    /// Unused, but the ONLY construction of `Mode::Fast` — deleting it would
    /// trip rustc `dead_code` ("variant is never constructed") on the
    /// surviving enum. The unmask veto keeps it.
    pub fn boost(&mut self) {
        self.mode = Mode::Fast;
    }

    pub fn describe(&self) -> &'static str {
        match self.mode {
            Mode::Normal => "normal",
            Mode::Fast => "fast",
        }
    }
}

/// Mentioned nowhere, no unmask consequence — deleted.
pub fn dead_helper() -> i32 {
    0
}
