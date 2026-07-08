pub struct ExtEvent;

impl ExtEvent {
    pub fn dispatch() {}
}

/// The `tracing::debug!` shape: an exported macro whose expansion calls into
/// its own crate via `$crate`.
#[macro_export]
macro_rules! ext_event {
    () => {
        $crate::ExtEvent::dispatch()
    };
}
