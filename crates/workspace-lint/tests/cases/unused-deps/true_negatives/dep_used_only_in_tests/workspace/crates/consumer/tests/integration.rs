// Aux-dir consumer — exercises the tests/ scan added to Workspace::load.
use provider::provided;

#[test]
fn provider_callable() {
    provided();
}
