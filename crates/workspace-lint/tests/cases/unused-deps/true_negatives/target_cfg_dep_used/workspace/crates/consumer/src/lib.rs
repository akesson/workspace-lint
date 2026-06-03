// References `provider` only under a platform gate; the resolver still sees it.
#[cfg(windows)]
pub fn use_it() {
    provider::hello();
}
