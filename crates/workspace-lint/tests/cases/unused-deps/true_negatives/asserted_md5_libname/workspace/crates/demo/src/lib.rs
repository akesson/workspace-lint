use md5::Md5;

// Package `md-5` (normalized `md_5`) ships a lib target named `md5`, so the only
// reference is to `md5`. The md5-libname separator-insensitive fallback matches.
pub fn hasher() -> Md5 {
    Md5::default()
}
