mod gadget;
/// Used by `app`; its only method is dead — the emptied impl goes too.
pub struct Live;

/// Used by `app`; keeps `kept`, loses `dropped`, the block stays.
pub struct Mixed;

impl Mixed {
    pub fn kept(&self) -> u32 {
        3
    }

}

/// Reached only through its associated const — stays.
pub struct Port;

impl Port {
    pub const DEFAULT: u16 = 80;
}

pub fn make_live() -> Live {
    Live
}

pub fn make_mixed() -> Mixed {
    Mixed
}
