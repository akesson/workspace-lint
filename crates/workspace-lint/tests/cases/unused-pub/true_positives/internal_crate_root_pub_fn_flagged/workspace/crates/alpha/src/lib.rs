pub fn unreferenced_function() {
    let _ = 42;
}

pub(crate) fn internal_only() {
    let _ = 1;
}

fn private_helper() {
    let _ = 2;
}
