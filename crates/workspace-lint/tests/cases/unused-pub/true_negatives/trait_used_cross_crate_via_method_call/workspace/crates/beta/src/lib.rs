use alpha::Shout;

// Private on purpose: the case is about `alpha::Shout`, and a pub fn here
// would itself be an unused-pub finding. The method call still produces the
// cross-crate member edge that must keep the trait pub.
#[allow(dead_code)]
fn demo() -> String {
    "hey".shout()
}
