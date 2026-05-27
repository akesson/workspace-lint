use provider::Thing;

fn touch() -> Thing {
    Thing
}

const _: Option<fn() -> Thing> = Some(touch);
